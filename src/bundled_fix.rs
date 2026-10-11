//! Reversible installation of the fix kit bundled with steamidra.

use crate::archive::extract_checked;
use crate::multiplayer::{FixKind, detect_fix, pe_image_bits};
use crate::patch::{active_patch, apply_patch};
use crate::{Game, Result, Runner};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MARKER: &str = ".azdl-bundled-fix";

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(from)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("bundled fix file is not a regular file: {}", from.display()).into());
    }
    fs::copy(from, to)?;
    Ok(())
}

fn configured(plugin: &Path) -> Result<bool> {
    let settings = plugin.join("settings.json");
    let bytes = match fs::read(settings) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    let value: Value = serde_json::from_slice(&bytes)?;
    Ok(value
        .get("autoEmbedOnlineFix")
        .and_then(Value::as_bool)
        .unwrap_or(true))
}

fn stage_linux(kit: &Path, staging: &Path, payload: &Path, bits: u8) -> Result<()> {
    let unpack = staging.join("linux");
    fs::create_dir(&unpack)?;
    extract_checked(&kit.join("linux.zip"), &unpack)?;
    copy_file(
        &unpack.join("OnlineFix.ini"),
        &payload.join("OnlineFix.ini"),
    )?;
    if bits == 64 {
        for name in [
            "OnlineFix64.dll",
            "SteamOverlay64.dll",
            "StubDRM64.dll",
            "dlllist.txt",
        ] {
            copy_file(&unpack.join(name), &payload.join(name))?;
        }
    } else {
        let optional = staging.join("x86");
        fs::create_dir(&optional)?;
        extract_checked(&unpack.join("x86_optional.zip"), &optional)?;
        for name in ["OnlineFix.dll", "SteamOverlay32.dll"] {
            copy_file(&optional.join(name), &payload.join(name))?;
        }
        copy_file(&optional.join("winmm32.dll"), &payload.join("winmm.dll"))?;
        fs::write(
            payload.join("dlllist.txt"),
            b"OnlineFix.dll\r\nSteamOverlay32.dll\r\n",
        )?;
    }
    Ok(())
}

fn stage_windows(kit: &Path, staging: &Path, payload: &Path, bits: u8) -> Result<()> {
    let windows = kit.join("windows");
    if bits == 64 {
        for name in ["SteamFix64.dll", "winmm.dll", "winmm.txt"] {
            copy_file(&windows.join(name), &payload.join(name))?;
        }
    } else {
        let optional = staging.join("x86");
        fs::create_dir(&optional)?;
        extract_checked(&windows.join("x86_optional.zip"), &optional)?;
        copy_file(
            &optional.join("SteamFix32.dll"),
            &payload.join("SteamFix32.dll"),
        )?;
        copy_file(&optional.join("winmm32.dll"), &payload.join("winmm.dll"))?;
        fs::write(payload.join("winmm.txt"), b"SteamFix32.dll")?;
    }
    Ok(())
}

fn target_directories(game: &Game, bits: u8) -> Result<BTreeSet<PathBuf>> {
    let root = fs::canonicalize(&game.install_dir)?;
    let executable = fs::canonicalize(&game.executable)?;
    if !executable.starts_with(&root) {
        return Err("game executable is outside its install directory".into());
    }
    let mut targets = BTreeSet::new();
    targets.insert(
        executable
            .parent()
            .ok_or("game executable has no parent")?
            .strip_prefix(&root)?
            .to_path_buf(),
    );
    for candidate in crate::steamless::collect_exe_candidates(&root) {
        if let Some(parent) = candidate.parent()
            && let Ok(rel) = parent.strip_prefix(&root)
        {
            targets.insert(rel.to_path_buf());
        }
    }
    let steam_api = if bits == 64 {
        "steam_api64.dll"
    } else {
        "steam_api.dll"
    };
    let mut stack = vec![(root.clone(), 0usize)];
    let mut entries = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            entries += 1;
            if entries > 50_000 {
                return Err("game has too many files to locate Steam API DLLs".into());
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() && depth < 10 {
                stack.push((path, depth + 1));
            } else if metadata.is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(steam_api)
            {
                targets.insert(directory.strip_prefix(&root)?.to_path_buf());
            }
        }
    }
    Ok(targets)
}

