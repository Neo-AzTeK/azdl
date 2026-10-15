use crate::multiplayer::{FixKind, detect_fix};
use crate::patch::active_patch;
use crate::runtime::resolve_runner;
use crate::steam::steam_root;
use crate::{Game, Runner, clean_command, resolve_launch};
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticReport {
    pub runner: Option<String>,
    pub checks: Vec<Diagnostic>,
}

impl DiagnosticReport {
    pub fn has_errors(&self) -> bool {
        self.checks
            .iter()
            .any(|check| matches!(check.severity, Severity::Error))
    }
}

fn add(checks: &mut Vec<Diagnostic>, severity: Severity, message: impl Into<String>) {
    checks.push(Diagnostic {
        severity,
        message: message.into(),
    });
}

fn usable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn command_available(program: &Path) -> bool {
    if program.is_absolute() || program.components().count() > 1 {
        return usable_file(program);
    }
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path).any(|directory| usable_file(&directory.join(program)))
    })
}

pub fn diagnose_game(game: &Game, data_dir: &Path) -> DiagnosticReport {
    let mut checks = Vec::new();
    let steam_profile = matches!(game.runner, Runner::Steam(_));
    let mut effective_game = game.clone();
    if !steam_profile {
        match resolve_launch(game) {
            Ok(launch) => effective_game.executable = launch.executable,
            Err(error) => add(
                &mut checks,
                Severity::Error,
                format!("Launch option: {error}"),
            ),
        }
    }
    let game = &effective_game;
    let install = match fs::canonicalize(&game.install_dir) {
        Ok(path) if path.is_dir() => Some(path),
        _ => {
            add(
                &mut checks,
                Severity::Error,
                format!(
                    "Install directory is missing: {}",
                    game.install_dir.display()
                ),
            );
            None
        }
    };
    if !steam_profile {
        match fs::canonicalize(&game.executable) {
            Ok(path) if path.is_file() => {
                if install.as_ref().is_some_and(|root| !path.starts_with(root)) {
                    add(
                        &mut checks,
                        Severity::Error,
                        "Executable is outside the install directory",
                    );
                }
            }
            _ => add(
                &mut checks,
                Severity::Error,
                format!("Executable is missing: {}", game.executable.display()),
            ),
        }
    }

    let runner = match resolve_runner(&game.runner, &game.executable, data_dir) {
        Ok(Runner::Native) => {
            if !usable_file(&game.executable) {
                add(
                    &mut checks,
                    Severity::Error,
                    "Native executable cannot be run",
                );
            }
            if cfg!(target_os = "linux")
                && game
                    .executable
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
            {
                add(
                    &mut checks,
                    Severity::Warning,
                    "Windows .exe is set to native; choose Wine or Proton if it does not run",
                );
            }
            Some("native".to_string())
        }
        Ok(Runner::Wine) => {
            if !command_available(Path::new("wine")) {
                add(&mut checks, Severity::Error, "Wine command is unavailable");
            }
            Some("wine".to_string())
        }
        Ok(Runner::Proton(path)) => {
            if !usable_file(&path) {
                add(
                    &mut checks,
                    Severity::Error,
                    format!("Proton script cannot be run: {}", path.display()),
                );
            }
            let steam_client = env::var_os("STEAM_COMPAT_CLIENT_INSTALL_PATH")
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
                .or_else(|| steam_root().ok().filter(|path| path.is_dir()));
            if steam_client.is_none() {
                add(
                    &mut checks,
                    Severity::Error,
                    "Steam client directory is unavailable for Proton",
                );
            }
            Some(format!("proton:{}", path.display()))
        }
        Ok(Runner::Steam(app_id)) => {
            add(
                &mut checks,
                Severity::Error,
                "Steam client launches are unavailable; choose a local executable and runner",
            );
            Some(format!("steam:{app_id}"))
        }
        Ok(Runner::Auto | Runner::ProtonAuto) => None,
        Err(error) => {
            add(&mut checks, Severity::Error, format!("Runner: {error}"));
            None
        }
    };

    for (key, value) in &game.environment {
        if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
            add(
                &mut checks,
                Severity::Error,
                format!("Invalid environment variable: {key}"),
            );
        }
    }
    match active_patch(data_dir, &game.id) {
        Ok(Some(patch)) => add(
            &mut checks,
            Severity::Info,
            if patch.enabled {
                "Reversible patch is enabled"
            } else {
                "Reversible patch is disabled"
            },
        ),
        Ok(None) => {}
        Err(error) => add(
            &mut checks,
            Severity::Error,
            format!("Patch state cannot be read: {error}"),
        ),
    }
    if !steam_profile && install.is_some() && game.executable.is_file() {
        match detect_fix(game) {
            Ok(FixKind::OnlineFix) => add(
                &mut checks,
                Severity::Info,
                "Online Fix marker found; multiplayer session has not been verified",
            ),
            Ok(FixKind::SteamFix) => add(
                &mut checks,
                Severity::Info,
                "Steam Fix marker found; multiplayer session has not been verified",
            ),
            Ok(FixKind::None) => {}
            Err(error) => add(
                &mut checks,
                Severity::Warning,
                format!("Fix markers could not be checked: {error}"),
            ),
        }
    }
    if checks.is_empty() {
        add(
            &mut checks,
            Severity::Info,
            "Local launch prerequisites found; game startup and multiplayer are unverified",
        );
    }
    DiagnosticReport { runner, checks }
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct SystemCheck {
    pub name: String,
    pub description: String,
    pub available: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub required: bool,
}

