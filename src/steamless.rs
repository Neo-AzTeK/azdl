//! SteamStub DRM detection and Steamless unpacking/rollback orchestrator.
use crate::{Game, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamStubStatus {
    pub relevant: bool,
    pub stub_present: bool,
    pub applied: bool,
    pub label: String,
}

/// Check if a Windows Portable Executable contains the SteamStub `.bind` section.
pub fn has_steamstub(path: &Path) -> bool {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut dos = [0u8; 0x40];
    if file.read_exact(&mut dos).is_err() {
        return false;
    }
    if dos[0] != b'M' || dos[1] != b'Z' {
        return false;
    }
    let pe_offset = u32::from_le_bytes([dos[0x3C], dos[0x3D], dos[0x3E], dos[0x3F]]) as u64;
    if pe_offset == 0 || pe_offset > 0x0400_0000 {
        return false;
    }
    if file.seek(SeekFrom::Start(pe_offset)).is_err() {
        return false;
    }
    let mut sig = [0u8; 4];
    if file.read_exact(&mut sig).is_err() || &sig != b"PE\0\0" {
        return false;
    }
    let mut coff = [0u8; 20];
    if file.read_exact(&mut coff).is_err() {
        return false;
    }
    let section_count = u16::from_le_bytes([coff[2], coff[3]]) as usize;
    let optional_size = u16::from_le_bytes([coff[16], coff[17]]) as usize;
    if section_count == 0 || section_count > 128 || optional_size > 65536 {
        return false;
    }
    if file.seek(SeekFrom::Current(optional_size as i64)).is_err() {
        return false;
    }
    for _ in 0..section_count {
        let mut section = [0u8; 40];
        if file.read_exact(&mut section).is_err() {
            break;
        }
        let name_bytes = &section[0..8];
        let name_len = name_bytes.iter().position(|&b| b == 0).unwrap_or(8);
        if &name_bytes[..name_len] == b".bind" {
            return true;
        }
    }
    false
}

/// Locate Steamless.CLI.exe in env, data_dir, or system PATH.
pub fn find_steamless_cli(data_dir: &Path) -> Option<PathBuf> {
    if let Some(val) = std::env::var_os("AZDL_STEAMLESS_PATH")
        .or_else(|| std::env::var_os("ARACHNEL_STEAMLESS_PATH"))
    {
        let path = PathBuf::from(val);
        if path.is_file() {
            return Some(path);
        }
    }
    let in_data = data_dir.join("steamless").join("Steamless.CLI.exe");
    if in_data.is_file() {
        return Some(in_data);
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("Steamless.CLI.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
            let candidate2 = dir.join("steamless");
            if candidate2.is_file() {
                return Some(candidate2);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn to_wine_path(path: &Path) -> Result<String> {
    let canonical = fs::canonicalize(path)?;
    let s = canonical.to_str().ok_or("path is not valid UTF-8")?;
    let win = s.replace('/', "\\");
    if !win.starts_with("Z:\\") {
        Ok(format!("Z:{win}"))
    } else {
        Ok(win)
    }
}

#[cfg(not(windows))]
fn which_wine() -> PathBuf {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("wine");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("wine")
}

/// Strip SteamStub DRM wrapper from an executable, creating a `.steamstub.bak` backup.
pub fn strip_executable(exe_path: &Path, cli_path: &Path) -> Result<()> {
    if !exe_path.is_file() {
        return Err("executable not found".into());
    }
    let working_dir = cli_path.parent().ok_or("CLI path has no parent")?;
    let unpacked = exe_path.with_file_name(format!(
        "{}.unpacked.exe",
        exe_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("game")
    ));
    let unpacked_alt = exe_path.with_extension("unpacked.exe");
    let unpacked_full = exe_path.with_file_name(format!(
        "{}.unpacked.exe",
        exe_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("game.exe")
    ));
    let _ = fs::remove_file(&unpacked);
    let _ = fs::remove_file(&unpacked_alt);
    let _ = fs::remove_file(&unpacked_full);

    #[cfg(windows)]
    {
        let status = Command::new(cli_path)
            .arg("--quiet")
            .arg(exe_path)
            .current_dir(working_dir)
            .status()?;
        if !status.success() {
            return Err(format!("Steamless exited with status {status}").into());
        }
    }

    #[cfg(not(windows))]
    {
        let wine = which_wine();
        let wine_target = to_wine_path(exe_path)?;
        let status = Command::new(wine)
            .arg(cli_path)
            .arg("--quiet")
            .arg(&wine_target)
            .env("WINEDEBUG", "-all")
            .current_dir(working_dir)
            .status()?;
        if !status.success() {
            return Err(format!("Steamless via Wine exited with status {status}").into());
        }
    }

    let found_unpacked = if unpacked_full.is_file() {
        unpacked_full
    } else if unpacked_alt.is_file() {
        unpacked_alt
    } else if unpacked.is_file() {
        unpacked
    } else {
        return Err("no unpacked output produced by Steamless".into());
    };

    let backup = exe_path.with_file_name(format!(
        "{}.steamstub.bak",
        exe_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("game.exe")
    ));
    let _ = fs::remove_file(&backup);
    fs::rename(exe_path, &backup)?;
    if let Err(e) = fs::rename(&found_unpacked, exe_path) {
        let _ = fs::rename(&backup, exe_path);
        return Err(format!("failed to replace executable: {e}").into());
    }
    Ok(())
}

/// Rollback a stripped executable from its `.steamstub.bak` file.
pub fn rollback_executable(exe_path: &Path) -> Result<bool> {
    let backup = exe_path.with_file_name(format!(
        "{}.steamstub.bak",
        exe_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("game.exe")
    ));
    if backup.is_file() {
        let _ = fs::remove_file(exe_path);
        fs::rename(&backup, exe_path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Scan candidate executables in an install directory.
pub fn collect_exe_candidates(dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if !dir.is_dir() {
        return candidates;
    }
    fn walk(current: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 3 {
            return;
        }
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(meta) = fs::symlink_metadata(&path) {
                if meta.is_file() && !meta.file_type().is_symlink() {
                    if path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                    {
                        out.push(path);
                    }
                } else if meta.is_dir() && !meta.file_type().is_symlink() {
                    walk(&path, depth + 1, out);
                }
            }
        }
    }
    walk(dir, 0, &mut candidates);
    candidates
}

/// Check SteamStub status across a game's executables.
pub fn scan_game_steamstub(game: &Game) -> SteamStubStatus {
    let candidates = collect_exe_candidates(&game.install_dir);
    if candidates.is_empty() {
        return SteamStubStatus {
            relevant: false,
            stub_present: false,
            applied: false,
            label: "Not needed".to_string(),
        };
    }
    let mut stub_present = false;
    let mut applied = false;
    for exe in &candidates {
        if has_steamstub(exe) {
            stub_present = true;
        }
        let backup = exe.with_file_name(format!(
            "{}.steamstub.bak",
            exe.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("game.exe")
        ));
        if backup.is_file() {
            applied = true;
        }
    }
    let label = if stub_present {
        "Needed".to_string()
    } else if applied {
        "Applied".to_string()
    } else {
        "Not needed".to_string()
    };
    SteamStubStatus {
        relevant: true,
        stub_present,
        applied: applied && !stub_present,
        label,
    }
}

/// Unpack all candidate executables that contain SteamStub DRM.
pub fn unpack_game(game: &Game, data_dir: &Path) -> Result<usize> {
    let cli = find_steamless_cli(data_dir).ok_or("Steamless.CLI.exe not found")?;
    let candidates = collect_exe_candidates(&game.install_dir);
    let mut stripped = 0;
    for exe in &candidates {
        if has_steamstub(exe) {
            strip_executable(exe, &cli)?;
            stripped += 1;
        }
    }
    Ok(stripped)
}

/// Restore all executables from `.steamstub.bak` backups.
pub fn rollback_game(game: &Game) -> Result<usize> {
    let candidates = collect_exe_candidates(&game.install_dir);
    let mut rolled_back = 0;
    for exe in &candidates {
        if rollback_executable(exe)? {
            rolled_back += 1;
        }
    }
    Ok(rolled_back)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Library, add_game};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn create_mock_pe(path: &Path, section_name: &[u8]) {
        let mut data = vec![0u8; 512];
        data[0] = b'M';
        data[1] = b'Z';
        data[0x3C] = 0x80;
        data[128] = b'P';
        data[129] = b'E';
        data[130] = 0;
        data[131] = 0;
        data[134] = 1; // 1 section
        data[148] = 0; // 0 optional header
        for (i, b) in section_name.iter().take(8).enumerate() {
            data[152 + i] = *b;
        }
        fs::write(path, data).unwrap();
    }

    #[test]
    fn test_steamstub_detection_and_scan() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("azdl-steamstub-{nonce}"));
        fs::create_dir_all(&root).unwrap();

        let stub_exe = root.join("game.exe");
        create_mock_pe(&stub_exe, b".bind\0\0\0");
        assert!(has_steamstub(&stub_exe));

        let clean_exe = root.join("clean.exe");
        create_mock_pe(&clean_exe, b".text\0\0\0");
        assert!(!has_steamstub(&clean_exe));

        let mut library = Library::default();
        add_game(
            &mut library,
            "stub-game",
            "Stub Game",
            &root,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = library.games.get("stub-game").unwrap();

        let status = scan_game_steamstub(game);
        assert!(status.relevant);
        assert!(status.stub_present);
        assert_eq!(status.label, "Needed");

        // Simulate applied unpack
        let backup = root.join("game.exe.steamstub.bak");
        fs::copy(&stub_exe, &backup).unwrap();
        create_mock_pe(&stub_exe, b".text\0\0\0");

        let status_applied = scan_game_steamstub(game);
        assert!(status_applied.applied);
        assert!(!status_applied.stub_present);
        assert_eq!(status_applied.label, "Applied");

        // Rollback
        assert!(rollback_executable(&stub_exe).unwrap());
        assert!(!backup.exists());
        assert!(has_steamstub(&stub_exe));

        fs::remove_dir_all(root).unwrap();
    }
}
