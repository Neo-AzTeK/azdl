//! Steam installation requests; Store metadata never grants content access.
use crate::{CatalogOrigin, Game, Library, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// A Steam game to install: identity and optional Store artwork only.
#[derive(Debug, Clone)]
pub struct SteamGame {
    /// Library ID, `steam-<app id>` unless resuming a recorded job.
    pub id: String,
    pub title: String,
    pub app_id: u32,
    pub cover_url: Option<String>,
    pub banner_url: Option<String>,
}

/// Result of an installation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The game is registered in the library.
    Installed(String),
    /// Files are installed but the launch executable must be chosen by the user.
    NeedsSelection(String),
}

/// Construct a direct Steam install request without querying content services.
pub fn game_for_app(app_id: u32, title: &str) -> Result<SteamGame> {
    if app_id == 0 || title.trim().is_empty() {
        return Err("Steam App ID and game title are required".into());
    }
    Ok(SteamGame {
        id: format!("steam-{app_id}"),
        title: title.trim().into(),
        app_id,
        cover_url: None,
        banner_url: None,
    })
}

/// Install a Steam game into a selected library drive.
pub fn install_game_in(
    library: &mut Library,
    data_dir: &Path,
    games_dir: &Path,
    game: &SteamGame,
) -> Result<InstallOutcome> {
    crate::steam_depot::install_steam_manifest_game_in(
        library,
        data_dir,
        games_dir,
        game,
        game.app_id,
    )
}

fn origin_for(game: &SteamGame) -> CatalogOrigin {
    CatalogOrigin {
        qualified_id: format!("steam:{}", game.id),
        upload_date: None,
        version: None,
    }
}

/// Record the Steam origin, App ID and Store artwork on an installed library entry.
pub(crate) fn stamp_origin(library: &mut Library, id: &str, game: &SteamGame) {
    if let Some(installed) = library.games.get_mut(id) {
        installed.origin = Some(origin_for(game));
        installed.steam_app_id = Some(game.app_id);
        if game.banner_url.is_some() {
            installed.banner_url = game.banner_url.clone();
        }
        if game.cover_url.is_some() {
            installed.cover_url = game.cover_url.clone();
        }
    }
}

/// Origins of installs waiting for executable selection; the directory name predates Steam-only installs.
fn pending_origin_path(data_dir: &Path, id: &str) -> Result<PathBuf> {
    crate::validate_id(id)?;
    Ok(data_dir.join("setup-pending").join(format!("{id}.json")))
}

pub(crate) fn save_pending_origin(data_dir: &Path, game: &SteamGame) -> Result<()> {
    let target = pending_origin_path(data_dir, &game.id)?;
    fs::create_dir_all(target.parent().ok_or("invalid pending origin path")?)?;
    let temp = target.with_extension("json.tmp");
    let mut file = File::create(&temp)?;
    serde_json::to_writer_pretty(&mut file, &origin_for(game))?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(&target)?;
    }
    fs::rename(temp, target)?;
    Ok(())
}

