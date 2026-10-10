use crate::catalog::{CatalogGame, CatalogSources, PackageType, find_game, install_game};
use crate::patch::active_patch;
use crate::{CatalogOrigin, Game, LaunchOption, Library, Result, save_library};
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct AvailableUpdate {
    pub game_id: String,
    pub title: String,
    pub qualified_id: String,
    pub remote_upload_date: Option<String>,
    pub remote_version: Option<String>,
}

fn compare_versions(remote: &str, local: &str) -> Ordering {
    let remote = remote.trim_start_matches(['v', 'V']);
    let local = local.trim_start_matches(['v', 'V']);
    let remote_parts: Vec<&str> = remote.split(['.', '-', '_']).collect();
    let local_parts: Vec<&str> = local.split(['.', '-', '_']).collect();
    for (remote_part, local_part) in remote_parts.iter().zip(local_parts.iter()) {
        let ordering = match (remote_part.parse::<u128>(), local_part.parse::<u128>()) {
            (Ok(remote), Ok(local)) => remote.cmp(&local),
            _ => remote_part.cmp(local_part),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    remote_parts.len().cmp(&local_parts.len())
}

pub fn has_update(origin: &CatalogOrigin, remote: &CatalogGame) -> bool {
    if origin.qualified_id != remote.qualified_id() {
        return false;
    }
    if let (Some(local), Some(current)) = (&origin.upload_date, &remote.upload_date)
        && current > local
    {
        return true;
    }
    if let (Some(local), Some(current)) = (&origin.version, &remote.version) {
        if local.bytes().all(|byte| byte.is_ascii_digit())
            && current.bytes().all(|byte| byte.is_ascii_digit())
        {
            return local != current;
        }
        return compare_versions(current, local) == Ordering::Greater;
    }
    false
}

pub fn available_updates(
    library: &Library,
    data_dir: &Path,
    sources: &CatalogSources,
) -> Result<Vec<AvailableUpdate>> {
    let mut updates = Vec::new();
    for (id, installed) in &library.games {
        let Some(origin) = &installed.origin else {
            continue;
        };
        let remote = find_game(data_dir, sources, &origin.qualified_id)?;
        if has_update(origin, &remote) {
            updates.push(AvailableUpdate {
                game_id: id.clone(),
                title: installed.title.clone(),
                qualified_id: origin.qualified_id.clone(),
                remote_upload_date: remote.upload_date,
                remote_version: remote.version,
            });
        }
    }
    Ok(updates)
}

fn remap_launch_settings(
    old: &Game,
    staged_dir: &Path,
    destination: &Path,
) -> (Vec<LaunchOption>, Option<String>) {
    let staged_root = match fs::canonicalize(staged_dir) {
        Ok(root) => root,
        Err(_) => return (Vec::new(), None),
    };
    let options: Vec<LaunchOption> = old
        .launch_options
        .iter()
        .filter_map(|option| {
            let relative_executable = option.executable.strip_prefix(&old.install_dir).ok()?;
            let executable = fs::canonicalize(staged_dir.join(relative_executable)).ok()?;
            if !executable.is_file() || !executable.starts_with(&staged_root) {
                return None;
            }
            let working_directory = match option.working_directory.as_ref() {
                Some(path) => {
                    let relative = path.strip_prefix(&old.install_dir).ok()?;
                    let staged = fs::canonicalize(staged_dir.join(relative)).ok()?;
                    if !staged.is_dir() || !staged.starts_with(&staged_root) {
                        return None;
                    }
                    Some(destination.join(staged.strip_prefix(&staged_root).ok()?))
                }
                None => None,
            };
            let mut preserved = option.clone();
            preserved.executable = destination.join(executable.strip_prefix(&staged_root).ok()?);
            preserved.working_directory = working_directory;
            Some(preserved)
        })
        .collect();
    let selected = old
        .selected_launch_option_id
        .as_ref()
        .filter(|id| options.iter().any(|option| &option.id == *id))
        .cloned();
    (options, selected)
}

/// Stage and validate a replacement, then swap a managed game directory.
/// The previous installation is retained at the returned path.
pub fn update_managed_game(
    library: &mut Library,
    data_dir: &Path,
    sources: &CatalogSources,
    id: &str,
) -> Result<PathBuf> {
    let old = library.games.get(id).ok_or("unknown game ID")?.clone();
    if active_patch(data_dir, id)?.is_some() {
        return Err("roll back the active patch before updating this game".into());
    }
    let origin = old.origin.as_ref().ok_or("game has no catalog origin")?;
    let remote = find_game(data_dir, sources, &origin.qualified_id)?;
    if !has_update(origin, &remote) {
        return Err("no newer catalog version is available".into());
    }
    if !matches!(
        remote.package_type,
        PackageType::Archive | PackageType::Executable | PackageType::Magnet
    ) {
        return Err("this source uses its own update flow".into());
    }
    let managed = data_dir.join("games").join(id);
    let old_dir = fs::canonicalize(&old.install_dir)?;
    if fs::canonicalize(&managed)? != old_dir {
        return Err("automatic updates require an azdl managed installation".into());
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let stage_root = data_dir.join("updates").join(format!("{id}-{nonce}"));
    fs::create_dir_all(&stage_root)?;
    let mut staged_library = Library::default();
    install_game(&mut staged_library, &stage_root, &remote)?;
    let staged = staged_library
        .games
        .get(id)
        .ok_or("staged catalog game ID changed")?;
    let relative_exe = staged
        .executable
        .strip_prefix(&staged.install_dir)?
        .to_path_buf();
    if !staged.executable.is_file() {
        return Err("staged executable is missing".into());
    }
    let staged_dir = staged.install_dir.clone();
    let (retained_options, retained_selection) = remap_launch_settings(&old, &staged_dir, &old_dir);
    let backup = data_dir
        .join("games")
        .join(format!(".{id}.previous-{nonce}"));

    fs::rename(&old_dir, &backup)?;
    if let Err(error) = fs::rename(&staged_dir, &old_dir) {
        return match fs::rename(&backup, &old_dir) {
            Ok(()) => Err(format!("could not activate update: {error}; previous game restored").into()),
            Err(restore_error) => Err(format!("could not activate update: {error}; could not restore previous game: {restore_error}; previous files are at {}", backup.display()).into()),
        };
    }
    let mut replacement = staged.clone();
    replacement.install_dir = old_dir.clone();
    replacement.executable = old_dir.join(relative_exe);
    replacement.runner = old.runner.clone();
    replacement.arguments = old.arguments.clone();
    replacement.environment = old.environment.clone();
    replacement.last_played_unix = old.last_played_unix;
    replacement.launch_options = retained_options;
    replacement.selected_launch_option_id = retained_selection;
    library.games.insert(id.to_string(), replacement);
    if let Err(error) = save_library(data_dir, library) {
        library.games.insert(id.to_string(), old);
        let failed_new = stage_root.join("failed-new-install");
        if let Err(move_error) = fs::rename(&old_dir, &failed_new) {
            return Err(format!("could not save updated library: {error}; could not move new files aside: {move_error}; new files are at {} and previous files at {}", old_dir.display(), backup.display()).into());
        }
        return match fs::rename(&backup, &old_dir) {
            Ok(()) => Err(format!("could not save updated library: {error}; previous game restored").into()),
            Err(restore_error) => Err(format!("could not save updated library: {error}; could not restore previous game: {restore_error}; previous files are at {} and new files at {}", backup.display(), failed_new.display()).into()),
        };
    }
    let _ = fs::remove_dir_all(stage_root);
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::parse_feed;
    use crate::patch::apply_patch;
    use crate::{add_game, add_launch_option, select_launch_option};

    #[test]
    fn compares_catalog_markers() {
        let old = CatalogOrigin {
            qualified_id: "local:game".into(),
            upload_date: Some("2026-01-01".into()),
            version: Some("1.9".into()),
        };
        let newer = parse_feed(br#"{"entries":[{"id":"game","title":"Game","uploadDate":"2026-01-02","version":"1.10"}]}"#, "local").unwrap().remove(0);
        assert!(has_update(&old, &newer));
        let current = CatalogOrigin {
            upload_date: newer.upload_date.clone(),
            version: newer.version.clone(),
            ..old
        };
        assert!(!has_update(&current, &newer));
        assert_eq!(compare_versions("1.10", "1.9"), Ordering::Greater);
    }

    #[test]
    fn update_keeps_only_launch_options_present_in_replacement() {
        let root = std::env::temp_dir().join(format!(
            "azdl-update-options-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let old_dir = root.join("old");
        let staged_dir = root.join("staged");
        let destination = root.join("active");
        for dir in [&old_dir, &staged_dir] {
            fs::create_dir_all(dir.join("bin")).unwrap();
            fs::write(dir.join("base.sh"), b"base").unwrap();
            fs::write(dir.join("bin/kept.sh"), b"kept").unwrap();
        }
        fs::write(old_dir.join("bin/removed.sh"), b"removed").unwrap();
        let mut library = Library::default();
        add_game(&mut library, "demo", "Demo", &old_dir, Path::new("base.sh")).unwrap();
        let game = library.games.get_mut("demo").unwrap();
        add_launch_option(
            game,
            "kept",
            "Kept",
            Path::new("bin/kept.sh"),
            Some(Path::new("bin")),
            &["--online".into()],
        )
        .unwrap();
        add_launch_option(
            game,
            "removed",
            "Removed",
            Path::new("bin/removed.sh"),
            None,
            &[],
        )
        .unwrap();
        select_launch_option(game, Some("kept")).unwrap();
        let (options, selected) = remap_launch_settings(game, &staged_dir, &destination);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].id, "kept");
        assert_eq!(options[0].executable, destination.join("bin/kept.sh"));
        assert_eq!(
            options[0].working_directory.as_deref(),
            Some(destination.join("bin").as_path())
        );
        assert_eq!(options[0].arguments, ["--online"]);
        assert_eq!(selected.as_deref(), Some("kept"));
        select_launch_option(game, Some("removed")).unwrap();
        let (_, selected) = remap_launch_settings(game, &staged_dir, &destination);
        assert_eq!(selected, None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_to_update_active_patch() {
        let root = std::env::temp_dir().join(format!(
            "azdl-update-patch-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        let source = root.join("patch-source");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::write(install.join("game.exe"), b"original").unwrap();
        fs::write(source.join("game.exe"), b"patched").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "test",
            "Test",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        apply_patch(&library.games["test"], &root, &source).unwrap();
        let error = update_managed_game(&mut library, &root, &CatalogSources::default(), "test")
            .unwrap_err();
        assert!(error.to_string().contains("roll back the active patch"));
        fs::remove_dir_all(root).unwrap();
    }
}
