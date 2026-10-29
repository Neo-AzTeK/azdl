//! Goldberg Steam Emulator configuration and automatic DLC entitlement generator.
//!
//! Goldberg Steam Emulator reads configuration from the `steam_settings/` directory:
//! - `DLC.txt`: lists entitled DLCs in `<appid>=<title>` or `<appid>` format.
//! - `configs.app.ini`: app configuration with DLC unlock rules (`unlock_all=1`).
//! - `installed_app_ids.txt`: specifies base and DLC app IDs recognized as installed.
//! - `depots.txt`: specifies DLC depots recognized as mounted.
//! - `steam_appid.txt`: specifies the base game Steam App ID.
//! - `force_account_name.txt`: offline player account name.
//! - `force_language.txt`: offline UI language.
//!
//! This module provides automatic synchronization of DLC entitlements and emulator
//! binary provisioning for games using or compatible with the Goldberg Steam Emulator,
//! ensuring Godot, Unity, Unreal, and custom engine games recognize downloaded DLCs.

use crate::Game;
use crate::cmd::clean_command;
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

/// Marker file stored inside `steam_settings/` to track files managed by azdl.
pub const GOLDBERG_MANAGED_MARKER: &str = ".azdl-goldberg-managed";

const GOLDBERG_RELEASE_URL: &str = "https://github.com/Detanup01/gbe_fork/releases/download/release-2026_09_27/emu-win-release-vs22.7z";

/// Check if a directory has any `.goldberg.bak` backup files.
pub fn has_goldberg_backup(game_dir: &Path) -> bool {
    if game_dir.join("steam_api64.dll.goldberg.bak").is_file()
        || game_dir.join("steam_api.dll.goldberg.bak").is_file()
    {
        return true;
    }
    for exe in crate::steamless::collect_exe_candidates(game_dir) {
        if let Some(parent) = exe.parent()
            && (parent.join("steam_api64.dll.goldberg.bak").is_file()
                || parent.join("steam_api.dll.goldberg.bak").is_file())
        {
            return true;
        }
    }
    false
}

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
    settings_dir.join("DLC.txt").is_file()
        || settings_dir.join("steam_appid.txt").is_file()
        || settings_dir.join(GOLDBERG_MANAGED_MARKER).is_file()
        || has_goldberg_backup(game_dir)
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

/// Ensures modern Goldberg emulator binaries (x64 and x86) are cached in `<data_dir>/redist/goldberg`.
pub fn ensure_goldberg_binaries(data_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let base = data_dir.join("redist").join("goldberg");
    let x64 = base.join("x64").join("steam_api64.dll");
    let x86 = base.join("x86").join("steam_api.dll");

    if x64.is_file() && x86.is_file() {
        return Ok((x64, x86));
    }

    fs::create_dir_all(base.join("x64"))?;
    fs::create_dir_all(base.join("x86"))?;

    let archive_path = data_dir.join("redist").join("gbe_fork.7z");
    let status = clean_command("curl")
        .args(["-f", "-L", "--silent", "--show-error", "-o"])
        .arg(&archive_path)
        .arg(GOLDBERG_RELEASE_URL)
        .status()?;

    if status.success() && archive_path.is_file() {
        let extract_dir = data_dir.join("redist").join("gbe_tmp");
        let _ = fs::create_dir_all(&extract_dir);
        let _ = clean_command("bsdtar")
            .args(["-xf"])
            .arg(&archive_path)
            .args(["-C"])
            .arg(&extract_dir)
            .status();

        let extracted_x64 = extract_dir
            .join("release")
            .join("regular")
            .join("x64")
            .join("steam_api64.dll");
        let extracted_x86 = extract_dir
            .join("release")
            .join("regular")
            .join("x86")
            .join("steam_api.dll");

        if extracted_x64.is_file() {
            let _ = fs::copy(&extracted_x64, &x64);
        }
        if extracted_x86.is_file() {
            let _ = fs::copy(&extracted_x86, &x86);
        }

        let _ = fs::remove_dir_all(&extract_dir);
        let _ = fs::remove_file(&archive_path);
    }

    if x64.is_file() && x86.is_file() {
        Ok((x64, x86))
    } else {
        Err(Error::new(
            ErrorKind::NotFound,
            "Could not locate or download Goldberg emulator binaries",
        ))
    }
}