fn check_binary(name: &str, description: &str, required: bool, version_flag: &str) -> SystemCheck {
    let which = if cfg!(windows) {
        clean_command("where").arg(name).output()
    } else {
        clean_command("which").arg(name).output()
    };
    let path = which
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .map(|s| s.lines().next().unwrap_or("").trim().to_string())
        })
        .filter(|s| !s.is_empty());

    let available = path.is_some();
    let version = if available {
        clean_command(name)
            .arg(version_flag)
            .output()
            .ok()
            .and_then(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                let err = String::from_utf8_lossy(&o.stderr);
                let combined = if out.trim().is_empty() { err } else { out };
                combined.lines().next().map(|line| line.trim().to_string())
            })
    } else {
        None
    };

    SystemCheck {
        name: name.to_string(),
        description: description.to_string(),
        available,
        path,
        version,
        required,
    }
}

pub fn system_diagnostics() -> Vec<SystemCheck> {
    #[allow(unused_mut)]
    let mut checks = vec![
        check_binary(
            "curl",
            "HTTP/HTTPS download transport and feed fetcher",
            true,
            "--version",
        ),
        if cfg!(windows) {
            let bsdtar_check = check_binary(
                "bsdtar",
                "Multi-format archive extraction (ZIP, 7z, TAR, ISO)",
                true,
                "--version",
            );
            if bsdtar_check.available {
                bsdtar_check
            } else {
                check_binary(
                    "tar",
                    "Windows built-in archive extraction engine (ZIP, 7z, TAR)",
                    true,
                    "--version",
                )
            }
        } else {
            check_binary(
                "bsdtar",
                "Multi-format archive extraction (ZIP, 7z, TAR, ISO)",
                true,
                "--version",
            )
        },
        check_binary(
            "unrar",
            "Proprietary RAR 4/5 archive decompression",
            false,
            "-v",
        ),
        check_binary(
            "aria2c",
            "Multi-source BitTorrent / magnet transport engine",
            false,
            "--version",
        ),
    ];

    #[cfg(target_os = "linux")]
    {
        checks.push(check_binary(
            "bwrap",
            "Bubblewrap namespace sandbox for untrusted unpackers",
            false,
            "--version",
        ));
        checks.push(check_binary(
            "wine",
            "Windows NT compatibility runtime",
            false,
            "--version",
        ));
    }

    checks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Library, add_game};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    #[test]
    fn reports_native_launch_permission_and_missing_executable() {
        use std::os::unix::fs::PermissionsExt;
        let root = env::temp_dir().join(format!(
            "azdl-doctor-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let installed = root.join("installed");
        fs::create_dir_all(&installed).unwrap();
        let executable = installed.join("game.sh");
        fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "demo",
            "Demo",
            &installed,
            Path::new("game.sh"),
        )
        .unwrap();
        let game = library.games.get_mut("demo").unwrap();
        game.runner = Runner::Native;
        assert!(diagnose_game(game, &root).has_errors());
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!diagnose_game(game, &root).has_errors());
        fs::remove_file(&executable).unwrap();
        assert!(diagnose_game(game, &root).has_errors());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_system_diagnostics_never_invokes_steam_and_completes_quickly() {
        let start = std::time::Instant::now();
        let checks = system_diagnostics();
        let elapsed = start.elapsed();

        // Diagnostics should never hang or take multiple seconds
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "system_diagnostics took too long: {elapsed:?}"
        );

        // Steam client must never be in system diagnostics (prevents auto-launching GUI)
        assert!(
            !checks.iter().any(|c| c.name.eq_ignore_ascii_case("steam")),
            "steam should not be in system diagnostics checks"
        );
    }

    #[test]
    fn test_diagnose_game_steamfix_does_not_require_steam() {
        let root = env::temp_dir().join(format!(
            "azdl-doctor-fix-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let installed = root.join("installed");
        fs::create_dir_all(&installed).unwrap();
        let executable = installed.join("game.exe");
        fs::write(&executable, b"MZ dummy PE").unwrap();
        fs::write(installed.join("steamfix.ini"), b"[SteamFix]\nFakeAppId=480").unwrap();

        let mut library = Library::default();
        add_game(
            &mut library,
            "fix-demo",
            "Fix Demo",
            &installed,
            Path::new("game.exe"),
        )
        .unwrap();

        let game = library.games.get("fix-demo").unwrap();
        let report = diagnose_game(game, &root);

        // Report should not contain warnings demanding Steam to be running
        for check in report.checks {
            assert!(
                !check.message.contains("Steam is not running"),
                "report should not demand Steam to be running: {}",
                check.message
            );
            assert!(
                !check.message.contains("Steam will be requested at launch"),
                "report should not threaten Steam auto-launch: {}",
                check.message
            );
        }

        fs::remove_dir_all(root).unwrap();
    }
}
