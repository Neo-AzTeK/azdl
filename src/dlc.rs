//! Reversible installation of explicitly supplied DLC archives.

use crate::archive::{extract_checked, multipart_names};
use crate::catalog::{AddonKind, CatalogAddon};
use crate::jobs::{
    JobKind, JobStatus, clear_job_if_present, job_id_for_target, maybe_load_job,
    recorded_download_with_sha256,
};
use crate::sha256::file_sha256;
use crate::{Game, Result, validate_id};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlcFile {
    pub path: PathBuf,
    pub sha256: String,
    #[serde(default)]
    pub staged_path: PathBuf,
    /// Whether azdl may remove this file after the last managed DLC reference is gone.
    #[serde(default = "default_true")]
    pub managed: bool,
    /// Whether this transaction created the target rather than reusing existing bytes.
    #[serde(default = "default_true")]
    pub created: bool,
    /// SHA-256 of the base file saved under `backup/` before an archive overlay.
    #[serde(default)]
    pub backup_sha256: Option<String>,
    /// This DLC shares the visible overlay bytes and an independent base backup.
    #[serde(default)]
    pub shared_overlay: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlcManifest {
    pub game_id: String,
    pub dlc_id: String,
    pub install_dir: PathBuf,
    pub files: Vec<DlcFile>,
}

/// Steam DLC availability is resolved by `steam_content` and `steam_metadata`.
/// Historical archive receipts remain usable for rollback and removal.
pub fn available_for_game(game: &Game, data_dir: &Path) -> Result<Vec<CatalogAddon>> {
    validate_id(&game.id)?;
    crate::migration::migrate(data_dir)?;
    let path = data_dir
        .join("content-sources")
        .join(format!("{}.json", game.id));
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn state_dir(data_dir: &Path, game_id: &str, dlc_id: &str) -> PathBuf {
    data_dir.join("dlc").join(game_id).join(dlc_id)
}

fn dlc_io<T>(operation: &str, path: &Path, result: io::Result<T>) -> Result<T> {
    result
        .map_err(|error| format!("Cannot {operation} DLC path {}: {error}", path.display()).into())
}

/// Finish and close a manifest before publishing its file or containing directory.
fn write_manifest_file(path: &Path, manifest: &DlcManifest) -> Result<()> {
    let mut output = dlc_io(
        "create manifest at",
        path,
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path),
    )?;
    serde_json::to_writer_pretty(&mut output, manifest)
        .map_err(|error| format!("Cannot write DLC manifest {}: {error}", path.display()))?;
    dlc_io("write manifest at", path, output.write_all(b"\n"))?;
    dlc_io("sync manifest at", path, output.sync_all())?;
    Ok(())
}

/// Sync copied bytes through a writable handle, including on Windows.
fn copy_synced_file(source: &Path, backup: &Path) -> Result<()> {
    let mut input = dlc_io("read copy source at", source, File::open(source))?;
    let mut output = dlc_io(
        "create copy at",
        backup,
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(backup),
    )?;
    dlc_io("write copy at", backup, io::copy(&mut input, &mut output))?;
    dlc_io("sync copy at", backup, output.sync_all())?;
    Ok(())
}

/// Serialize game launch and content mutations across processes.
pub fn lock_game(data_dir: &Path, game_id: &str) -> Result<File> {
    validate_id(game_id)?;
    let root = data_dir.join("content-locks");
    if fs::symlink_metadata(&root).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("Game lock root is a symbolic link".into());
    }
    fs::create_dir_all(&root)?;
    let path = root.join(format!("{game_id}.lock"));
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("Game lock path is a symbolic link".into());
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

fn is_prefix_of(source: &Path, candidate: &Path) -> Result<bool> {
    let mut source = File::open(source)?;
    let mut candidate = File::open(candidate)?;
    let mut actual = [0_u8; 64 * 1024];
    let mut expected = [0_u8; 64 * 1024];
    loop {
        let count = candidate.read(&mut actual)?;
        if count == 0 {
            return Ok(true);
        }
        if source.read_exact(&mut expected[..count]).is_err()
            || actual[..count] != expected[..count]
        {
            return Ok(false);
        }
    }
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
        write_manifest_file(&temp, &manifest)?;
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
        if validate_id(&id).is_err()
            || !fs::symlink_metadata(entry.path().join("complete"))
                .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
        {
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

fn ensure_no_interrupted(game: &Game, data_dir: &Path) -> Result<()> {
    if !list_interrupted(game, data_dir)?.is_empty() {
        return Err("Recover interrupted DLC before changing game content".into());
    }
    Ok(())
}

/// Install a DLC archive, preserving replaced base files for recovery and removal.
pub fn install_archive(
    game: &Game,
    data_dir: &Path,
    dlc_id: &str,
    archive: &Path,
) -> Result<usize> {
    validate_id(&game.id)?;
    validate_id(dlc_id)?;
    let _lock = lock_game(data_dir, &game.id)?;
    install_archive_unlocked(game, data_dir, dlc_id, archive)
}

fn install_archive_unlocked(
    game: &Game,
    data_dir: &Path,
    dlc_id: &str,
    archive: &Path,
) -> Result<usize> {
    let archive = fs::canonicalize(archive)?;
    if !archive.is_file() {
        return Err("DLC archive is missing".into());
    }
    install_payload_unlocked(game, data_dir, dlc_id, true, |payload| {
        extract_checked(&archive, payload)
    })
}

/// Install files already downloaded into an isolated directory using the DLC transaction.
pub fn install_staged(
    game: &Game,
    data_dir: &Path,
    dlc_id: &str,
    source_dir: &Path,
) -> Result<usize> {
    install_staged_with_guard(game, data_dir, dlc_id, source_dir, || Ok(()))
}

/// Commit staged files after a final precondition check under the game content lock.
pub(crate) fn install_staged_with_guard(
    game: &Game,
    data_dir: &Path,
    dlc_id: &str,
    source_dir: &Path,
    guard: impl FnOnce() -> Result<()>,
) -> Result<usize> {
    install_staged_payload(game, data_dir, dlc_id, source_dir, false, guard)
}

/// Consume launcher-owned, verified download staging while preserving the DLC journal.
pub(crate) fn install_downloaded_staged_with_guard(
    game: &Game,
    data_dir: &Path,
    dlc_id: &str,
    source_dir: &Path,
    guard: impl FnOnce() -> Result<()>,
) -> Result<usize> {
    install_staged_payload(game, data_dir, dlc_id, source_dir, true, guard)
}

fn install_staged_payload(
    game: &Game,
    data_dir: &Path,
    dlc_id: &str,
    source_dir: &Path,
    consume: bool,
    guard: impl FnOnce() -> Result<()>,
) -> Result<usize> {
    validate_id(&game.id)?;
    validate_id(dlc_id)?;
    let _lock = lock_game(data_dir, &game.id)?;
    let source_dir = fs::canonicalize(source_dir)?;
    if !source_dir.is_dir() {
        return Err("DLC staging directory is missing".into());
    }
    guard()?;
    install_payload_unlocked(game, data_dir, dlc_id, false, |payload| {
        let mut files = Vec::new();
        collect_files(&source_dir, &source_dir, &mut files)?;
        for relative in files {
            let from = source_dir.join(&relative);
            let to = payload.join(&relative);
            fs::create_dir_all(to.parent().ok_or("invalid DLC path")?)?;
            if consume {
                move_staged_file(&from, &to)?;
            } else {
                copy_synced_file(&from, &to)?;
            }
        }
        Ok(())
    })
}

/// Move an owned staging file, copying durably only across filesystems.
fn move_staged_file(source: &Path, target: &Path) -> Result<()> {
    move_staged_file_with(source, target, |from, to| fs::rename(from, to))
}

fn move_staged_file_with(
    source: &Path,
    target: &Path,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<()> {
    if fs::symlink_metadata(target).is_ok() {
        return Err(format!(
            "DLC staging destination already exists: {}",
            target.display()
        )
        .into());
    }
    let metadata = dlc_io(
        "inspect staging source at",
        source,
        fs::symlink_metadata(source),
    )?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "DLC staging source is not a regular file: {}",
            source.display()
        )
        .into());
    }
    // Archive attributes apply only to this owned staging copy. Its final file
    // must remain writable for sync and removable by the rollback journal.
    let mut permissions = metadata.permissions();
    #[cfg(unix)]
    let needs_write = {
        use std::os::unix::fs::PermissionsExt;
        let mode = permissions.mode();
        permissions.set_mode(mode | 0o200);
        mode & 0o200 == 0
    };
    #[cfg(windows)]
    // Clearing the Windows read-only attribute preserves the file's ACL.
    #[allow(clippy::permissions_set_readonly_false)]
    let needs_write = {
        let readonly = permissions.readonly();
        permissions.set_readonly(false);
        readonly
    };
    #[cfg(not(any(unix, windows)))]
    let needs_write = false;
    if needs_write {
        dlc_io(
            "make staging file writable at",
            source,
            fs::set_permissions(source, permissions),
        )?;
    }
    match rename(source, target) {
        Ok(()) => {
            let output = dlc_io(
                "open moved file at",
                target,
                fs::OpenOptions::new().write(true).open(target),
            )?;
            dlc_io("sync moved file at", target, output.sync_all())
        }
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            copy_synced_file(source, target)?;
            dlc_io(
                "remove copied staging source at",
                source,
                fs::remove_file(source),
            )
        }
        Err(error) => dlc_io("move staged file to", target, Err(error)),
    }
}