/// Provisions Goldberg emulator binaries for a game directory, backing up untouched original binaries.
/// Collect every `steam_api(64).dll` the game might load, anywhere under its
/// install directory. The flag is `true` for the 64-bit `steam_api64.dll`.
///
/// Engines rarely keep the Steamworks DLL at the install root: Unity loads it
/// from `<Game>_Data/Plugins/x86_64/`, and other engines nest it under `Binaries`
/// or `bin` folders that hold no executable. Replacing only the root and
/// executable-adjacent copies leaves the genuine Steamworks DLL in place, so the
/// game binds to a real Steam client at launch and crashes under an isolated
/// prefix. A bounded, symlink-rejecting walk covers every copy instead.
fn collect_steam_api_targets(game_dir: &Path) -> Vec<(PathBuf, bool)> {
    let mut targets = Vec::new();
    fn walk(current: &Path, depth: usize, out: &mut Vec<(PathBuf, bool)>) {
        if depth > 8 || out.len() >= 64 {
            return;
        }
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= 64 {
                return;
            }
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_file() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name == "steam_api64.dll" {
                    out.push((path, true));
                } else if name == "steam_api.dll" {
                    out.push((path, false));
                }
            } else if meta.is_dir() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                // Skip our own managed/runtime folders and VCS metadata; none hold
                // a Steamworks DLL the game loads.
                if matches!(
                    name.as_str(),
                    ".depotdownloader"
                        | "staging"
                        | "intermediate"
                        | "saved"
                        | ".git"
                        | "compatdata"
                        | "prefixes"
                        | "steam_settings"
                ) {
                    continue;
                }
                walk(&path, depth + 1, out);
            }
        }
    }
    walk(game_dir, 0, &mut targets);
    targets
}

pub fn provision_goldberg_binaries(game_dir: &Path, data_dir: &Path) -> Result<bool> {
    let Ok((emu_x64, emu_x86)) = ensure_goldberg_binaries(data_dir) else {
        return Ok(false);
    };

    let mut modified = false;

    // Helper to inspect whether a dll is already a Goldberg emulator
    let is_goldberg_dll = |path: &Path| -> bool {
        if let Ok(bytes) = fs::read(path) {
            bytes
                .windows(8)
                .any(|w| w == b"Goldberg" || w == b"ColdClie")
        } else {
            false
        }
    };

    let targets = collect_steam_api_targets(game_dir);

    // Capture the genuine Steamworks DLL before it is overwritten so Goldberg can
    // be told the exact interface versions the game negotiates.
    let mut original_dll: Option<Vec<u8>> = None;

    for (target_path, is_x64) in targets {
        let backup = target_path.with_extension("dll.goldberg.bak");
        // Source the genuine DLL for interface extraction from the live file on a
        // first provision, or from the backup when it was already replaced.
        if original_dll.is_none() {
            if !is_goldberg_dll(&target_path) {
                original_dll = fs::read(&target_path).ok();
            } else if backup.is_file() && !is_goldberg_dll(&backup) {
                original_dll = fs::read(&backup).ok();
            }
        }
        if !is_goldberg_dll(&target_path) {
            if !backup.is_file() {
                let _ = fs::copy(&target_path, &backup);
            }
            let source = if is_x64 { &emu_x64 } else { &emu_x86 };
            if fs::copy(source, &target_path).is_ok() {
                modified = true;
            }
        }
    }

    // Goldberg reads `steam_interfaces.txt` to answer `SteamInternal_CreateInterface`
    // with the versions the game asks for. Without it, interface lookups can fail and
    // the game crashes during Steamworks init. Generate it from the original DLL.
    if let Some(bytes) = original_dll {
        let interfaces = extract_steam_interfaces(&bytes);
        if !interfaces.is_empty() {
            let settings_dir = find_settings_directory(game_dir);
            if fs::create_dir_all(&settings_dir).is_ok() {
                let path = settings_dir.join("steam_interfaces.txt");
                if !path.is_file() {
                    let content = interfaces.join("\n") + "\n";
                    if fs::write(&path, content).is_ok() {
                        modified = true;
                    }
                }
            }
        }
    }

    Ok(modified)
}

/// Scan a genuine `steam_api(64).dll` for the Steamworks interface version
/// identifiers it exports (`SteamClient020`, `SteamUser023`, the
/// `STEAM*_INTERFACE_VERSION###` aliases, …) for Goldberg's `steam_interfaces.txt`.
///
/// Interface tokens are ASCII runs that begin with `Steam`/`STEAM` and end in
/// three digits not preceded by another digit. Results are de-duplicated and
/// sorted for a stable file.
fn extract_steam_interfaces(bytes: &[u8]) -> Vec<String> {
    let mut found: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut token = Vec::new();
    let is_token_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for &b in bytes.iter().chain(std::iter::once(&0u8)) {
        if is_token_byte(b) {
            token.push(b);
            continue;
        }
        if token.len() >= 8 && token.len() <= 64 {
            let text = String::from_utf8_lossy(&token);
            let upper = text.to_ascii_uppercase();
            let ends_in_three_digits = text.len() >= 4
                && text.as_bytes()[text.len() - 3..]
                    .iter()
                    .all(|c| c.is_ascii_digit())
                && !text.as_bytes()[text.len() - 4].is_ascii_digit();
            if (upper.starts_with("STEAM")) && ends_in_three_digits {
                found.insert(text.into_owned());
            }
        }
        token.clear();
    }
    found.into_iter().collect()
}

