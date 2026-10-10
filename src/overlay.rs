use crate::{Game, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const OFF: &str = ".arachnel-off";
pub const MANAGED_APP_ID_MARKER: &str = ".azdl-steam-appid-managed";
const DLLS: &[&str] = &[
    "winmm.dll",
    "SteamFix64.dll",
    "SteamFix32.dll",
    "EpicFix64.dll",
    "OnlineFix64.dll",
    "OnlineFix32.dll",
    "OnlineFix.dll",
    "SteamOverlay32.dll",
    "SteamOverlay64.dll",
    "StubDRM32.dll",
    "StubDRM64.dll",
];

#[derive(Debug, Serialize)]
pub struct OverlayStatus {
    pub present: bool,
    pub enabled: bool,
    pub mixed: bool,
    pub can_toggle: bool,
    pub active_files: usize,
    pub disabled_files: usize,
    pub directories: Vec<PathBuf>,
}

struct OverlayDirectory {
    path: PathBuf,
    files: BTreeMap<String, String>,
    switch_dlls: bool,
}

fn skip_directory(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        ".depotdownloader" | "staging" | "engine" | "intermediate" | "saved" | "thirdparty"
    )
}

fn scan(install_dir: &Path) -> Result<Vec<OverlayDirectory>> {
    let root = fs::canonicalize(install_dir)?;
    if !root.is_dir() {
        return Err("game install directory is missing".into());
    }
    let mut stack = vec![(root, 0usize)];
    let mut seen_directories = 0usize;
    let mut found = Vec::new();
    while let Some((directory, depth)) = stack.pop() {
        seen_directories += 1;
        if seen_directories > 4000 {
            return Err("game has too many directories to scan for local fixes".into());
        }
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if metadata.is_dir() {
                if depth < 6 && !skip_directory(&name) {
                    stack.push((entry.path(), depth + 1));
                }
            } else if metadata.is_file() {
                files.insert(name.to_ascii_lowercase(), name);
            }
        }
        let identifies_fix = files.contains_key("onlinefix.ini")
            || files.contains_key("steamfix.ini")
            || files.contains_key("winmm.txt")
            || files.contains_key("dlllist.txt")
            || DLLS.iter().skip(1).any(|name| {
                let lower = name.to_ascii_lowercase();
                files.contains_key(&lower) || files.contains_key(&format!("{lower}{OFF}"))
            });
        if identifies_fix || files.contains_key(MANAGED_APP_ID_MARKER) {
            found.push(OverlayDirectory {
                path: directory,
                files,
                switch_dlls: identifies_fix,
            });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

fn paths(directory: &OverlayDirectory, name: &str) -> (Option<PathBuf>, Option<PathBuf>) {
    let lower = name.to_ascii_lowercase();
    let active = directory
        .files
        .get(&lower)
        .map(|file| directory.path.join(file));
    let disabled = directory
        .files
        .get(&format!("{lower}{OFF}"))
        .map(|file| directory.path.join(file));
    (active, disabled)
}

pub fn overlay_status(game: &Game) -> Result<OverlayStatus> {
    let directories = scan(&game.install_dir)?;
    let mut active_files = 0;
    let mut disabled_files = 0;
    for directory in &directories {
        let names = DLLS
            .iter()
            .copied()
            .filter(|_| directory.switch_dlls)
            .chain(["steam_appid.txt"]);
        for name in names {
            let (active, disabled) = paths(directory, name);
            active_files += usize::from(active.is_some());
            disabled_files += usize::from(disabled.is_some());
        }
    }
    Ok(OverlayStatus {
        present: !directories.is_empty(),
        enabled: active_files > 0,
        mixed: active_files > 0 && disabled_files > 0,
        can_toggle: active_files + disabled_files > 0,
        active_files,
        disabled_files,
        directories: directories
            .into_iter()
            .map(|directory| directory.path)
            .collect(),
    })
}

fn file_name(path: &Path) -> Result<String> {
    Ok(path
        .file_name()
        .ok_or("invalid overlay filename")?
        .to_string_lossy()
        .into_owned())
}

/// Rename only known local fix DLLs and nearby steam_appid.txt files.
/// Every destination is checked before the first move; completed moves are rolled back on error.
pub fn set_overlay_enabled(game: &Game, enabled: bool) -> Result<usize> {
    let directories = scan(&game.install_dir)?;
    let mut moves = Vec::<(PathBuf, PathBuf)>::new();
    let mut found_switchable = false;
    for directory in &directories {
        let names = DLLS
            .iter()
            .copied()
            .filter(|_| directory.switch_dlls)
            .chain(["steam_appid.txt"]);
        for name in names {
            let (active, disabled) = paths(directory, name);
            found_switchable |= active.is_some() || disabled.is_some();
            let source = if enabled { disabled } else { active };
            if let Some(source) = source {
                let target = if enabled {
                    let name = file_name(&source)?;
                    directory.path.join(&name[..name.len() - OFF.len()])
                } else {
                    directory.path.join(format!("{}{OFF}", file_name(&source)?))
                };
                moves.push((source, target));
            }
        }
    }
    if !found_switchable {
        return Err("no switchable local fix files were found".into());
    }
    let moved_count = moves.len();
    for (source, target) in &moves {
        let metadata = fs::symlink_metadata(source)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(format!("fix file is not a regular file: {}", source.display()).into());
        }
        if fs::symlink_metadata(target).is_ok() {
            return Err(format!(
                "fix toggle destination already exists: {}",
                target.display()
            )
            .into());
        }
    }
    let mut completed = Vec::<(PathBuf, PathBuf)>::new();
    for (source, target) in moves {
        if let Err(error) = fs::rename(&source, &target) {
            let rollback = completed
                .iter()
                .rev()
                .try_for_each(|(source, target)| fs::rename(target, source));
            return match rollback {
                Ok(()) => Err(format!(
                    "could not toggle local fix: {error}; previous files restored"
                )
                .into()),
                Err(restore) => Err(format!(
                    "could not toggle local fix: {error}; rollback failed: {restore}"
                )
                .into()),
            };
        }
        completed.push((source, target));
    }
    Ok(moved_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Library, add_game};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn toggles_nested_fix_without_touching_unrelated_files_or_conflicts() {
        let root = std::env::temp_dir().join(format!(
            "azdl-overlay-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        let overlay = install.join("Binaries/Win64");
        fs::create_dir_all(&overlay).unwrap();
        fs::write(install.join("game.exe"), b"game").unwrap();
        fs::write(overlay.join("SteamFix.ini"), b"FakeAppId=480\n").unwrap();
        fs::write(overlay.join("winmm.dll"), b"proxy").unwrap();
        fs::write(overlay.join("SteamFix64.dll"), b"fix").unwrap();
        fs::write(overlay.join("steam_appid.txt"), b"480\n").unwrap();
        fs::write(install.join("winmm.dll"), b"unrelated").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "game",
            "Game",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = &library.games["game"];
        let state = overlay_status(game).unwrap();
        assert!(state.present && state.enabled && state.can_toggle);
        assert_eq!(set_overlay_enabled(game, false).unwrap(), 3);
        assert!(!overlay_status(game).unwrap().enabled);
        assert!(overlay.join("SteamFix64.dll.arachnel-off").is_file());
        assert!(overlay.join("steam_appid.txt.arachnel-off").is_file());
        assert_eq!(fs::read(install.join("winmm.dll")).unwrap(), b"unrelated");
        fs::write(overlay.join("winmm.dll"), b"conflict").unwrap();
        assert!(set_overlay_enabled(game, true).is_err());
        assert!(overlay.join("SteamFix64.dll.arachnel-off").is_file());
        fs::remove_file(overlay.join("winmm.dll")).unwrap();
        assert_eq!(set_overlay_enabled(game, true).unwrap(), 3);
        assert!(overlay_status(game).unwrap().enabled);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn toggles_fake_app_id_without_dll() {
        let root = std::env::temp_dir().join(format!(
            "azdl-appid-overlay-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("game.exe"), b"game").unwrap();
        fs::write(root.join("OnlineFix.ini"), b"FakeAppId=480\n").unwrap();
        fs::write(root.join("steam_appid.txt"), b"480\n").unwrap();
        let mut library = Library::default();
        add_game(&mut library, "game", "Game", &root, Path::new("game.exe")).unwrap();
        let game = &library.games["game"];
        assert!(overlay_status(game).unwrap().enabled);
        assert_eq!(set_overlay_enabled(game, false).unwrap(), 1);
        assert!(!overlay_status(game).unwrap().enabled);
        assert_eq!(set_overlay_enabled(game, true).unwrap(), 1);
        assert!(root.join("steam_appid.txt").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn many_files_in_one_directory_do_not_exhaust_directory_limit() {
        let root = std::env::temp_dir().join(format!(
            "azdl-many-files-overlay-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        for index in 0..4001 {
            fs::write(root.join(format!("asset-{index}")), b"").unwrap();
        }
        fs::write(root.join("game.exe"), b"game").unwrap();
        fs::write(root.join("SteamFix.ini"), b"FakeAppId=480\n").unwrap();
        fs::write(root.join("SteamFix64.dll"), b"fix").unwrap();
        let mut library = Library::default();
        add_game(&mut library, "game", "Game", &root, Path::new("game.exe")).unwrap();
        assert!(overlay_status(&library.games["game"]).unwrap().enabled);
        fs::remove_dir_all(root).unwrap();
    }
}
