use crate::archive::{extract_game_archive, recorded_multipart_download};
use crate::discovery::discover_executables;
use crate::jobs::{JobKind, recorded_download};
use crate::{Game, Library, Result, Runner, add_game, clean_command, validate_id};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct SetupCandidate {
    pub install_dir: PathBuf,
    pub executable: PathBuf,
    pub score: i64,
}

fn child_directories(parent: &Path, roots: &mut Vec<PathBuf>) -> Result<()> {
    if !parent.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !matches!(
                name.as_str(),
                "common files" | "windowsapps" | "windows" | "users"
            ) {
                roots.push(path);
            }
        }
    }
    Ok(())
}

/// Search the usual Windows install locations in a Wine prefix.
pub fn setup_candidates(prefix: &Path, title: &str) -> Result<Vec<SetupCandidate>> {
    let drive = prefix.join("drive_c");
    if !drive.is_dir() {
        return Ok(Vec::new());
    }
    let mut roots = Vec::new();
    for base in ["Program Files", "Program Files (x86)", "Games"] {
        child_directories(&drive.join(base), &mut roots)?;
    }
    child_directories(&drive, &mut roots)?;
    roots.retain(|path| {
        !matches!(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase()
                .as_str(),
            "program files" | "program files (x86)" | "games" | "programdata"
        )
    });
    roots.sort();
    roots.dedup();

    let mut candidates = Vec::new();
    for root in roots {
        for candidate in discover_executables(&root, title)? {
            candidates.push(SetupCandidate {
                install_dir: root.clone(),
                executable: candidate.path,
                score: candidate.score,
            });
        }
    }
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.executable.cmp(&b.executable))
    });
    Ok(candidates)
}

pub fn scan_setup(data_dir: &Path, id: &str, title: &str) -> Result<Vec<SetupCandidate>> {
    validate_id(id)?;
    setup_candidates(&data_dir.join("prefixes").join(id), title)
}

pub enum SetupOutcome {
    Imported(PathBuf),
    NeedsSelection(Vec<SetupCandidate>),
}

pub fn import_setup(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
) -> Result<SetupOutcome> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    let candidates = scan_setup(data_dir, id, title)?;
    let Some(first) = candidates.first() else {
        return Ok(SetupOutcome::NeedsSelection(candidates));
    };
    if first.score < 600
        || candidates
            .get(1)
            .is_some_and(|second| first.score - second.score < 150)
    {
        return Ok(SetupOutcome::NeedsSelection(candidates));
    }
    add_game(library, id, title, &first.install_dir, &first.executable)?;
    library
        .games
        .get_mut(id)
        .ok_or("imported game missing")?
        .runner = Runner::Wine;
    Ok(SetupOutcome::Imported(first.executable.clone()))
}

/// Run a Windows setup executable in a per-game Wine prefix and try to import the result.
pub fn run_setup(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    installer: &Path,
    arguments: &[String],
) -> Result<SetupOutcome> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    if !cfg!(target_os = "linux") {
        return Err("setup run currently supports Linux with Wine".into());
    }
    let installer = fs::canonicalize(installer)?;
    if !installer.is_file() {
        return Err("installer path is not a file".into());
    }
    let extension = installer
        .extension()
        .and_then(|part| part.to_str())
        .unwrap_or_default();
    if !extension.eq_ignore_ascii_case("exe") && !extension.eq_ignore_ascii_case("msi") {
        return Err("installer must be an .exe or .msi file".into());
    }
    #[cfg(windows)]
    {
        let _ = data_dir;
        let mut command = if extension.eq_ignore_ascii_case("msi") {
            let mut cmd = clean_command("msiexec");
            cmd.args(["/i", installer.to_str().ok_or("non-UTF-8 installer path")?]);
            cmd
        } else {
            clean_command(&installer)
        };
        command.args(arguments);
        if let Some(parent) = installer.parent() {
            command.current_dir(parent);
        }
        let status = command.status()?;
        if !status.success() {
            return Err(format!("installer exited with {status}").into());
        }
        Ok(SetupOutcome::NeedsSelection(Vec::new()))
    }
    #[cfg(not(windows))]
    {
        let prefix = data_dir.join("prefixes").join(id);
        fs::create_dir_all(&prefix)?;
        let mut command = clean_command("wine");
        command.env("WINEPREFIX", fs::canonicalize(&prefix)?);
        command.current_dir(installer.parent().ok_or("installer has no parent")?);
        if extension.eq_ignore_ascii_case("msi") {
            command.args(["msiexec", "/i"]);
        }
        let status = command.arg(installer).args(arguments).status()?;
        if !status.success() {
            return Err(format!(
                "installer exited with {status}; Wine prefix and installed files remain at {}",
                prefix.display()
            )
            .into());
        }
        import_setup(library, data_dir, id, title)
    }
}

