//! Reversible installation of explicitly supplied DLC archives.

use crate::archive::{extract_checked, multipart_names};
use crate::catalog::{AddonKind, CatalogAddon};
use crate::jobs::{JobKind, clear_job, job_id_for_target, recorded_download_with_sha256};
use crate::sha256::file_sha256;
use crate::{Game, Result, validate_id};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlcFile {
    pub path: PathBuf,
    pub sha256: String,
    #[serde(default)]
    pub staged_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlcManifest {
    pub game_id: String,
    pub dlc_id: String,
    pub install_dir: PathBuf,
    pub files: Vec<DlcFile>,
}

/// DLC entries explicitly provided by the installed game's source plugin.
pub fn available_for_game(game: &Game, data_dir: &Path) -> Result<Vec<CatalogAddon>> {
    validate_id(&game.id)?;
    let Some(origin) = game.origin.as_ref() else {
        return Ok(Vec::new());
    };
    let Some((source, _)) = origin.qualified_id.split_once(':') else {
        return Ok(Vec::new());
    };
    if source == "steamidra" {
        return Ok(Vec::new());
    }
    if !data_dir.join("plugins").join(source).is_dir() {
        return Ok(Vec::new());
    }
    let catalog = crate::plugins::catalog_for(data_dir, source)?;
    let Some(entry) = catalog.into_iter().find(|entry| {
        entry.qualified_id() == origin.qualified_id
            || (game.steam_app_id.is_some() && entry.steam_app_id == game.steam_app_id)
    }) else {
        return Ok(Vec::new());
    };
    Ok(entry
        .addons
        .into_iter()
        .filter(|addon| addon.kind == AddonKind::Dlc)
        .collect())
}

fn state_dir(data_dir: &Path, game_id: &str, dlc_id: &str) -> PathBuf {
    data_dir.join("dlc").join(game_id).join(dlc_id)
}

pub(crate) fn lock_game(data_dir: &Path, game_id: &str) -> Result<File> {
    let parent = data_dir.join("dlc").join(game_id);
    fs::create_dir_all(&parent)?;
    let path = parent.join(".game.lock");
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("DLC lock path is a symbolic link".into());
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.lock()?;
    Ok(file)
}

fn collect_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = fs::symlink_metadata(&path)?.file_type();
        if kind.is_symlink() {
            return Err("DLC archive contains a symbolic link".into());
        }
        if kind.is_dir() {
            collect_files(root, &path, files)?;
        } else if kind.is_file() {
            files.push(path.strip_prefix(root)?.to_path_buf());
        } else {
            return Err("DLC archive contains an unsupported file".into());
        }
    }
    Ok(())
}

fn check_target(install: &Path, relative: &Path) -> Result<PathBuf> {
    let mut target = install.to_path_buf();
    let parts: Vec<_> = relative.components().collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("DLC file path escapes the game directory".into());
    }
    for (index, part) in parts.iter().enumerate() {
        target.push(part);
        match fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("DLC target is a symbolic link: {}", target.display()).into());
            }
            Ok(meta) if index + 1 < parts.len() && meta.is_dir() => {}
            Ok(meta) if index + 1 == parts.len() && meta.is_file() => {}
            Ok(_) => return Err("DLC target has an incompatible file type".into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(target)
}

fn read_manifest(state: &Path, game: &Game, dlc_id: &str) -> Result<DlcManifest> {
    let metadata = fs::symlink_metadata(state)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("DLC state is not a regular directory".into());
    }
    let manifest_path = state.join("manifest.json");
    let manifest_meta = fs::symlink_metadata(&manifest_path)?;
    if !manifest_meta.is_file() || manifest_meta.file_type().is_symlink() {
        return Err("DLC manifest is not a regular file".into());
    }
    let mut manifest: DlcManifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    let install = fs::canonicalize(&game.install_dir)?;
    if manifest.game_id != game.id
        || manifest.dlc_id != dlc_id
        || (!manifest.install_dir.as_os_str().is_empty() && manifest.install_dir != install)
    {
        return Err("DLC state does not match this game installation".into());
    }
    manifest.install_dir = install;
    Ok(manifest)
}

/// Convert legacy absolute-path manifests to relocatable records before a game move.
pub(crate) fn prepare_move(game: &Game, data_dir: &Path) -> Result<()> {
    if !list_interrupted(game, data_dir)?.is_empty() {
        return Err("Recover interrupted DLC before moving this game".into());
    }
    for id in list_installed(game, data_dir)? {
        let state = state_dir(data_dir, &game.id, &id);
        let mut manifest = read_manifest(&state, game, &id)?;
        manifest.install_dir = PathBuf::new();
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let temp = state.join(format!("manifest.json.{}.{nonce}.tmp", std::process::id()));
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        serde_json::to_writer_pretty(&mut output, &manifest)?;
        output.write_all(b"\n")?;
        output.sync_all()?;
        #[cfg(windows)]
        {
            let target = state.join("manifest.json");
            let backup = state.join(format!("manifest.json.{nonce}.bak"));
            fs::rename(&target, &backup)?;
            if let Err(error) = fs::rename(&temp, &target) {
                let _ = fs::rename(&backup, &target);
                return Err(error.into());
            }
            fs::remove_file(backup)?;
        }
        #[cfg(not(windows))]
        fs::rename(temp, state.join("manifest.json"))?;
    }
    Ok(())
}

