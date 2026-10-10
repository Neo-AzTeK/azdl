use crate::archive::{
    extract_checked_with_passphrase, recorded_multipart_download, validate_archive,
};
use crate::jobs::{JobKind, recorded_download};
use crate::{Game, Result, validate_id};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
pub struct PatchEntry {
    pub path: PathBuf,
    pub replaced: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PatchManifest {
    pub game_id: String,
    pub install_dir: PathBuf,
    pub source_dir: PathBuf,
    #[serde(default)]
    pub source_archive: Option<PathBuf>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub entries: Vec<PatchEntry>,
}

fn default_enabled() -> bool {
    true
}

fn patch_dir(data_dir: &Path, id: &str) -> PathBuf {
    data_dir.join("patches").join(id)
}

pub fn active_patch(data_dir: &Path, id: &str) -> Result<Option<PatchManifest>> {
    validate_id(id)?;
    let state = patch_dir(data_dir, id);
    match fs::symlink_metadata(&state) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err("patch state path is not a directory".into());
        }
        Ok(_) => {}
    }
    let manifest: PatchManifest = serde_json::from_slice(&fs::read(state.join("manifest.json"))?)?;
    if manifest.game_id != id {
        return Err("patch manifest game ID does not match".into());
    }
    Ok(Some(manifest))
}

fn collect_files(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let kind = fs::symlink_metadata(&path)?.file_type();
        if kind.is_symlink() {
            return Err(format!("patch contains a symbolic link: {}", path.display()).into());
        }
        if kind.is_dir() {
            collect_files(root, &path, files)?;
        } else if kind.is_file() {
            files.push(path.strip_prefix(root)?.to_path_buf());
        } else {
            return Err(format!("patch contains an unsupported file: {}", path.display()).into());
        }
    }
    Ok(())
}