fn install_payload_unlocked(
    game: &Game,
    data_dir: &Path,
    dlc_id: &str,
    allow_base_overlay: bool,
    prepare_payload: impl FnOnce(&Path) -> Result<()>,
) -> Result<usize> {
    let install = fs::canonicalize(&game.install_dir)?;
    if !install.is_dir() {
        return Err("Base game installation is missing".into());
    }
    ensure_no_interrupted(game, data_dir)?;
    let state = state_dir(data_dir, &game.id, dlc_id);
    if fs::symlink_metadata(&state).is_ok() {
        return Err("DLC is already installed or has an interrupted installation".into());
    }
    let parent = state.parent().ok_or("invalid DLC state directory")?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staging = parent.join(format!(".{dlc_id}-{}-{nonce}", std::process::id()));
    fs::create_dir(&staging)?;
    let result = (|| -> Result<usize> {
        let payload = staging.join("payload");
        fs::create_dir(&payload)?;
        prepare_payload(&payload)?;
        let mut files = Vec::new();
        collect_files(&payload, &payload, &mut files)?;
        files.sort();
        if files.is_empty() {
            return Err("DLC archive contains no files".into());
        }
        let installed_manifests = list_installed(game, data_dir)?
            .into_iter()
            .map(|id| read_manifest(&state_dir(data_dir, &game.id, &id), game, &id))
            .collect::<Result<Vec<_>>>()?;
        let mut entries = Vec::with_capacity(files.len());
        let mut portable_paths = BTreeSet::new();
        for relative in files {
            let portable = relative.to_string_lossy().replace('\\', "/").to_lowercase();
            if !portable_paths.insert(portable.clone()) {
                return Err("DLC files collide on Windows".into());
            }
            if installed_manifests
                .iter()
                .flat_map(|manifest| &manifest.files)
                .any(|file| {
                    file.path != relative
                        && file
                            .path
                            .to_string_lossy()
                            .replace('\\', "/")
                            .to_lowercase()
                            == portable
                })
            {
                return Err("DLC file path collides with an installed DLC on Windows".into());
            }
            let target = check_target(&install, &relative)?;
            let sha256 = file_sha256(&payload.join(&relative))?;
            let references = installed_manifests
                .iter()
                .flat_map(|manifest| manifest.files.iter().map(move |file| (manifest, file)))
                .filter(|(_, file)| file.path == relative)
                .collect::<Vec<_>>();
            let mut backup_sha256 = None;
            let mut shared_overlay = false;
            let created = match fs::symlink_metadata(&target) {
                Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                    let existing_hash = file_sha256(&target)?;
                    if existing_hash != sha256 {
                        if !allow_base_overlay || !references.is_empty() {
                            return Err(format!(
                                "DLC conflicts with an existing game file: {}",
                                relative.display()
                            )
                            .into());
                        }
                        let backup = staging.join("backup").join(&relative);
                        fs::create_dir_all(backup.parent().ok_or("invalid DLC backup path")?)?;
                        copy_synced_file(&target, &backup)?;
                        if file_sha256(&backup)? != existing_hash
                            || file_sha256(&target)? != existing_hash
                        {
                            return Err("Base file changed while preparing DLC backup".into());
                        }
                        backup_sha256 = Some(existing_hash);
                    } else if let Some((owner, file)) = references
                        .iter()
                        .find(|(_, file)| file.backup_sha256.is_some())
                    {
                        if references.iter().any(|(_, other)| {
                            other.sha256 != sha256 || other.backup_sha256 != file.backup_sha256
                        }) {
                            return Err("Installed DLC overlay ownership is inconsistent".into());
                        }
                        let backup_hash = file
                            .backup_sha256
                            .as_ref()
                            .ok_or("DLC overlay has no backup hash")?;
                        let from = check_target(
                            &state_dir(data_dir, &game.id, &owner.dlc_id),
                            &Path::new("backup").join(&relative),
                        )?;
                        if file_sha256(&from)? != *backup_hash {
                            return Err("Installed DLC overlay backup has changed".into());
                        }
                        let backup = staging.join("backup").join(&relative);
                        fs::create_dir_all(backup.parent().ok_or("invalid DLC backup path")?)?;
                        copy_synced_file(&from, &backup)?;
                        if file_sha256(&backup)? != *backup_hash {
                            return Err("Shared DLC overlay backup copy does not match".into());
                        }
                        backup_sha256 = Some(backup_hash.clone());
                        shared_overlay = true;
                    }
                    false
                }
                Ok(_) => return Err("DLC target has an incompatible file type".into()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => true,
                Err(error) => return Err(error.into()),
            };
            let managed = created
                || backup_sha256.is_some()
                || references
                    .iter()
                    .any(|(_, file)| file.sha256 == sha256 && file.managed);
            let name = relative
                .file_name()
                .ok_or("DLC file has no name")?
                .to_string_lossy();
            let staged_path = if created || backup_sha256.is_some() {
                relative.with_file_name(format!(".{name}.azdl-{dlc_id}-{nonce}.partial"))
            } else {
                PathBuf::new()
            };
            if !staged_path.as_os_str().is_empty()
                && fs::symlink_metadata(check_target(&install, &staged_path)?).is_ok()
            {
                return Err("DLC staging path already exists".into());
            }
            entries.push(DlcFile {
                sha256,
                path: relative,
                staged_path,
                managed,
                created,
                backup_sha256,
                shared_overlay,
            });
        }
        for portable in &portable_paths {
            let mut parent = portable.as_str();
            while let Some(split) = parent.rfind('/') {
                parent = &parent[..split];
                if portable_paths.contains(parent) {
                    return Err("DLC files use a file as a directory on Windows".into());
                }
            }
        }
        let manifest = DlcManifest {
            game_id: game.id.clone(),
            dlc_id: dlc_id.to_string(),
            install_dir: PathBuf::new(),
            files: entries,
        };
        write_manifest_file(&staging.join("manifest.json"), &manifest)?;
        dlc_io(
            "publish transaction at",
            &state,
            fs::rename(&staging, &state),
        )?;
        for entry in &manifest.files {
            if !entry.created && (entry.backup_sha256.is_none() || entry.shared_overlay) {
                let target = check_target(&install, &entry.path)?;
                if file_sha256(&target)? != entry.sha256 {
                    return Err("Shared DLC file changed during installation".into());
                }
                continue;
            }
            let target = check_target(&install, &entry.path)?;
            let staged_target = check_target(&install, &entry.staged_path)?;
            if let Some(backup_hash) = &entry.backup_sha256 {
                if file_sha256(&target)? != *backup_hash {
                    return Err("Base file changed during DLC installation".into());
                }
            } else if fs::symlink_metadata(&target).is_ok() {
                return Err("DLC target changed during installation".into());
            }
            let parent = target.parent().ok_or("invalid DLC target")?;
            dlc_io(
                "create installation directory at",
                parent,
                fs::create_dir_all(parent),
            )?;
            let source = state.join("payload").join(&entry.path);
            move_staged_file(&source, &staged_target)?;
            if let Some(backup_hash) = &entry.backup_sha256 {
                if file_sha256(&target)? != *backup_hash {
                    return Err("Base file changed during DLC installation".into());
                }
                dlc_io(
                    "replace existing file at",
                    &target,
                    fs::remove_file(&target),
                )?;
            } else if fs::symlink_metadata(&target).is_ok() {
                return Err("DLC target changed during installation".into());
            }
            if fs::hard_link(&staged_target, &target).is_err() {
                // Removable filesystems may not support hard links.
                let mut input = File::open(&staged_target)?;
                let mut output = dlc_io(
                    "create installed file at",
                    &target,
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&target),
                )?;
                dlc_io(
                    "write installed file at",
                    &target,
                    io::copy(&mut input, &mut output),
                )?;
                dlc_io("sync installed file at", &target, output.sync_all())?;
            }
            dlc_io(
                "remove staged file at",
                &staged_target,
                fs::remove_file(&staged_target),
            )?;
        }
        let payload = state.join("payload");
        dlc_io(
            "remove transaction payload at",
            &payload,
            fs::remove_dir_all(&payload),
        )?;
        let complete = state.join("complete");
        let output = dlc_io(
            "create completion record at",
            &complete,
            File::create(&complete),
        )?;
        dlc_io("sync completion record at", &complete, output.sync_all())?;
        Ok(manifest.files.len())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
        if state.is_dir() {
            let _ = recover_unlocked(game, data_dir, dlc_id);
        }
    }
    result.map_err(|error| {
        format!(
            "DLC {dlc_id} installation into {} failed: {error}",
            install.display()
        )
        .into()
    })
}