/// Download an HTTPS setup executable, then run it in the game's Wine prefix.
pub fn fetch_setup(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    url: &str,
    filename: &Path,
    arguments: &[String],
) -> Result<(PathBuf, SetupOutcome)> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    if !cfg!(target_os = "linux") {
        return Err("setup fetch currently supports Linux with Wine".into());
    }
    let downloads = data_dir.join("setup-downloads").join(id);
    fs::create_dir_all(&downloads)?;
    let placeholder = Game {
        id: id.to_string(),
        title: title.to_string(),
        install_dir: fs::canonicalize(&downloads)?,
        executable: PathBuf::new(),
        runner: Runner::Wine,
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        last_played_unix: None,
        origin: None,
        steam_app_id: None,
        launch_options: Vec::new(),
        selected_launch_option_id: None,
        cover_url: None,
        banner_url: None,
    };
    let installer = recorded_download(data_dir, &placeholder, url, filename, JobKind::Setup)?;
    let outcome = run_setup(library, data_dir, id, title, &installer, arguments)?;
    let _ = fs::remove_dir_all(&downloads);
    let job_id = crate::jobs::job_id_for_target(&installer);
    let _ = crate::jobs::clear_job(data_dir, &job_id);
    Ok((installer, outcome))
}

fn installer_score(relative: &Path) -> Option<i64> {
    let name = relative.file_name()?.to_string_lossy().to_ascii_lowercase();
    let lower = relative.to_string_lossy().to_ascii_lowercase();
    if ["redist", "prereq", "_commonredist", "support"]
        .iter()
        .any(|part| lower.contains(part))
    {
        return None;
    }
    if name == "setup.exe" {
        Some(1000)
    } else if name == "install.exe" {
        Some(900)
    } else if name.ends_with(".msi") {
        Some(800)
    } else if (name.contains("setup") || name.contains("installer")) && name.ends_with(".exe") {
        Some(700)
    } else {
        None
    }
}

fn installer_candidates(root: &Path) -> Result<Vec<(i64, PathBuf)>> {
    let mut candidates = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut entries_seen = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            entries_seen += 1;
            if entries_seen > 50_000 {
                return Err("setup archive has too many files to scan".into());
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err("setup archive contains a symbolic link".into());
            }
            if metadata.is_dir() {
                if depth < 10 {
                    stack.push((path, depth + 1));
                }
            } else if metadata.is_file() {
                if let Some(score) = installer_score(path.strip_prefix(root)?) {
                    candidates.push((score - depth as i64 * 10, path));
                }
            } else {
                return Err("setup archive contains an unsupported file".into());
            }
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(candidates)
}

/// Extract a setup archive, run its clear installer match, and import the installed game.
pub fn run_setup_archive(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    archive: &Path,
    arguments: &[String],
) -> Result<SetupOutcome> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    let archive = fs::canonicalize(archive)?;
    if !archive.is_file() {
        return Err("setup archive is not a file".into());
    }
    let unpack_root = data_dir.join("setup-unpack");
    fs::create_dir_all(&unpack_root)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staging = unpack_root.join(format!("{id}-{}-{nonce}", std::process::id()));
    fs::create_dir(&staging)?;
    extract_game_archive(&archive, &staging)
        .map_err(|error| format!("{error}; extracted files remain at {}", staging.display()))?;
    let _ = crate::archive::normalize_windows_paths(&staging);
    let candidates = installer_candidates(&staging)
        .map_err(|error| format!("{error}; extracted files remain at {}", staging.display()))?;
    let Some(first) = candidates.first() else {
        return Err(format!(
            "no setup executable found; extracted files remain at {}",
            staging.display()
        )
        .into());
    };
    if candidates
        .get(1)
        .is_some_and(|second| first.0 - second.0 < 150)
    {
        return Err(format!(
            "multiple setup executables found; inspect extracted files at {}",
            staging.display()
        )
        .into());
    }
    let outcome = run_setup(library, data_dir, id, title, &first.1, arguments)
        .map_err(|error| format!("{error}; extracted files remain at {}", staging.display()))?;
    let _ = fs::remove_dir_all(&staging);
    Ok(outcome)
}