fn validate_target(install: &Path, relative: &Path) -> Result<bool> {
    let mut current = install.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    if components.is_empty() {
        return Err("patch path is empty".into());
    }
    for (index, component) in components.iter().enumerate() {
        if !matches!(component, Component::Normal(_)) {
            return Err("patch path must stay within the game directory".into());
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(
                    format!("patch target is a symbolic link: {}", current.display()).into(),
                );
            }
            Ok(metadata) if index + 1 == components.len() && metadata.is_file() => {
                return Ok(true);
            }
            Ok(metadata) if index + 1 < components.len() && metadata.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "patch target has an incompatible type: {}",
                    current.display()
                )
                .into());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn replace_file(source: &Path, target: &Path) -> Result<()> {
    let parent = target.parent().ok_or("invalid patch target")?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = parent.join(format!(".azdl-patch-{}-{nonce}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        io::copy(&mut input, &mut output)?;
        output.flush()?;
        output.sync_all()?;
        fs::set_permissions(&temp, fs::metadata(source)?.permissions())?;
        #[cfg(windows)]
        if target.exists() {
            fs::remove_file(target)?;
        }
        fs::rename(&temp, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn write_manifest(state: &Path, manifest: &PatchManifest) -> Result<()> {
    let target = state.join("manifest.json");
    let temp = state.join("manifest.json.tmp");
    let mut file = File::create(&temp)?;
    serde_json::to_writer_pretty(&mut file, manifest)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(&target)?;
    }
    fs::rename(temp, target)?;
    Ok(())
}

fn overlay_files(manifest: &PatchManifest, state: &Path) -> Result<()> {
    for entry in &manifest.entries {
        validate_target(&manifest.install_dir, &entry.path)?;
        replace_file(
            &state.join("payload").join(&entry.path),
            &manifest.install_dir.join(&entry.path),
        )?;
    }
    Ok(())
}

fn restore_files(manifest: &PatchManifest, state: &Path) -> Result<()> {
    for entry in manifest.entries.iter().rev() {
        let target = manifest.install_dir.join(&entry.path);
        validate_target(&manifest.install_dir, &entry.path)?;
        if entry.replaced {
            replace_file(&state.join("backup").join(&entry.path), &target)?;
        } else if target.exists() {
            fs::remove_file(&target)?;
        }
    }
    Ok(())
}

/// Overlay an extracted patch directory, retaining originals for one active rollback.
pub fn apply_patch(game: &Game, data_dir: &Path, source_dir: &Path) -> Result<usize> {
    apply_patch_files(game, data_dir, source_dir, None)
}

fn apply_patch_files(
    game: &Game,
    data_dir: &Path,
    source_dir: &Path,
    source_archive: Option<PathBuf>,
) -> Result<usize> {
    validate_id(&game.id)?;
    let install = fs::canonicalize(&game.install_dir)?;
    if !install.is_dir() {
        return Err("game install directory is missing".into());
    }
    let source = fs::canonicalize(source_dir)?;
    if !source.is_dir() || source.starts_with(&install) || install.starts_with(&source) {
        return Err("patch source must be a separate directory".into());
    }
    let active = patch_dir(data_dir, &game.id);
    if fs::symlink_metadata(&active).is_ok() {
        return Err("game already has an active patch; roll it back first".into());
    }
    let mut files = Vec::new();
    collect_files(&source, &source, &mut files)?;
    files.sort();
    if files.is_empty() {
        return Err("patch directory has no files".into());
    }
    let mut entries = Vec::with_capacity(files.len());
    for relative in files {
        let replaced = validate_target(&install, &relative)?;
        entries.push(PatchEntry {
            path: relative,
            replaced,
        });
    }

    let patch_root = data_dir.join("patches");
    fs::create_dir_all(&patch_root)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staging = patch_root.join(format!(".{}-{}-{nonce}", game.id, std::process::id()));
    fs::create_dir(&staging)?;
    let manifest = PatchManifest {
        game_id: game.id.clone(),
        install_dir: install.clone(),
        source_dir: source.clone(),
        source_archive,
        enabled: true,
        entries,
    };
    let prepare = (|| -> Result<()> {
        for entry in &manifest.entries {
            let payload = staging.join("payload").join(&entry.path);
            fs::create_dir_all(payload.parent().ok_or("invalid patch path")?)?;
            fs::copy(source.join(&entry.path), payload)?;
            if entry.replaced {
                let backup = staging.join("backup").join(&entry.path);
                fs::create_dir_all(backup.parent().ok_or("invalid backup path")?)?;
                fs::copy(install.join(&entry.path), backup)?;
            }
        }
        write_manifest(&staging, &manifest)?;
        fs::rename(&staging, &active)?;
        Ok(())
    })();
    if let Err(error) = prepare {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    if let Err(error) = overlay_files(&manifest, &active) {
        return match rollback_patch(game, data_dir) {
            Ok(()) => Err(format!("patch failed and was rolled back: {error}").into()),
            Err(restore_error) => Err(format!(
                "patch failed: {error}; rollback failed: {restore_error}; recovery files are at {}",
                active.display()
            )
            .into()),
        };
    }
    Ok(manifest.entries.len())
}

/// Extract a local archive into a temporary directory, then apply it with rollback support.
pub fn apply_patch_archive(game: &Game, data_dir: &Path, archive: &Path) -> Result<usize> {
    apply_patch_archive_with_root(game, data_dir, archive, None)
}

/// Select a folder inside an archive when its packaging wrapper is ambiguous.
pub fn apply_patch_archive_with_root(
    game: &Game,
    data_dir: &Path,
    archive: &Path,
    archive_root: Option<&Path>,
) -> Result<usize> {
    validate_id(&game.id)?;
    if active_patch(data_dir, &game.id)?.is_some() {
        return Err("game already has an active patch; roll it back first".into());
    }
    let archive = fs::canonicalize(archive)?;
    if !archive.is_file() {
        return Err("patch archive is not a file".into());
    }
    if let Some(root) = archive_root
        && (root.as_os_str().is_empty()
            || !root
                .components()
                .all(|component| matches!(component, Component::Normal(_))))
    {
        return Err("archive root must be a relative folder inside the patch archive".into());
    }
    validate_archive(&archive)?;
    let unpack_root = data_dir.join("patch-unpack");
    fs::create_dir_all(&unpack_root)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staging = unpack_root.join(format!("{}-{}-{nonce}", game.id, std::process::id()));
    fs::create_dir(&staging)?;
    let result = (|| -> Result<usize> {
        // Online-Fix publishes one shared archive password. Users can override it
        // for another package through the environment.
        let passphrase = std::env::var("AZDL_PATCH_ARCHIVE_PASSWORD")
            .unwrap_or_else(|_| "online-fix.me".to_string());
        extract_checked_with_passphrase(&archive, &staging, Some(&passphrase))?;
        let mut source = archive_root.map_or_else(|| staging.clone(), |root| staging.join(root));
        if !source.is_dir() {
            return Err("selected archive root is not a folder".into());
        }
        if archive_root.is_none() {
            loop {
                let children =
                    fs::read_dir(&source)?.collect::<std::result::Result<Vec<_>, _>>()?;
                if children.len() != 1 || !fs::symlink_metadata(children[0].path())?.is_dir() {
                    break;
                }
                let wrapper = children[0].path();
                let name = children[0]
                    .file_name()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                let normalized: String = name
                    .chars()
                    .filter(|ch| ch.is_ascii_alphanumeric())
                    .collect();
                if fs::symlink_metadata(game.install_dir.join(children[0].file_name())).is_ok() {
                    break;
                }
                let known_wrapper = matches!(
                    normalized.as_str(),
                    "patch" | "patchfiles" | "onlinefix" | "onlinefixme" | "overlay" | "crack"
                );
                let overlaps_install = fs::read_dir(&wrapper)?
                    .collect::<std::result::Result<Vec<_>, _>>()?
                    .iter()
                    .any(|entry| {
                        fs::symlink_metadata(game.install_dir.join(entry.file_name())).is_ok()
                    });
                if !known_wrapper && !overlaps_install {
                    break;
                }
                source = wrapper;
            }
        }
        apply_patch_files(game, data_dir, &source, Some(archive.clone()))
    })();
    let cleanup = fs::remove_dir_all(&staging);
    match (result, cleanup) {
        (Ok(count), Ok(())) => Ok(count),
        (Ok(_), Err(error)) => {
            Err(format!("patch applied, but temporary extraction cleanup failed: {error}").into())
        }
        (Err(error), _) => Err(error),
    }
}

/// Download an HTTPS patch archive, then apply it to an installed game.
/// The archive remains in patch-downloads so its source can be inspected later.
pub fn fetch_patch_archive(
    game: &Game,
    data_dir: &Path,
    url: &str,
    filename: &Path,
) -> Result<(PathBuf, usize)> {
    validate_id(&game.id)?;
    if active_patch(data_dir, &game.id)?.is_some() {
        return Err("game already has an active patch; roll it back first".into());
    }
    let downloads = data_dir.join("patch-downloads").join(&game.id);
    fs::create_dir_all(&downloads)?;
    let mut download_game = game.clone();
    download_game.install_dir = fs::canonicalize(&downloads)?;
    let archive = recorded_download(data_dir, &download_game, url, filename, JobKind::Patch)?;
    let count = apply_patch_archive(game, data_dir, &archive)?;
    Ok((archive, count))
}

/// Download all volumes of a RAR patch, then apply the first volume.
pub fn fetch_patch_archive_parts(
    game: &Game,
    data_dir: &Path,
    first_name: &Path,
    urls: &[String],
) -> Result<(PathBuf, usize)> {
    validate_id(&game.id)?;
    if active_patch(data_dir, &game.id)?.is_some() {
        return Err("game already has an active patch; roll it back first".into());
    }
    let downloads = data_dir.join("patch-downloads").join(&game.id);
    fs::create_dir_all(&downloads)?;
    let mut download_game = game.clone();
    download_game.install_dir = fs::canonicalize(&downloads)?;
    let archive =
        recorded_multipart_download(data_dir, &download_game, first_name, urls, JobKind::Patch)?;
    let count = apply_patch_archive(game, data_dir, &archive)?;
    Ok((archive, count))
}

/// Restore overwritten files and remove files introduced by the active patch.
pub fn rollback_patch(game: &Game, data_dir: &Path) -> Result<()> {
    let manifest = active_patch(data_dir, &game.id)?.ok_or("game has no active patch")?;
    let install = fs::canonicalize(&game.install_dir)?;
    if install != manifest.install_dir {
        return Err("game install directory changed since the patch was applied".into());
    }
    let state = patch_dir(data_dir, &game.id);
    if manifest.enabled {
        restore_files(&manifest, &state)?;
    }
    fs::remove_dir_all(state)?;
    Ok(())
}

/// Restore original game files while keeping the staged patch available.
pub fn disable_patch(game: &Game, data_dir: &Path) -> Result<()> {
    let mut manifest = active_patch(data_dir, &game.id)?.ok_or("game has no active patch")?;
    let install = fs::canonicalize(&game.install_dir)?;
    if install != manifest.install_dir {
        return Err("game install directory changed since the patch was applied".into());
    }
    if !manifest.enabled {
        return Ok(());
    }
    let state = patch_dir(data_dir, &game.id);
    restore_files(&manifest, &state)?;
    manifest.enabled = false;
    write_manifest(&state, &manifest)
}

/// Reapply staged patch files after a disable, without downloading them again.
pub fn enable_patch(game: &Game, data_dir: &Path) -> Result<()> {
    let mut manifest = active_patch(data_dir, &game.id)?.ok_or("game has no active patch")?;
    let install = fs::canonicalize(&game.install_dir)?;
    if install != manifest.install_dir {
        return Err("game install directory changed since the patch was applied".into());
    }
    if manifest.enabled {
        return Ok(());
    }
    let state = patch_dir(data_dir, &game.id);
    overlay_files(&manifest, &state)?;
    manifest.enabled = true;
    write_manifest(&state, &manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Library, add_game};
    use std::process::Command;

    #[test]
    fn applies_and_restores_patch_files() {
        let root = std::env::temp_dir().join(format!(
            "azdl-patch-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        let source = root.join("source");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(install.join("game.exe"), b"original").unwrap();
        fs::write(source.join("game.exe"), b"patched").unwrap();
        fs::write(source.join("bin/extra.dll"), b"extra").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "test",
            "Test",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = &library.games["test"];
        assert_eq!(apply_patch(game, &root, &source).unwrap(), 2);
        assert_eq!(fs::read(install.join("game.exe")).unwrap(), b"patched");
        assert!(install.join("bin/extra.dll").is_file());
        assert!(apply_patch(game, &root, &source).is_err());
        fs::remove_dir_all(&source).unwrap();
        disable_patch(game, &root).unwrap();
        assert!(!active_patch(&root, "test").unwrap().unwrap().enabled);
        assert_eq!(fs::read(install.join("game.exe")).unwrap(), b"original");
        assert!(!install.join("bin/extra.dll").exists());
        enable_patch(game, &root).unwrap();
        assert!(active_patch(&root, "test").unwrap().unwrap().enabled);
        assert_eq!(fs::read(install.join("game.exe")).unwrap(), b"patched");
        assert!(install.join("bin/extra.dll").is_file());
        fs::write(install.join("game.exe"), b"edited while enabled").unwrap();
        enable_patch(game, &root).unwrap();
        assert_eq!(
            fs::read(install.join("game.exe")).unwrap(),
            b"edited while enabled"
        );
        disable_patch(game, &root).unwrap();
        fs::write(install.join("game.exe"), b"edited while disabled").unwrap();
        fs::write(install.join("bin/extra.dll"), b"new user file").unwrap();
        disable_patch(game, &root).unwrap();
        assert_eq!(
            fs::read(install.join("bin/extra.dll")).unwrap(),
            b"new user file"
        );
        rollback_patch(game, &root).unwrap();
        assert_eq!(
            fs::read(install.join("game.exe")).unwrap(),
            b"edited while disabled"
        );
        assert_eq!(
            fs::read(install.join("bin/extra.dll")).unwrap(),
            b"new user file"
        );
        assert!(active_patch(&root, "test").unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_patch_manifest_defaults_to_enabled() {
        let manifest: PatchManifest = serde_json::from_slice(
            br#"{"game_id":"test","install_dir":"/tmp/game","source_dir":"/tmp/source","entries":[]}"#,
        )
        .unwrap();
        assert!(manifest.enabled);
    }

    #[test]
    fn applies_archive_with_wrapper_directory() {
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-patch-archive-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        let wrapped = root.join("source/patch-files");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&wrapped).unwrap();
        fs::write(install.join("game.exe"), b"original").unwrap();
        fs::write(wrapped.join("game.exe"), b"patched").unwrap();
        let archive = root.join("patch.zip");
        assert!(
            Command::new("bsdtar")
                .args(["-a", "-c", "-f"])
                .arg(&archive)
                .arg("-C")
                .arg(root.join("source"))
                .arg("patch-files")
                .status()
                .unwrap()
                .success()
        );
        let mut library = Library::default();
        add_game(
            &mut library,
            "test",
            "Test",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = &library.games["test"];
        assert_eq!(apply_patch_archive(game, &root, &archive).unwrap(), 1);
        assert_eq!(fs::read(install.join("game.exe")).unwrap(), b"patched");
        assert_eq!(
            active_patch(&root, "test").unwrap().unwrap().source_archive,
            Some(archive)
        );
        rollback_patch(game, &root).unwrap();
        assert_eq!(fs::read(install.join("game.exe")).unwrap(), b"original");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applies_password_protected_online_fix_zip() {
        if Command::new("zip").arg("-v").output().is_err()
            || Command::new("bsdtar").arg("--version").output().is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-encrypted-patch-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        let source = root.join("source");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::write(install.join("game.exe"), b"original").unwrap();
        fs::write(source.join("game.exe"), b"patched").unwrap();
        let archive = root.join("online-fix.zip");
        assert!(
            Command::new("zip")
                .args(["-q", "-P", "online-fix.me"])
                .arg(&archive)
                .arg("game.exe")
                .current_dir(&source)
                .status()
                .unwrap()
                .success()
        );
        let mut library = Library::default();
        add_game(
            &mut library,
            "test",
            "Test",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = &library.games["test"];
        assert_eq!(apply_patch_archive(game, &root, &archive).unwrap(), 1);
        assert_eq!(fs::read(install.join("game.exe")).unwrap(), b"patched");
        rollback_patch(game, &root).unwrap();
        assert_eq!(fs::read(install.join("game.exe")).unwrap(), b"original");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applies_and_rolls_back_multipart_rar_patch() {
        if Command::new("bsdtar").arg("--version").output().is_err()
            || Command::new("unrar").arg("-?").output().is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-multipart-patch-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("game.exe"), b"game").unwrap();
        fs::write(install.join("test.bin"), b"original").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "test",
            "Test",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = &library.games["test"];
        let archive = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/test_read_format_rar5_multiarchive_solid.part01.rar");
        assert_eq!(apply_patch_archive(game, &root, &archive).unwrap(), 9);
        assert_eq!(fs::metadata(install.join("test.bin")).unwrap().len(), 1200);
        rollback_patch(game, &root).unwrap();
        assert_eq!(fs::read(install.join("test.bin")).unwrap(), b"original");
        assert!(!install.join("test1.bin").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archive_keeps_game_subdirectory_for_patch_files() {
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-patch-subdir-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        let source = root.join("source");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(install.join("game.exe"), b"game").unwrap();
        fs::write(source.join("bin/SteamFix64.dll"), b"fix").unwrap();
        let archive = root.join("fix.zip");
        assert!(
            Command::new("bsdtar")
                .args(["-a", "-c", "-f"])
                .arg(&archive)
                .arg("-C")
                .arg(&source)
                .arg("bin")
                .status()
                .unwrap()
                .success()
        );
        let mut library = Library::default();
        add_game(
            &mut library,
            "test",
            "Test",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = &library.games["test"];
        assert_eq!(apply_patch_archive(game, &root, &archive).unwrap(), 1);
        assert!(install.join("bin/SteamFix64.dll").is_file());
        assert!(!install.join("SteamFix64.dll").exists());
        rollback_patch(game, &root).unwrap();
        assert!(!install.join("bin/SteamFix64.dll").exists());
        let wrapped = source.join("unknown/bundle");
        fs::create_dir_all(&wrapped).unwrap();
        fs::write(wrapped.join("OnlineFix.ini"), b"fix").unwrap();
        let wrapped_archive = root.join("wrapped.zip");
        assert!(
            Command::new("bsdtar")
                .args(["-a", "-c", "-f"])
                .arg(&wrapped_archive)
                .arg("-C")
                .arg(&source)
                .arg("unknown")
                .status()
                .unwrap()
                .success()
        );
        assert!(
            apply_patch_archive_with_root(
                game,
                &root,
                &wrapped_archive,
                Some(Path::new("../escape"))
            )
            .is_err()
        );
        assert_eq!(
            apply_patch_archive_with_root(
                game,
                &root,
                &wrapped_archive,
                Some(Path::new("unknown/bundle"))
            )
            .unwrap(),
            1
        );
        assert!(install.join("OnlineFix.ini").is_file());
        rollback_patch(game, &root).unwrap();
        assert!(!install.join("OnlineFix.ini").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_archive_with_symbolic_link() {
        use std::os::unix::fs::symlink;
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-patch-archive-link-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        let source = root.join("source");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::write(install.join("game.exe"), b"original").unwrap();
        symlink("game.exe", source.join("linked.exe")).unwrap();
        let archive = root.join("patch.tar");
        assert!(
            Command::new("bsdtar")
                .args(["-c", "-f"])
                .arg(&archive)
                .arg("-C")
                .arg(&source)
                .arg(".")
                .status()
                .unwrap()
                .success()
        );
        let mut library = Library::default();
        add_game(
            &mut library,
            "test",
            "Test",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        assert!(apply_patch_archive(&library.games["test"], &root, &archive).is_err());
        assert_eq!(fs::read(install.join("game.exe")).unwrap(), b"original");
        assert!(!root.join("patch-unpack").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_patch_paths() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "azdl-patch-link-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        let source = root.join("source");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::write(install.join("game.exe"), b"original").unwrap();
        symlink(install.join("game.exe"), source.join("game.exe")).unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "test",
            "Test",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        assert!(apply_patch(&library.games["test"], &root, &source).is_err());
        assert_eq!(fs::read(install.join("game.exe")).unwrap(), b"original");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_game_target() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "azdl-patch-target-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        let source = root.join("source");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::write(install.join("game.exe"), b"original").unwrap();
        fs::write(root.join("outside.dll"), b"outside").unwrap();
        symlink(root.join("outside.dll"), install.join("patch.dll")).unwrap();
        fs::write(source.join("patch.dll"), b"patched").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "test",
            "Test",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        assert!(apply_patch(&library.games["test"], &root, &source).is_err());
        assert_eq!(fs::read(root.join("outside.dll")).unwrap(), b"outside");
        fs::remove_dir_all(root).unwrap();
    }
}
