//! Runtime dependency detection, downloading, and automated prefix provisioning.
//!
//! Windows games running under Wine/Proton frequently require common Windows runtime
//! redistributables:
//! - Visual C++ 2015-2022 Redistributable (x64 and x86)
//! - DirectX End-User Runtimes (D3DX9, D3DX10, D3DX11)
//!
//! This module provides:
//! - Non-destructive inspection of Wine prefix registry (`system.reg`/`user.reg`) and CRT DLLs.
//! - Detection of dummy stubs vs. real CRT libraries (requiring real binary sizes >= 40KB).
//! - Automatic download of official Microsoft VC++ redistributables (`https://aka.ms/vs/17/release/vc_redist.x64.exe`).
//! - Discovery of bundled `DXSETUP.exe` in game trees and silent execution (`/silent`).
//! - Silent execution in isolated Wine/Proton prefixes with clean error handling.

use crate::cmd::clean_command;
use crate::{Game, Runner};
use serde::{Deserialize, Serialize};
#[cfg(not(windows))]
use std::env;
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

const VC_REDIST_X64_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";
const VC_REDIST_X86_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x86.exe";

/// Minimum size of a real MSVC CRT DLL (Wine stubs are usually ~1KB; real ones are >40KB).
const MIN_REAL_CRT_BYTES: u64 = 40 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyStatus {
    pub vcredist_x64: bool,
    pub vcredist_x86: bool,
    pub directx: bool,
    pub missing: Vec<String>,
}

/// Inspects whether a file exists in the prefix and has a genuine binary size.
fn prefix_has_real_dll(prefix_dir: &Path, relative_subpath: &str) -> bool {
    let drive_c = prefix_dir.join("drive_c");
    let target = drive_c.join(relative_subpath);

    if let Ok(meta) = fs::metadata(&target)
        && meta.is_file()
        && meta.len() >= MIN_REAL_CRT_BYTES
    {
        return true;
    }

    // Check lowercase variant
    let lower_target = drive_c.join(relative_subpath.to_ascii_lowercase());
    if let Ok(meta) = fs::metadata(&lower_target)
        && meta.is_file()
        && meta.len() >= MIN_REAL_CRT_BYTES
    {
        return true;
    }

    false
}

/// Checks if VC++ 2015-2022 Redistributable is installed in the given Wine prefix.
pub fn check_vcredist_installed(prefix_dir: &Path, want_x64: bool) -> bool {
    if !prefix_dir.is_dir() {
        return false;
    }

    let needles = if want_x64 {
        [
            "Microsoft Visual C++ 2015-2022 Redistributable (x64)",
            "Visual C++ 2022 X64 Minimum Runtime",
            "Visual C++ 2019 X64 Minimum Runtime",
            "Visual C++ 2015 X64 Minimum Runtime",
        ]
    } else {
        [
            "Microsoft Visual C++ 2015-2022 Redistributable (x86)",
            "Visual C++ 2022 X86 Minimum Runtime",
            "Visual C++ 2019 X86 Minimum Runtime",
            "Visual C++ 2015 X86 Minimum Runtime",
        ]
    };

    let mut reg_hit = false;
    for reg_file in ["user.reg", "system.reg"] {
        let path = prefix_dir.join(reg_file);
        if let Ok(text) = fs::read_to_string(&path) {
            for needle in &needles {
                if text.contains(needle) {
                    reg_hit = true;
                    break;
                }
            }
        }
        if reg_hit {
            break;
        }
    }

    let sys_folder = if want_x64 {
        "windows/system32"
    } else {
        "windows/syswow64"
    };

    let vcruntime = format!("{sys_folder}/vcruntime140.dll");
    let msvcp = format!("{sys_folder}/msvcp140.dll");

    let has_vcruntime = prefix_has_real_dll(prefix_dir, &vcruntime);
    let has_msvcp = prefix_has_real_dll(prefix_dir, &msvcp);

    // If both real CRT DLLs are present and registry hit exists, or extended CRT is present
    let has_extended = prefix_has_real_dll(prefix_dir, &format!("{sys_folder}/vcruntime140_1.dll"));

    has_vcruntime && has_msvcp && (reg_hit || has_extended)
}

/// Checks if DirectX 9/10/11 runtime DLLs are present in the given Wine prefix.
pub fn check_directx_installed(prefix_dir: &Path) -> bool {
    if !prefix_dir.is_dir() {
        return false;
    }

    prefix_has_real_dll(prefix_dir, "windows/system32/d3dx9_43.dll")
        || prefix_has_real_dll(prefix_dir, "windows/syswow64/d3dx9_43.dll")
}

