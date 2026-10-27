//! Wine / Proton runtime error probing, crash handler detection, and process management.
//!
//! When Windows games run via Wine or Proton, crashes often do not cleanly terminate
//! the root process. Instead, Wine may spawn `winedbg` or Windows Error Reporting
//! (`werfault.exe`), or engines spawn `UnityCrashHandler64.exe` or `CrashReportClient.exe`.
//! In these states, the Wine prefix and GUI stay alive indefinitely in a hung error state,
//! leaving the launcher reporting the game as still "Running".
//!
//! This module provides:
//! - Inspection of running processes and command lines (`/proc` on Linux).
//! - Detection of crash handlers and unhandled exception dialogs.
//! - Verification of whether the actual game executable is still alive.
//! - Clean termination of hanging prefix processes.

#[cfg(target_os = "linux")]
use std::fs;
use std::path::PathBuf;

/// Hints used to identify processes belonging to a specific game launch.
#[derive(Debug, Clone, Default)]
pub struct WineProbeHints {
    pub install_dir: Option<PathBuf>,
    pub executable_name: Option<String>,
    pub prefix_dir: Option<PathBuf>,
    pub fake_app_id: Option<String>,
}

/// Diagnosed runtime status of a launched game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameRunStatus {
    /// The game executable is actively running.
    Running,
    /// The game encountered a fatal error or crash handler is active.
    Crashed {
        reason: String,
        culprit_pid: Option<u32>,
    },
    /// All related game processes have exited.
    Exited,
}

/// Checks whether a window title or error dialog text represents a fatal crash.
pub fn title_looks_like_error(title: &str) -> bool {
    let t = title.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    // Non-fatal splashes or SteamFix injectors
    if lower.contains("injector") || lower.contains("splash") {
        return false;
    }

    lower == "error"
        || lower.contains("fatal error")
        || lower.contains("error code")
        || lower.contains("unhandled exception")
        || lower.contains("access violation")
        || lower.contains("protection failed")
        || lower.contains("self-protection")
        || lower.contains("failed to initialize")
        || lower.contains("d3d11-compatible gpu is required")
        || lower.contains("dx11 feature level")
        || lower.contains("d3d12-compatible")
        || lower.contains("ошибка")
}

/// Identifies whether a process cmdline represents a known crash reporter or debugger.
pub fn detect_crash_reporter(cmdline: &str) -> Option<&'static str> {
    let lower = cmdline.to_ascii_lowercase();
    if lower.contains("winedbg") {
        Some("Wine Crash Debugger (winedbg)")
    } else if lower.contains("werfault.exe") || lower.contains("werfault") {
        Some("Windows Error Reporting (WerFault.exe)")
    } else if lower.contains("unitycrashhandler") {
        Some("Unity Crash Handler")
    } else if lower.contains("crashreportclient") {
        Some("Unreal Engine Crash Reporter")
    } else if lower.contains("epicgameslaunchercrashhandler") {
        Some("Epic Games Crash Reporter")
    } else if lower.contains("errorreport.exe") {
        Some("Generic Error Reporter")
    } else {
        None
    }
}

/// Tests whether a process cmdline matches the game hints.
pub fn cmdline_matches_hints(cmdline: &str, hints: &WineProbeHints) -> bool {
    if cmdline.is_empty() {
        return false;
    }
    let lower = cmdline.to_ascii_lowercase();

    if let Some(ref install) = hints.install_dir {
        let install_str = install.to_string_lossy().to_ascii_lowercase();
        if !install_str.is_empty() && lower.contains(&install_str) {
            return true;
        }
        let wine_style = install_str.replace('/', "\\");
        if !wine_style.is_empty() && lower.contains(&wine_style) {
            return true;
        }
    }

    if let Some(ref exe) = hints.executable_name {
        let exe_lower = exe.to_ascii_lowercase();
        if !exe_lower.is_empty() && lower.contains(&exe_lower) {
            return true;
        }
    }

    if let Some(ref fake_id) = hints.fake_app_id
        && !fake_id.is_empty()
        && lower.contains(&format!("steam_app_{fake_id}"))
    {
        return true;
    }

    false
}

#[cfg(target_os = "linux")]
fn read_proc_cmdline(pid: u32) -> Option<String> {
    let path = format!("/proc/{pid}/cmdline");
    let bytes = fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    // cmdline arguments in /proc are separated by null bytes
    let clean = bytes
        .split(|&b| b == 0)
        .map(|s| String::from_utf8_lossy(s))
        .collect::<Vec<_>>()
        .join(" ");
    Some(clean)
}

#[cfg(target_os = "linux")]
fn read_proc_cwd(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Discovers all running PIDs related to a launched game.
pub fn find_related_pids(launch_pid: u32, hints: &WineProbeHints) -> Vec<u32> {
    #[cfg(not(target_os = "linux"))]
    let _ = hints;
    let mut pids = Vec::new();
    if launch_pid > 0 {
        pids.push(launch_pid);
    }

    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = fs::read_dir("/proc") else {
            return pids;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            let Ok(pid) = name_str.parse::<u32>() else {
                continue;
            };
            if pid <= 1 || pids.contains(&pid) {
                continue;
            }

            if let Some(cmd) = read_proc_cmdline(pid)
                && cmdline_matches_hints(&cmd, hints)
            {
                pids.push(pid);
                continue;
            }

            if let Some(ref install) = hints.install_dir
                && let Some(cwd) = read_proc_cwd(pid)
                && (cwd == *install || cwd.starts_with(install))
            {
                pids.push(pid);
            }
        }
    }

    pids
}

