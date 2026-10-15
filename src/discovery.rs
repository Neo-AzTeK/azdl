use crate::{Library, Result, add_game};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 10;
const MAX_ENTRIES: usize = 50_000;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub score: i64,
}

fn compact(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn excluded(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "cmd.exe",
        "powershell.exe",
        "conhost.exe",
        "uninstall.exe",
        "unins000.exe",
        "ffmpeg.exe",
        "ffprobe.exe",
        "ffplay.exe",
        "7z.exe",
        "7za.exe",
        "7zr.exe",
        "curl.exe",
        "wget.exe",
        "steam.exe",
        "dotnet.exe",
        "gdb.exe",
        "gdbserver.exe",
    ];
    const PARTS: &[&str] = &[
        "unins",
        "setup",
        "redist",
        "vcredist",
        "dxsetup",
        "dxwebsetup",
        "oalinst",
        "unitycrashhandler",
        "crashpad",
        "crashreport",
        "crashhandler",
        "prereq",
        "steamless",
        "steamstub",
        "easyhook",
        "dedicatedserver",
        "launcher_helper",
    ];
    let lower = name.to_ascii_lowercase();
    if EXACT.contains(&lower.as_str())
        || PARTS.iter().any(|part| lower.contains(part))
        || lower.ends_with(".so")
        || lower.contains(".so.")
        || lower.ends_with(".dll")
        || lower.ends_with(".dylib")
    {
        return true;
    }
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    ["editor", "server", "settings", "configuration", "config"]
        .iter()
        .any(|ending| stem == *ending || stem.ends_with(ending))
}

fn native_executable(path: &Path, _metadata: &fs::Metadata) -> Result<bool> {
    let Ok(mut file) = File::open(path) else {
        return Ok(false);
    };
    let mut header = [0u8; 4];
    let count = file.read(&mut header).unwrap_or(0);
    let is_elf = count >= 4 && header == [0x7f, b'E', b'L', b'F'];
    let is_script = count >= 2
        && header[..2] == *b"#!"
        && !path.extension().is_some_and(|ext| {
            ext.eq_ignore_ascii_case("txt")
                || ext.eq_ignore_ascii_case("log")
                || ext.eq_ignore_ascii_case("md")
        });
    if !is_elf && !is_script {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if _metadata.permissions().mode() & 0o111 == 0 {
            let _ = fs::set_permissions(
                path,
                fs::Permissions::from_mode(_metadata.permissions().mode() | 0o755),
            );
        }
    }
    Ok(true)
}

/// List likely game executables, best candidate first. Symlinks are ignored.
pub fn discover_executables(root: &Path, title: &str) -> Result<Vec<Candidate>> {
    let root = fs::canonicalize(root)?;
    if !root.is_dir() {
        return Err("install path is not a directory".into());
    }
    let title = compact(title);
    let mut results = Vec::new();
    let mut stack = vec![(root.clone(), 0usize)];
    let mut entries_seen = 0usize;

    while let Some((dir, depth)) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            entries_seen += 1;
            if entries_seen > MAX_ENTRIES {
                return Err("game folder has too many entries to scan".into());
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if excluded(&name) {
                continue;
            }
            let windows_exe = name.to_ascii_lowercase().ends_with(".exe");
            if !windows_exe && !native_executable(&path, &metadata)? {
                continue;
            }

            let relative_path = path.strip_prefix(&root)?.to_path_buf();
            let stem = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            let clean_stem = compact(&stem);
            let mut score = 200 - (depth as i64 * 40);
            score += (metadata.len() / (1024 * 1024)).min(200) as i64;
            if !title.is_empty() && clean_stem == title {
                score += 1000;
            } else if title.len() >= 4
                && clean_stem.len() >= 4
                && (title.contains(&clean_stem) || clean_stem.contains(&title))
            {
                score += 650;
            }
            if stem.contains("shipping") {
                score += 400;
            }
            if path
                .with_file_name(format!(
                    "{}_Data",
                    path.file_stem().unwrap_or_default().to_string_lossy()
                ))
                .is_dir()
            {
                score += 350;
            }
            if path.parent().is_some_and(|parent| {
                parent.join("steam_api64.dll").exists() || parent.join("steam_api.dll").exists()
            }) {
                score += 100;
            }
            if stem.contains("launcher") || stem.contains("bootstrap") || stem.contains("patcher") {
                score -= 100;
            }
            #[cfg(unix)]
            if !windows_exe {
                score += 300;
                #[cfg(target_arch = "x86_64")]
                if name.ends_with("x86_64") || name.ends_with("64") || stem.ends_with("64") {
                    score += 50;
                }
            }
            #[cfg(windows)]
            if windows_exe {
                score += 300;
            }
            results.push(Candidate {
                path,
                relative_path,
                score,
            });
        }
    }

    results.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });
    Ok(results)
}

pub fn import_game(library: &mut Library, id: &str, title: &str, root: &Path) -> Result<PathBuf> {
    let _ = crate::archive::normalize_windows_paths(root);
    let candidates = discover_executables(root, title)?;
    let Some(first) = candidates.first() else {
        return Err(
            "no runnable game executable found; use `azdl add` with an explicit path".into(),
        );
    };
    if candidates
        .get(1)
        .is_some_and(|second| first.score - second.score < 150)
    {
        return Err("multiple plausible executables found; use `azdl scan` then `azdl add` with the one you want".into());
    }
    add_game(library, id, title, root, &first.path)?;
    Ok(first.path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "azdl-discovery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn finds_game_and_ignores_install_helpers() {
        let root = scratch();
        fs::write(root.join("setup.exe"), vec![0u8; 4096]).unwrap();
        fs::write(root.join("unins000.exe"), vec![0u8; 4096]).unwrap();
        fs::write(root.join("Star Quest.exe"), b"game").unwrap();
        fs::write(root.join("helper.exe"), vec![0u8; 2048]).unwrap();
        let candidates = discover_executables(&root, "Star Quest").unwrap();
        assert_eq!(candidates[0].relative_path, Path::new("Star Quest.exe"));
        assert_eq!(candidates.len(), 2);
        let mut library = Library::default();
        import_game(&mut library, "star-quest", "Star Quest", &root).unwrap();
        assert!(library.games.contains_key("star-quest"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ambiguous_install_requires_selection() {
        let root = scratch();
        fs::write(root.join("mode-a.exe"), b"one").unwrap();
        fs::write(root.join("mode-b.exe"), b"two").unwrap();
        let mut library = Library::default();
        assert!(import_game(&mut library, "mode", "Unknown", &root).is_err());
        assert!(library.games.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