/// Diagnoses the dependency status of a Wine prefix.
pub fn diagnose_prefix_dependencies(prefix_dir: &Path) -> DependencyStatus {
    let vc_x64 = check_vcredist_installed(prefix_dir, true);
    let vc_x86 = check_vcredist_installed(prefix_dir, false);
    let dx = check_directx_installed(prefix_dir);

    let mut missing = Vec::new();
    if !vc_x64 {
        missing.push("Visual C++ 2015-2022 (x64)".into());
    }
    if !vc_x86 {
        missing.push("Visual C++ 2015-2022 (x86)".into());
    }
    if !dx {
        missing.push("DirectX (d3dx9_43.dll)".into());
    }

    DependencyStatus {
        vcredist_x64: vc_x64,
        vcredist_x86: vc_x86,
        directx: dx,
        missing,
    }
}

/// Searches a game directory for a bundled `DXSETUP.exe`.
pub fn find_dxsetup_in_game(game_dir: &Path) -> Option<PathBuf> {
    if !game_dir.is_dir() {
        return None;
    }

    // Common search locations
    let direct_candidates = [
        game_dir.join("DXSETUP.exe"),
        game_dir
            .join("_CommonRedist")
            .join("DirectX")
            .join("Jun2010")
            .join("DXSETUP.exe"),
        game_dir.join("DirectX").join("DXSETUP.exe"),
        game_dir.join("redist").join("DirectX").join("DXSETUP.exe"),
        game_dir.join("Support").join("DirectX").join("DXSETUP.exe"),
    ];

    for candidate in &direct_candidates {
        if candidate.is_file() {
            return Some(candidate.clone());
        }
    }

    // Bounded recursive walk (up to 3 levels)
    find_file_recursive(game_dir, "dxsetup.exe", 0, 3)
}

fn find_file_recursive(
    dir: &Path,
    target_name: &str,
    depth: usize,
    max_depth: usize,
) -> Option<PathBuf> {
    if depth > max_depth {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && name.eq_ignore_ascii_case(target_name)
            {
                return Some(path);
            }
        } else if path.is_dir()
            && let Some(found) = find_file_recursive(&path, target_name, depth + 1, max_depth)
        {
            return Some(found);
        }
    }
    None
}

/// Ensures the official Microsoft VC++ Redistributable installer is cached locally.
pub fn ensure_vcredist_installer(data_dir: &Path, want_x64: bool) -> Result<PathBuf> {
    let redist_dir = data_dir.join("redist");
    fs::create_dir_all(&redist_dir)?;

    let (url, filename) = if want_x64 {
        (VC_REDIST_X64_URL, "vc_redist.x64.exe")
    } else {
        (VC_REDIST_X86_URL, "vc_redist.x86.exe")
    };

    let target_path = redist_dir.join(filename);
    if target_path.is_file() && fs::metadata(&target_path)?.len() > 1024 * 1024 {
        return Ok(target_path);
    }

    let status = clean_command("curl")
        .args(["-f", "-L", "--silent", "--show-error", "-o"])
        .arg(&target_path)
        .arg(url)
        .status()?;

    if !status.success() || !target_path.is_file() {
        let _ = fs::remove_file(&target_path);
        return Err(Error::other(format!(
            "Failed to download VC++ redistributable from {url}"
        )));
    }

    Ok(target_path)
}

/// Executes an installer binary in the prefix using Wine or Proton.
pub fn run_installer_in_prefix(
    prefix_dir: &Path,
    runner: &Runner,
    data_dir: &Path,
    installer: &Path,
    args: &[&str],
    allowed_exit_codes: &[i32],
) -> Result<bool> {
    if !installer.is_file() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("Installer not found at {}", installer.display()),
        ));
    }

    fs::create_dir_all(prefix_dir)?;

    #[cfg(windows)]
    {
        let _ = (prefix_dir, runner, data_dir);
        let mut cmd = clean_command(installer);
        cmd.args(args);
        let status = cmd.status()?;
        let code = status.code().unwrap_or(-1);
        if status.success() || allowed_exit_codes.contains(&code) {
            return Ok(true);
        }
        Err(Error::other(format!(
            "Installer exited with status {status}"
        )))
    }

    #[cfg(not(windows))]
    {
        let mut cmd = match runner {
            Runner::Proton(proton) => {
                let steam = env::var_os("STEAM_COMPAT_CLIENT_INSTALL_PATH")
                    .map(PathBuf::from)
                    .filter(|path| path.is_dir())
                    .or_else(|| crate::steam::steam_root().ok().filter(|path| path.is_dir()))
                    .unwrap_or_else(|| data_dir.to_path_buf());
                let mut c = clean_command(proton);
                c.env_remove("LD_LIBRARY_PATH").env_remove("LD_PRELOAD");
                c.env("STEAM_COMPAT_CLIENT_INSTALL_PATH", steam);
                c.env("STEAM_COMPAT_DATA_PATH", prefix_dir);
                c.env("WINEDEBUG", "-all");
                c.args([
                    "run",
                    installer.to_str().ok_or_else(|| {
                        Error::new(ErrorKind::InvalidInput, "non-UTF-8 installer path")
                    })?,
                ]);
                c
            }
            Runner::Wine | Runner::Auto | Runner::ProtonAuto => {
                let mut c = clean_command("wine");
                c.env("WINEPREFIX", prefix_dir);
                c.env("WINEDEBUG", "-all");
                c.arg(installer);
                c
            }
            Runner::Native | Runner::Steam(_) => {
                return Ok(false);
            }
        };

        cmd.args(args);
        if let Some(parent) = installer.parent() {
            cmd.current_dir(parent);
        }

        let status = cmd.status()?;
        let code = status.code().unwrap_or(-1);

        if status.success() || allowed_exit_codes.contains(&code) {
            Ok(true)
        } else {
            Err(Error::other(format!("Installer failed with code {code}")))
        }
    }
}

