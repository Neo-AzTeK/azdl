//! Subprocess environment sanitizer and command builder.
//!
//! When `azdl` is packaged and distributed as a Linux AppImage, the runtime
//! mounts the application bundle under a temporary `$APPDIR` mount point and
//! modifies dynamic linker variables (`LD_LIBRARY_PATH`, `PATH`, etc.) to point
//! to the AppImage's internal bundled libraries.
//!
//! If child processes (such as `curl`, `wine`, `bsdtar`, `xdg-open`, or
//! games) inherit these bundled paths, dynamic linking will fail (often with exit
//! status 127) due to symbol version mismatches against the host's system libraries.
//!
//! This module provides [`clean_command`] and [`sanitize_command`] to strip or restore
//! AppImage-specific variables, allowing host binaries to execute cleanly.

use std::ffi::OsStr;
use std::process::Command;

/// Sanitizes a [`Command`]'s environment for executing external host binaries.
///
/// If running inside a Linux AppImage or similar isolated bundle where `$APPDIR`
/// or `$LD_LIBRARY_PATH_ORIG` is set, this cleans dynamic linker paths and
/// desktop integration environment variables to prevent host binary link failures.
pub fn sanitize_command(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        sanitize_unix_command(cmd);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Creates a new [`Command`] configured with a sanitized environment for host execution.
pub fn clean_command<S: AsRef<OsStr>>(program: S) -> Command {
    let mut cmd = Command::new(program);
    sanitize_command(&mut cmd);
    cmd
}

#[cfg(unix)]
fn sanitize_unix_command(cmd: &mut Command) {
    let appdir = std::env::var("APPDIR").ok();
    let appimage = std::env::var("APPIMAGE").ok();
    let ld_orig = std::env::var("LD_LIBRARY_PATH_ORIG").ok();
    let has_mount_in_ld = std::env::var("LD_LIBRARY_PATH")
        .map(|v| v.contains("/.mount_"))
        .unwrap_or(false);

    let is_appimage =
        appdir.is_some() || appimage.is_some() || ld_orig.is_some() || has_mount_in_ld;
    if !is_appimage {
        return;
    }

    let appdir_ref = appdir.as_deref().unwrap_or("");

    // Helper closure to test if a path component is inside the AppImage
    let is_appdir_path = |path: &str| -> bool {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return false;
        }
        if !appdir_ref.is_empty() && trimmed.starts_with(appdir_ref) {
            return true;
        }
        trimmed.contains("/.mount_")
    };

    // 1. LD_LIBRARY_PATH
    if let Some(orig) = ld_orig {
        if orig.trim().is_empty() {
            cmd.env_remove("LD_LIBRARY_PATH");
        } else {
            cmd.env("LD_LIBRARY_PATH", orig);
        }
    } else if let Ok(current) = std::env::var("LD_LIBRARY_PATH") {
        let filtered: Vec<&str> = current
            .split(':')
            .map(str::trim)
            .filter(|p| !p.is_empty() && !is_appdir_path(p))
            .collect();

        if filtered.is_empty() {
            cmd.env_remove("LD_LIBRARY_PATH");
        } else {
            cmd.env("LD_LIBRARY_PATH", filtered.join(":"));
        }
    }

    // 2. LD_PRELOAD
    if let Ok(preload_orig) = std::env::var("LD_PRELOAD_ORIG") {
        if preload_orig.trim().is_empty() {
            cmd.env_remove("LD_PRELOAD");
        } else {
            cmd.env("LD_PRELOAD", preload_orig);
        }
    } else if let Ok(current_preload) = std::env::var("LD_PRELOAD") {
        let filtered: Vec<&str> = current_preload
            .split(':')
            .map(str::trim)
            .filter(|p| !p.is_empty() && !is_appdir_path(p))
            .collect();

        if filtered.is_empty() {
            cmd.env_remove("LD_PRELOAD");
        } else {
            cmd.env("LD_PRELOAD", filtered.join(":"));
        }
    }

    // 3. PATH
    if let Ok(orig_path) = std::env::var("PATH_ORIG") {
        if !orig_path.trim().is_empty() {
            cmd.env("PATH", orig_path);
        }
    } else if let Ok(current_path) = std::env::var("PATH") {
        let filtered: Vec<&str> = current_path
            .split(':')
            .map(str::trim)
            .filter(|p| !p.is_empty() && !is_appdir_path(p))
            .collect();

        if !filtered.is_empty() {
            cmd.env("PATH", filtered.join(":"));
        }
    }

    // 4. XDG_DATA_DIRS
    if let Ok(orig_xdg) = std::env::var("XDG_DATA_DIRS_ORIG") {
        if orig_xdg.trim().is_empty() {
            cmd.env_remove("XDG_DATA_DIRS");
        } else {
            cmd.env("XDG_DATA_DIRS", orig_xdg);
        }
    } else if let Ok(current_xdg) = std::env::var("XDG_DATA_DIRS") {
        let filtered: Vec<&str> = current_xdg
            .split(':')
            .map(str::trim)
            .filter(|p| !p.is_empty() && !is_appdir_path(p))
            .collect();

        if filtered.is_empty() {
            cmd.env_remove("XDG_DATA_DIRS");
        } else {
            cmd.env("XDG_DATA_DIRS", filtered.join(":"));
        }
    }

    // 5. AppImage-specific desktop / runtime variables
    for var_name in [
        "GSETTINGS_SCHEMA_DIR",
        "PYTHONHOME",
        "PYTHONPATH",
        "PERLLIB",
        "PERL5LIB",
        "QT_PLUGIN_PATH",
        "QT_QPA_PLATFORM_PLUGIN_PATH",
        "GTK_PATH",
        "GTK_EXE_PREFIX",
        "GST_PLUGIN_SYSTEM_PATH",
        "GST_PLUGIN_SYSTEM_PATH_1_0",
        "GST_PLUGIN_SCANNER",
    ] {
        if let Ok(val) = std::env::var(var_name)
            && is_appdir_path(&val)
        {
            cmd.env_remove(var_name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::ffi::OsString;

    #[test]
    fn clean_command_creates_valid_command() {
        let cmd = clean_command("echo");
        assert_eq!(cmd.get_program(), "echo");
    }

    #[cfg(unix)]
    #[test]
    fn sanitize_command_handles_appimage_vars() {
        let mut cmd = Command::new("test");

        // Helper to check if var is removed or set
        let get_env_val = |c: &Command, key: &str| -> Option<Option<OsString>> {
            for (k, v) in c.get_envs() {
                if k == OsStr::new(key) {
                    return Some(v.map(|s| s.to_os_string()));
                }
            }
            None
        };

        // When APPDIR is set, sanitize_command runs
        unsafe {
            std::env::set_var("APPDIR", "/tmp/.mount_test123");
            std::env::set_var(
                "LD_LIBRARY_PATH",
                "/tmp/.mount_test123/usr/lib:/tmp/.mount_test123/usr/lib64",
            );
        }

        sanitize_command(&mut cmd);

        // All paths were in APPDIR, so LD_LIBRARY_PATH should be removed (None in get_envs)
        let ld_status = get_env_val(&cmd, "LD_LIBRARY_PATH");
        assert_eq!(
            ld_status,
            Some(None),
            "LD_LIBRARY_PATH should be marked for removal"
        );

        unsafe {
            std::env::remove_var("APPDIR");
            std::env::remove_var("LD_LIBRARY_PATH");
        }
    }
}