pub fn fetch_setup_archive(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    url: &str,
    filename: &Path,
    arguments: &[String],
) -> Result<(PathBuf, SetupOutcome)> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    if !cfg!(target_os = "linux") {
        return Err("setup archive fetch currently supports Linux with Wine".into());
    }
    let downloads = data_dir.join("setup-downloads").join(id);
    fs::create_dir_all(&downloads)?;
    let placeholder = Game {
        id: id.to_string(),
        title: title.to_string(),
        install_dir: fs::canonicalize(&downloads)?,
        executable: PathBuf::new(),
        runner: Runner::Wine,
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        last_played_unix: None,
        origin: None,
        steam_app_id: None,
        launch_options: Vec::new(),
        selected_launch_option_id: None,
        cover_url: None,
        banner_url: None,
    };
    let archive = recorded_download(data_dir, &placeholder, url, filename, JobKind::SetupArchive)?;
    let outcome = run_setup_archive(library, data_dir, id, title, &archive, arguments)?;
    Ok((archive, outcome))
}

/// Download a complete split RAR setup package before running its installer.
pub fn fetch_setup_archive_parts(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    first_name: &Path,
    urls: &[String],
    arguments: &[String],
) -> Result<(PathBuf, SetupOutcome)> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    if !cfg!(target_os = "linux") {
        return Err("setup archive fetch currently supports Linux with Wine".into());
    }
    let downloads = data_dir.join("setup-downloads").join(id);
    fs::create_dir_all(&downloads)?;
    let placeholder = Game {
        id: id.to_string(),
        title: title.to_string(),
        install_dir: fs::canonicalize(&downloads)?,
        executable: PathBuf::new(),
        runner: Runner::Wine,
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        last_played_unix: None,
        origin: None,
        steam_app_id: None,
        launch_options: Vec::new(),
        selected_launch_option_id: None,
        cover_url: None,
        banner_url: None,
    };
    let archive = recorded_multipart_download(
        data_dir,
        &placeholder,
        first_name,
        urls,
        JobKind::SetupArchive,
    )?;
    let outcome = run_setup_archive(library, data_dir, id, title, &archive, arguments)?;
    Ok((archive, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn imports_game_from_program_files_and_reuses_prefix() {
        let root = std::env::temp_dir().join(format!(
            "azdl-setup-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let game_dir = root.join("prefixes/test/drive_c/Program Files/Star Quest");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("Star Quest.exe"), b"game").unwrap();
        fs::write(game_dir.join("setup.exe"), b"installer").unwrap();
        let mut library = Library::default();
        assert!(matches!(
            import_setup(&mut library, &root, "test", "Star Quest").unwrap(),
            SetupOutcome::Imported(_)
        ));
        assert_eq!(library.games["test"].runner, Runner::Wine);
        assert_eq!(
            library.games["test"].install_dir,
            fs::canonicalize(&game_dir).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ambiguous_setup_needs_selection() {
        let root = std::env::temp_dir().join(format!(
            "azdl-setup-choice-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let game_dir = root.join("prefixes/test/drive_c/Games/Unknown Game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("mode-a.exe"), b"one").unwrap();
        fs::write(game_dir.join("mode-b.exe"), b"two").unwrap();
        let mut library = Library::default();
        assert!(matches!(
            import_setup(&mut library, &root, "test", "Unknown Game").unwrap(),
            SetupOutcome::NeedsSelection(ref choices) if choices.len() == 2
        ));
        assert!(library.games.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unrelated_executable_needs_selection() {
        let root = std::env::temp_dir().join(format!(
            "azdl-setup-unrelated-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let tools_dir = root.join("prefixes/test/drive_c/Program Files/Utility");
        fs::create_dir_all(&tools_dir).unwrap();
        fs::write(tools_dir.join("utility.exe"), b"tool").unwrap();
        let mut library = Library::default();
        assert!(matches!(
            import_setup(&mut library, &root, "test", "Star Quest").unwrap(),
            SetupOutcome::NeedsSelection(ref choices) if choices.len() == 1
        ));
        assert!(library.games.is_empty());
        assert!(scan_setup(&root, "../test", "Star Quest").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selects_setup_exe_over_redistributable() {
        let root = std::env::temp_dir().join(format!(
            "azdl-setup-archive-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        fs::create_dir_all(source.join("redist")).unwrap();
        fs::write(source.join("setup.exe"), b"installer").unwrap();
        fs::write(source.join("redist/vcredist.exe"), b"dependency").unwrap();
        let candidates = installer_candidates(&source).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].1, source.join("setup.exe"));
        fs::remove_dir_all(root).unwrap();
    }
}