/// Download an explicit provider DLC archive and install it into its base game.
pub fn install_catalog_dlc(game: &Game, data_dir: &Path, addon: &CatalogAddon) -> Result<usize> {
    install_catalog_dlc_with(game, data_dir, addon, |placeholder, url, name, hash| {
        recorded_download_with_sha256(data_dir, placeholder, url, name, JobKind::Archive, hash)
    })
}

/// Verify that a paused archive belongs to the requested game's DLC staging directory.
pub fn validate_catalog_dlc_resume(
    data_dir: &Path,
    job_id: &str,
    game_id: &str,
    dlc_id: &str,
) -> Result<()> {
    validate_id(game_id)?;
    validate_id(dlc_id)?;
    let job = crate::jobs::load_job(data_dir, job_id)?;
    if job.kind != JobKind::Archive
        || job.effective_status() != JobStatus::Paused
        || job
            .install
            .as_ref()
            .is_none_or(|plan| plan.game_id != game_id)
    {
        return Err("Paused DLC transfer does not belong to this game".into());
    }
    let root = data_dir.join("dlc-downloads");
    let game_dir = root.join(game_id);
    let expected = game_dir.join(dlc_id);
    for path in [&root, &game_dir, &expected] {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("DLC download path is not a regular directory".into());
        }
    }
    let parent = job.target.parent().ok_or("invalid download target")?;
    if fs::canonicalize(parent)? != fs::canonicalize(expected)? {
        return Err("Paused transfer does not belong to this DLC".into());
    }
    Ok(())
}

/// Resume a paused provider archive and complete its DLC installation.
/// Returns `None` for ordinary download jobs outside DLC staging.
pub fn resume_catalog_dlc_job(data_dir: &Path, job_id: &str) -> Result<Option<usize>> {
    resume_catalog_dlc_job_with(
        data_dir,
        job_id,
        crate::jobs::retry_job,
        |placeholder, url, name, hash| {
            recorded_download_with_sha256(data_dir, placeholder, url, name, JobKind::Archive, hash)
        },
    )
}

