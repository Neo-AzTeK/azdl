#[cfg(target_os = "linux")]
use crate::validate_id;
use crate::{Game, Result};
#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::fs::{self, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;
#[cfg(target_os = "linux")]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
fn desktop_directory(override_dir: Option<&Path>, create: bool) -> Result<PathBuf> {
    let directory = if let Some(directory) = override_dir {
        directory.to_path_buf()
    } else {
        let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
        let configured = Command::new("xdg-user-dir")
            .arg("DESKTOP")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|output| PathBuf::from(output.trim()));
        configured
            .filter(|path| path.is_absolute() && path != &home)
            .unwrap_or_else(|| home.join("Desktop"))
    };
    if create {
        fs::create_dir_all(&directory)?;
    }
    if directory.is_dir() {
        Ok(fs::canonicalize(directory)?)
    } else if directory.is_absolute() {
        Ok(directory)
    } else {
        Ok(env::current_dir()?.join(directory))
    }
}

#[cfg(target_os = "linux")]
fn desktop_value(value: &str) -> String {
    let mut result = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            _ => result.push(ch),
        }
    }
    result
}

#[cfg(target_os = "linux")]
fn exec_argument(value: &str) -> Result<String> {
    if value.chars().any(char::is_control) {
        return Err("shortcut path contains a control character".into());
    }
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\\\\\"),
            '"' => escaped.push_str("\\\\\""),
            '$' => escaped.push_str("\\\\$"),
            '`' => escaped.push_str("\\\\`"),
            '%' => escaped.push_str("%%"),
            _ => escaped.push(ch),
        }
    }
    Ok(format!("\"{escaped}\""))
}

#[cfg(target_os = "linux")]
fn shortcut_path(game: &Game, directory: Option<&Path>, create: bool) -> Result<PathBuf> {
    validate_id(&game.id)?;
    Ok(desktop_directory(directory, create)?.join(format!("azdl-{}.desktop", game.id)))
}

#[cfg(target_os = "linux")]
fn owned_shortcut(path: &Path, id: &str, data_dir: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Err("shortcut path exists but is not a regular file".into());
    }
    let content = fs::read_to_string(path)?;
    Ok(content
        .lines()
        .any(|line| line == format!("X-azdl-GameId={id}"))
        && content.lines().any(|line| {
            line == format!(
                "X-azdl-DataDir={}",
                desktop_value(&data_dir.to_string_lossy())
            )
        }))
}

/// Create or refresh a launcher shortcut for a game in the selected azdl library.
#[cfg(target_os = "linux")]
pub fn create_shortcut(
    game: &Game,
    data_dir: &Path,
    launcher: &Path,
    directory: Option<&Path>,
) -> Result<PathBuf> {
    let path = shortcut_path(game, directory, true)?;
    let data_dir = fs::canonicalize(data_dir)?;
    let launcher = fs::canonicalize(launcher)?;
    if !launcher.is_file() {
        return Err("azdl launcher is not a file".into());
    }
    if fs::symlink_metadata(&path).is_ok() && !owned_shortcut(&path, &game.id, &data_dir)? {
        return Err("shortcut path belongs to another application or library".into());
    }
    let exec = [
        exec_argument("/usr/bin/env")?,
        exec_argument(&format!("AZDL_DATA_DIR={}", data_dir.display()))?,
        exec_argument(&launcher.to_string_lossy())?,
        exec_argument("launch")?,
        exec_argument(&game.id)?,
    ]
    .join(" ");
    let content = format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName={}\nExec={exec}\nIcon=applications-games\nTerminal=false\nCategories=Game;\nStartupNotify=true\nX-azdl-GameId={}\nX-azdl-DataDir={}\n",
        desktop_value(&game.title),
        game.id,
        desktop_value(&data_dir.to_string_lossy())
    );
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = path.with_extension(format!("desktop.{}.{nonce}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    drop(file);
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o755))?;
    fs::rename(&temp, &path)?;
    Ok(path)
}

#[cfg(target_os = "linux")]
pub fn shortcut_status(game: &Game, data_dir: &Path, directory: Option<&Path>) -> Result<bool> {
    let path = shortcut_path(game, directory, false)?;
    let data_dir = fs::canonicalize(data_dir)?;
    owned_shortcut(&path, &game.id, &data_dir)
}

#[cfg(target_os = "linux")]
pub fn remove_shortcut(game: &Game, data_dir: &Path, directory: Option<&Path>) -> Result<bool> {
    let path = shortcut_path(game, directory, false)?;
    let data_dir = fs::canonicalize(data_dir)?;
    if !owned_shortcut(&path, &game.id, &data_dir)? {
        return Ok(false);
    }
    fs::remove_file(path)?;
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
pub fn create_shortcut(
    _game: &Game,
    _data_dir: &Path,
    _launcher: &Path,
    _directory: Option<&Path>,
) -> Result<PathBuf> {
    Err("desktop shortcuts are currently supported on Linux only".into())
}

#[cfg(not(target_os = "linux"))]
pub fn shortcut_status(_game: &Game, _data_dir: &Path, _directory: Option<&Path>) -> Result<bool> {
    Err("desktop shortcuts are currently supported on Linux only".into())
}

#[cfg(not(target_os = "linux"))]
pub fn remove_shortcut(_game: &Game, _data_dir: &Path, _directory: Option<&Path>) -> Result<bool> {
    Err("desktop shortcuts are currently supported on Linux only".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::{Library, add_game};

    #[test]
    fn shortcut_uses_azdl_launch_and_can_be_removed_safely() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!("azdl-shortcut-{}-{nonce}", std::process::id()));
        let data_dir = root.join("Library $ % Data");
        let desktop = root.join("Desktop");
        let install = root.join("game");
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("game.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
        let launcher = root.join("azdl launcher");
        fs::write(&launcher, b"launcher").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "test",
            "Test % Game",
            &install,
            Path::new("game.sh"),
        )
        .unwrap();
        let game = &library.games["test"];
        let missing = root.join("missing-desktop");
        assert!(!shortcut_status(game, &data_dir, Some(&missing)).unwrap());
        assert!(!missing.exists());
        let path = create_shortcut(game, &data_dir, &launcher, Some(&desktop)).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("Name=Test % Game"));
        assert!(content.contains("\"launch\" \"test\""));
        assert!(content.contains("AZDL_DATA_DIR="));
        assert!(shortcut_status(game, &data_dir, Some(&desktop)).unwrap());
        if Command::new("desktop-file-validate")
            .arg("--version")
            .output()
            .is_ok()
        {
            assert!(
                Command::new("desktop-file-validate")
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        assert!(remove_shortcut(game, &data_dir, Some(&desktop)).unwrap());
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
