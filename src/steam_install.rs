//! Steam installation requests; Store metadata never grants content access.
use crate::Result;
use crate::catalog::{CatalogGame, PackageType};

/// Construct a direct Steam install request without querying content services.
pub fn game_for_app(app_id: u32, title: &str) -> Result<CatalogGame> {
    if app_id == 0 || title.trim().is_empty() {
        return Err("Steam App ID and game title are required".into());
    }
    Ok(CatalogGame {
        source_id: "steam".into(),
        id: format!("steam-{app_id}"),
        title: title.trim().into(),
        description: String::new(),
        uri: None,
        filename: None,
        archive_parts: Vec::new(),
        sha256: None,
        archive_part_hashes: Vec::new(),
        package_type: PackageType::Steam(app_id),
        upload_date: None,
        version: None,
        addons: Vec::new(),
        cover_url: None,
        banner_url: None,
        file_size: None,
        genres: Vec::new(),
        steam_app_id: Some(app_id),
    })
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
    if let crate::catalog::InstallOutcome::Installed(installed_id) = outcome {
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
            expected_sha256: None,
            install: Some(crate::jobs::InstallPlan {
                game_id: "old-id".into(),
                title: "Base".into(),
                kind: crate::jobs::InstallKind::Archive,
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
        assert_eq!(game.source_id, "steam");
        assert_eq!(game.id, "steam-480");
        assert!(game.addons.is_empty());
        assert!(game_for_app(0, "Spacewar").is_err());
        assert!(game_for_app(480, " ").is_err());
    }
}