fn resume_catalog_dlc_job_with(
    data_dir: &Path,
    job_id: &str,
    mut retry: impl FnMut(&Path, &str) -> Result<PathBuf>,
    download: impl FnMut(&Game, &str, &Path, Option<&str>) -> Result<PathBuf>,
) -> Result<Option<usize>> {
    let job = crate::jobs::load_job(data_dir, job_id)?;
    if job.kind != JobKind::Archive {
        return Ok(None);
    }
    let root = data_dir.join("dlc-downloads");
    match fs::symlink_metadata(&root) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err("DLC download root is not a regular directory".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let root = fs::canonicalize(root)?;
    let parent = fs::canonicalize(job.target.parent().ok_or("invalid download target")?)?;
    let Ok(relative) = parent.strip_prefix(&root) else {
        return Ok(None);
    };
    let parts = relative.components().collect::<Vec<_>>();
    let [Component::Normal(game_id), Component::Normal(dlc_id)] = parts.as_slice() else {
        return Err("DLC archive job has an invalid staging path".into());
    };
    let game_id = game_id.to_str().ok_or("DLC game ID is not UTF-8")?;
    let dlc_id = dlc_id.to_str().ok_or("DLC ID is not UTF-8")?;
    validate_id(game_id)?;
    validate_id(dlc_id)?;
    if job
        .install
        .as_ref()
        .is_none_or(|plan| plan.game_id != game_id)
    {
        return Err("DLC archive job does not match its game".into());
    }
    let _lock = lock_game(data_dir, game_id)?;
    let library = crate::load_library(data_dir)?;
    let game = library
        .games
        .get(game_id)
        .ok_or("DLC base game is no longer installed")?;
    let addon = available_for_game(game, data_dir)?
        .into_iter()
        .find(|addon| addon.id == dlc_id && addon.content_available)
        .ok_or("DLC is no longer offered by the game's content source")?;
    let filename = addon
        .filename
        .as_deref()
        .ok_or("DLC archive filename is missing")?;
    let names = if addon.archive_parts.is_empty() {
        vec![filename.to_string()]
    } else {
        multipart_names(filename, addon.archive_parts.len())?
    };
    let index = names
        .iter()
        .position(|name| job.target.file_name() == Some(std::ffi::OsStr::new(name)))
        .ok_or("DLC archive job file does not match the source")?;
    let expected_url = if addon.archive_parts.is_empty() {
        addon.uri.as_deref().ok_or("DLC download URL is missing")?
    } else {
        &addon.archive_parts[index]
    };
    if job.url != expected_url {
        cleanup_catalog_download(data_dir, &parent, &parent, &names, false)?;
        return Err("DLC download source changed; start it again from the game page".into());
    }
    if let Err(error) = retry(data_dir, job_id) {
        let cleanup = cleanup_catalog_download(data_dir, &parent, &parent, &names, true);
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => {
                Err(format!("{error}; archive cleanup failed: {cleanup_error}").into())
            }
        };
    }
    install_catalog_dlc_unlocked_with(game, data_dir, &addon, download).map(Some)
}

fn create_download_dir(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("DLC download path is not a regular directory".into());
    }
    Ok(())
}

