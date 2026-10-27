use crate::discovery::import_game;
use crate::jobs::{JobKind, recorded_download};
use crate::{Game, Library, Result, Runner, clean_command, validate_id};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn tar_command() -> std::process::Command {
    #[cfg(windows)]
    {
        if clean_command("bsdtar").arg("--version").output().is_ok() {
            clean_command("bsdtar")
        } else {
            clean_command("tar")
        }
    }
    #[cfg(not(windows))]
    {
        clean_command("bsdtar")
    }
}

fn archive_listing(archive: &Path, verbose: bool) -> Result<String> {
    let output = tar_command()
        .arg(if verbose { "-tvf" } else { "-tf" })
        .arg(archive)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.stdout.is_empty() && stderr.contains("Too small block encountered") {
            return Ok(String::from_utf8(output.stdout)?);
        }
        return Err(format!(
            "could not list archive ({}): {}",
            output.status,
            stderr.trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Reject path traversal, links, and special files before extraction.
pub fn validate_archive(archive: &Path) -> Result<()> {
    let names = archive_listing(archive, false)?;
    if names.is_empty() {
        return Err("archive is empty".into());
    }
    for name in names.lines() {
        let normalized = name.replace('\\', "/");
        let path = Path::new(&normalized);
        if normalized.starts_with('/')
            || normalized.as_bytes().get(1) == Some(&b':')
            || path.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(format!("unsafe archive path: {name}").into());
        }
    }
    for line in archive_listing(archive, true)?.lines() {
        if !matches!(line.as_bytes().first(), Some(b'-' | b'd')) {
            return Err("archive contains a link or unsupported entry".into());
        }
    }
    Ok(())
}

pub fn extract_checked(archive: &Path, destination: &Path) -> Result<()> {
    extract_checked_with_passphrase(archive, destination, None)
}

/// Extract a game package with the published Online-Fix archive password.
/// Plain archives also extract successfully with a passphrase supplied.
pub fn extract_game_archive(archive: &Path, destination: &Path) -> Result<()> {
    let passphrase =
        std::env::var("AZDL_ARCHIVE_PASSWORD").unwrap_or_else(|_| "online-fix.me".to_string());
    extract_checked_with_passphrase(archive, destination, Some(&passphrase))
}

#[cfg(target_os = "linux")]
fn validate_extracted_tree(destination: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let mut stack = vec![(destination.to_path_buf(), 0usize)];
    let mut entries = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        if depth > 64 {
            return Err("archive contains too many nested folders".into());
        }
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            entries += 1;
            if entries > 1_000_000 {
                return Err("archive contains too many files".into());
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                stack.push((path, depth + 1));
            } else if !metadata.is_file() || metadata.nlink() != 1 {
                return Err("archive contains a link or unsupported entry".into());
            }
        }
    }
    if entries == 0 {
        return Err("archive is empty".into());
    }
    Ok(())
}

fn merge_or_rename_dir(src: &Path, dest: &Path) -> Result<()> {
    if !dest.exists() {
        if fs::rename(src, dest).is_ok() {
            return Ok(());
        }
        fs::create_dir_all(dest)?;
    } else if !dest.is_dir() {
        fs::remove_file(dest)?;
        fs::create_dir_all(dest)?;
    }
    if let Ok(entries) = fs::read_dir(src) {
        for entry in entries.flatten() {
            let src_child = entry.path();
            let dest_child = dest.join(entry.file_name());
            let is_child_dir = entry.metadata().map(|m| m.is_dir()).unwrap_or(false);
            if is_child_dir {
                merge_or_rename_dir(&src_child, &dest_child)?;
            } else {
                if dest_child.is_dir() {
                    let _ = fs::remove_dir_all(&dest_child);
                } else if dest_child.exists() {
                    let _ = fs::remove_file(&dest_child);
                }
                let _ = fs::rename(&src_child, &dest_child);
            }
        }
    }
    let _ = fs::remove_dir(src);
    Ok(())
}