/// Synchronizes Goldberg configuration files (`DLC.txt`, `configs.app.ini`, `steam_appid.txt`, etc.).
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

    // 2. Write configs.app.ini for modern Goldberg emulator forks
    let ini_path = settings_dir.join("configs.app.ini");
    let mut ini_lines = vec![
        "[app::general]".to_string(),
        "is_beta_branch=0".to_string(),
        "branch_name=public".to_string(),
        "".to_string(),
        "[app::dlcs]".to_string(),
        "unlock_all=1".to_string(),
    ];
    for &(id, name) in dlc_entries {
        if !name.trim().is_empty() {
            ini_lines.push(format!("{id}={}", name.trim()));
        } else {
            ini_lines.push(format!("{id}={id}"));
        }
    }
    ini_lines.push("".to_string());
    let ini_content = ini_lines.join("\n");
    if fs::read_to_string(&ini_path).unwrap_or_default() != ini_content {
        fs::write(&ini_path, ini_content)?;
        modified = true;
    }

    // 3. Write installed_app_ids.txt
    let installed_apps_path = settings_dir.join("installed_app_ids.txt");
    let mut app_lines = Vec::new();
    if let Some(app_id) = steam_app_id {
        app_lines.push(app_id.to_string());
    }
    for &(id, _) in dlc_entries {
        if Some(id) != steam_app_id {
            app_lines.push(id.to_string());
        }
    }
    if !app_lines.is_empty() {
        let app_content = app_lines.join("\n") + "\n";
        if fs::read_to_string(&installed_apps_path).unwrap_or_default() != app_content {
            fs::write(&installed_apps_path, app_content)?;
            modified = true;
        }
    }

    // 4. Write depots.txt
    let depots_path = settings_dir.join("depots.txt");
    let mut depot_lines = Vec::new();
    for &(id, _) in dlc_entries {
        depot_lines.push(id.to_string());
    }
    if !depot_lines.is_empty() {
        let depot_content = depot_lines.join("\n") + "\n";
        if fs::read_to_string(&depots_path).unwrap_or_default() != depot_content {
            fs::write(&depots_path, depot_content)?;
            modified = true;
        }
    }

    // 5. Write steam_appid.txt in steam_settings and game root/exe parent
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

    // 6. Write force_account_name.txt if provided and not already present
    if let Some(name) = player_name
        && !name.trim().is_empty()
    {
        let name_path = settings_dir.join("force_account_name.txt");
        if !name_path.exists() {
            fs::write(&name_path, format!("{}\n", name.trim()))?;
            modified = true;
        }
    }

    // 7. Write force_language.txt if provided and not already present
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

/// Rolls back Goldberg configuration and emulator binaries managed by azdl.
pub fn rollback_goldberg_config(game_dir: &Path) -> Result<bool> {
    if !game_dir.is_dir() {
        return Ok(false);
    }

    let mut restored = false;

    // Restore binary backups
    let mut backups = Vec::new();
    let r64 = game_dir.join("steam_api64.dll.goldberg.bak");
    if r64.is_file() {
        backups.push(r64);
    }
    let r32 = game_dir.join("steam_api.dll.goldberg.bak");
    if r32.is_file() {
        backups.push(r32);
    }
    for exe in crate::steamless::collect_exe_candidates(game_dir) {
        if let Some(parent) = exe.parent() {
            let p64 = parent.join("steam_api64.dll.goldberg.bak");
            if p64.is_file() && !backups.contains(&p64) {
                backups.push(p64);
            }
            let p32 = parent.join("steam_api.dll.goldberg.bak");
            if p32.is_file() && !backups.contains(&p32) {
                backups.push(p32);
            }
        }
    }

    for backup in backups {
        let original = if backup
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .contains("steam_api64.dll")
        {
            backup.with_file_name("steam_api64.dll")
        } else {
            backup.with_file_name("steam_api.dll")
        };
        if fs::copy(&backup, &original).is_ok() {
            let _ = fs::remove_file(&backup);
            restored = true;
        }
    }

    let settings_dir = find_settings_directory(game_dir);
    let marker_path = settings_dir.join(GOLDBERG_MANAGED_MARKER);
    if marker_path.is_file() {
        let dlc_path = settings_dir.join("DLC.txt");
        let backup_dlc = dlc_path.with_extension("txt.goldberg.bak");
        if backup_dlc.is_file() {
            let _ = fs::copy(&backup_dlc, &dlc_path);
            let _ = fs::remove_file(backup_dlc);
        } else {
            let _ = fs::remove_file(&dlc_path);
        }

        let _ = fs::remove_file(settings_dir.join("configs.app.ini"));
        let _ = fs::remove_file(settings_dir.join("installed_app_ids.txt"));
        let _ = fs::remove_file(settings_dir.join("depots.txt"));
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
        restored = true;
    }

    Ok(restored)
}