fn cleanup_catalog_download(
    data_dir: &Path,
    download_dir: &Path,
    job_target_dir: &Path,
    names: &[String],
    preserve_paused: bool,
) -> Result<()> {
    if preserve_paused {
        for name in names {
            let id = job_id_for_target(&job_target_dir.join(name));
            if maybe_load_job(data_dir, &id)?.is_some_and(|job| job.status == JobStatus::Paused) {
                return Ok(());
            }
        }
    }
    for name in names {
        let id = job_id_for_target(&job_target_dir.join(name));
        clear_job_if_present(data_dir, &id)?;
    }
    match fs::symlink_metadata(download_dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            fs::remove_dir_all(download_dir)?;
        }
        Ok(_) => return Err("DLC download path is not a regular directory".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn install_catalog_dlc_with(
    game: &Game,
    data_dir: &Path,
    addon: &CatalogAddon,
    download: impl FnMut(&Game, &str, &Path, Option<&str>) -> Result<PathBuf>,
) -> Result<usize> {
    validate_id(&game.id)?;
    validate_id(&addon.id)?;
    let _lock = lock_game(data_dir, &game.id)?;
    install_catalog_dlc_unlocked_with(game, data_dir, addon, download)
}

fn install_catalog_dlc_unlocked_with(
    game: &Game,
    data_dir: &Path,
    addon: &CatalogAddon,
    mut download: impl FnMut(&Game, &str, &Path, Option<&str>) -> Result<PathBuf>,
) -> Result<usize> {
    validate_id(&game.id)?;
    validate_id(&addon.id)?;
    ensure_no_interrupted(game, data_dir)?;
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
    let names = if addon.archive_parts.is_empty() {
        vec![filename.to_string()]
    } else {
        let names = multipart_names(filename, addon.archive_parts.len())?;
        if names.len() != addon.archive_part_hashes.len() {
            return Err("DLC archive part hash count does not match URLs".into());
        }
        names
    };
    let download_root = data_dir.join("dlc-downloads");
    create_download_dir(&download_root)?;
    let game_downloads = download_root.join(&game.id);
    create_download_dir(&game_downloads)?;
    let download_dir = game_downloads.join(&addon.id);
    create_download_dir(&download_dir)?;
    let mut placeholder = game.clone();
    placeholder.install_dir = fs::canonicalize(&download_dir)?;
    placeholder.title = format!("{} DLC: {}", game.title, addon.title);
    let result = (|| -> Result<usize> {
        let archive = if addon.archive_parts.is_empty() {
            let url = addon.uri.as_deref().ok_or("DLC download URL is missing")?;
            download(
                &placeholder,
                url,
                Path::new(filename),
                addon.sha256.as_deref(),
            )?
        } else {
            let mut first = None;
            for ((name, url), hash) in names
                .iter()
                .zip(&addon.archive_parts)
                .zip(&addon.archive_part_hashes)
            {
                let path = download(&placeholder, url, Path::new(name), hash.as_deref())?;
                first.get_or_insert(path);
            }
            first.ok_or("DLC has no archive volumes")?
        };
        install_archive_unlocked(game, data_dir, &addon.id, &archive)
    })();
    let cleanup = cleanup_catalog_download(
        data_dir,
        &download_dir,
        &placeholder.install_dir,
        &names,
        true,
    );
    match (result, cleanup) {
        (Ok(count), Ok(())) => Ok(count),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => {
            Err(format!("DLC installed, but archive cleanup failed: {error}").into())
        }
        (Err(error), Err(cleanup_error)) => {
            Err(format!("{error}; archive cleanup failed: {cleanup_error}").into())
        }
    }
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
    let mut partial = Vec::new();
    for file in &manifest.files {
        if !file.created && file.backup_sha256.is_none() {
            continue;
        }
        let target = check_target(&manifest.install_dir, &file.path)?;
        if target.exists() {
            let current_hash = file_sha256(&target)?;
            if current_hash == file.sha256 || file.backup_sha256.as_ref() == Some(&current_hash) {
                continue;
            }
            let payload = state.join("payload").join(&file.path);
            if !fs::symlink_metadata(&payload)
                .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
                || !is_prefix_of(&payload, &target)?
            {
                return Err(
                    format!("Interrupted DLC file was modified: {}", file.path.display()).into(),
                );
            }
            partial.push(target);
        }
    }
    for path in staged {
        fs::remove_file(path)?;
    }
    for path in partial {
        fs::remove_file(path)?;
    }
    remove_unlocked(game, data_dir, dlc_id)?;
    if let Ok(steam_dlc_id) = dlc_id.parse::<u32>() {
        crate::steam_content::clear_observation(data_dir, &game.id, steam_dlc_id)?;
    }
    Ok(())
}

/// Roll back an interrupted DLC install after verifying any completed files.
pub fn recover_interrupted(game: &Game, data_dir: &Path, dlc_id: &str) -> Result<()> {
    recover_interrupted_with_guard(game, data_dir, dlc_id, || Ok(()))
}

/// Recover an interrupted DLC after checking launch state under the game lock.
pub fn recover_interrupted_with_guard(
    game: &Game,
    data_dir: &Path,
    dlc_id: &str,
    guard: impl FnOnce() -> Result<()>,
) -> Result<()> {
    validate_id(&game.id)?;
    validate_id(dlc_id)?;
    let _lock = lock_game(data_dir, &game.id)?;
    guard()?;
    recover_unlocked(game, data_dir, dlc_id)
}

/// Remove a DLC and all files recorded in its install manifest.
pub fn remove(game: &Game, data_dir: &Path, dlc_id: &str) -> Result<()> {
    remove_with_guard(game, data_dir, dlc_id, || Ok(()))
}

/// Remove a DLC after checking launch state under the game lock.
pub fn remove_with_guard(
    game: &Game,
    data_dir: &Path,
    dlc_id: &str,
    guard: impl FnOnce() -> Result<()>,
) -> Result<()> {
    validate_id(&game.id)?;
    validate_id(dlc_id)?;
    let _lock = lock_game(data_dir, &game.id)?;
    guard()?;
    ensure_no_interrupted(game, data_dir)?;
    if !fs::symlink_metadata(state_dir(data_dir, &game.id, dlc_id).join("complete"))
        .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
    {
        return Err("DLC installation was interrupted; recover it before removal".into());
    }
    remove_unlocked(game, data_dir, dlc_id)?;
    if let Ok(steam_dlc_id) = dlc_id.parse::<u32>() {
        crate::steam_content::clear_observation(data_dir, &game.id, steam_dlc_id)?;
    }
    Ok(())
}

fn remove_unlocked(game: &Game, data_dir: &Path, dlc_id: &str) -> Result<()> {
    let state = state_dir(data_dir, &game.id, dlc_id);
    let manifest = read_manifest(&state, game, dlc_id)?;
    let install = &manifest.install_dir;
    let complete = fs::symlink_metadata(state.join("complete"))
        .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink());
    let other_manifests = list_installed(game, data_dir)?
        .into_iter()
        .filter(|id| id != dlc_id)
        .map(|id| read_manifest(&state_dir(data_dir, &game.id, &id), game, &id))
        .collect::<Result<Vec<_>>>()?;
    for file in &manifest.files {
        if !file.managed || (!complete && !file.created && file.backup_sha256.is_none()) {
            continue;
        }
        let target = check_target(install, &file.path)?;
        if let Some(backup_hash) = &file.backup_sha256 {
            let backup = check_target(&state, &Path::new("backup").join(&file.path))?;
            let metadata = fs::symlink_metadata(&backup)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || file_sha256(&backup)? != *backup_hash
            {
                return Err("DLC base backup is missing or changed".into());
            }
        }
        if target.exists() {
            let current_hash = file_sha256(&target)?;
            if current_hash != file.sha256 && file.backup_sha256.as_ref() != Some(&current_hash) {
                return Err(format!(
                    "DLC file has been modified; cannot safely remove {}",
                    file.path.display()
                )
                .into());
            }
        }
    }
    for file in manifest.files.iter().rev() {
        if !file.managed || (!complete && !file.created && file.backup_sha256.is_none()) {
            continue;
        }
        if let Some(backup_hash) = &file.backup_sha256 {
            let other_owners = other_manifests
                .iter()
                .flat_map(|other| &other.files)
                .filter(|entry| entry.path == file.path)
                .collect::<Vec<_>>();
            if !other_owners.is_empty() {
                if other_owners.iter().any(|other| {
                    !other.managed
                        || other.sha256 != file.sha256
                        || other.backup_sha256.as_ref() != Some(backup_hash)
                }) {
                    return Err("Another DLC has incompatible overlay ownership".into());
                }
                continue;
            }
            let target = check_target(install, &file.path)?;
            let staged = check_target(install, &file.staged_path)?;
            let backup = check_target(&state, &Path::new("backup").join(&file.path))?;
            match fs::symlink_metadata(&staged) {
                Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                    fs::remove_file(&staged)?;
                }
                Ok(_) => return Err("DLC restore staging path is not a regular file".into()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let mut source = File::open(&backup)?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged)?;
            io::copy(&mut source, &mut output)?;
            output.sync_all()?;
            drop(output);
            if file_sha256(&staged)? != *backup_hash {
                return Err("DLC restore staging file does not match backup".into());
            }
            if target.exists() {
                let current_hash = file_sha256(&target)?;
                if current_hash == *backup_hash {
                    fs::remove_file(staged)?;
                    continue;
                }
                if current_hash != file.sha256 {
                    return Err(format!(
                        "DLC file has been modified; cannot safely remove {}",
                        file.path.display()
                    )
                    .into());
                }
                fs::remove_file(&target)?;
            }
            fs::rename(staged, target)?;
            continue;
        }
        if other_manifests.iter().any(|manifest| {
            manifest.files.iter().any(|other| {
                other.path == file.path && other.sha256 == file.sha256 && other.managed
            })
        }) {
            continue;
        }
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
    fn owned_staging_moves_with_a_safe_cross_device_copy_fallback() {
        let root = std::env::temp_dir().join(format!("azdl-dlc-move-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        let target = root.join("target");
        fs::write(&source, b"verified bytes").unwrap();
        let mut readonly = fs::metadata(&source).unwrap().permissions();
        readonly.set_readonly(true);
        fs::set_permissions(&source, readonly).unwrap();
        move_staged_file(&source, &target).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&target).unwrap(), b"verified bytes");
        assert!(!fs::metadata(&target).unwrap().permissions().readonly());
        fs::write(&source, b"other bytes").unwrap();
        assert!(move_staged_file(&source, &target).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"other bytes");
        assert_eq!(fs::read(&target).unwrap(), b"verified bytes");
        fs::remove_file(&target).unwrap();
        let mut readonly = fs::metadata(&source).unwrap().permissions();
        readonly.set_readonly(true);
        fs::set_permissions(&source, readonly).unwrap();
        move_staged_file_with(&source, &target, |_, _| {
            Err(io::ErrorKind::CrossesDevices.into())
        })
        .unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&target).unwrap(), b"other bytes");
        fs::write(&source, b"keep on failure").unwrap();
        let error = move_staged_file_with(&source, &root.join("denied"), |_, _| {
            Err(io::ErrorKind::PermissionDenied.into())
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("denied"));
        assert_eq!(fs::read(&source).unwrap(), b"keep on failure");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_dlc_finalizes_moves_and_removes_without_open_file_handles() {
        let root = std::env::temp_dir().join(format!("azdl-dlc-finalize-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let install_dir = root.join("games/base");
        let source = root.join("source/Content/DLC");
        fs::create_dir_all(&install_dir).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::write(install_dir.join("base.exe"), b"base").unwrap();
        fs::write(source.join("bonus.dat"), b"bonus").unwrap();
        let mut game = Game {
            id: "base".into(),
            title: "Base".into(),
            executable: install_dir.join("base.exe"),
            install_dir,
            runner: Runner::Native,
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
        assert_eq!(
            install_staged(&game, &root, "bonus", &root.join("source")).unwrap(),
            1
        );
        assert_eq!(list_installed(&game, &root).unwrap(), ["bonus"]);
        assert!(!state_dir(&root, "base", "bonus").join("payload").exists());
        prepare_move(&game, &root).unwrap();
        let moved = root.join("moved");
        fs::rename(&game.install_dir, &moved).unwrap();
        game.install_dir = moved;
        remove(&game, &root, "bonus").unwrap();
        assert!(!game.install_dir.join("Content/DLC/bonus.dat").exists());
        assert_eq!(
            fs::read(game.install_dir.join("base.exe")).unwrap(),
            b"base"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_is_synced_with_write_access_without_overwriting_existing_files() {
        let root = std::env::temp_dir().join(format!("azdl-dlc-backup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let backup = root.join("backup.bin");
        fs::write(&source, b"base").unwrap();
        let permissions = fs::metadata(&source).unwrap().permissions();
        let mut readonly = permissions.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&source, readonly).unwrap();
        copy_synced_file(&source, &backup).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), b"base");
        assert!(!fs::metadata(&backup).unwrap().permissions().readonly());
        let error = copy_synced_file(&source, &backup).unwrap_err().to_string();
        assert!(error.contains("create copy"));
        assert!(error.contains(backup.to_string_lossy().as_ref()));
        assert_eq!(fs::read(&backup).unwrap(), b"base");
        fs::set_permissions(source, permissions).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archive_overlay_restores_base_after_move_and_interruption() {
        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-overlay-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data_dir = root.join("data");
        let install_dir = root.join("games/base");
        fs::create_dir_all(install_dir.join("content")).unwrap();
        fs::write(install_dir.join("content/shared.bin"), b"base").unwrap();
        let mut game = Game {
            id: "base".into(),
            title: "Base".into(),
            install_dir,
            executable: PathBuf::from("base.exe"),
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
        let install_overlay = |game: &Game, id: &str| {
            install_payload_unlocked(game, &data_dir, id, true, |payload| {
                fs::create_dir_all(payload.join("content"))?;
                fs::write(payload.join("content/shared.bin"), b"addon")?;
                Ok(())
            })
        };
        assert_eq!(install_overlay(&game, "addon").unwrap(), 1);
        assert_eq!(
            fs::read(game.install_dir.join("content/shared.bin")).unwrap(),
            b"addon"
        );
        let state = state_dir(&data_dir, &game.id, "addon");
        assert_eq!(
            fs::read(state.join("backup/content/shared.bin")).unwrap(),
            b"base"
        );
        assert_eq!(install_overlay(&game, "other").unwrap(), 1);
        let other_state = state_dir(&data_dir, &game.id, "other");
        assert_eq!(
            fs::read(other_state.join("backup/content/shared.bin")).unwrap(),
            b"base"
        );
        fs::remove_file(other_state.join("complete")).unwrap();
        recover_interrupted(&game, &data_dir, "other").unwrap();
        assert_eq!(
            fs::read(game.install_dir.join("content/shared.bin")).unwrap(),
            b"addon"
        );
        assert_eq!(install_overlay(&game, "other").unwrap(), 1);
        remove(&game, &data_dir, "addon").unwrap();
        assert_eq!(
            fs::read(game.install_dir.join("content/shared.bin")).unwrap(),
            b"addon"
        );
        remove(&game, &data_dir, "other").unwrap();
        assert_eq!(
            fs::read(game.install_dir.join("content/shared.bin")).unwrap(),
            b"base"
        );
        assert_eq!(install_overlay(&game, "addon").unwrap(), 1);
        assert_eq!(install_overlay(&game, "other").unwrap(), 1);
        remove(&game, &data_dir, "other").unwrap();
        remove(&game, &data_dir, "addon").unwrap();
        assert_eq!(
            fs::read(game.install_dir.join("content/shared.bin")).unwrap(),
            b"base"
        );
        assert_eq!(install_overlay(&game, "addon").unwrap(), 1);
        fs::write(game.install_dir.join("content/shared.bin"), b"user edit").unwrap();
        assert!(remove(&game, &data_dir, "addon").is_err());
        assert_eq!(
            fs::read(game.install_dir.join("content/shared.bin")).unwrap(),
            b"user edit"
        );
        fs::write(game.install_dir.join("content/shared.bin"), b"addon").unwrap();
        prepare_move(&game, &data_dir).unwrap();
        let moved = root.join("moved/base");
        fs::create_dir_all(moved.parent().unwrap()).unwrap();
        fs::rename(&game.install_dir, &moved).unwrap();
        game.install_dir = moved;
        remove(&game, &data_dir, "addon").unwrap();
        assert_eq!(
            fs::read(game.install_dir.join("content/shared.bin")).unwrap(),
            b"base"
        );
        assert_eq!(install_overlay(&game, "addon").unwrap(), 1);
        fs::remove_file(state.join("complete")).unwrap();
        fs::remove_file(game.install_dir.join("content/shared.bin")).unwrap();
        recover_interrupted(&game, &data_dir, "addon").unwrap();
        assert_eq!(
            fs::read(game.install_dir.join("content/shared.bin")).unwrap(),
            b"base"
        );
        assert!(!state.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalog_dlc_rejects_path_like_id_before_creating_staging() {
        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-id-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let game = Game {
            id: "base".into(),
            title: "Base".into(),
            install_dir: root.join("games/base"),
            executable: PathBuf::from("base.exe"),
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
        let addon = CatalogAddon {
            id: "../escape".into(),
            title: "Invalid DLC".into(),
            kind: AddonKind::Dlc,
            uri: Some("https://example.invalid/dlc.zip".into()),
            filename: Some("dlc.zip".into()),
            archive_parts: Vec::new(),
            sha256: None,
            archive_part_hashes: Vec::new(),
            content_available: true,
            referer: None,
        };
        assert!(install_catalog_dlc(&game, &root, &addon).is_err());
        assert!(!root.join("dlc-downloads").exists());
    }

    #[test]
    fn catalog_dlc_cleans_failed_archive_without_changing_base() {
        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-cleanup-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let game_dir = root.join("games/base");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("base.exe"), b"base").unwrap();
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
        let addon = CatalogAddon {
            id: "bonus".into(),
            title: "Bonus".into(),
            kind: AddonKind::Dlc,
            uri: Some("https://example.invalid/bonus.zip".into()),
            filename: Some("bonus.zip".into()),
            archive_parts: Vec::new(),
            sha256: None,
            archive_part_hashes: Vec::new(),
            content_available: true,
            referer: None,
        };
        let staging = root.join("dlc-downloads/base/bonus");
        assert!(
            install_catalog_dlc_with(&game, &root, &addon, |placeholder, _, name, _| {
                let path = placeholder.install_dir.join(name);
                fs::write(&path, b"not an archive")?;
                crate::jobs::save_job(
                    &root,
                    &crate::jobs::DownloadJob {
                        id: job_id_for_target(&path),
                        kind: JobKind::Archive,
                        url: "https://example.invalid/bonus.zip".into(),
                        target: path.clone(),
                        status: crate::jobs::JobStatus::Complete,
                        pid: None,
                        error: None,
                        updated_unix: crate::jobs::now()?,
                        expected_sha256: None,
                        install: None,
                        install_state: None,
                        install_error: None,
                        progress_bytes: None,
                        total_bytes: None,
                    },
                )?;
                Ok(path)
            })
            .is_err()
        );
        assert!(!staging.exists());
        assert!(
            crate::jobs::load_job(&root, &job_id_for_target(&staging.join("bonus.zip"))).is_err()
        );
        assert!(list_interrupted(&game, &root).unwrap().is_empty());
        assert!(
            install_catalog_dlc_with(&game, &root, &addon, |placeholder, _, name, _| {
                fs::write(
                    placeholder.install_dir.join(name).with_extension("part"),
                    b"partial",
                )?;
                Err("network unavailable".into())
            })
            .is_err()
        );
        assert!(!staging.exists());
        let stopped = install_catalog_dlc_with(&game, &root, &addon, |placeholder, _, name, _| {
            let path = placeholder.install_dir.join(name);
            fs::write(path.with_file_name("bonus.zip.part"), b"partial")?;
            crate::jobs::save_job(
                &root,
                &crate::jobs::DownloadJob {
                    id: job_id_for_target(&path),
                    kind: JobKind::Archive,
                    url: "https://example.invalid/bonus.zip".into(),
                    target: path.clone(),
                    status: JobStatus::Paused,
                    pid: None,
                    error: None,
                    updated_unix: crate::jobs::now()?,
                    expected_sha256: None,
                    install: None,
                    install_state: None,
                    install_error: None,
                    progress_bytes: None,
                    total_bytes: None,
                },
            )?;
            Err(crate::jobs::TransferStopped::Paused(job_id_for_target(&path)).into())
        })
        .unwrap_err();
        assert!(matches!(
            stopped.downcast_ref::<crate::jobs::TransferStopped>(),
            Some(crate::jobs::TransferStopped::Paused(_))
        ));
        assert!(staging.join("bonus.zip.part").exists());
        assert_eq!(
            crate::jobs::load_job(&root, &job_id_for_target(&staging.join("bonus.zip")))
                .unwrap()
                .status,
            JobStatus::Paused
        );
        clear_job_if_present(&root, &job_id_for_target(&staging.join("bonus.zip"))).unwrap();
        fs::remove_dir_all(&staging).unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::create_dir_all(root.join("jobs")).unwrap();
        let target = fs::canonicalize(&staging).unwrap().join("bonus.zip");
        let id = job_id_for_target(&target);
        fs::write(staging.join("bonus.zip.part"), b"partial").unwrap();
        fs::write(root.join("jobs").join(format!("{id}.pause")), b"pause").unwrap();
        let stopped = install_catalog_dlc(&game, &root, &addon).unwrap_err();
        assert!(matches!(
            stopped.downcast_ref::<crate::jobs::TransferStopped>(),
            Some(crate::jobs::TransferStopped::Paused(job_id)) if job_id == &id
        ));
        assert_eq!(
            fs::read(staging.join("bonus.zip.part")).unwrap(),
            b"partial"
        );
        validate_catalog_dlc_resume(&root, &id, "base", "bonus").unwrap();
        clear_job_if_present(&root, &id).unwrap();
        fs::write(root.join("jobs").join(format!("{id}.cancel")), b"cancel").unwrap();
        let stopped = install_catalog_dlc(&game, &root, &addon).unwrap_err();
        assert!(matches!(
            stopped.downcast_ref::<crate::jobs::TransferStopped>(),
            Some(crate::jobs::TransferStopped::Cancelled(Some(job_id))) if job_id == &id
        ));
        assert!(!staging.exists());
        assert!(crate::jobs::load_job(&root, &id).is_err());
        assert_eq!(fs::read(game_dir.join("base.exe")).unwrap(), b"base");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resumed_catalog_archive_commits_dlc_and_cleans_transfer() {
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-resume-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let game_dir = root.join("games/base");
        let source = root.join("source/Content/DLC");
        let cache = root.join("content-sources");
        let staging = root.join("dlc-downloads/base/bonus");
        for dir in [&game_dir, &source, &cache, &staging] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(game_dir.join("base.exe"), b"base").unwrap();
        fs::write(source.join("bonus.dat"), b"bonus").unwrap();
        let content_source = |url: &str| CatalogAddon {
            id: "bonus".into(),
            title: "Bonus".into(),
            kind: AddonKind::Dlc,
            uri: Some(url.into()),
            filename: Some("bonus.zip".into()),
            archive_parts: Vec::new(),
            sha256: None,
            archive_part_hashes: Vec::new(),
            content_available: true,
            referer: None,
        };
        fs::write(
            cache.join("base.json"),
            serde_json::to_vec(&vec![content_source("https://example.invalid/bonus.zip")]).unwrap(),
        )
        .unwrap();
        let archive = root.join("bonus.zip");
        assert!(
            Command::new("bsdtar")
                .args(["-a", "-cf"])
                .arg(&archive)
                .arg("-C")
                .arg(root.join("source"))
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
            origin: Some(crate::CatalogOrigin {
                qualified_id: "stub:demo".into(),
                upload_date: None,
                version: None,
            }),
            steam_app_id: None,
            launch_options: Vec::new(),
            selected_launch_option_id: None,
            cover_url: None,
            banner_url: None,
        };
        let mut library = Library::default();
        library.games.insert(game.id.clone(), game.clone());
        crate::save_library(&root, &library).unwrap();
        let target = staging.join("bonus.zip");
        let id = job_id_for_target(&target);
        fs::write(staging.join("bonus.zip.part"), b"partial").unwrap();
        crate::jobs::save_job(
            &root,
            &crate::jobs::DownloadJob {
                id: id.clone(),
                kind: JobKind::Archive,
                url: "https://example.invalid/bonus.zip".into(),
                target: target.clone(),
                status: JobStatus::Paused,
                pid: None,
                error: None,
                updated_unix: crate::jobs::now().unwrap(),
                expected_sha256: None,
                install: Some(crate::jobs::InstallPlan {
                    game_id: game.id.clone(),
                    title: "Base DLC: Bonus".into(),
                    kind: crate::jobs::InstallKind::Archive,
                }),
                install_state: None,
                install_error: None,
                progress_bytes: None,
                total_bytes: None,
            },
        )
        .unwrap();
        validate_catalog_dlc_resume(&root, &id, "base", "bonus").unwrap();
        fs::create_dir_all(staging.parent().unwrap().join("other")).unwrap();
        assert!(validate_catalog_dlc_resume(&root, &id, "base", "other").is_err());
        assert!(validate_catalog_dlc_resume(&root, &id, "../base", "bonus").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&staging, staging.parent().unwrap().join("alias")).unwrap();
            assert!(validate_catalog_dlc_resume(&root, &id, "base", "alias").is_err());
        }
        let mut mismatched = crate::jobs::load_job(&root, &id).unwrap();
        mismatched.install.as_mut().unwrap().game_id = "other".into();
        crate::jobs::save_job(&root, &mismatched).unwrap();
        assert!(validate_catalog_dlc_resume(&root, &id, "base", "bonus").is_err());
        mismatched.install.as_mut().unwrap().game_id = "base".into();
        mismatched.status = JobStatus::Interrupted;
        crate::jobs::save_job(&root, &mismatched).unwrap();
        assert!(validate_catalog_dlc_resume(&root, &id, "base", "bonus").is_err());
        mismatched.status = JobStatus::Paused;
        crate::jobs::save_job(&root, &mismatched).unwrap();
        let count = resume_catalog_dlc_job_with(
            &root,
            &id,
            |data_dir, job_id| {
                let mut job = crate::jobs::load_job(data_dir, job_id)?;
                fs::copy(&archive, &job.target)?;
                fs::remove_file(job.partial_path())?;
                job.status = JobStatus::Complete;
                crate::jobs::save_job(data_dir, &job)?;
                Ok(job.target)
            },
            |placeholder, _, name, _| Ok(placeholder.install_dir.join(name)),
        )
        .unwrap();
        assert_eq!(count, Some(1));
        assert_eq!(fs::read(game_dir.join("base.exe")).unwrap(), b"base");
        assert_eq!(
            fs::read(game_dir.join("Content/DLC/bonus.dat")).unwrap(),
            b"bonus"
        );
        assert!(!staging.exists());
        assert!(crate::jobs::load_job(&root, &id).is_err());
        fs::write(
            cache.join("base.json"),
            serde_json::to_vec(&vec![content_source(
                "https://example.invalid/new-bonus.zip",
            )])
            .unwrap(),
        )
        .unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("bonus.zip.part"), b"old source").unwrap();
        crate::jobs::save_job(
            &root,
            &crate::jobs::DownloadJob {
                id: id.clone(),
                kind: JobKind::Archive,
                url: "https://example.invalid/bonus.zip".into(),
                target,
                status: JobStatus::Paused,
                pid: None,
                error: None,
                updated_unix: crate::jobs::now().unwrap(),
                expected_sha256: None,
                install: Some(crate::jobs::InstallPlan {
                    game_id: game.id.clone(),
                    title: "Base DLC: Bonus".into(),
                    kind: crate::jobs::InstallKind::Archive,
                }),
                install_state: None,
                install_error: None,
                progress_bytes: None,
                total_bytes: None,
            },
        )
        .unwrap();
        let error = resume_catalog_dlc_job_with(
            &root,
            &id,
            |_, _| panic!("changed DLC source must not resume"),
            |_, _, _, _| panic!("changed DLC source must not install"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("source changed"), "{error}");
        assert!(!staging.exists());
        assert!(crate::jobs::load_job(&root, &id).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identical_shared_files_keep_their_original_owner() {
        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-shared-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("games/base");
        let source = root.join("source");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&source).unwrap();
        let target = install.join("shared.bin");
        fs::write(&target, b"same bytes").unwrap();
        fs::write(source.join("shared.bin"), b"same bytes").unwrap();
        let game = Game {
            id: "base".into(),
            title: "Base".into(),
            install_dir: install,
            executable: PathBuf::from("base.exe"),
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

        fs::write(&target, b"different").unwrap();
        assert!(install_staged(&game, &root, "conflict", &source).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"different");
        fs::write(&target, b"same bytes").unwrap();

        install_staged(&game, &root, "base-shared", &source).unwrap();
        let base_shared = read_manifest(
            &state_dir(&root, "base", "base-shared"),
            &game,
            "base-shared",
        )
        .unwrap();
        assert!(!base_shared.files[0].managed);
        assert!(!base_shared.files[0].created);
        remove(&game, &root, "base-shared").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"same bytes");

        fs::remove_file(&target).unwrap();
        install_staged(&game, &root, "dlc-a", &source).unwrap();
        install_staged(&game, &root, "dlc-b", &source).unwrap();
        let shared = read_manifest(&state_dir(&root, "base", "dlc-b"), &game, "dlc-b").unwrap();
        assert!(shared.files[0].managed);
        assert!(!shared.files[0].created);
        remove(&game, &root, "dlc-b").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"same bytes");
        remove(&game, &root, "dlc-a").unwrap();
        assert!(!target.exists());
        fs::remove_dir_all(root).unwrap();
    }

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
        assert_eq!(
            install_archive(&game, &root, "another", &archive).unwrap(),
            1
        );
        fs::write(game_dir.join("Content/DLC/bonus.dat"), b"modified").unwrap();
        assert!(remove(&game, &root, "bonus").is_err());
        assert_eq!(
            fs::read(game_dir.join("Content/DLC/bonus.dat")).unwrap(),
            b"modified"
        );
        fs::write(game_dir.join("Content/DLC/bonus.dat"), b"bonus").unwrap();
        remove(&game, &root, "bonus").unwrap();
        assert_eq!(fs::read(game_dir.join("base.exe")).unwrap(), b"base");
        assert_eq!(
            fs::read(game_dir.join("Content/DLC/bonus.dat")).unwrap(),
            b"bonus"
        );
        remove(&game, &root, "another").unwrap();
        assert!(!game_dir.join("Content/DLC/bonus.dat").exists());
        assert_eq!(
            install_staged(&game, &root, "depot-dlc", &source).unwrap(),
            1
        );
        assert_eq!(
            fs::read(game_dir.join("Content/DLC/bonus.dat")).unwrap(),
            b"bonus"
        );
        remove(&game, &root, "depot-dlc").unwrap();
        let stable_source = root.join("stable-source");
        fs::create_dir_all(&stable_source).unwrap();
        fs::write(stable_source.join("stable.dat"), b"stable").unwrap();
        install_staged(&game, &root, "stable", &stable_source).unwrap();
        install_archive(&game, &root, "bonus2", &archive).unwrap();
        let interrupted = state_dir(&root, &game.id, "bonus2");
        let manifest: DlcManifest =
            serde_json::from_slice(&fs::read(interrupted.join("manifest.json")).unwrap()).unwrap();
        fs::remove_file(interrupted.join("complete")).unwrap();
        let staged = game_dir.join(&manifest.files[0].staged_path);
        fs::write(&staged, b"partial").unwrap();
        assert_eq!(list_interrupted(&game, &root).unwrap(), vec!["bonus2"]);
        let error = install_staged(&game, &root, "bonus3", &source)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Recover interrupted DLC"), "{error}");
        assert!(!state_dir(&root, &game.id, "bonus3").exists());
        let error = remove(&game, &root, "stable").unwrap_err().to_string();
        assert!(error.contains("Recover interrupted DLC"), "{error}");
        assert_eq!(fs::read(game_dir.join("stable.dat")).unwrap(), b"stable");
        let target = game_dir.join("Content/DLC/bonus.dat");
        fs::write(&target, b"changed").unwrap();
        assert!(recover_interrupted(&game, &root, "bonus2").is_err());
        assert!(staged.exists());
        let payload = interrupted.join("payload/Content/DLC/bonus.dat");
        fs::create_dir_all(payload.parent().unwrap()).unwrap();
        fs::write(&payload, b"bonus").unwrap();
        fs::write(&target, b"bon").unwrap();
        recover_interrupted(&game, &root, "bonus2").unwrap();
        assert!(!staged.exists());
        assert!(!game_dir.join("Content/DLC/bonus.dat").exists());
        remove(&game, &root, "stable").unwrap();
        assert!(!game_dir.join("stable.dat").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