/// Apply steamidra's bundled fix once, retaining original files in azdl's patch backup.
/// Existing disabled bundled fixes stay disabled until the user enables the patch.
pub fn ensure_steamidra_fix(game: &Game, data_dir: &Path) -> Result<bool> {
    if matches!(game.runner, Runner::Steam(_))
        || !game
            .origin
            .as_ref()
            .is_some_and(|origin| origin.qualified_id.starts_with("steamidra:steam-"))
    {
        return Ok(false);
    }
    let managed_root = data_dir.join("games");
    let managed_install = fs::canonicalize(managed_root.join(&game.id));
    let game_install = fs::canonicalize(&game.install_dir);
    if !matches!((managed_install, game_install), (Ok(managed), Ok(actual)) if managed == actual) {
        return Ok(false);
    }
    let plugin = data_dir.join("plugins").join("steamidra");
    if !plugin.is_dir() {
        return Ok(false);
    }
    if !configured(&plugin)? || detect_fix(game)? != FixKind::None {
        return Ok(false);
    }
    if let Some(patch) = active_patch(data_dir, &game.id)? {
        if !patch
            .entries
            .iter()
            .any(|entry| entry.path == Path::new(MARKER))
        {
            eprintln!(
                "Bundled fix was not applied to {} because another patch is active",
                game.title
            );
        }
        return Ok(false);
    }
    let bits = pe_image_bits(&game.executable)
        .ok_or("bundled fix needs a 32-bit or 64-bit Windows executable")?;
    let targets = target_directories(game, bits)?;
    let kit = plugin.join("online_fix_kit");
    let staging_root = data_dir.join("fix-staging");
    fs::create_dir_all(&staging_root)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staging = staging_root.join(format!("{}-{}-{nonce}", game.id, std::process::id()));
    fs::create_dir(&staging)?;
    let result = (|| -> Result<()> {
        let template = staging.join("template");
        fs::create_dir(&template)?;
        if cfg!(windows) {
            stage_windows(&kit, &staging, &template, bits)?;
        } else {
            stage_linux(&kit, &staging, &template, bits)?;
        }
        let payload = staging.join("payload");
        fs::create_dir(&payload)?;
        for directory in &targets {
            let destination = payload.join(directory);
            fs::create_dir_all(&destination)?;
            for entry in fs::read_dir(&template)? {
                let entry = entry?;
                copy_file(&entry.path(), &destination.join(entry.file_name()))?;
            }
        }
        fs::write(payload.join(MARKER), b"steamidra bundled fix\n")?;
        apply_patch(game, data_dir, &payload)?;
        Ok(())
    })();
    if let Err(error) = fs::remove_dir_all(&staging) {
        eprintln!("Could not remove bundled fix staging directory: {error}");
    }
    result?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::rollback_patch;
    use crate::{CatalogOrigin, Library, add_game};
    use std::process::Command;

    #[test]
    fn installs_bundled_fix_with_rollback_and_respects_setting() {
        let root = std::env::temp_dir().join(format!(
            "azdl-bundled-fix-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("games/steam-42");
        let kit = root.join("plugins/steamidra/online_fix_kit");
        let source = root.join("kit-source");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&kit).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut exe = vec![0u8; 0x100];
        exe[0] = b'M';
        exe[1] = b'Z';
        exe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        exe[0x80..0x84].copy_from_slice(b"PE\0\0");
        exe[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        fs::write(install.join("game.exe"), exe).unwrap();
        let plugin_dir = install.join("Game_Data/Plugins/x86_64");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("steam_api64.dll"), b"original steam api").unwrap();
        for name in [
            "OnlineFix.ini",
            "OnlineFix64.dll",
            "SteamOverlay64.dll",
            "StubDRM64.dll",
            "dlllist.txt",
        ] {
            fs::write(source.join(name), name.as_bytes()).unwrap();
        }
        let status = Command::new("zip")
            .current_dir(&source)
            .arg("-q")
            .arg(kit.join("linux.zip"))
            .args([
                "OnlineFix.ini",
                "OnlineFix64.dll",
                "SteamOverlay64.dll",
                "StubDRM64.dll",
                "dlllist.txt",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let mut library = Library::default();
        add_game(
            &mut library,
            "steam-42",
            "Test game",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = library.games.get_mut("steam-42").unwrap();
        game.origin = Some(CatalogOrigin {
            qualified_id: "steamidra:steam-42".into(),
            upload_date: None,
            version: None,
        });
        let settings = root.join("plugins/steamidra/settings.json");
        fs::write(&settings, br#"{"autoEmbedOnlineFix":false}"#).unwrap();
        assert!(!ensure_steamidra_fix(game, &root).unwrap());
        fs::write(&settings, br#"{"autoEmbedOnlineFix":true}"#).unwrap();
        assert!(ensure_steamidra_fix(game, &root).unwrap());
        assert!(install.join("OnlineFix64.dll").is_file());
        assert!(plugin_dir.join("OnlineFix64.dll").is_file());
        assert_eq!(
            fs::read(install.join("dlllist.txt")).unwrap(),
            b"dlllist.txt"
        );
        assert!(!ensure_steamidra_fix(game, &root).unwrap());
        rollback_patch(game, &root).unwrap();
        assert!(!install.join("OnlineFix64.dll").exists());
        assert!(!plugin_dir.join("OnlineFix64.dll").exists());
        assert_eq!(
            fs::read(plugin_dir.join("steam_api64.dll")).unwrap(),
            b"original steam api"
        );
        assert!(!install.join("dlllist.txt").exists());
        assert!(install.join("game.exe").is_file());
        fs::remove_dir_all(root).unwrap();
    }
}