/// List DLC records installed for a game.
pub fn list_installed(game: &Game, data_dir: &Path) -> Result<Vec<String>> {
    validate_id(&game.id)?;
    let root = data_dir.join("dlc").join(&game.id);
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if validate_id(&id).is_err() || !entry.path().join("complete").is_file() {
            continue;
        }
        read_manifest(&entry.path(), game, &id)?;
        ids.push(id);
    }
    ids.sort();
    Ok(ids)
}

/// List DLC installs that were interrupted before their completion marker was written.
pub fn list_interrupted(game: &Game, data_dir: &Path) -> Result<Vec<String>> {
    validate_id(&game.id)?;
    let root = data_dir.join("dlc").join(&game.id);
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if validate_id(&id).is_ok() && !entry.path().join("complete").exists() {
            read_manifest(&entry.path(), game, &id)?;
            ids.push(id);
        }
    }
    ids.sort();
    Ok(ids)
}

/// Install an additive DLC archive. Existing game files are never overwritten.
pub fn install_archive(
    game: &Game,
    data_dir: &Path,
    dlc_id: &str,
    archive: &Path,
) -> Result<usize> {
    validate_id(&game.id)?;
    validate_id(dlc_id)?;
    let _lock = lock_game(data_dir, &game.id)?;
    let install = fs::canonicalize(&game.install_dir)?;
    if !install.is_dir() {
        return Err("Base game installation is missing".into());
    }
    let state = state_dir(data_dir, &game.id, dlc_id);
    if fs::symlink_metadata(&state).is_ok() {
        return Err("DLC is already installed or has an interrupted installation".into());
    }
    let archive = fs::canonicalize(archive)?;
    if !archive.is_file() {
        return Err("DLC archive is missing".into());
    }
    let parent = state.parent().ok_or("invalid DLC state directory")?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staging = parent.join(format!(".{dlc_id}-{}-{nonce}", std::process::id()));
    fs::create_dir(&staging)?;
    let result = (|| -> Result<usize> {
        let payload = staging.join("payload");
        fs::create_dir(&payload)?;
        extract_checked(&archive, &payload)?;
        let mut files = Vec::new();
        collect_files(&payload, &payload, &mut files)?;
        files.sort();
        if files.is_empty() {
            return Err("DLC archive contains no files".into());
        }
        let mut entries = Vec::with_capacity(files.len());
        for relative in files {
            let target = check_target(&install, &relative)?;
            if fs::symlink_metadata(&target).is_ok() {
                return Err(format!(
                    "DLC would overwrite an existing game file: {}",
                    relative.display()
                )
                .into());
            }
            let name = relative
                .file_name()
                .ok_or("DLC file has no name")?
                .to_string_lossy();
            let staged_path =
                relative.with_file_name(format!(".{name}.azdl-{dlc_id}-{nonce}.partial"));
            if fs::symlink_metadata(check_target(&install, &staged_path)?).is_ok() {
                return Err("DLC staging path already exists".into());
            }
            entries.push(DlcFile {
                sha256: file_sha256(&payload.join(&relative))?,
                path: relative,
                staged_path,
            });
        }
        let manifest = DlcManifest {
            game_id: game.id.clone(),
            dlc_id: dlc_id.to_string(),
            install_dir: PathBuf::new(),
            files: entries,
        };
        let mut output = File::create(staging.join("manifest.json"))?;
        serde_json::to_writer_pretty(&mut output, &manifest)?;
        output.write_all(b"\n")?;
        output.sync_all()?;
        fs::rename(&staging, &state)?;
        for entry in &manifest.files {
            let target = check_target(&install, &entry.path)?;
            let staged_target = check_target(&install, &entry.staged_path)?;
            if fs::symlink_metadata(&target).is_ok() {
                return Err("DLC target changed during installation".into());
            }
            fs::create_dir_all(target.parent().ok_or("invalid DLC target")?)?;
            let mut source = File::open(state.join("payload").join(&entry.path))?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged_target)?;
            if let Err(error) = io::copy(&mut source, &mut output).and_then(|_| output.sync_all()) {
                drop(output);
                let _ = fs::remove_file(&staged_target);
                return Err(error.into());
            }
            drop(output);
            if fs::symlink_metadata(&target).is_ok() {
                return Err("DLC target changed during installation".into());
            }
            fs::rename(&staged_target, &target)?;
        }
        fs::remove_dir_all(state.join("payload"))?;
        File::create(state.join("complete"))?.sync_all()?;
        Ok(manifest.files.len())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
        if state.is_dir() {
            let _ = recover_unlocked(game, data_dir, dlc_id);
        }
    }
    result
}

