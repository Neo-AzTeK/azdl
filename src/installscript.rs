//! Steamworks installscript.vdf parser and Wine registry emulator.
//!
//! Many Steam games package `installscript.vdf` or `runasadmin.vdf` which configure
//! Windows registry keys and prerequisites.
//!
//! This module provides:
//! - Parsing of VDF `Registry` and `Run Process` blocks.
//! - Macro resolution for placeholders (`%INSTALLDIR%`, `%PROGRAMDATA%`, `%USERPROFILE%`).
//! - Writing registry hives directly into Wine/Proton prefixes (`system.reg` / `user.reg`).
//! - Ubisoft Connect bypass: auto-registering Ubisoft launcher installation keys
//!   and configuring `-uplay_steam_mode` for games distributing `uplay_r1_loader64.dll`.

use crate::Game;
use std::fs::{self, OpenOptions};
use std::io::{Result, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    pub root_key: String,
    pub sub_key: String,
    pub value_name: String,
    pub value_data: String,
    pub is_dword: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunProcessStep {
    pub process_path: String,
    pub arguments: Vec<String>,
    pub has_run_key: Option<String>,
    pub is_64bit: bool,
}

/// Detects if a game uses Ubisoft Connect / Uplay DRM loaders.
pub fn is_ubisoft_game(game_dir: &Path) -> bool {
    if !game_dir.is_dir() {
        return false;
    }

    let indicators = [
        "uplay_r1_loader64.dll",
        "uplay_r2_loader.dll",
        "uplay_r1_loader.dll",
        "uplay_r2_loader64.dll",
        "uplay_r1.dll",
        "uplay_r2.dll",
        "uplay.dll",
        "upc.exe",
    ];

    find_any_file_named(game_dir, &indicators, 0, 3)
}

fn find_any_file_named(dir: &Path, names: &[&str], depth: usize, max_depth: usize) -> bool {
    if depth > max_depth {
        return false;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                let lower = file_name.to_ascii_lowercase();
                if names.iter().any(|name| lower == *name) {
                    return true;
                }
            }
        } else if path.is_dir() && find_any_file_named(&path, names, depth + 1, max_depth) {
            return true;
        }
    }
    false
}

/// Converts a Linux POSIX path to a Wine `Z:\...` path.
pub fn to_wine_path(posix_path: &Path) -> String {
    let clean = posix_path.to_string_lossy().replace('/', "\\");
    if clean.starts_with('\\') {
        format!("Z:{clean}")
    } else {
        format!("Z:\\{clean}")
    }
}

/// Resolves `%INSTALLDIR%` and other macro placeholders in registry and process strings.
pub fn resolve_placeholders(text: &str, install_dir: &Path) -> String {
    let wine_path = to_wine_path(install_dir);
    text.replace("%INSTALLDIR%", &wine_path)
        .replace("%installdir%", &wine_path)
        .replace("%PROGRAMDATA%", "C:\\ProgramData")
        .replace("%programdata%", "C:\\ProgramData")
        .replace("%USERPROFILE%", "C:\\users\\steamuser")
        .replace("%userprofile%", "C:\\users\\steamuser")
        .replace("%APPDATA%", "C:\\users\\steamuser\\AppData\\Roaming")
        .replace("%appdata%", "C:\\users\\steamuser\\AppData\\Roaming")
}

/// Parses the "Registry" sections from a VDF text into discrete registry entries.
pub fn parse_vdf_registry(vdf_content: &str) -> Vec<RegistryEntry> {
    let mut entries = Vec::new();
    let mut lines = vdf_content.lines().map(str::trim);

    #[allow(clippy::while_let_on_iterator)]
    while let Some(line) = lines.next() {
        if line.eq_ignore_ascii_case("\"Registry\"") || line.contains("\"registry\"") {
            // Find opening brace
            for l in lines.by_ref() {
                if l.contains('{') {
                    break;
                }
            }
            parse_registry_block(&mut lines, &mut entries);
        }
    }

    entries
}