/// Recursively normalize any Windows-style backslashes ('\') in filenames into genuine POSIX directories on Linux.
pub fn normalize_windows_paths(root: &Path) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }

    let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut passes = 0;
    const MAX_PASSES: usize = 16;

    loop {
        passes += 1;
        if passes > MAX_PASSES {
            break;
        }

        let mut entries_to_fix = Vec::new();
        let mut stack = vec![canonical_root.clone()];

        while let Some(dir) = stack.pop() {
            if let Ok(read) = fs::read_dir(&dir) {
                for entry in read.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    let is_dir = entry.metadata().map(|m| m.is_dir()).unwrap_or(false);
                    if name.contains('\\') {
                        entries_to_fix.push((path.clone(), is_dir));
                    }
                    if is_dir {
                        stack.push(path);
                    }
                }
            }
        }

        if entries_to_fix.is_empty() {
            break;
        }

        // Deepest paths first so child entries are normalized before parent directories
        entries_to_fix.sort_by_key(|b| std::cmp::Reverse(b.0.as_os_str().len()));

        let mut changed = false;

        for (src_path, is_dir) in entries_to_fix {
            if !src_path.exists() {
                continue;
            }
            let parent = src_path.parent().unwrap_or(&canonical_root);
            let file_name = src_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let parts: Vec<&str> = file_name.split('\\').filter(|p| !p.is_empty()).collect();
            if parts.is_empty() {
                continue;
            }

            // Path traversal guard: strictly reject and skip any path containing ".." or "."
            if parts.iter().any(|&p| matches!(p, ".." | ".")) {
                continue;
            }

            let mut dest_path = parent.to_path_buf();
            for part in &parts[..parts.len() - 1] {
                dest_path.push(part);
            }
            let _ = fs::create_dir_all(&dest_path);
            dest_path.push(parts[parts.len() - 1]);

            // Guard: ensure destination path does not escape the canonical root directory
            if !dest_path.starts_with(&canonical_root) {
                continue;
            }

            if src_path != dest_path {
                if is_dir {
                    if merge_or_rename_dir(&src_path, &dest_path).is_ok() {
                        changed = true;
                    }
                } else {
                    if dest_path.is_dir() {
                        let _ = fs::remove_dir_all(&dest_path);
                    } else if dest_path.exists() {
                        let _ = fs::remove_file(&dest_path);
                    }
                    if fs::rename(&src_path, &dest_path).is_ok() {
                        changed = true;
                    }
                }
            }
        }

        if !changed {
            break;
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn extract_unlistable_rar(archive: &Path, destination: &Path, passphrase: &str) -> Result<()> {
    if fs::symlink_metadata(destination)?.file_type().is_symlink()
        || fs::read_dir(destination)?.next().is_some()
    {
        return Err("encrypted-header RAR extraction needs an empty real directory".into());
    }
    let destination = fs::canonicalize(destination)?;
    let archive = fs::canonicalize(archive)?;
    let output = clean_command("bwrap")
        .args(["--ro-bind", "/", "/", "--bind"])
        .arg(&destination)
        .arg(&destination)
        .args(["--die-with-parent", "--", "unrar", "x", "-o-", "-idq", "-y"])
        .arg(format!("-p{passphrase}"))
        .arg(&archive)
        .arg(&destination)
        .output()
        .map_err(|error| format!("safe RAR extraction requires bubblewrap: {error}"))?;
    let result = if output.status.success() {
        validate_extracted_tree(&destination)
    } else {
        Err(format!(
            "sandboxed RAR extraction failed ({}): {} {}",
            output.status,
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into())
    };
    if result.is_ok() {
        normalize_windows_paths(&destination)?;
    } else {
        for entry in fs::read_dir(&destination)? {
            let path = entry?.path();
            if fs::symlink_metadata(&path)?.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
    }
    result
}

pub fn extract_checked_with_passphrase(
    archive: &Path,
    destination: &Path,
    passphrase: Option<&str>,
) -> Result<()> {
    if !destination.is_dir() {
        return Err("archive destination is not a directory".into());
    }
    let mut signature = [0u8; 8];
    let length = fs::File::open(archive)?.read(&mut signature)?;
    let rar = length >= 7 && signature.starts_with(b"Rar!\x1a\x07");
    if let Err(error) = validate_archive(archive) {
        if rar && let Some(passphrase) = passphrase {
            #[cfg(target_os = "linux")]
            {
                extract_unlistable_rar(archive, destination, passphrase)?;
                normalize_windows_paths(destination)?;
                return Ok(());
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = passphrase;
                return Err("encrypted-header RAR extraction requires Linux bubblewrap".into());
            }
        }
        return Err(error);
    }
    if rar && let Some(passphrase) = passphrase {
        let output = clean_command("unrar")
            .args(["x", "-o-", "-idq", "-y"])
            .arg(format!("-p{passphrase}"))
            .arg(archive)
            .arg(destination)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "RAR extraction failed ({}): {} {}",
                output.status,
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        normalize_windows_paths(destination)?;
        return Ok(());
    }
    let mut command = tar_command();
    command
        .args(["-x", "-f"])
        .arg(archive)
        .arg("-C")
        .arg(destination);
    if let Some(passphrase) = passphrase {
        command.arg("--passphrase").arg(passphrase);
    }
    #[cfg(unix)]
    command.args([
        "--no-same-owner",
        "--no-same-permissions",
        "--no-xattrs",
        "--no-acls",
    ]);
    #[cfg(not(unix))]
    command.args(["--no-same-owner", "--no-same-permissions"]);
    let status = command.status()?;
    if !status.success() {
        return Err(format!("archive extraction failed ({status})").into());
    }
    normalize_windows_paths(destination)?;
    Ok(())
}

/// Extract a portable game archive into azdl's managed games folder and import it.
/// If executable selection is ambiguous, the files remain installed for `scan`/`add`.
pub fn install_archive(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    archive: &Path,
) -> Result<PathBuf> {
    install_archive_in(
        library,
        data_dir,
        &data_dir.join("games"),
        id,
        title,
        archive,
    )
}

/// Install an archive into a configured library drive while retaining azdl's data directory for jobs.
pub fn install_archive_in(
    library: &mut Library,
    _data_dir: &Path,
    games: &Path,
    id: &str,
    title: &str,
    archive: &Path,
) -> Result<PathBuf> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    let archive = fs::canonicalize(archive)?;
    if !archive.is_file() {
        return Err("archive path is not a file".into());
    }
    fs::create_dir_all(games)?;
    let final_dir = games.join(id);
    if fs::symlink_metadata(&final_dir).is_ok() {
        return Err(format!("install directory already exists: {}", final_dir.display()).into());
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staging = games.join(format!(".{id}.extract-{}-{nonce}", std::process::id()));
    fs::create_dir(&staging)?;

    extract_game_archive(&archive, &staging)
        .map_err(|error| format!("{error}; staging files are at {}", staging.display()))?;
    normalize_windows_paths(&staging)?;
    fs::rename(&staging, &final_dir)?;
    normalize_windows_paths(&final_dir)?;
    import_game(library, id, title, &final_dir)
        .map_err(|error| format!("{error}; extracted files are at {}", final_dir.display()))?;
    Ok(final_dir)
}

/// Download a portable archive, extract it and register its detected executable.
pub fn fetch_archive(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    url: &str,
    archive_name: &Path,
) -> Result<PathBuf> {
    fetch_archive_in(
        library,
        data_dir,
        &data_dir.join("games"),
        id,
        title,
        url,
        archive_name,
    )
}

/// Download and install a portable archive into a configured library drive.
pub fn fetch_archive_in(
    library: &mut Library,
    data_dir: &Path,
    games: &Path,
    id: &str,
    title: &str,
    url: &str,
    archive_name: &Path,
) -> Result<PathBuf> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    let downloads = data_dir.join("downloads").join(id);
    fs::create_dir_all(&downloads)?;
    let game = Game {
        id: id.to_string(),
        title: title.to_string(),
        install_dir: fs::canonicalize(&downloads)?,
        executable: PathBuf::new(),
        runner: Runner::Auto,
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
    let archive = recorded_download(data_dir, &game, url, archive_name, JobKind::Archive)?;
    let result = install_archive_in(library, data_dir, games, id, title, &archive);
    if result.is_ok() {
        let _ = fs::remove_dir_all(&downloads);
        let job_id = crate::jobs::job_id_for_target(&archive);
        let _ = crate::jobs::clear_job(data_dir, &job_id);
    }
    result
}

pub(crate) fn multipart_names(first_name: &str, count: usize) -> Result<Vec<String>> {
    let lower = first_name.to_ascii_lowercase();
    let part = lower
        .rfind(".part")
        .filter(|_| lower.ends_with(".rar"))
        .ok_or("first archive filename must end in .part1.rar or .part01.rar")?;
    let digits = &first_name[part + 5..first_name.len() - 4];
    if digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || digits.parse::<usize>()? != 1
    {
        return Err("first archive filename must identify part 1".into());
    }
    if !(2..=100).contains(&count) {
        return Err("multipart archive needs 2 to 100 HTTPS URLs".into());
    }
    let prefix = &first_name[..part + 5];
    Ok((1..=count)
        .map(|number| format!("{prefix}{number:0width$}.rar", width = digits.len()))
        .collect())
}

/// Record each volume as an individual resumable download in the same folder.
pub(crate) fn recorded_multipart_download(
    data_dir: &Path,
    download_game: &Game,
    first_name: &Path,
    urls: &[String],
    kind: JobKind,
) -> Result<PathBuf> {
    if first_name.components().count() != 1
        || !matches!(first_name.components().next(), Some(Component::Normal(_)))
    {
        return Err("first archive filename must be a single filename".into());
    }
    let first_name = first_name.to_str().ok_or("archive filename is not UTF-8")?;
    let names = multipart_names(first_name, urls.len())?;
    if urls.iter().any(|url| !url.starts_with("https://")) {
        return Err("every archive part URL must use HTTPS".into());
    }
    let mut first_archive = None;
    for (name, url) in names.iter().zip(urls) {
        let downloaded = recorded_download(data_dir, download_game, url, Path::new(name), kind)?;
        if first_archive.is_none() {
            first_archive = Some(downloaded);
        }
    }
    first_archive.ok_or_else(|| "no archive parts provided".into())
}

/// Fetch every part into one directory, then install the complete RAR set.
pub fn fetch_archive_parts(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    first_name: &Path,
    urls: &[String],
) -> Result<PathBuf> {
    fetch_archive_parts_in(
        library,
        data_dir,
        &data_dir.join("games"),
        id,
        title,
        first_name,
        urls,
    )
}

/// Fetch multipart archives into a configured library drive.
pub fn fetch_archive_parts_in(
    library: &mut Library,
    data_dir: &Path,
    games: &Path,
    id: &str,
    title: &str,
    first_name: &Path,
    urls: &[String],
) -> Result<PathBuf> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    let downloads = data_dir.join("downloads").join(id);
    fs::create_dir_all(&downloads)?;
    let game = Game {
        id: id.to_string(),
        title: title.to_string(),
        install_dir: fs::canonicalize(&downloads)?,
        executable: PathBuf::new(),
        runner: Runner::Auto,
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
    let first_archive =
        recorded_multipart_download(data_dir, &game, first_name, urls, JobKind::Archive)?;
    let result = install_archive_in(library, data_dir, games, id, title, &first_archive);
    if result.is_ok() {
        let _ = fs::remove_dir_all(&downloads);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[cfg(target_os = "linux")]
    #[test]
    fn extracts_rar_with_encrypted_headers_in_sandbox() {
        if Command::new("bwrap").arg("--version").output().is_err()
            || Command::new("unrar").arg("-?").output().is_err()
        {
            return;
        }
        let archive =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/encrypted-header-rar4.rar");
        let root = std::env::temp_dir().join(format!(
            "azdl-rar-headers-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        assert!(extract_checked_with_passphrase(&archive, &root, Some("wrong")).is_err());
        assert!(fs::read_dir(&root).unwrap().next().is_none());
        extract_checked_with_passphrase(&archive, &root, Some("12345678")).unwrap();
        assert_eq!(
            fs::read(root.join("foo.txt")).unwrap(),
            b"data of foo.txt\n"
        );
        assert_eq!(
            fs::read(root.join("bar.txt")).unwrap(),
            b"data of bar.txt\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sandboxed_rar_tree_rejects_links() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "azdl-rar-link-check-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        symlink("/tmp/outside", root.join("link")).unwrap();
        assert!(validate_extracted_tree(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multipart_filenames_keep_the_first_parts_number_width() {
        assert_eq!(
            multipart_names("game.part01.rar", 3).unwrap(),
            ["game.part01.rar", "game.part02.rar", "game.part03.rar"]
        );
        assert!(multipart_names("game.part02.rar", 3).is_err());
        assert!(multipart_names("game.rar", 3).is_err());
        assert!(multipart_names("game.part1.rar", 1).is_err());
    }

    #[test]
    fn extracts_complete_local_multipart_rar() {
        if Command::new("bsdtar").arg("--version").output().is_err()
            || Command::new("unrar").arg("-?").output().is_err()
        {
            return;
        }
        let archive = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/test_read_format_rar5_multiarchive_solid.part01.rar");
        let root = std::env::temp_dir().join(format!(
            "azdl-rar-parts-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        extract_game_archive(&archive, &root).unwrap();
        assert_eq!(fs::metadata(root.join("test6.bin")).unwrap().len(), 4096);
        assert_eq!(
            fs::metadata(root.join("elf-Linux-ARMv7-ls")).unwrap().len(),
            90808
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extracts_encrypted_rar_after_validating_members() {
        if Command::new("bsdtar").arg("--version").output().is_err()
            || Command::new("unrar").arg("-?").output().is_err()
        {
            return;
        }
        let archive =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/encrypted-rar4.rar");
        let root = std::env::temp_dir().join(format!(
            "azdl-rar-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        extract_checked_with_passphrase(&archive, &root, Some("12345678")).unwrap();
        assert_eq!(
            fs::read(root.join("foo.txt")).unwrap(),
            b"data of foo.txt\n"
        );
        assert_eq!(
            fs::read(root.join("bar.txt")).unwrap(),
            b"data of bar.txt\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extracts_portable_archive_and_registers_game() {
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-archive-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("Example Game.exe"), b"game").unwrap();
        fs::write(source.join("setup.exe"), b"setup").unwrap();
        let archive = root.join("game.zip");
        let status = Command::new("bsdtar")
            .args(["-a", "-c", "-f"])
            .arg(&archive)
            .arg("-C")
            .arg(&source)
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success());
        let mut library = Library::default();
        let installed =
            install_archive(&mut library, &root, "example", "Example Game", &archive).unwrap();
        assert!(installed.join("Example Game.exe").is_file());
        assert_eq!(
            library.games["example"].executable.file_name().unwrap(),
            "Example Game.exe"
        );
        let other_drive = root.join("other-drive");
        let installed_elsewhere = install_archive_in(
            &mut library,
            &root,
            &other_drive,
            "example-elsewhere",
            "Example Game",
            &archive,
        )
        .unwrap();
        assert!(installed_elsewhere.starts_with(&other_drive));
        assert_eq!(
            library.games["example-elsewhere"].install_dir,
            installed_elsewhere
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installs_password_protected_portable_zip() {
        if Command::new("bsdtar").arg("--version").output().is_err()
            || Command::new("zip").arg("-v").output().is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-encrypted-game-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("Example Game.exe"), b"game").unwrap();
        let archive = root.join("game.zip");
        assert!(
            Command::new("zip")
                .args(["-q", "-P", "online-fix.me"])
                .arg(&archive)
                .arg("Example Game.exe")
                .current_dir(&source)
                .status()
                .unwrap()
                .success()
        );
        let mut library = Library::default();
        let installed =
            install_archive(&mut library, &root, "example", "Example Game", &archive).unwrap();
        assert_eq!(
            fs::read(installed.join("Example Game.exe")).unwrap(),
            b"game"
        );
        assert_eq!(
            library.games["example"].executable.file_name().unwrap(),
            "Example Game.exe"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_archive_path_escape() {
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-archive-escape-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let archive = root.join("escape.tar");
        let mut header = [0u8; 512];
        header[..14].copy_from_slice(b"../escaped.txt");
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(b"00000000005\0");
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header.iter().map(|value| u32::from(*value)).sum();
        header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        let mut data = Vec::from(header);
        let mut body = [0u8; 512];
        body[..5].copy_from_slice(b"evil\n");
        data.extend(body);
        data.extend([0u8; 1024]);
        fs::write(&archive, data).unwrap();
        let mut library = Library::default();
        assert!(install_archive(&mut library, &root, "bad", "Bad", &archive).is_err());
        assert!(!root.join("games/escaped.txt").exists());
        assert!(library.games.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalizes_nested_and_merging_windows_paths() {
        let root = std::env::temp_dir().join(format!(
            "azdl-norm-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        // 1. Flat file with multiple backslashes
        let flat_file = root.join(r"Game\Binaries\Win64\Game.exe");
        fs::write(&flat_file, b"executable content").unwrap();

        // 2. Sibling flat file in same nested destination
        let sibling_file = root.join(r"Game\Binaries\Win64\Game.dll");
        fs::write(&sibling_file, b"dll content").unwrap();

        // 3. Pre-existing destination directory to test directory merging
        let existing_dest = root.join("Game").join("Binaries").join("Win64");
        fs::create_dir_all(&existing_dest).unwrap();
        fs::write(existing_dest.join("PreExisting.txt"), b"initial data").unwrap();

        // 4. Directory containing backslashes in its name
        let backslash_dir = root.join(r"Game\Data");
        fs::create_dir_all(&backslash_dir).unwrap();
        fs::write(backslash_dir.join("config.ini"), b"data=1").unwrap();

        // 5. Malicious path with traversal that must be skipped safely
        let traversal_file = root.join(r"Game\..\escape.txt");
        fs::write(&traversal_file, b"evil").unwrap();

        normalize_windows_paths(&root).unwrap();

        // Verify genuine directories were created
        assert_eq!(
            fs::read(root.join("Game/Binaries/Win64/Game.exe")).unwrap(),
            b"executable content"
        );
        assert_eq!(
            fs::read(root.join("Game/Binaries/Win64/Game.dll")).unwrap(),
            b"dll content"
        );
        assert_eq!(
            fs::read(root.join("Game/Binaries/Win64/PreExisting.txt")).unwrap(),
            b"initial data"
        );
        assert_eq!(
            fs::read(root.join("Game/Data/config.ini")).unwrap(),
            b"data=1"
        );

        // Verify the backslash-named directory was merged and removed
        assert!(!root.join(r"Game\Data").exists());

        // Verify traversal file didn't overwrite root parent
        assert!(!root.parent().unwrap().join("escape.txt").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extracts_archive_with_windows_backslashes_into_real_directories() {
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-tar-windows-paths-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        fn make_tar_entry(name: &str, content: &[u8]) -> Vec<u8> {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..108].copy_from_slice(b"0000644\0");
            header[108..116].copy_from_slice(b"0000000\0");
            header[116..124].copy_from_slice(b"0000000\0");
            header[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
            header[136..148].copy_from_slice(b"00000000000\0");
            header[148..156].fill(b' ');
            header[156] = b'0';
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");
            let checksum: u32 = header.iter().map(|value| u32::from(*value)).sum();
            header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());

            let mut data = Vec::from(header);
            data.extend_from_slice(content);
            let remainder = content.len() % 512;
            if remainder != 0 {
                data.extend(vec![0u8; 512 - remainder]);
            }
            data
        }

        let archive = root.join("game_winpaths.tar");
        let mut tar_bytes = Vec::new();
        tar_bytes.extend(make_tar_entry(
            r"MyGame\Binaries\Win64\MyGame.exe",
            b"my game executable",
        ));
        tar_bytes.extend(make_tar_entry(
            r"MyGame\Engine\Config\Default.ini",
            b"[Engine]\nSetting=1\n",
        ));
        tar_bytes.extend([0u8; 1024]);
        fs::write(&archive, tar_bytes).unwrap();

        let extract_dir = root.join("extracted");
        fs::create_dir_all(&extract_dir).unwrap();

        extract_checked(&archive, &extract_dir).unwrap();

        // Genuine directories must exist
        let exe = extract_dir.join("MyGame/Binaries/Win64/MyGame.exe");
        let ini = extract_dir.join("MyGame/Engine/Config/Default.ini");
        assert!(exe.is_file(), "MyGame.exe must be in POSIX hierarchy");
        assert!(ini.is_file(), "Default.ini must be in POSIX hierarchy");
        assert_eq!(fs::read(&exe).unwrap(), b"my game executable");
        assert_eq!(fs::read(&ini).unwrap(), b"[Engine]\nSetting=1\n");

        // No flat backslash files should remain in extract_dir
        for entry in fs::read_dir(&extract_dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(!name.contains('\\'), "No backslash in filename: {name}");
        }

        // Discovery must successfully find the executable inside the normalized directory
        let candidates = crate::discovery::discover_executables(&extract_dir, "MyGame").unwrap();
        assert!(!candidates.is_empty());
        assert_eq!(candidates[0].path.file_name().unwrap(), "MyGame.exe");

        // install_archive must also work smoothly with Windows-path archives
        let mut library = Library::default();
        let installed =
            install_archive(&mut library, &root, "mygame-imported", "MyGame", &archive).unwrap();
        assert!(installed.join("MyGame/Binaries/Win64/MyGame.exe").is_file());
        assert_eq!(
            library.games["mygame-imported"]
                .executable
                .file_name()
                .unwrap(),
            "MyGame.exe"
        );

        fs::remove_dir_all(root).unwrap();
    }
}