/// Download an explicit provider DLC archive and install it into its base game.
pub fn install_catalog_dlc(game: &Game, data_dir: &Path, addon: &CatalogAddon) -> Result<usize> {
    if addon.kind != AddonKind::Dlc || !addon.content_available {
        return Err("The selected provider has no downloadable content for this DLC".into());
    }
    let filename = addon
        .filename
        .as_deref()
        .ok_or("DLC archive filename is missing")?;
    if !matches!(
        Path::new(filename)
            .components()
            .collect::<Vec<_>>()
            .as_slice(),
        [Component::Normal(_)]
    ) {
        return Err("DLC archive filename must be a single file name".into());
    }
    let download_dir = data_dir
        .join("dlc-downloads")
        .join(&game.id)
        .join(&addon.id);
    fs::create_dir_all(&download_dir)?;
    let mut placeholder = game.clone();
    placeholder.install_dir = fs::canonicalize(&download_dir)?;
    placeholder.title = format!("{} DLC: {}", game.title, addon.title);
    let archive = if addon.archive_parts.is_empty() {
        let url = addon.uri.as_deref().ok_or("DLC download URL is missing")?;
        recorded_download_with_sha256(
            data_dir,
            &placeholder,
            url,
            Path::new(filename),
            JobKind::Archive,
            addon.sha256.as_deref(),
        )?
    } else {
        let names = multipart_names(filename, addon.archive_parts.len())?;
        if names.len() != addon.archive_part_hashes.len() {
            return Err("DLC archive part hash count does not match URLs".into());
        }
        let mut first = None;
        for ((name, url), hash) in names
            .iter()
            .zip(&addon.archive_parts)
            .zip(&addon.archive_part_hashes)
        {
            let path = recorded_download_with_sha256(
                data_dir,
                &placeholder,
                url,
                Path::new(name),
                JobKind::Archive,
                hash.as_deref(),
            )?;
            first.get_or_insert(path);
        }
        first.ok_or("DLC has no archive volumes")?
    };
    let count = install_archive(game, data_dir, &addon.id, &archive)?;
    if addon.archive_parts.is_empty() {
        let _ = clear_job(data_dir, &job_id_for_target(&archive));
    } else {
        for name in multipart_names(filename, addon.archive_parts.len())? {
            let _ = clear_job(data_dir, &job_id_for_target(&download_dir.join(name)));
        }
    }
    let _ = fs::remove_dir_all(&download_dir);
    Ok(count)
}

