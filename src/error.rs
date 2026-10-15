//! Structured error definitions for azdl launcher operations.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Structured, machine-readable error for launcher operations and IPC responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum LauncherError {
    /// Game ID was not found in the local library.
    GameNotFound { id: String, message: String },
    /// Game is currently running.
    GameAlreadyRunning { id: String, message: String },
    /// Configured or requested compatibility runner is missing or invalid.
    RunnerNotFound { runner: String, message: String },
    /// Configured game executable does not exist or is not a valid binary.
    ExecutableNotFound { path: String, message: String },
    /// Multiplayer fix requires Steam running in the background.
    SteamNotRunning { title: String, message: String },
    /// Storage drive was not found or is unmounted.
    DriveNotFound { drive_id: String, message: String },
    /// Target storage location does not have enough free disk space.
    DriveFull {
        path: String,
        needed_bytes: u64,
        available_bytes: u64,
        message: String,
    },
    /// Game ID format validation failed.
    InvalidId { id: String, message: String },
    /// Requested plugin is not installed.
    PluginNotFound { id: String, message: String },
    /// No download provider plugin installed (e.g. steamidra).
    NoPluginAvailable { message: String },
    /// Archive file is corrupted or unsupported.
    CorruptedArchive { path: String, message: String },
    /// External process returned non-zero exit code or failed to launch.
    ProcessFailed { command: String, message: String },
    /// State synchronization lock poisoned.
    LockPoisoned { message: String },
    /// General or unclassified error.
    General { message: String },
}

impl LauncherError {
    pub fn game_not_found(id: impl Into<String>) -> Self {
        let id = id.into();
        Self::GameNotFound {
            message: format!("Game '{id}' not found in library"),
            id,
        }
    }

    pub fn game_already_running(id: impl Into<String>) -> Self {
        let id = id.into();
        Self::GameAlreadyRunning {
            message: format!("Game '{id}' is already running"),
            id,
        }
    }

    pub fn runner_not_found(runner: impl Into<String>) -> Self {
        let runner = runner.into();
        Self::RunnerNotFound {
            message: format!("Compatibility tool or runner '{runner}' not found"),
            runner,
        }
    }

    pub fn executable_not_found(path: impl Into<String>) -> Self {
        let path = path.into();
        Self::ExecutableNotFound {
            message: format!("Executable '{path}' does not exist"),
            path,
        }
    }

    pub fn steam_not_running(title: impl Into<String>) -> Self {
        let title = title.into();
        Self::SteamNotRunning {
            message: format!("Steam is required to run multiplayer fixes for '{title}'"),
            title,
        }
    }

    pub fn drive_not_found(drive_id: impl Into<String>) -> Self {
        let drive_id = drive_id.into();
        Self::DriveNotFound {
            message: format!("Storage drive '{drive_id}' was not found"),
            drive_id,
        }
    }

    pub fn drive_full(path: impl Into<String>, needed_bytes: u64, available_bytes: u64) -> Self {
        let path = path.into();
        Self::DriveFull {
            message: format!(
                "Insufficient disk space on {path}: need {needed_bytes} bytes, but only {available_bytes} bytes available"
            ),
            path,
            needed_bytes,
            available_bytes,
        }
    }

    pub fn invalid_id(id: impl Into<String>) -> Self {
        let id = id.into();
        Self::InvalidId {
            message: format!("Invalid identifier '{id}'"),
            id,
        }
    }

    pub fn plugin_not_found(id: impl Into<String>) -> Self {
        let id = id.into();
        Self::PluginNotFound {
            message: format!("Plugin '{id}' is not installed"),
            id,
        }
    }

    pub fn no_plugin_available(msg: impl Into<String>) -> Self {
        Self::NoPluginAvailable {
            message: msg.into(),
        }
    }

    pub fn corrupted_archive(path: impl Into<String>, msg: impl Into<String>) -> Self {
        let path = path.into();
        let msg = msg.into();
        Self::CorruptedArchive {
            message: format!("Corrupted archive '{path}': {msg}"),
            path,
        }
    }

    pub fn process_failed(command: impl Into<String>, msg: impl Into<String>) -> Self {
        let command = command.into();
        let msg = msg.into();
        Self::ProcessFailed {
            message: format!("Process '{command}' failed: {msg}"),
            command,
        }
    }

    pub fn lock_poisoned(msg: impl Into<String>) -> Self {
        Self::LockPoisoned {
            message: msg.into(),
        }
    }

    pub fn general(msg: impl Into<String>) -> Self {
        Self::General {
            message: msg.into(),
        }
    }
}

impl fmt::Display for LauncherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GameNotFound { message, .. }
            | Self::GameAlreadyRunning { message, .. }
            | Self::RunnerNotFound { message, .. }
            | Self::ExecutableNotFound { message, .. }
            | Self::SteamNotRunning { message, .. }
            | Self::DriveNotFound { message, .. }
            | Self::DriveFull { message, .. }
            | Self::InvalidId { message, .. }
            | Self::PluginNotFound { message, .. }
            | Self::NoPluginAvailable { message }
            | Self::CorruptedArchive { message, .. }
            | Self::ProcessFailed { message, .. }
            | Self::LockPoisoned { message }
            | Self::General { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for LauncherError {}

impl From<String> for LauncherError {
    fn from(s: String) -> Self {
        Self::general(s)
    }
}

impl From<&str> for LauncherError {
    fn from(s: &str) -> Self {
        Self::general(s)
    }
}

impl From<Box<dyn std::error::Error>> for LauncherError {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        Self::general(e.to_string())
    }
}

impl From<std::io::Error> for LauncherError {
    fn from(e: std::io::Error) -> Self {
        Self::general(e.to_string())
    }
}

impl From<serde_json::Error> for LauncherError {
    fn from(e: serde_json::Error) -> Self {
        Self::general(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launcher_error_serialization() {
        let err = LauncherError::runner_not_found("GE-Proton-99");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains(r#""code":"runner_not_found""#));
        assert!(json.contains(r#""runner":"GE-Proton-99""#));
        assert_eq!(
            err.to_string(),
            "Compatibility tool or runner 'GE-Proton-99' not found"
        );

        let roundtrip: LauncherError = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, err);
    }

    #[test]
    fn test_launcher_error_conversions() {
        let err1: LauncherError = "something broke".into();
        assert_eq!(err1, LauncherError::general("something broke"));

        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err2: LauncherError = io_err.into();
        assert!(err2.to_string().contains("file missing"));
    }
}
