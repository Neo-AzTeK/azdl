//! Goldberg Steam Emulator configuration and automatic DLC entitlement generator.
//!
//! Goldberg Steam Emulator reads configuration from the `steam_settings/` directory:
//! - `DLC.txt`: lists entitled DLCs in `<appid>=<title>` or `<appid>` format.
//! - `steam_appid.txt`: specifies the base game Steam App ID.
//! - `force_account_name.txt`: offline player account name.
//! - `force_language.txt`: offline UI language.
//!
//! This module provides automatic synchronization of DLC entitlements for games
//! using the Goldberg Steam Emulator, ensuring Unity, Unreal, and custom engine games
//! recognize downloaded DLCs without modifying engine binaries.

use crate::Game;
use std::fs;
use std::io::Result;
use std::path::{Path, PathBuf};

/// Marker file stored inside `steam_settings/` to track files managed by azdl.
pub const GOLDBERG_MANAGED_MARKER: &str = ".azdl-goldberg-managed";

/// Check if a directory is compatible with or contains Goldberg Steam Emulator.
pub fn is_goldberg_compatible(game_dir: &Path) -> bool {
    if !game_dir.is_dir() {
        return false;
    }

    if game_dir.join("steam_settings").is_dir() {
        return true;
    }

    // Check for steam_api64.dll or steam_api.dll next to main executables
    for exe in crate::steamless::collect_exe_candidates(game_dir) {
        if let Some(parent) = exe.parent()
            && (parent.join("steam_api64.dll").is_file()
                || parent.join("steam_api.dll").is_file()
                || parent.join("steam_settings").is_dir())
        {
            return true;
        }
    }

    // Root check
    game_dir.join("steam_api64.dll").is_file() || game_dir.join("steam_api.dll").is_file()
}

/// Check if Goldberg DLC configuration exists in the game directory.
pub fn has_goldberg_settings(game_dir: &Path) -> bool {
    let settings_dir = find_settings_directory(game_dir);
    settings_dir.join("DLC.txt").is_file() || settings_dir.join("steam_appid.txt").is_file()
}

/// Find the appropriate `steam_settings` directory for a game directory.
pub fn find_settings_directory(game_dir: &Path) -> PathBuf {
    // If a nested steam_settings directory already exists, prefer it
    for exe in crate::steamless::collect_exe_candidates(game_dir) {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("steam_settings");
            if candidate.is_dir() {
                return candidate;
            }
            if parent.join("steam_api64.dll").is_file() || parent.join("steam_api.dll").is_file() {
                return candidate;
            }
        }
    }

    game_dir.join("steam_settings")
}

/// Synchronizes Goldberg configuration files (`DLC.txt`, `steam_appid.txt`, etc.).
///
/// Returns `true` if any file was written or updated.
pub fn sync_goldberg_config(
    game_dir: &Path,
    steam_app_id: Option<u32>,
    dlc_entries: &[(u32, &str)],
    player_name: Option<&str>,
    language: Option<&str>,
) -> Result<bool> {
    if !game_dir.is_dir() {
        return Ok(false);
    }

    let settings_dir = find_settings_directory(game_dir);
    fs::create_dir_all(&settings_dir)?;

    let mut modified = false;

    // 1. Write DLC.txt
    if !dlc_entries.is_empty() {
        let dlc_path = settings_dir.join("DLC.txt");
        let mut lines = Vec::new();
        for &(id, name) in dlc_entries {
            if !name.trim().is_empty() {
                lines.push(format!("{id}={}", name.trim()));
            } else {
                lines.push(format!("{id}"));
            }
        }
        lines.sort();
        let content = lines.join("\n") + "\n";

        let current = fs::read_to_string(&dlc_path).unwrap_or_default();
        if current != content {
            // Backup existing non-managed DLC.txt if present
            if dlc_path.is_file() && !settings_dir.join(GOLDBERG_MANAGED_MARKER).is_file() {
                let backup = dlc_path.with_extension("txt.goldberg.bak");
                let _ = fs::copy(&dlc_path, backup);
            }
            fs::write(&dlc_path, content)?;
            modified = true;
        }
    }

    // 2. Write steam_appid.txt in steam_settings and game root/exe parent
    if let Some(app_id) = steam_app_id {
        let app_id_str = format!("{app_id}\n");

        let settings_app_id = settings_dir.join("steam_appid.txt");
        let current_settings = fs::read_to_string(&settings_app_id).unwrap_or_default();
        if current_settings != app_id_str {
            fs::write(&settings_app_id, &app_id_str)?;
            modified = true;
        }

        let root_app_id = game_dir.join("steam_appid.txt");
        if !root_app_id.exists() {
            fs::write(&root_app_id, &app_id_str)?;
            modified = true;
        }
    }

    // 3. Write force_account_name.txt if provided and not already present
    if let Some(name) = player_name
        && !name.trim().is_empty()
    {
        let name_path = settings_dir.join("force_account_name.txt");
        if !name_path.exists() {
            fs::write(&name_path, format!("{}\n", name.trim()))?;
            modified = true;
        }
    }

    // 4. Write force_language.txt if provided and not already present
    if let Some(lang) = language
        && !lang.trim().is_empty()
    {
        let lang_path = settings_dir.join("force_language.txt");
        if !lang_path.exists() {
            fs::write(&lang_path, format!("{}\n", lang.trim()))?;
            modified = true;
        }
    }

    // Mark as managed by azdl
    let marker_path = settings_dir.join(GOLDBERG_MANAGED_MARKER);
    if !marker_path.is_file() {
        fs::write(marker_path, b"managed by azdl\n")?;
    }

    Ok(modified)
}

