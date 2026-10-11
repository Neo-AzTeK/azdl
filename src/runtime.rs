use crate::steam::{library_paths, steam_root};
use crate::{Result, Runner, clean_command};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ProtonEntry {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
}

fn foreign_arch(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if !lower.starts_with("ge-proton") {
        return false;
    }
    let host_arm = cfg!(target_arch = "aarch64");
    (host_arm && lower.contains("x86_64")) || (!host_arm && lower.contains("aarch64"))
}

fn usable_script(script: &Path) -> Result<bool> {
    if !script.is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(script)?.permissions().mode() & 0o111 == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn add_children(
    root: &Path,
    source: &str,
    entries: &mut Vec<ProtonEntry>,
    seen: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    for child in fs::read_dir(root)? {
        let child = child?;
        let name = child.file_name().to_string_lossy().to_string();
        if foreign_arch(&name) {
            continue;
        }
        let script = child.path().join("proton");
        if !usable_script(&script)? {
            continue;
        }
        let script = fs::canonicalize(script)?;
        if seen.insert(script.clone()) {
            entries.push(ProtonEntry {
                name,
                path: script,
                source: source.to_string(),
            });
        }
    }
    Ok(())
}

fn preference(entry: &ProtonEntry) -> i32 {
    let lower = entry.name.to_ascii_lowercase();
    if lower.starts_with("ge-proton") {
        3
    } else if lower.contains("experimental") {
        2
    } else {
        1
    }
}

/// Find installed Proton scripts in Steam libraries, compatibility tools, and azdl runners.
pub fn discover_proton(data_dir: &Path) -> Result<Vec<ProtonEntry>> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    let client_root = env::var_os("STEAM_COMPAT_CLIENT_INSTALL_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| steam_root().ok().filter(|path| path.is_dir()));
    if let Some(root) = client_root {
        for library in library_paths(&root)? {
            let common = library.join("steamapps/common");
            if common.is_dir() {
                for child in fs::read_dir(&common)? {
                    let child = child?;
                    let name = child.file_name().to_string_lossy().to_ascii_lowercase();
                    if name.contains("proton") && !foreign_arch(&name) {
                        let script = child.path().join("proton");
                        if usable_script(&script)? {
                            let script = fs::canonicalize(script)?;
                            if seen.insert(script.clone()) {
                                entries.push(ProtonEntry {
                                    name: child.file_name().to_string_lossy().to_string(),
                                    path: script,
                                    source: "steam".into(),
                                });
                            }
                        }
                    }
                }
            }
        }
        add_children(
            &root.join("compatibilitytools.d"),
            "steam-custom",
            &mut entries,
            &mut seen,
        )?;
    }
    add_children(&data_dir.join("runners"), "azdl", &mut entries, &mut seen)?;
    entries.sort_by(|a, b| {
        preference(b)
            .cmp(&preference(a))
            .then_with(|| b.name.cmp(&a.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(entries)
}

pub fn resolve_runner(configured: &Runner, executable: &Path, data_dir: &Path) -> Result<Runner> {
    let windows_game = cfg!(target_os = "linux")
        && executable
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("exe"));
    match configured {
        Runner::Auto if windows_game => {
            let client_available = env::var_os("STEAM_COMPAT_CLIENT_INSTALL_PATH")
                .is_some_and(|path| PathBuf::from(path).is_dir())
                || steam_root().is_ok_and(|path| path.is_dir());
            if client_available
                && let Some(entry) = discover_proton(data_dir)
                    .ok()
                    .and_then(|r| r.into_iter().next())
            {
                Ok(Runner::Proton(entry.path))
            } else {
                Ok(Runner::Wine)
            }
        }
        Runner::Auto => Ok(Runner::Native),
        Runner::ProtonAuto => {
            let entry = discover_proton(data_dir)?
                .into_iter()
                .next()
                .ok_or("no Proton runner found; install one in Steam or set a specific runner")?;
            Ok(Runner::Proton(entry.path))
        }
        other => Ok(other.clone()),
    }
}

pub fn fetch_latest_proton_ge_release() -> Result<(String, String)> {
    let output = clean_command("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "15",
            "-H",
            "User-Agent: azdl",
            "https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases/latest",
        ])
        .output()?;
    if !output.status.success() {
        return Err("failed to fetch latest Proton-GE release from GitHub".into());
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or("release has no tag_name")?
        .to_string();
    let assets = json
        .get("assets")
        .and_then(|v| v.as_array())
        .ok_or("release has no assets")?;
    let download_url = assets
        .iter()
        .find(|a| {
            a.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|name| name.ends_with(".tar.gz"))
        })
        .and_then(|a| a.get("browser_download_url"))
        .and_then(|u| u.as_str())
        .ok_or("no .tar.gz asset found in Proton-GE release")?
        .to_string();
    Ok((tag, download_url))
}

pub fn download_proton_ge(data_dir: &Path) -> Result<ProtonEntry> {
    let (tag, url) = fetch_latest_proton_ge_release()?;
    let temp_dir = data_dir.join("proton-download");
    fs::create_dir_all(&temp_dir)?;
    let archive_path = temp_dir.join(format!("{tag}.tar.gz"));

    let status = clean_command("curl")
        .args(["--fail", "--location", "--retry", "3", "--output"])
        .arg(&archive_path)
        .arg(&url)
        .status()?;
    if !status.success() {
        return Err("failed to download Proton-GE archive".into());
    }

    let runners_dir = data_dir.join("runners");
    fs::create_dir_all(&runners_dir)?;

    let unpack_status = clean_command("bsdtar")
        .args(["-xzf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(&runners_dir)
        .status()?;
    if !unpack_status.success() {
        return Err("failed to extract Proton-GE archive".into());
    }
    let _ = fs::remove_file(&archive_path);

    let proton_dir = runners_dir.join(&tag);
    let script = proton_dir.join("proton");
    if script.is_file() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&script, fs::Permissions::from_mode(0o755));
        }
        Ok(ProtonEntry {
            name: tag,
            path: fs::canonicalize(&script)?,
            source: "azdl".into(),
        })
    } else {
        discover_proton(data_dir)?
            .into_iter()
            .find(|e| e.name.contains(&tag))
            .ok_or_else(|| "extracted Proton-GE script not found".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn finds_custom_runner_and_filters_foreign_architecture() {
        let root = std::env::temp_dir().join(format!(
            "azdl-runtime-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let local = root.join("runners/GE-Proton11-5-x86_64");
        let foreign = root.join("runners/GE-Proton11-5-aarch64");
        fs::create_dir_all(&local).unwrap();
        fs::create_dir_all(&foreign).unwrap();
        fs::write(local.join("proton"), b"runner").unwrap();
        fs::write(foreign.join("proton"), b"runner").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(local.join("proton"), fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(foreign.join("proton"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let entries = discover_proton(&root).unwrap();
        if cfg!(target_arch = "x86_64") {
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.name == "GE-Proton11-5-x86_64")
            );
            assert!(
                !entries
                    .iter()
                    .any(|entry| entry.name == "GE-Proton11-5-aarch64")
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