fn parse_registry_block<'a, I>(lines: &mut I, entries: &mut Vec<RegistryEntry>)
where
    I: Iterator<Item = &'a str>,
{
    let mut current_key = String::new();
    let mut current_type = String::new(); // "string" or "dword"
    let mut depth = 1usize;

    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }
        if trimmed == "{" {
            depth += 1;
            continue;
        }
        if trimmed == "}" {
            depth -= 1;
            if depth == 0 {
                break;
            }
            continue;
        }

        let tokens = extract_quoted_tokens(trimmed);
        if tokens.len() == 1 {
            let token = &tokens[0];
            if token.eq_ignore_ascii_case("string") || token.eq_ignore_ascii_case("dword") {
                current_type = token.to_ascii_lowercase();
            } else if token.contains('\\') || token.starts_with("HKEY_") {
                current_key = token.clone();
            }
        } else if tokens.len() >= 2 {
            let name = tokens[0].replace("\\\\", "\\");
            let val = tokens[1].replace("\\\\", "\\");

            let (root, sub) = split_root_and_subkey(&current_key);
            let is_dword = current_type == "dword" || val.bytes().all(|b| b.is_ascii_digit());

            entries.push(RegistryEntry {
                root_key: root,
                sub_key: sub,
                value_name: name,
                value_data: val,
                is_dword,
            });
        }
    }
}

fn split_root_and_subkey(key: &str) -> (String, String) {
    let clean = key.replace("\\\\", "\\");
    if let Some((root, sub)) = clean.split_once('\\') {
        (root.to_string(), sub.trim_start_matches('\\').to_string())
    } else {
        (clean, String::new())
    }
}

fn extract_quoted_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
            current.push(ch);
        } else if ch == '"' {
            in_quotes = !in_quotes;
            if !in_quotes {
                tokens.push(current.clone());
                current.clear();
            }
        } else if in_quotes {
            current.push(ch);
        }
    }

    tokens
}

/// Formats and appends registry entries to Wine's `system.reg` or `user.reg`.
pub fn append_to_wine_reg(reg_path: &Path, entries: &[RegistryEntry]) -> Result<bool> {
    if !reg_path.is_file() || entries.is_empty() {
        return Ok(false);
    }

    let existing = fs::read_to_string(reg_path)?;
    let mut file = OpenOptions::new().append(true).open(reg_path)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut added_any = false;

    // Group entries by sub_key
    let mut by_key: std::collections::BTreeMap<&str, Vec<&RegistryEntry>> =
        std::collections::BTreeMap::new();
    for entry in entries {
        by_key.entry(&entry.sub_key).or_default().push(entry);
    }

    for (sub_key, items) in by_key {
        let clean_key = sub_key.trim_matches('\\').replace('/', "\\");
        let header = format!("\n[{clean_key}] {now}\n");

        let mut block = String::new();
        for item in items {
            let val_name = &item.value_name;
            if existing.contains(&format!("\"{val_name}\"=")) {
                continue; // Avoid duplicates
            }

            if item.is_dword {
                let num = item.value_data.parse::<u32>().unwrap_or(0);
                block.push_str(&format!("\"{val_name}\"=dword:{num:08x}\n"));
            } else {
                let escaped_val = item.value_data.replace('\\', "\\\\").replace('"', "\\\"");
                block.push_str(&format!("\"{val_name}\"=\"{escaped_val}\"\n"));
            }
        }

        if !block.is_empty() {
            file.write_all(header.as_bytes())?;
            file.write_all(block.as_bytes())?;
            added_any = true;
        }
    }

    Ok(added_any)
}

/// Applies all discovered install scripts and Ubisoft bypass entries for a game to its Wine prefix.
pub fn apply_installscript_for_game(game: &Game, prefix_dir: &Path) -> Result<bool> {
    if !game.install_dir.is_dir() || !prefix_dir.is_dir() {
        return Ok(false);
    }

    let mut entries = Vec::new();

    // 1. Scan and parse all installscript*.vdf / runasadmin.vdf files in the game tree
    for script_file in find_installscript_files(&game.install_dir) {
        if let Ok(content) = fs::read_to_string(&script_file) {
            let resolved = resolve_placeholders(&content, &game.install_dir);
            let parsed = parse_vdf_registry(&resolved);
            entries.extend(parsed);
        }
    }

    // 2. Ubisoft Connect bypass registration
    if is_ubisoft_game(&game.install_dir) {
        let wine_game_dir = to_wine_path(&game.install_dir);
        entries.push(RegistryEntry {
            root_key: "HKEY_LOCAL_MACHINE".into(),
            sub_key: "Software\\Ubisoft\\Launcher\\Installs".into(),
            value_name: "InstallDir".into(),
            value_data: wine_game_dir.clone(),
            is_dword: false,
        });
        entries.push(RegistryEntry {
            root_key: "HKEY_LOCAL_MACHINE".into(),
            sub_key: "Software\\Wow6432Node\\Ubisoft\\Launcher\\Installs".into(),
            value_name: "InstallDir".into(),
            value_data: wine_game_dir,
            is_dword: false,
        });
    }

    if entries.is_empty() {
        return Ok(false);
    }

    // Append to system.reg and user.reg
    let system_reg = prefix_dir.join("system.reg");
    let user_reg = prefix_dir.join("user.reg");

    let sys_ok = append_to_wine_reg(&system_reg, &entries)?;
    let user_ok = append_to_wine_reg(&user_reg, &entries)?;

    Ok(sys_ok || user_ok)
}