/// Checks if the primary game executable is currently alive in the discovered processes.
pub fn is_game_executable_alive(pids: &[u32], hints: &WineProbeHints) -> bool {
    let Some(ref exe) = hints.executable_name else {
        return false;
    };
    let needle = exe.to_ascii_lowercase();

    #[cfg(target_os = "linux")]
    {
        for &pid in pids {
            if let Some(cmd) = read_proc_cmdline(pid) {
                let lower = cmd.to_ascii_lowercase();
                // Exclude crash handlers that wrap the executable name
                if detect_crash_reporter(&lower).is_some() {
                    continue;
                }
                if lower.contains(&needle) {
                    return true;
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pids, needle);
    }

    false
}

/// Scans related processes for known crash reporter processes.
pub fn find_active_crash_reporter(pids: &[u32]) -> Option<(String, u32)> {
    #[cfg(target_os = "linux")]
    {
        for &pid in pids {
            if let Some(cmd) = read_proc_cmdline(pid)
                && let Some(reporter) = detect_crash_reporter(&cmd)
            {
                return Some((reporter.to_string(), pid));
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = pids;
    }

    None
}

/// Probes the game run status, distinguishing between normal execution, crash states, and exit.
pub fn check_game_status(launch_pid: u32, hints: &WineProbeHints) -> GameRunStatus {
    let pids = find_related_pids(launch_pid, hints);
    if pids.is_empty() {
        return GameRunStatus::Exited;
    }

    // Check if any crash reporter is active
    if let Some((reason, culprit_pid)) = find_active_crash_reporter(&pids) {
        return GameRunStatus::Crashed {
            reason,
            culprit_pid: Some(culprit_pid),
        };
    }

    // If we have an executable name hint, check if the game executable itself is alive
    if hints.executable_name.is_some() && !is_game_executable_alive(&pids, hints) {
        // If the launch pid itself is also dead, the game exited or crashed silently
        let launch_alive = pids.contains(&launch_pid);
        if !launch_alive {
            return GameRunStatus::Exited;
        }
    }

    GameRunStatus::Running
}

/// Terminate all discovered processes related to the game launch.
pub fn terminate_game_tree(launch_pid: u32, hints: &WineProbeHints) -> usize {
    let pids = find_related_pids(launch_pid, hints);
    let count = pids.len();

    #[cfg(unix)]
    {
        for &pid in &pids {
            let _ = crate::clean_command("kill")
                .args(["-TERM", "--", &pid.to_string()])
                .status();
        }
        // If prefix_dir is specified and wineserver is available, cleanly stop the prefix
        if let Some(ref pfx) = hints.prefix_dir {
            let _ = crate::clean_command("wineserver")
                .args(["-k"])
                .env("WINEPREFIX", pfx)
                .status();
        }
    }

    #[cfg(windows)]
    {
        for &pid in &pids {
            let _ = crate::clean_command("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .status();
        }
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_crash_reporters_from_cmdline() {
        assert_eq!(
            detect_crash_reporter("C:\\windows\\system32\\winedbg.exe --auto 1234"),
            Some("Wine Crash Debugger (winedbg)")
        );
        assert_eq!(
            detect_crash_reporter("C:\\Program Files\\Game\\UnityCrashHandler64.exe"),
            Some("Unity Crash Handler")
        );
        assert_eq!(
            detect_crash_reporter(
                "Z:\\games\\test\\Engine\\Binaries\\Win64\\CrashReportClient.exe"
            ),
            Some("Unreal Engine Crash Reporter")
        );
        assert_eq!(
            detect_crash_reporter("C:\\windows\\system32\\werfault.exe /shared 5678"),
            Some("Windows Error Reporting (WerFault.exe)")
        );
        assert_eq!(
            detect_crash_reporter("Z:\\games\\test\\game.exe -screen-fullscreen 1"),
            None
        );
    }

    #[test]
    fn validates_error_dialog_titles() {
        assert!(title_looks_like_error("Fatal Error"));
        assert!(title_looks_like_error(
            "Fatal Error: DX11 feature level 11.0 is required"
        ));
        assert!(title_looks_like_error("Unhandled Exception"));
        assert!(title_looks_like_error("Error Code: 0x80070057"));
        assert!(title_looks_like_error("Protection failed to initialize"));
        assert!(title_looks_like_error("Критическая ошибка"));

        // Harmless splashes / SteamFix injectors must not be flagged
        assert!(!title_looks_like_error("SteamFix Injector"));
        assert!(!title_looks_like_error("Game Splash"));
        assert!(!title_looks_like_error("Super Meat Boy"));
        assert!(!title_looks_like_error(""));
    }

    #[test]
    fn matches_cmdline_hints() {
        let hints = WineProbeHints {
            install_dir: Some(PathBuf::from("/home/user/games/elden_ring")),
            executable_name: Some("eldenring.exe".into()),
            prefix_dir: None,
            fake_app_id: Some("480".into()),
        };

        assert!(cmdline_matches_hints(
            "wine Z:\\home\\user\\games\\elden_ring\\eldenring.exe",
            &hints
        ));
        assert!(cmdline_matches_hints(
            "wine C:\\windows\\system32\\steam_app_480.exe",
            &hints
        ));
        assert!(!cmdline_matches_hints(
            "wine Z:\\home\\user\\games\\cyberpunk\\cyberpunk2077.exe",
            &hints
        ));
    }
}
