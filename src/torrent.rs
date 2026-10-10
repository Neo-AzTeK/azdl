use crate::archive::install_archive;
use crate::discovery::{discover_executables, import_game};
use crate::jobs::{InstallState, JobStatus};
use crate::{Library, Result, validate_id};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn validate_magnet(uri: &str) -> Result<()> {
    let lower = uri.to_ascii_lowercase();
    let hash = lower.strip_prefix("magnet:?").and_then(|query| {
        query
            .split('&')
            .find_map(|part| part.strip_prefix("xt=urn:btih:"))
    });
    let valid_hash = hash.is_some_and(|hash| {
        (hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            || (hash.len() == 32
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte)))
    });
    if uri.len() > 8192 || !valid_hash {
        return Err("expected a BitTorrent magnet URI with an info hash".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentJob {
    pub id: String,
    pub title: String,
    pub uri: String,
    pub status: JobStatus,
    pub pid: Option<u32>,
    pub error: Option<String>,
    pub updated_unix: u64,
    #[serde(default)]
    pub download_complete: bool,
    #[serde(default)]
    pub completed_bytes: u64,
    #[serde(default)]
    pub install_state: Option<InstallState>,
}

impl TorrentJob {
    pub fn download_dir(&self, data_dir: &Path) -> PathBuf {
        data_dir.join("downloads").join(&self.id)
    }

    pub fn downloaded_bytes(&self, data_dir: &Path) -> u64 {
        directory_bytes(&self.download_dir(data_dir))
            .unwrap_or(self.completed_bytes)
            .max(self.completed_bytes)
    }

    pub fn effective_status(&self) -> JobStatus {
        if self.status == JobStatus::Running && !self.pid.is_some_and(process_alive) {
            JobStatus::Interrupted
        } else {
            self.status
        }
    }
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).is_dir()
}

#[cfg(not(target_os = "linux"))]
fn process_alive(_pid: u32) -> bool {
    true
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn job_root(data_dir: &Path) -> PathBuf {
    data_dir.join("torrent-jobs")
}

fn job_path(data_dir: &Path, id: &str) -> Result<PathBuf> {
    validate_id(id)?;
    Ok(job_root(data_dir).join(format!("{id}.json")))
}

fn flag_path(data_dir: &Path, id: &str, kind: &str) -> Result<PathBuf> {
    validate_id(id)?;
    Ok(job_root(data_dir).join(format!("{id}.{kind}")))
}

fn save_job(data_dir: &Path, job: &TorrentJob) -> Result<()> {
    let target = job_path(data_dir, &job.id)?;
    fs::create_dir_all(job_root(data_dir))?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = target.with_extension(format!("json.{}.{nonce}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    serde_json::to_writer_pretty(&mut file, job)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(&target)?;
    }
    fs::rename(temp, target)?;
    Ok(())
}

pub fn load_torrent_job(data_dir: &Path, id: &str) -> Result<TorrentJob> {
    let job: TorrentJob = serde_json::from_slice(&fs::read(job_path(data_dir, id)?)?)?;
    if job.id != id {
        return Err("torrent job ID does not match its file".into());
    }
    Ok(job)
}

pub fn list_torrent_jobs(data_dir: &Path) -> Result<Vec<TorrentJob>> {
    let root = job_root(data_dir);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut jobs = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let id = path
                .file_stem()
                .ok_or("invalid torrent job file")?
                .to_string_lossy();
            jobs.push(load_torrent_job(data_dir, &id)?);
        }
    }
    jobs.sort_by(|a, b| {
        b.updated_unix
            .cmp(&a.updated_unix)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(jobs)
}

fn directory_bytes(root: &Path) -> Result<u64> {
    if !root.is_dir() {
        return Ok(0);
    }
    let mut total = 0u64;
    let mut entries = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = stack.pop() {
        if depth > 64 {
            return Err("torrent download tree is too deep".into());
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            entries += 1;
            if entries > 1_000_000 {
                return Err("torrent download has too many files".into());
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() {
                stack.push((entry.path(), depth + 1));
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

struct JobLock(PathBuf);

impl Drop for JobLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn acquire_lock(data_dir: &Path, id: &str) -> Result<JobLock> {
    let lock = flag_path(data_dir, id, "lock")?;
    fs::create_dir_all(job_root(data_dir))?;
    for _ in 0..2 {
        match OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(mut file) => {
                writeln!(file, "{}", std::process::id())?;
                file.sync_all()?;
                return Ok(JobLock(lock));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let pid = fs::read_to_string(&lock)?.trim().parse::<u32>().ok();
                if pid.is_some_and(process_alive) {
                    return Err("torrent job is already running".into());
                }
                if pid.is_none()
                    && fs::metadata(&lock)?
                        .modified()?
                        .elapsed()
                        .is_ok_and(|age| age < Duration::from_secs(5))
                {
                    return Err("torrent job is starting".into());
                }
                fs::remove_file(&lock)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err("could not acquire torrent job".into())
}

fn archive_paths(root: &Path, depth_limit: usize) -> Result<Vec<PathBuf>> {
    let mut archives = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if depth < depth_limit {
                    stack.push((path, depth + 1));
                }
            } else if metadata.is_file() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if [".zip", ".7z", ".rar", ".tar", ".tar.gz", ".tgz", ".tar.xz"]
                    .iter()
                    .any(|ending| name.ends_with(ending))
                {
                    archives.push(path);
                }
            }
        }
    }
    archives.sort();
    Ok(archives)
}

pub fn install_download_tree(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    downloaded: &Path,
) -> Result<PathBuf> {
    validate_id(id)?;
    if library.games.contains_key(id) {
        return Err("game ID already exists".into());
    }
    let downloaded = fs::canonicalize(downloaded)?;
    if !downloaded.is_dir() {
        return Err("download path is not a directory".into());
    }
    let candidates = discover_executables(&downloaded, title)?;
    if !candidates.is_empty() {
        let games = data_dir.join("games");
        fs::create_dir_all(&games)?;
        let final_dir = games.join(id);
        if fs::symlink_metadata(&final_dir).is_ok() {
            return Err(
                format!("install directory already exists: {}", final_dir.display()).into(),
            );
        }
        fs::rename(&downloaded, &final_dir)?;
        import_game(library, id, title, &final_dir)
            .map_err(|error| format!("{error}; downloaded files are at {}", final_dir.display()))?;
        return Ok(final_dir);
    }
    let archives = archive_paths(&downloaded, 8)?;
    if archives.len() == 1 {
        return install_archive(library, data_dir, id, title, &archives[0]);
    }
    Err(format!(
        "downloaded files need an explicit install choice; inspect {}",
        downloaded.display()
    )
    .into())
}

/// Download a magnet through aria2c and install a portable game payload.
pub fn fetch_magnet(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    uri: &str,
) -> Result<PathBuf> {
    let program = env::var_os("AZDL_ARIA2_COMMAND")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("aria2c"));
    fetch_magnet_with_program(library, data_dir, id, title, uri, &program)
}

fn fetch_magnet_with_program(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    uri: &str,
    program: &Path,
) -> Result<PathBuf> {
    validate_id(id)?;
    validate_magnet(uri)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    match load_torrent_job(data_dir, id) {
        Ok(existing) => {
            if existing.title != title || existing.uri != uri {
                return Err("torrent job ID belongs to another game or magnet".into());
            }
            if existing.status == JobStatus::Cancelled {
                return Err("cancelled torrent jobs cannot resume".into());
            }
            if existing.status == JobStatus::Paused {
                return Err("torrent job is paused; use torrent resume".into());
            }
        }
        Err(error) if error.to_string().contains("No such file or directory") => {
            save_job(
                data_dir,
                &TorrentJob {
                    id: id.to_string(),
                    title: title.to_string(),
                    uri: uri.to_string(),
                    status: JobStatus::Queued,
                    pid: None,
                    error: None,
                    updated_unix: now()?,
                    download_complete: false,
                    completed_bytes: 0,
                    install_state: Some(InstallState::Pending),
                },
            )?;
        }
        Err(error) => return Err(error),
    }
    work_magnet_job(library, data_dir, id, program)
}

fn clear_flag(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn work_magnet_job(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    program: &Path,
) -> Result<PathBuf> {
    let _lock = acquire_lock(data_dir, id)?;
    let mut job = load_torrent_job(data_dir, id)?;
    if job.status == JobStatus::Cancelled || job.status == JobStatus::Paused {
        return Err("torrent job is cancelled or paused".into());
    }
    let downloads = job.download_dir(data_dir);
    let pause = flag_path(data_dir, id, "pause")?;
    let cancel = flag_path(data_dir, id, "cancel")?;
    if cancel.exists() {
        fs::remove_dir_all(&downloads).ok();
        job.status = JobStatus::Cancelled;
        job.pid = None;
        job.updated_unix = now()?;
        save_job(data_dir, &job)?;
        clear_flag(&cancel)?;
        return Err("torrent job cancelled".into());
    }
    if pause.exists() {
        job.status = JobStatus::Paused;
        job.pid = None;
        job.updated_unix = now()?;
        save_job(data_dir, &job)?;
        return Err("torrent job paused".into());
    }
    if !job.download_complete {
        fs::create_dir_all(&downloads)?;
        let mut child = match Command::new(program)
            .args([
                "--no-conf",
                "--continue=true",
                "--check-integrity=true",
                "--auto-file-renaming=false",
                "--seed-time=0",
                "--bt-save-metadata=true",
                "--dir",
            ])
            .arg(&downloads)
            .arg(&job.uri)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                job.status = JobStatus::Failed;
                job.error = Some(format!(
                    "cannot start aria2c: {error}; install aria2 or set AZDL_ARIA2_COMMAND"
                ));
                job.updated_unix = now()?;
                save_job(data_dir, &job)?;
                return Err(job.error.clone().unwrap().into());
            }
        };
        job.status = JobStatus::Running;
        job.pid = Some(std::process::id());
        job.error = None;
        job.updated_unix = now()?;
        save_job(data_dir, &job)?;
        loop {
            if cancel.exists() || pause.exists() {
                let cancelled = cancel.exists();
                let _ = child.kill();
                let _ = child.wait();
                if cancelled {
                    fs::remove_dir_all(&downloads)?;
                    job.status = JobStatus::Cancelled;
                    job.error = None;
                    clear_flag(&cancel)?;
                } else {
                    job.status = JobStatus::Paused;
                    job.error = None;
                }
                job.pid = None;
                job.updated_unix = now()?;
                save_job(data_dir, &job)?;
                return Err(if cancelled {
                    "torrent job cancelled".into()
                } else {
                    "torrent job paused".into()
                });
            }
            if let Some(status) = child.try_wait()? {
                job.pid = None;
                job.updated_unix = now()?;
                if !status.success() {
                    job.status = JobStatus::Failed;
                    job.error = Some(format!(
                        "magnet download failed ({status}); partial files remain at {}",
                        downloads.display()
                    ));
                    save_job(data_dir, &job)?;
                    return Err(job.error.clone().unwrap().into());
                }
                job.status = JobStatus::Complete;
                job.download_complete = true;
                job.completed_bytes = directory_bytes(&downloads)?;
                job.install_state = Some(InstallState::Pending);
                save_job(data_dir, &job)?;
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
    job.install_state = Some(InstallState::Installing);
    job.updated_unix = now()?;
    save_job(data_dir, &job)?;
    match install_download_tree(library, data_dir, id, &job.title, &downloads) {
        Ok(path) => {
            crate::save_library(data_dir, library)?;
            job.install_state = Some(InstallState::Complete);
            job.error = None;
            job.updated_unix = now()?;
            save_job(data_dir, &job)?;
            Ok(path)
        }
        Err(error) => {
            job.install_state = Some(
                if downloads.is_dir() || data_dir.join("games").join(id).is_dir() {
                    InstallState::NeedsSelection
                } else {
                    InstallState::Failed
                },
            );
            job.error = Some(error.to_string());
            job.updated_unix = now()?;
            save_job(data_dir, &job)?;
            Err(error)
        }
    }
}

pub fn pause_magnet(data_dir: &Path, id: &str) -> Result<()> {
    let mut job = load_torrent_job(data_dir, id)?;
    if matches!(job.status, JobStatus::Complete | JobStatus::Cancelled) {
        return Err("completed or cancelled torrent jobs cannot pause".into());
    }
    fs::write(flag_path(data_dir, id, "pause")?, b"")?;
    if job.effective_status() != JobStatus::Running {
        job.status = JobStatus::Paused;
        job.pid = None;
        job.updated_unix = now()?;
        save_job(data_dir, &job)?;
    }
    Ok(())
}

pub fn cancel_magnet(data_dir: &Path, id: &str) -> Result<()> {
    let mut job = load_torrent_job(data_dir, id)?;
    if matches!(job.status, JobStatus::Complete | JobStatus::Cancelled) {
        return Err("completed or cancelled torrent jobs cannot cancel".into());
    }
    fs::write(flag_path(data_dir, id, "cancel")?, b"")?;
    if job.effective_status() != JobStatus::Running {
        let downloads = job.download_dir(data_dir);
        if downloads.is_dir() {
            fs::remove_dir_all(downloads)?;
        }
        job.status = JobStatus::Cancelled;
        job.pid = None;
        job.updated_unix = now()?;
        save_job(data_dir, &job)?;
        clear_flag(&flag_path(data_dir, id, "pause")?)?;
        clear_flag(&flag_path(data_dir, id, "cancel")?)?;
    }
    Ok(())
}

pub fn resume_magnet(library: &mut Library, data_dir: &Path, id: &str) -> Result<PathBuf> {
    let mut job = load_torrent_job(data_dir, id)?;
    if job.status == JobStatus::Cancelled || job.install_state == Some(InstallState::Complete) {
        return Err("torrent job cannot resume".into());
    }
    clear_flag(&flag_path(data_dir, id, "pause")?)?;
    job.status = if job.download_complete {
        JobStatus::Complete
    } else {
        JobStatus::Queued
    };
    job.updated_unix = now()?;
    save_job(data_dir, &job)?;
    let program = env::var_os("AZDL_ARIA2_COMMAND")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("aria2c"));
    work_magnet_job(library, data_dir, id, &program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn validates_magnet_and_imports_portable_tree() {
        assert!(validate_magnet("https://example.org/game").is_err());
        assert!(validate_magnet("magnet:?dn=missing-hash").is_err());
        assert!(
            validate_magnet("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Game")
                .is_ok()
        );
        let root = env::temp_dir().join(format!(
            "azdl-torrent-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let downloaded = root.join("downloads/test");
        fs::create_dir_all(&downloaded).unwrap();
        fs::write(downloaded.join("Test Game.exe"), b"game").unwrap();
        let mut library = Library::default();
        let installed =
            install_download_tree(&mut library, &root, "test", "Test Game", &downloaded).unwrap();
        assert!(installed.join("Test Game.exe").exists());
        assert!(library.games.contains_key("test"));
        fs::remove_dir_all(root).unwrap();
    }
}