/// Synchronizes Goldberg settings and emulator binaries for a game by resolving its installed DLCs and App ID.
pub fn prepare_goldberg_for_game(game: &Game, data_dir: &Path) -> Result<bool> {
    if !is_goldberg_compatible(&game.install_dir) {
        return Ok(false);
    }

    let bin_modified = provision_goldberg_binaries(&game.install_dir, data_dir).unwrap_or(false);

    let installed = crate::dlc::list_installed(game, data_dir).unwrap_or_default();
    let mut dlc_entries = Vec::new();

    for id_str in &installed {
        if let Ok(id) = id_str.parse::<u32>() {
            dlc_entries.push((id, id_str.as_str()));
        }
    }

    let app_id = game.steam_app_id;

    let cfg_modified = sync_goldberg_config(
        &game.install_dir,
        app_id,
        &dlc_entries,
        Some("Player"),
        Some("english"),
    )?;

    Ok(bin_modified || cfg_modified)
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

        let ini_content = fs::read_to_string(settings.join("configs.app.ini")).unwrap();
        assert!(ini_content.contains("unlock_all=1"));
        assert!(ini_content.contains("3875100=Extra Content"));

        let apps_content = fs::read_to_string(settings.join("installed_app_ids.txt")).unwrap();
        assert!(apps_content.contains("480"));
        assert!(apps_content.contains("3875100"));

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
        assert!(!settings.join("configs.app.ini").exists());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn extracts_steam_interface_versions_from_dll_bytes() {
        // Interface identifiers are NUL-delimited inside the real DLL.
        let blob = b"junk\0SteamClient020\0SteamUser023\0\
            STEAMAPPS_INTERFACE_VERSION008\0noise123more\0Steam12\0lowercaseclient017\0";
        let found = extract_steam_interfaces(blob);
        assert!(found.contains(&"SteamClient020".to_string()));
        assert!(found.contains(&"SteamUser023".to_string()));
        assert!(found.contains(&"STEAMAPPS_INTERFACE_VERSION008".to_string()));
        // Not an interface: digits in the middle, too short, or not Steam-prefixed.
        assert!(!found.iter().any(|s| s == "noise123more"));
        assert!(!found.iter().any(|s| s == "Steam12"));
        assert!(!found.iter().any(|s| s == "lowercaseclient017"));
    }

    #[test]
    fn collects_steam_api_dlls_in_nested_engine_folders() {
        let dir = test_dir("nested-api");
        // A Unity layout keeps the real Steamworks DLL under <Game>_Data/Plugins,
        // away from any executable; excluded runtime folders must be skipped.
        let plugins = dir.join("MoonSweepers_Data").join("Plugins").join("x86_64");
        fs::create_dir_all(&plugins).unwrap();
        fs::create_dir_all(dir.join("compatdata").join("pfx")).unwrap();
        fs::write(dir.join("steam_api64.dll"), b"root64").unwrap();
        fs::write(dir.join("steam_api.dll"), b"root32").unwrap();
        fs::write(plugins.join("steam_api64.dll"), b"nested64").unwrap();
        fs::write(
            dir.join("compatdata").join("pfx").join("steam_api64.dll"),
            b"prefix-copy",
        )
        .unwrap();

        let targets = collect_steam_api_targets(&dir);
        assert!(targets.contains(&(dir.join("steam_api64.dll"), true)));
        assert!(targets.contains(&(dir.join("steam_api.dll"), false)));
        assert!(targets.contains(&(plugins.join("steam_api64.dll"), true)));
        // The prefix copy lives under an excluded folder and must not be touched.
        assert!(
            !targets
                .iter()
                .any(|(path, _)| path.starts_with(dir.join("compatdata")))
        );

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

    #[test]
    fn rollback_restores_original_steam_api_dll_backup() {
        let dir = test_dir("restore_dll");
        let dll = dir.join("steam_api64.dll");
        let bak = dir.join("steam_api64.dll.goldberg.bak");
        fs::write(&dll, b"modified emulator bytes").unwrap();
        fs::write(&bak, b"original valve bytes").unwrap();

        assert!(has_goldberg_backup(&dir));
        assert!(has_goldberg_settings(&dir));

        assert!(rollback_goldberg_config(&dir).unwrap());
        assert!(!bak.exists());
        assert_eq!(fs::read(&dll).unwrap(), b"original valve bytes");

        let _ = fs::remove_dir_all(dir);
    }
}