/// Rolls back Goldberg configuration managed by azdl.
pub fn rollback_goldberg_config(game_dir: &Path) -> Result<bool> {
    if !game_dir.is_dir() {
        return Ok(false);
    }

    let settings_dir = find_settings_directory(game_dir);
    let marker_path = settings_dir.join(GOLDBERG_MANAGED_MARKER);
    if !marker_path.is_file() {
        return Ok(false);
    }

    let dlc_path = settings_dir.join("DLC.txt");
    let backup_dlc = dlc_path.with_extension("txt.goldberg.bak");
    if backup_dlc.is_file() {
        let _ = fs::copy(&backup_dlc, &dlc_path);
        let _ = fs::remove_file(backup_dlc);
    } else {
        let _ = fs::remove_file(&dlc_path);
    }

    let _ = fs::remove_file(settings_dir.join("steam_appid.txt"));
    let _ = fs::remove_file(settings_dir.join("force_account_name.txt"));
    let _ = fs::remove_file(settings_dir.join("force_language.txt"));
    let _ = fs::remove_file(marker_path);

    // Remove settings_dir if empty
    if let Ok(mut entries) = fs::read_dir(&settings_dir)
        && entries.next().is_none()
    {
        let _ = fs::remove_dir(&settings_dir);
    }

    Ok(true)
}

/// Synchronizes Goldberg settings for a game by resolving its installed DLCs and App ID.
pub fn prepare_goldberg_for_game(game: &Game, data_dir: &Path) -> Result<bool> {
    if !is_goldberg_compatible(&game.install_dir) {
        return Ok(false);
    }

    let installed = crate::dlc::list_installed(game, data_dir).unwrap_or_default();
    let mut dlc_entries = Vec::new();

    for id_str in &installed {
        if let Ok(id) = id_str.parse::<u32>() {
            dlc_entries.push((id, id_str.as_str()));
        }
    }

    let app_id = game.steam_app_id;

    sync_goldberg_config(
        &game.install_dir,
        app_id,
        &dlc_entries,
        Some("Player"),
        Some("english"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "azdl_goldberg_test_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn sync_and_rollback_goldberg_config() {
        let dir = test_dir("sync");
        fs::write(dir.join("steam_api64.dll"), b"fake dll").unwrap();
        assert!(is_goldberg_compatible(&dir));
        assert!(!has_goldberg_settings(&dir));

        let dlcs = [(3875100, "Extra Content"), (123456, "Soundtrack")];
        assert!(
            sync_goldberg_config(&dir, Some(480), &dlcs, Some("TestUser"), Some("english"))
                .unwrap()
        );
        assert!(has_goldberg_settings(&dir));

        let settings = dir.join("steam_settings");
        assert!(settings.is_dir());
        let dlc_content = fs::read_to_string(settings.join("DLC.txt")).unwrap();
        assert!(dlc_content.contains("3875100=Extra Content"));
        assert!(dlc_content.contains("123456=Soundtrack"));

        let appid_content = fs::read_to_string(settings.join("steam_appid.txt")).unwrap();
        assert_eq!(appid_content.trim(), "480");

        let account = fs::read_to_string(settings.join("force_account_name.txt")).unwrap();
        assert_eq!(account.trim(), "TestUser");

        let lang = fs::read_to_string(settings.join("force_language.txt")).unwrap();
        assert_eq!(lang.trim(), "english");

        // Rollback
        assert!(rollback_goldberg_config(&dir).unwrap());
        assert!(!has_goldberg_settings(&dir));
        assert!(!settings.join("DLC.txt").exists());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sync_preserves_existing_custom_dlc_backup() {
        let dir = test_dir("backup");
        let settings = dir.join("steam_settings");
        fs::create_dir_all(&settings).unwrap();
        fs::write(settings.join("DLC.txt"), "999=Original\n").unwrap();

        let dlcs = [(1000, "New DLC")];
        assert!(sync_goldberg_config(&dir, Some(500), &dlcs, None, None).unwrap());

        assert!(settings.join("DLC.txt.goldberg.bak").is_file());
        let dlc_content = fs::read_to_string(settings.join("DLC.txt")).unwrap();
        assert!(dlc_content.contains("1000=New DLC"));

        assert!(rollback_goldberg_config(&dir).unwrap());
        let restored = fs::read_to_string(settings.join("DLC.txt")).unwrap();
        assert_eq!(restored.trim(), "999=Original");

        let _ = fs::remove_dir_all(dir);
    }
}