/// Installs VC++ Redistributable into a game's isolated Wine prefix.
pub fn install_vcredist(
    prefix_dir: &Path,
    runner: &Runner,
    data_dir: &Path,
    want_x64: bool,
) -> Result<bool> {
    let installer = ensure_vcredist_installer(data_dir, want_x64)?;
    // Exit codes: 0 = success, 1638 = already installed / newer version present, 3010 = reboot needed
    run_installer_in_prefix(
        prefix_dir,
        runner,
        data_dir,
        &installer,
        &["/install", "/quiet", "/norestart"],
        &[0, 1638, 3010],
    )
}

/// Installs DirectX End-User Runtimes from a bundled `DXSETUP.exe`.
pub fn install_directx(
    prefix_dir: &Path,
    runner: &Runner,
    data_dir: &Path,
    game_dir: &Path,
) -> Result<bool> {
    let Some(dxsetup) = find_dxsetup_in_game(game_dir) else {
        return Ok(false);
    };

    // Exit codes: 0 = success, 1638/3010 = reboot/newer, 247 = Wine specific DirectX success code
    run_installer_in_prefix(
        prefix_dir,
        runner,
        data_dir,
        &dxsetup,
        &["/silent"],
        &[0, 1638, 3010, 247],
    )
}

/// Ensures required runtime dependencies are installed in the game prefix before launch.
pub fn ensure_game_dependencies(
    game: &Game,
    runner: &Runner,
    data_dir: &Path,
) -> Result<Vec<String>> {
    if matches!(runner, Runner::Native | Runner::Steam(_)) {
        return Ok(Vec::new());
    }

    let prefix_dir = data_dir.join("prefixes").join(&game.id);
    let mut installed_actions = Vec::new();

    // Check VC++ x64
    if !check_vcredist_installed(&prefix_dir, true)
        && let Ok(true) = install_vcredist(&prefix_dir, runner, data_dir, true)
    {
        installed_actions.push("Installed Visual C++ 2015-2022 (x64)".into());
    }

    // Check DirectX
    if !check_directx_installed(&prefix_dir)
        && let Ok(true) = install_directx(&prefix_dir, runner, data_dir, &game.install_dir)
    {
        installed_actions.push("Installed DirectX runtimes via DXSETUP".into());
    }

    Ok(installed_actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("azdl_dep_test_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn detects_missing_dependencies_in_empty_prefix() {
        let dir = test_dir("empty");
        let status = diagnose_prefix_dependencies(&dir);
        assert!(!status.vcredist_x64);
        assert!(!status.vcredist_x86);
        assert!(!status.directx);
        assert_eq!(status.missing.len(), 3);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detects_installed_vcredist_and_directx() {
        let dir = test_dir("installed");
        let sys32 = dir.join("drive_c/windows/system32");
        fs::create_dir_all(&sys32).unwrap();

        // Create fake CRT files with real sizes >= 40KB
        let real_bytes = vec![0x90; 50 * 1024];
        fs::write(sys32.join("vcruntime140.dll"), &real_bytes).unwrap();
        fs::write(sys32.join("msvcp140.dll"), &real_bytes).unwrap();
        fs::write(sys32.join("d3dx9_43.dll"), &real_bytes).unwrap();

        // Write registry entry in system.reg
        fs::write(
            dir.join("system.reg"),
            "\"DisplayName\"=\"Microsoft Visual C++ 2015-2022 Redistributable (x64) - 14.38\"\n",
        )
        .unwrap();

        let status = diagnose_prefix_dependencies(&dir);
        assert!(status.vcredist_x64);
        assert!(status.directx);
        assert!(!status.vcredist_x86); // x86 still missing

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn finds_bundled_dxsetup_in_game_tree() {
        let dir = test_dir("dxsetup");
        let redist = dir.join("_CommonRedist/DirectX/Jun2010");
        fs::create_dir_all(&redist).unwrap();
        let dx_file = redist.join("DXSETUP.exe");
        fs::write(&dx_file, b"installer").unwrap();

        let found = find_dxsetup_in_game(&dir);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), dx_file);

        let _ = fs::remove_dir_all(dir);
    }
}