fn recover_unlocked(game: &Game, data_dir: &Path, dlc_id: &str) -> Result<()> {
    let state = state_dir(data_dir, &game.id, dlc_id);
    if state.join("complete").exists() {
        return Err("DLC installation is complete; use Remove instead".into());
    }
    let manifest = read_manifest(&state, game, dlc_id)?;
    let mut staged = Vec::new();
    for file in &manifest.files {
        if file.staged_path.as_os_str().is_empty() {
            continue;
        }
        let path = check_target(&manifest.install_dir, &file.staged_path)?;
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => staged.push(path),
            Ok(_) => return Err("Interrupted DLC staging file is not a regular file".into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    // Verify all installed targets before deleting staging files or the state record.
    for file in &manifest.files {
        let target = check_target(&manifest.install_dir, &file.path)?;
        if target.exists() && file_sha256(&target)? != file.sha256 {
            return Err(
                format!("Interrupted DLC file was modified: {}", file.path.display()).into(),
            );
        }
    }
    for path in staged {
        fs::remove_file(path)?;
    }
    remove_unlocked(game, data_dir, dlc_id)
}

/// Roll back an interrupted DLC install after verifying any completed files.
pub fn recover_interrupted(game: &Game, data_dir: &Path, dlc_id: &str) -> Result<()> {
    validate_id(&game.id)?;
    validate_id(dlc_id)?;
    let _lock = lock_game(data_dir, &game.id)?;
    recover_unlocked(game, data_dir, dlc_id)
}

/// Remove a DLC and all files recorded in its install manifest.
pub fn remove(game: &Game, data_dir: &Path, dlc_id: &str) -> Result<()> {
    validate_id(&game.id)?;
    validate_id(dlc_id)?;
    let _lock = lock_game(data_dir, &game.id)?;
    if !state_dir(data_dir, &game.id, dlc_id)
        .join("complete")
        .is_file()
    {
        return Err("DLC installation was interrupted; recover it before removal".into());
    }
    remove_unlocked(game, data_dir, dlc_id)
}

fn remove_unlocked(game: &Game, data_dir: &Path, dlc_id: &str) -> Result<()> {
    let state = state_dir(data_dir, &game.id, dlc_id);
    let manifest = read_manifest(&state, game, dlc_id)?;
    let install = &manifest.install_dir;
    for file in &manifest.files {
        let target = check_target(install, &file.path)?;
        if target.exists() && file_sha256(&target)? != file.sha256 {
            return Err(format!(
                "DLC file has been modified; cannot safely remove {}",
                file.path.display()
            )
            .into());
        }
    }
    for file in manifest.files.iter().rev() {
        let target = check_target(install, &file.path)?;
        if target.exists() {
            fs::remove_file(&target)?;
        }
        let mut parent = target.parent();
        while let Some(dir) = parent {
            if dir == install {
                break;
            }
            if fs::remove_dir(dir).is_err() {
                break;
            }
            parent = dir.parent();
        }
    }
    fs::remove_dir_all(state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Library, Runner};
    use std::collections::BTreeMap;
    use std::process::Command;

    #[test]
    fn installs_and_removes_additive_dlc_without_touching_base_files() {
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("azdl-dlc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let game_dir = root.join("games/base");
        let source = root.join("source");
        fs::create_dir_all(source.join("Content/DLC")).unwrap();
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("base.exe"), b"base").unwrap();
        fs::write(source.join("Content/DLC/bonus.dat"), b"bonus").unwrap();
        let archive = root.join("bonus.zip");
        assert!(
            Command::new("bsdtar")
                .args(["-a", "-cf"])
                .arg(&archive)
                .arg("-C")
                .arg(&source)
                .arg(".")
                .status()
                .unwrap()
                .success()
        );
        let game = Game {
            id: "base".into(),
            title: "Base".into(),
            install_dir: game_dir.clone(),
            executable: game_dir.join("base.exe"),
            runner: Runner::Auto,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            last_played_unix: None,
            origin: None,
            steam_app_id: None,
            launch_options: Vec::new(),
            selected_launch_option_id: None,
            cover_url: None,
            banner_url: None,
        };
        assert!(available_for_game(&game, &root).unwrap().is_empty());
        assert_eq!(install_archive(&game, &root, "bonus", &archive).unwrap(), 1);
        assert!(
            list_installed(&game, &root)
                .unwrap()
                .contains(&"bonus".to_string())
        );
        let mut library = Library::default();
        library.games.insert(game.id.clone(), game.clone());
        let drive =
            crate::settings::add_storage_drive(&root, "Other", &root.join("other")).unwrap();
        let manifest_path = state_dir(&root, &game.id, "bonus").join("manifest.json");
        let mut legacy: DlcManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        legacy.install_dir = fs::canonicalize(&game_dir).unwrap();
        fs::write(&manifest_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let moved =
            crate::settings::move_game_to_drive(&root, &mut library, &game.id, &drive.id).unwrap();
        assert!(!game_dir.exists());
        let game = library.games.get(&game.id).unwrap().clone();
        assert_eq!(game.install_dir, moved);
        assert!(
            list_installed(&game, &root)
                .unwrap()
                .contains(&"bonus".to_string())
        );
        let game_dir = moved;
        assert!(install_archive(&game, &root, "another", &archive).is_err());
        fs::write(game_dir.join("Content/DLC/bonus.dat"), b"modified").unwrap();
        assert!(remove(&game, &root, "bonus").is_err());
        assert_eq!(
            fs::read(game_dir.join("Content/DLC/bonus.dat")).unwrap(),
            b"modified"
        );
        fs::write(game_dir.join("Content/DLC/bonus.dat"), b"bonus").unwrap();
        remove(&game, &root, "bonus").unwrap();
        assert_eq!(fs::read(game_dir.join("base.exe")).unwrap(), b"base");
        assert!(!game_dir.join("Content/DLC/bonus.dat").exists());
        install_archive(&game, &root, "bonus2", &archive).unwrap();
        let interrupted = state_dir(&root, &game.id, "bonus2");
        let manifest: DlcManifest =
            serde_json::from_slice(&fs::read(interrupted.join("manifest.json")).unwrap()).unwrap();
        fs::remove_file(interrupted.join("complete")).unwrap();
        let staged = game_dir.join(&manifest.files[0].staged_path);
        fs::write(&staged, b"partial").unwrap();
        assert_eq!(list_interrupted(&game, &root).unwrap(), vec!["bonus2"]);
        recover_interrupted(&game, &root, "bonus2").unwrap();
        assert!(!staged.exists());
        assert!(!game_dir.join("Content/DLC/bonus.dat").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
