use crate::runtime::resolve_runner;
use crate::steam::steam_root;
use crate::{Game, Runner, clean_command, resolve_launch};
use serde::Serialize;
use std::env;
use std::ffi::{OsStr, OsString};
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

fn find_command_in_path(program: &Path, paths: &OsStr, extensions: &[OsString]) -> Option<PathBuf> {
    for directory in env::split_paths(paths) {
        let candidate = directory.join(program);
        if usable_file(&candidate) {
            return Some(candidate);
        }
        if program.extension().is_none() {
            for extension in extensions {
                let mut filename = program.as_os_str().to_os_string();
                filename.push(extension);
                let candidate = directory.join(filename);
                if usable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn command_path(program: &Path) -> Option<PathBuf> {
    if program.is_absolute() || program.components().count() > 1 {
        return usable_file(program).then(|| program.to_path_buf());
    }
    let paths = env::var_os("PATH")?;
    #[cfg(windows)]
    let extensions = env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|extension| {
            extension.starts_with('.')
                && extension.len() > 1
                && extension[1..]
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric())
        })
        .map(OsString::from)
        .collect::<Vec<_>>();
    #[cfg(not(windows))]
    let extensions = Vec::new();
    find_command_in_path(program, &paths, &extensions)
}

fn command_available(program: &Path) -> bool {
    command_path(program).is_some()
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
    if matches!(
        game.runner,
        Runner::Wine | Runner::Proton(_) | Runner::Auto | Runner::ProtonAuto
    ) {
        let prefix_dir = data_dir.join("prefixes").join(&game.id);
        if prefix_dir.is_dir() {
            let dep_status = crate::dependencies::diagnose_prefix_dependencies(&prefix_dir);
            if dep_status.missing.is_empty() {
                add(
                    &mut checks,
                    Severity::Info,
                    "Prefix runtime dependencies (VC++ 2015-2022, DirectX) are satisfied",
                );
            } else {
                add(
                    &mut checks,
                    Severity::Info,
                    format!(
                        "Prefix runtime dependencies pending auto-install: {}",
                        dep_status.missing.join(", ")
                    ),
                );
            }
        }
    }
    if checks.is_empty() {
        add(
            &mut checks,
            Severity::Info,
            "Local launch prerequisites found; game startup is unverified",
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
    let path = command_path(Path::new(name));
    let available = path.is_some();
    let version = if let Some(path) = &path {
        clean_command(path)
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
        path: path.map(|path| path.to_string_lossy().into_owned()),
        version,
        required,
    }
}

pub fn system_diagnostics() -> Vec<SystemCheck> {
    #[allow(unused_mut)]
    let mut checks = vec![
        check_binary(
            "curl",
            "HTTPS transport for manifests, runtimes and redistributables",
            true,
            "--version",
        ),
        if cfg!(windows) {
            let bsdtar_check = check_binary(
                "bsdtar",
                "Manifest bundle and runtime archive extraction",
                true,
                "--version",
            );
            if bsdtar_check.available {
                bsdtar_check
            } else {
                check_binary(
                    "tar",
                    "Windows built-in archive extraction for manifest bundles",
                    true,
                    "--version",
                )
            }
        } else {
            check_binary(
                "bsdtar",
                "Manifest bundle and runtime archive extraction",
                true,
                "--version",
            )
        },
    ];

    #[cfg(target_os = "linux")]
    {
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

    #[test]
    fn path_lookup_preserves_directory_order_and_executable_extensions() {
        let root = env::temp_dir().join(format!(
            "azdl-doctor-path-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(first.join("tool.exe")).unwrap();
        fs::create_dir_all(&second).unwrap();
        let write_executable = |path: &Path| {
            fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        };
        write_executable(&second.join("tool.exe"));
        let paths = env::join_paths([&first, &second]).unwrap();
        let extensions = [OsString::from(".exe"), OsString::from(".cmd")];
        assert_eq!(
            find_command_in_path(Path::new("tool"), &paths, &extensions),
            Some(second.join("tool.exe"))
        );
        write_executable(&first.join("tool.cmd"));
        assert_eq!(
            find_command_in_path(Path::new("tool"), &paths, &extensions),
            Some(first.join("tool.cmd"))
        );
        assert_eq!(
            find_command_in_path(Path::new("tool.exe"), &paths, &extensions),
            Some(second.join("tool.exe"))
        );
        assert!(find_command_in_path(Path::new("missing"), &paths, &extensions).is_none());
        assert_eq!(
            command_path(&second.join("tool.exe")),
            Some(second.join("tool.exe"))
        );
        fs::remove_dir_all(root).unwrap();
    }

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
}