/// Attach the recorded Steam origin when a pending install gets its executable.
pub fn attach_pending_origin(data_dir: &Path, game: &mut Game) -> Result<()> {
    match fs::read(pending_origin_path(data_dir, &game.id)?) {
        Ok(bytes) => {
            let origin: CatalogOrigin = serde_json::from_slice(&bytes)?;
            game.steam_app_id = origin
                .qualified_id
                .strip_prefix("steam:steam-")
                .and_then(|id| id.parse().ok());
            game.origin = Some(origin);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub fn clear_pending_origin(data_dir: &Path, id: &str) -> Result<()> {
    match fs::remove_file(pending_origin_path(data_dir, id)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Resume an interrupted or paused base download from its recorded target and ID.
/// Manifest archives and verified depot groups remain pinned in the transfer cache.
pub fn resume_job(data_dir: &std::path::Path, id: &str) -> Result<std::path::PathBuf> {
    let job = crate::jobs::load_job(data_dir, id)?;
    if matches!(
        job.effective_status(),
        crate::jobs::JobStatus::Running
            | crate::jobs::JobStatus::Complete
            | crate::jobs::JobStatus::Cancelled
    ) {
        return Err("Steam download is active or already finished".into());
    }
    let app_id = job
        .url
        .strip_prefix("steam://")
        .and_then(|id| id.parse::<u32>().ok())
        .filter(|id| *id > 0)
        .ok_or("Invalid Steam base download URL")?;
    let plan = job
        .install
        .as_ref()
        .ok_or("Steam download has no installation plan")?;
    crate::validate_id(&plan.game_id)?;
    let root = job.target.parent().ok_or("Invalid Steam download target")?;
    if job.target.file_name() != Some(std::ffi::OsStr::new(&plan.game_id)) {
        return Err("Steam download target does not match its game ID".into());
    }
    let settings = crate::settings::load_settings(data_dir)?;
    let root = std::fs::canonicalize(root)?;
    if !settings
        .storage_drives
        .iter()
        .any(|drive| std::fs::canonicalize(&drive.path).is_ok_and(|path| path == root))
    {
        return Err("Steam download target is no longer a configured storage drive".into());
    }
    let mut library = crate::load_library(data_dir)?;
    if library.games.contains_key(&plan.game_id) {
        return Err("Game is already registered in the library".into());
    }
    let mut game = game_for_app(app_id, &plan.title)?;
    game.id.clone_from(&plan.game_id);
    crate::jobs::clear_pause(data_dir, id)?;
    crate::jobs::clear_cancel(data_dir, id)?;
    let outcome = crate::steam_depot::install_steam_manifest_game_in(
        &mut library,
        data_dir,
        &root,
        &game,
        app_id,
    )?;
    if let InstallOutcome::Installed(installed_id) = outcome {
        let mut fresh = crate::load_library(data_dir)?;
        if let Some(installed) = library.games.remove(&installed_id) {
            fresh.games.insert(installed_id, installed);
        }
        crate::save_library(data_dir, &fresh)?;
    }
    Ok(job.target)
}

/// Remove only the recorded base transfer cache after cancellation.
pub(crate) fn cleanup_cancelled_download(
    data_dir: &std::path::Path,
    job: &crate::jobs::DownloadJob,
) -> Result<()> {
    let app_id = job
        .url
        .strip_prefix("steam://")
        .and_then(|id| id.parse::<u32>().ok())
        .filter(|id| *id > 0)
        .ok_or("Invalid Steam base download URL")?;
    let plan = job
        .install
        .as_ref()
        .ok_or("Steam download has no installation plan")?;
    crate::validate_id(&plan.game_id)?;
    let root = data_dir.join("downloads");
    if std::fs::symlink_metadata(&root).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("Steam download root is a symbolic link".into());
    }
    let cache = root.join(format!("steam-{app_id}-{}", plan.game_id));
    match std::fs::symlink_metadata(&cache) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            std::fs::remove_dir_all(cache)?
        }
        Ok(_) => return Err("Steam transfer cache is not a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn transfer_fixture() -> (std::path::PathBuf, crate::jobs::DownloadJob) {
        let root = std::env::temp_dir().join(format!(
            "azdl-base-controls-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("games")).unwrap();
        let target = root.join("games/old-id");
        let job = crate::jobs::DownloadJob {
            id: crate::jobs::job_id_for_target(&target),
            kind: crate::jobs::JobKind::Game,
            url: "steam://100".into(),
            target,
            status: crate::jobs::JobStatus::Paused,
            pid: None,
            error: None,
            updated_unix: 0,
            install: Some(crate::jobs::InstallPlan {
                game_id: "old-id".into(),
                title: "Base".into(),
            }),
            install_state: Some(crate::jobs::InstallState::Pending),
            install_error: None,
            progress_bytes: Some(32),
            total_bytes: Some(100),
        };
        crate::jobs::save_job(&root, &job).unwrap();
        (root, job)
    }

    #[test]
    fn base_cancel_removes_only_its_cache_and_commit_rejects_stop() {
        let (root, mut job) = transfer_fixture();
        let cache = root.join("downloads/steam-100-old-id");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("partial.bin"), b"partial").unwrap();
        let unrelated = root.join("downloads/keep");
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(unrelated.join("file"), b"keep").unwrap();
        job.install_state = Some(crate::jobs::InstallState::Installing);
        crate::jobs::save_job(&root, &job).unwrap();
        assert!(crate::jobs::pause_job(&root, &job.id).is_err());
        assert!(crate::jobs::cancel_job(&root, &job.id).is_err());
        assert!(cache.exists());
        job.install_state = Some(crate::jobs::InstallState::Pending);
        crate::jobs::save_job(&root, &job).unwrap();
        crate::jobs::cancel_job(&root, &job.id).unwrap();
        assert!(!cache.exists());
        assert_eq!(std::fs::read(unrelated.join("file")).unwrap(), b"keep");
        assert_eq!(
            crate::jobs::load_job(&root, &job.id).unwrap().status,
            crate::jobs::JobStatus::Cancelled
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn base_resume_rejects_changed_target_and_unconfigured_drive_before_network() {
        let (root, mut job) = transfer_fixture();
        job.install.as_mut().unwrap().game_id = "another-id".into();
        crate::jobs::save_job(&root, &job).unwrap();
        assert!(
            resume_job(&root, &job.id)
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
        job.install.as_mut().unwrap().game_id = "old-id".into();
        let other = root.join("unconfigured");
        std::fs::create_dir_all(&other).unwrap();
        job.target = other.join("old-id");
        crate::jobs::save_job(&root, &job).unwrap();
        assert!(
            resume_job(&root, &job.id)
                .unwrap_err()
                .to_string()
                .contains("configured storage")
        );
        assert!(!job.target.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direct_request_requires_no_installed_resources() {
        let game = game_for_app(480, "Spacewar").unwrap();
        assert_eq!(game.id, "steam-480");
        assert_eq!(game.app_id, 480);
        assert!(game_for_app(0, "Spacewar").is_err());
        assert!(game_for_app(480, " ").is_err());
    }
}