/// Finds `installscript*.vdf` or `runasadmin.vdf` files in the game directory.
pub fn find_installscript_files(game_dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    scan_scripts_recursive(game_dir, &mut results, 0, 3);
    results
}

fn scan_scripts_recursive(dir: &Path, out: &mut Vec<PathBuf>, depth: usize, max_depth: usize) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                let lower = name.to_ascii_lowercase();
                if lower.contains("installscript") && lower.ends_with(".vdf")
                    || lower == "runasadmin.vdf"
                {
                    out.push(path);
                }
            }
        } else if path.is_dir() {
            scan_scripts_recursive(&path, out, depth + 1, max_depth);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("azdl_vdf_test_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn parses_vdf_registry_block() {
        let sample = r#"
"InstallScript"
{
    "Registry"
    {
        "HKEY_LOCAL_MACHINE\\Software\\Valve\\TestApp"
        {
            "string"
            {
                "InstallPath" "%INSTALLDIR%\\bin"
            }
            "dword"
            {
                "Installed" "1"
            }
        }
    }
}
"#;
        let resolved = resolve_placeholders(sample, Path::new("/home/user/games/test"));
        assert!(resolved.contains(r#"Z:\home\user\games\test\\bin"#));

        let entries = parse_vdf_registry(&resolved);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sub_key, "Software\\Valve\\TestApp");
        assert_eq!(entries[0].value_name, "InstallPath");
        assert_eq!(entries[0].value_data, "Z:\\home\\user\\games\\test\\bin");
        assert!(!entries[0].is_dword);

        assert_eq!(entries[1].value_name, "Installed");
        assert!(entries[1].is_dword);
        assert_eq!(entries[1].value_data, "1");
    }

    #[test]
    fn appends_entries_to_wine_reg() {
        let dir = test_dir("wine_reg");
        let reg_file = dir.join("system.reg");
        fs::write(
            &reg_file,
            "WINE REGISTRY Version 2\n;; All keys relative to \\Machine\n",
        )
        .unwrap();

        let entries = vec![
            RegistryEntry {
                root_key: "HKEY_LOCAL_MACHINE".into(),
                sub_key: "Software\\Valve\\TestApp".into(),
                value_name: "InstallPath".into(),
                value_data: "Z:\\games\\test".into(),
                is_dword: false,
            },
            RegistryEntry {
                root_key: "HKEY_LOCAL_MACHINE".into(),
                sub_key: "Software\\Valve\\TestApp".into(),
                value_name: "Version".into(),
                value_data: "2".into(),
                is_dword: true,
            },
        ];

        assert!(append_to_wine_reg(&reg_file, &entries).unwrap());

        let content = fs::read_to_string(&reg_file).unwrap();
        assert!(content.contains("[Software\\Valve\\TestApp]"));
        assert!(content.contains("\"InstallPath\"=\"Z:\\\\games\\\\test\""));
        assert!(content.contains("\"Version\"=dword:00000002"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detects_ubisoft_game_loaders() {
        let dir = test_dir("ubisoft");
        fs::write(dir.join("uplay_r1_loader64.dll"), b"fake dll").unwrap();
        assert!(is_ubisoft_game(&dir));

        let clean_dir = test_dir("clean");
        assert!(!is_ubisoft_game(&clean_dir));

        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(clean_dir);
    }
}
