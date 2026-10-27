use crate::sha256::file_sha256;
use crate::{Game, Result, clean_command, load_library, validate_id};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum JobKind {
    Game,
    Dlc,
    Archive,
    Setup,
    SetupArchive,
    Patch,
    PluginPackage,
    AdditionalFile,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    Queued,
    Running,
    Complete,
    Failed,
    Interrupted,
    Paused,
    Cancelled,
}

/// A transfer stopped by an explicit user request, rather than an error.
#[derive(Debug)]
pub enum TransferStopped {
    /// The transfer was paused and its partial file remains available.
    Paused(String),
    /// The transfer was cancelled, optionally identifying its job.
    Cancelled(Option<String>),
}

impl std::fmt::Display for TransferStopped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Paused(id) => {
                write!(formatter, "download job {id} paused; partial file retained")
            }
            Self::Cancelled(Some(id)) => write!(formatter, "download job {id} cancelled"),
            Self::Cancelled(None) => formatter.write_str("Download cancelled by user"),
        }
    }
}

impl std::error::Error for TransferStopped {}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InstallKind {
    File,
    Archive,
    Setup,
    SetupArchive,
}

impl InstallKind {
    pub fn job_kind(self) -> JobKind {
        match self {
            Self::File => JobKind::Game,
            Self::Archive => JobKind::Archive,
            Self::Setup => JobKind::Setup,
            Self::SetupArchive => JobKind::SetupArchive,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallPlan {
    pub game_id: String,
    pub title: String,
    pub kind: InstallKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InstallState {
    Pending,
    Installing,
    Complete,
    NeedsSelection,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallFinish {
    Complete,
    NeedsSelection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadJob {
    pub id: String,
    pub kind: JobKind,
    pub url: String,
    pub target: PathBuf,
    pub status: JobStatus,
    pub pid: Option<u32>,
    pub error: Option<String>,
    pub updated_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<InstallPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_state: Option<InstallState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Ok(meta) = p.symlink_metadata() {
                if meta.is_file() {
                    total += meta.len();
                } else if meta.is_dir() {
                    total += dir_size(&p);
                }
            }
        }
    }
    total
}

impl DownloadJob {
    pub fn partial_path(&self) -> PathBuf {
        let mut name = self.target.file_name().unwrap_or_default().to_os_string();
        name.push(".part");
        self.target.with_file_name(name)
    }

    pub fn downloaded_bytes(&self) -> u64 {
        if let Some(bytes) = self.progress_bytes {
            return bytes;
        }
        if self.status == JobStatus::Complete {
            if self.target.is_dir() {
                return dir_size(&self.target);
            }
            fs::metadata(&self.target).map(|m| m.len()).unwrap_or(0)
        } else {
            let partial = self.partial_path();
            if partial.is_dir() {
                return dir_size(&partial);
            }
            if let Ok(meta) = fs::metadata(partial) {
                return meta.len();
            }
            0
        }
    }

    pub fn effective_status(&self) -> JobStatus {
        if self.status == JobStatus::Running && !self.pid.is_some_and(process_alive) {
            JobStatus::Interrupted
        } else {
            self.status
        }
    }

    pub fn effective_install_state(&self) -> Option<InstallState> {
        if self.install_state == Some(InstallState::Installing)
            && !self.pid.is_some_and(process_alive)
        {
            Some(InstallState::Failed)
        } else {
            self.install_state
        }
    }
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).is_dir()
}

#[cfg(target_os = "windows")]
fn process_alive(pid: u32) -> bool {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    if pid == 0 {
        return false;
    }
    // SYNCHRONIZE is sufficient to test whether the launcher worker has exited.
    let handle = unsafe { OpenProcess(0x0010_0000, 0, pid) };
    if handle.is_null() {
        return false;
    }
    const WAIT_TIMEOUT: u32 = 0x0000_0102;
    let alive = unsafe { WaitForSingleObject(handle, 0) == WAIT_TIMEOUT };
    unsafe { CloseHandle(handle) };
    alive
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn process_alive(_pid: u32) -> bool {
    true
}

pub(crate) fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn job_id(target: &Path) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in target.to_string_lossy().as_bytes() {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub fn job_id_for_target(target: &Path) -> String {
    job_id(target)
}

fn normalized_sha256(value: Option<&str>) -> Result<Option<String>> {
    value
        .map(|value| {
            if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("SHA-256 must be 64 hexadecimal characters".into());
            }
            Ok(value.to_ascii_lowercase())
        })
        .transpose()
}

fn verified_file(job: &DownloadJob) -> Result<()> {
    let metadata = fs::symlink_metadata(&job.target)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("download target is not a regular file".into());
    }
    let Some(expected) = &job.expected_sha256 else {
        return Ok(());
    };
    let actual = file_sha256(&job.target)?;
    if actual == *expected {
        return Ok(());
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let mut name = job
        .target
        .file_name()
        .ok_or("invalid download target")?
        .to_os_string();
    name.push(format!(".bad-sha256-{}-{nonce}", std::process::id()));
    let quarantine = job.target.with_file_name(name);
    fs::rename(&job.target, &quarantine)?;
    Err(format!(
        "SHA-256 mismatch: expected {expected}, got {actual}; downloaded file kept at {}",
        quarantine.display()
    )
    .into())
}

fn job_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("jobs")
}

fn job_path(data_dir: &Path, id: &str) -> Result<PathBuf> {
    if id.len() != 16 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("job ID must be a 16-digit hexadecimal value".into());
    }
    Ok(job_dir(data_dir).join(format!("{id}.json")))
}

pub(crate) fn save_job(data_dir: &Path, job: &DownloadJob) -> Result<()> {
    let target = job_path(data_dir, &job.id)?;
    fs::create_dir_all(job_dir(data_dir))?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = target.with_extension(format!("json.{}.{nonce}.tmp", std::process::id()));
    let mut file = File::create(&temp)?;
    serde_json::to_writer_pretty(&mut file, job)?;
    file.write_all(b"\n")?;
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(&target)?;
    }
    fs::rename(temp, target)?;
    Ok(())
}

pub fn load_job(data_dir: &Path, id: &str) -> Result<DownloadJob> {
    maybe_load_job(data_dir, id)?.ok_or_else(|| "download job not found".into())
}

pub(crate) fn maybe_load_job(data_dir: &Path, id: &str) -> Result<Option<DownloadJob>> {
    let bytes = match fs::read(job_path(data_dir, id)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let job: DownloadJob = serde_json::from_slice(&bytes)?;
    if job.id != id {
        return Err("job ID does not match its file".into());
    }
    Ok(Some(job))
}

pub fn list_jobs(data_dir: &Path) -> Result<Vec<DownloadJob>> {
    let root = job_dir(data_dir);
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
            let id = path.file_stem().unwrap_or_default().to_string_lossy();
            let mut job = load_job(data_dir, &id)?;
            if job.kind == JobKind::Dlc
                && job.status == JobStatus::Running
                && !dlc_worker_active(data_dir, &id)?
            {
                job.status = JobStatus::Interrupted;
                job.install_state = Some(InstallState::Failed);
            }
            jobs.push(job);
        }
    }
    jobs.sort_by(|a, b| {
        b.updated_unix
            .cmp(&a.updated_unix)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(jobs)
}

/// Whether a live DLC worker is downloading or committing content for this game.
pub fn has_active_dlc_download(data_dir: &Path, game_id: &str) -> Result<bool> {
    Ok(list_jobs(data_dir)?.iter().any(|job| {
        job.kind == JobKind::Dlc
            && job.status == JobStatus::Running
            && job
                .install
                .as_ref()
                .is_some_and(|plan| plan.game_id == game_id)
    }))
}

fn dlc_active_lock_path(data_dir: &Path, id: &str) -> Result<PathBuf> {
    Ok(job_path(data_dir, id)?.with_extension("active-lock"))
}

/// Exclusive lock owned by one in-process DLC worker.
pub struct DlcWorkerLock(File);

impl Drop for DlcWorkerLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Hold this lock for the entire in-process DLC depot download and commit.
pub fn lock_dlc_worker(data_dir: &Path, id: &str) -> Result<DlcWorkerLock> {
    let path = dlc_active_lock_path(data_dir, id)?;
    fs::create_dir_all(job_dir(data_dir))?;
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("DLC worker lock is a symbolic link".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            return Err("DLC download job is already running".into());
        }
        Err(fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    Ok(DlcWorkerLock(file))
}

fn dlc_worker_active(data_dir: &Path, id: &str) -> Result<bool> {
    let path = dlc_active_lock_path(data_dir, id)?;
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("DLC worker lock is not a regular file".into());
    }
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    match file.try_lock() {
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
        Err(fs::TryLockError::WouldBlock) => Ok(true),
        Err(fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

struct JobLock(PathBuf);

impl Drop for JobLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn acquire_lock(data_dir: &Path, id: &str) -> Result<JobLock> {
    let lock = job_path(data_dir, id)?.with_extension("lock");
    fs::create_dir_all(job_dir(data_dir))?;
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
                    return Err("download job is already running".into());
                }
                // A second worker can see the lock between create_new and the PID write.
                if pid.is_none()
                    && fs::metadata(&lock)?
                        .modified()?
                        .elapsed()
                        .is_ok_and(|age| age < Duration::from_secs(5))
                {
                    return Err("download job is starting".into());
                }
                fs::remove_file(&lock)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err("could not acquire download job".into())
}

fn target_for(game: &Game, relative: &Path) -> Result<PathBuf> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("download destination must be within the game directory".into());
    }
    let target = game.install_dir.join(relative);
    let parent = target.parent().ok_or("invalid download destination")?;
    fs::create_dir_all(parent)?;
    let parent = fs::canonicalize(parent)?;
    if !parent.starts_with(&game.install_dir) {
        return Err("download destination leaves game directory".into());
    }
    Ok(parent.join(target.file_name().ok_or("invalid download filename")?))
}

fn pause_path(data_dir: &Path, id: &str) -> Result<PathBuf> {
    Ok(job_path(data_dir, id)?.with_extension("pause"))
}

fn cancel_path(data_dir: &Path, id: &str) -> Result<PathBuf> {
    Ok(job_path(data_dir, id)?.with_extension("cancel"))
}

fn remove_partial(job: &DownloadJob) -> Result<()> {
    let part = job.partial_path();
    match fs::symlink_metadata(&part) {
        Ok(metadata) if metadata.is_file() => fs::remove_file(part).map_err(Into::into),
        Ok(_) => Err("partial download path is not a regular file".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn clear_pause(data_dir: &Path, id: &str) -> Result<()> {
    match fs::remove_file(pause_path(data_dir, id)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn clear_cancel(data_dir: &Path, id: &str) -> Result<()> {
    match fs::remove_file(cancel_path(data_dir, id)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn lock_dlc_phase(data_dir: &Path, id: &str) -> Result<File> {
    let path = job_path(data_dir, id)?.with_extension("phase-lock");
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("DLC job lock is a symbolic link".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.lock()?;
    Ok(file)
}

/// Close the cancellation window before committing a staged DLC payload.
pub fn begin_dlc_commit(data_dir: &Path, id: &str) -> Result<()> {
    let _lock = lock_dlc_phase(data_dir, id)?;
    let mut job = load_job(data_dir, id)?;
    if job.kind != JobKind::Dlc || job.status != JobStatus::Running {
        return Err("DLC download job is no longer running".into());
    }
    if cancel_path(data_dir, id)?.exists() {
        job.status = JobStatus::Cancelled;
        job.pid = None;
        job.updated_unix = now()?;
        save_job(data_dir, &job)?;
        clear_cancel(data_dir, id)?;
        return Err("Download cancelled by user".into());
    }
    job.install_state = Some(InstallState::Installing);
    job.updated_unix = now()?;
    save_job(data_dir, &job)
}

/// Check for cancellation before a DLC worker starts another costly step.
pub(crate) fn check_dlc_cancelled(data_dir: &Path, id: &str) -> Result<()> {
    if cancel_path(data_dir, id)?.exists() {
        return Err("Download cancelled by user".into());
    }
    Ok(())
}

/// Persist a DLC worker's terminal state without racing a cancellation request.
pub(crate) fn finish_dlc_job(data_dir: &Path, id: &str, error: Option<&str>) -> Result<bool> {
    let _lock = lock_dlc_phase(data_dir, id)?;
    let mut job = load_job(data_dir, id)?;
    if job.kind != JobKind::Dlc {
        return Err("job is not a DLC download".into());
    }
    let cancelled = job.status == JobStatus::Cancelled || cancel_path(data_dir, id)?.exists();
    job.status = if cancelled {
        JobStatus::Cancelled
    } else if error.is_some() {
        JobStatus::Failed
    } else {
        JobStatus::Complete
    };
    job.install_state = Some(if error.is_some() || cancelled {
        InstallState::Failed
    } else {
        InstallState::Complete
    });
    job.pid = None;
    job.error = error.map(str::to_owned);
    job.updated_unix = now()?;
    save_job(data_dir, &job)?;
    clear_cancel(data_dir, id)?;
    Ok(cancelled)
}

enum TransferOutcome {
    Complete(PathBuf),
    Paused,
    Cancelled,
}

fn transfer(data_dir: &Path, job: &DownloadJob) -> Result<TransferOutcome> {
    let target = &job.target;
    if target.exists() {
        return Err("destination already exists".into());
    }
    let part = job.partial_path();
    if fs::symlink_metadata(&part).is_ok_and(|metadata| !metadata.is_file()) {
        return Err("partial download path is not a regular file".into());
    }
    let pause = pause_path(data_dir, &job.id)?;
    let cancel = cancel_path(data_dir, &job.id)?;
    if cancel.exists() {
        remove_partial(job)?;
        return Ok(TransferOutcome::Cancelled);
    }
    if pause.exists() {
        return Ok(TransferOutcome::Paused);
    }
    let mut child = clean_command("curl")
        .args([
            "--fail",
            "--location",
            "--retry",
            "3",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--continue-at",
            "-",
        ])
        .arg("--output")
        .arg(&part)
        .arg(&job.url)
        .spawn()?;
    loop {
        if cancel.exists() {
            let _ = child.kill();
            let _ = child.wait();
            remove_partial(job)?;
            return Ok(TransferOutcome::Cancelled);
        }
        if pause.exists() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(TransferOutcome::Paused);
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(format!(
                    "download failed ({status}); partial file kept at {}",
                    part.display()
                )
                .into());
            }
            if cancel.exists() {
                remove_partial(job)?;
                return Ok(TransferOutcome::Cancelled);
            }
            fs::rename(&part, target)?;
            if cancel.exists() {
                fs::remove_file(target)?;
                return Ok(TransferOutcome::Cancelled);
            }
            return Ok(TransferOutcome::Complete(target.clone()));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

pub fn recorded_download(
    data_dir: &Path,
    game: &Game,
    url: &str,
    relative: &Path,
    kind: JobKind,
) -> Result<PathBuf> {
    recorded_download_with_sha256(data_dir, game, url, relative, kind, None)
}

pub fn recorded_download_with_sha256(
    data_dir: &Path,
    game: &Game,
    url: &str,
    relative: &Path,
    kind: JobKind,
    expected_sha256: Option<&str>,
) -> Result<PathBuf> {
    if !url.starts_with("https://") {
        return Err("download URL must use HTTPS".into());
    }
    let expected_sha256 = normalized_sha256(expected_sha256)?;
    let target = target_for(game, relative)?;
    let id = job_id(&target);
    let _lock = acquire_lock(data_dir, &id)?;
    let mut job = match maybe_load_job(data_dir, &id)? {
        Some(mut existing) => {
            if existing.target != target || existing.url != url || existing.kind != kind {
                return Err("download destination belongs to a different job".into());
            }
            if let Some(expected) = expected_sha256 {
                if existing
                    .expected_sha256
                    .as_ref()
                    .is_some_and(|saved| saved != &expected)
                {
                    return Err("download destination has a different SHA-256 expectation".into());
                }
                existing.expected_sha256 = Some(expected);
            }
            if existing.status == JobStatus::Complete && fs::symlink_metadata(&target).is_ok() {
                if let Err(error) = verified_file(&existing) {
                    existing.status = JobStatus::Failed;
                    existing.error = Some(error.to_string());
                    existing.updated_unix = now()?;
                    save_job(data_dir, &existing)?;
                    return Err(error);
                }
                save_job(data_dir, &existing)?;
                return Ok(target);
            }
            if existing.install.is_none() && !game.title.is_empty() {
                existing.install = Some(InstallPlan {
                    game_id: game.id.clone(),
                    title: game.title.clone(),
                    kind: match kind {
                        JobKind::Game => InstallKind::File,
                        JobKind::Archive => InstallKind::Archive,
                        JobKind::Setup => InstallKind::Setup,
                        JobKind::SetupArchive => InstallKind::SetupArchive,
                        _ => InstallKind::Archive,
                    },
                });
            }
            existing
        }
        None => DownloadJob {
            id,
            kind,
            url: url.to_string(),
            target: target.clone(),
            status: JobStatus::Interrupted,
            pid: None,
            error: None,
            updated_unix: now()?,
            expected_sha256,
            install: if !game.title.is_empty() {
                Some(InstallPlan {
                    game_id: game.id.clone(),
                    title: game.title.clone(),
                    kind: match kind {
                        JobKind::Game => InstallKind::File,
                        JobKind::Archive => InstallKind::Archive,
                        JobKind::Setup => InstallKind::Setup,
                        JobKind::SetupArchive => InstallKind::SetupArchive,
                        _ => InstallKind::Archive,
                    },
                })
            } else {
                None
            },
            install_state: None,
            install_error: None,
            progress_bytes: None,
            total_bytes: None,
        },
    };
    job.status = JobStatus::Running;
    job.pid = Some(std::process::id());
    job.error = None;
    job.updated_unix = now()?;
    save_job(data_dir, &job)?;
    let result = transfer(data_dir, &job);
    job.pid = None;
    job.updated_unix = now()?;
    match result {
        Ok(TransferOutcome::Complete(path)) => {
            if let Err(error) = verified_file(&job) {
                job.status = JobStatus::Failed;
                job.error = Some(error.to_string());
                save_job(data_dir, &job)?;
                return Err(error);
            }
            job.status = JobStatus::Complete;
            save_job(data_dir, &job)?;
            Ok(path)
        }
        Ok(TransferOutcome::Paused) => {
            job.status = JobStatus::Paused;
            save_job(data_dir, &job)?;
            Err(TransferStopped::Paused(job.id).into())
        }
        Ok(TransferOutcome::Cancelled) => {
            job.status = JobStatus::Cancelled;
            save_job(data_dir, &job)?;
            clear_cancel(data_dir, &job.id)?;
            Err(TransferStopped::Cancelled(Some(job.id)).into())
        }
        Err(error) => {
            job.status = JobStatus::Failed;
            job.error = Some(error.to_string());
            save_job(data_dir, &job)?;
            Err(format!("download job {} failed: {error}", job.id).into())
        }
    }
}

pub fn retry_job(data_dir: &Path, id: &str) -> Result<PathBuf> {
    resume_job(data_dir, id)
}

fn resume_job(data_dir: &Path, id: &str) -> Result<PathBuf> {
    let job = load_job(data_dir, id)?;
    if job.kind == JobKind::Dlc {
        return Err("DLC depot jobs must be restarted from the game page".into());
    }
    if job.status == JobStatus::Cancelled {
        return Err("cancelled download jobs cannot be resumed".into());
    }
    if job.effective_status() == JobStatus::Running {
        return Err("download job is already running".into());
    }
    if job.target.is_file() {
        return Err("download target already exists".into());
    }
    clear_pause(data_dir, id)?;
    let parent = job.target.parent().ok_or("invalid job target")?;
    let parent = fs::canonicalize(parent)?;
    let name = job.target.file_name().ok_or("invalid job filename")?;
    let game = Game {
        id: "job".into(),
        title: "Download job".into(),
        install_dir: parent,
        executable: PathBuf::new(),
        runner: crate::Runner::Auto,
        arguments: Vec::new(),
        environment: Default::default(),
        last_played_unix: None,
        origin: None,
        steam_app_id: None,
        launch_options: Vec::new(),
        selected_launch_option_id: None,
        cover_url: None,
        banner_url: None,
    };
    let path = recorded_download(data_dir, &game, &job.url, Path::new(name), job.kind)?;
    #[cfg(unix)]
    if job.kind == JobKind::Game {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(&path, permissions)?;
    }
    Ok(path)
}

/// Create a queued HTTPS transfer under azdl's background downloads directory.
pub fn queue_job(data_dir: &Path, url: &str, relative: &Path) -> Result<String> {
    queue_job_with_sha256(data_dir, url, relative, None)
}

pub fn queue_job_with_sha256(
    data_dir: &Path,
    url: &str,
    relative: &Path,
    expected_sha256: Option<&str>,
) -> Result<String> {
    if !url.starts_with("https://") {
        return Err("download URL must use HTTPS".into());
    }
    let expected_sha256 = normalized_sha256(expected_sha256)?;
    let downloads = data_dir.join("background-downloads");
    fs::create_dir_all(&downloads)?;
    let game = Game {
        id: "background".into(),
        title: "Background download".into(),
        install_dir: fs::canonicalize(downloads)?,
        executable: PathBuf::new(),
        runner: crate::Runner::Auto,
        arguments: Vec::new(),
        environment: Default::default(),
        last_played_unix: None,
        origin: None,
        steam_app_id: None,
        launch_options: Vec::new(),
        selected_launch_option_id: None,
        cover_url: None,
        banner_url: None,
    };
    let target = target_for(&game, relative)?;
    let id = job_id(&target);
    let _lock = acquire_lock(data_dir, &id)?;
    if maybe_load_job(data_dir, &id)?.is_some() || target.exists() {
        return Err("download job or target already exists".into());
    }
    save_job(
        data_dir,
        &DownloadJob {
            id: id.clone(),
            kind: JobKind::AdditionalFile,
            url: url.to_string(),
            target,
            status: JobStatus::Queued,
            pid: None,
            error: None,
            updated_unix: now()?,
            expected_sha256,
            install: None,
            install_state: None,
            install_error: None,
            progress_bytes: None,
            total_bytes: None,
        },
    )?;
    Ok(id)
}

/// Queue a download that continues with installation after the transfer completes.
pub fn queue_install_job(
    data_dir: &Path,
    game_id: &str,
    title: &str,
    kind: InstallKind,
    url: &str,
    filename: &Path,
) -> Result<String> {
    queue_install_job_with_sha256(data_dir, game_id, title, kind, url, filename, None)
}

pub fn queue_install_job_with_sha256(
    data_dir: &Path,
    game_id: &str,
    title: &str,
    kind: InstallKind,
    url: &str,
    filename: &Path,
    expected_sha256: Option<&str>,
) -> Result<String> {
    validate_id(game_id)?;
    if title.trim().is_empty() || load_library(data_dir)?.games.contains_key(game_id) {
        return Err("title is empty or game ID already exists".into());
    }
    let destination = match kind {
        InstallKind::File | InstallKind::Archive => data_dir.join("games").join(game_id),
        InstallKind::Setup | InstallKind::SetupArchive => data_dir.join("prefixes").join(game_id),
    };
    if fs::symlink_metadata(destination).is_ok() {
        return Err("install directory already exists for this game ID".into());
    }
    if list_jobs(data_dir)?.iter().any(|job| {
        job.install
            .as_ref()
            .is_some_and(|plan| plan.game_id == game_id)
            && job.status != JobStatus::Cancelled
    }) {
        return Err("an install job already exists for this game ID".into());
    }
    if filename.as_os_str().is_empty()
        || filename
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("download filename must be relative to the game directory".into());
    }
    let id = queue_job_with_sha256(
        data_dir,
        url,
        &Path::new(game_id).join(filename),
        expected_sha256,
    )?;
    let _lock = acquire_lock(data_dir, &id)?;
    let mut job = load_job(data_dir, &id)?;
    job.kind = kind.job_kind();
    job.install = Some(InstallPlan {
        game_id: game_id.into(),
        title: title.into(),
        kind,
    });
    job.install_state = Some(InstallState::Pending);
    save_job(data_dir, &job)?;
    Ok(id)
}

/// Run the saved install plan while holding the job lock. A failed install can be retried
/// without downloading the file again; the downloaded artifact remains in place.
pub fn finish_job_install<F>(data_dir: &Path, id: &str, install: F) -> Result<InstallFinish>
where
    F: FnOnce(&InstallPlan, &Path) -> Result<InstallFinish>,
{
    let _lock = acquire_lock(data_dir, id)?;
    let mut job = load_job(data_dir, id)?;
    let plan = job
        .install
        .clone()
        .ok_or("download job has no install plan")?;
    if job.status != JobStatus::Complete {
        return Err("download has not completed".into());
    }
    if !fs::symlink_metadata(&job.target)
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
    {
        return Err("downloaded file is missing or is not a regular file".into());
    }
    if let Err(error) = verified_file(&job) {
        job.status = JobStatus::Failed;
        job.error = Some(error.to_string());
        job.install_state = Some(InstallState::Failed);
        job.install_error = Some(error.to_string());
        job.updated_unix = now()?;
        save_job(data_dir, &job)?;
        return Err(error);
    }
    match job.effective_install_state() {
        Some(InstallState::Complete) => return Ok(InstallFinish::Complete),
        Some(InstallState::NeedsSelection) => return Ok(InstallFinish::NeedsSelection),
        Some(InstallState::Installing) => return Err("game installation is already running".into()),
        _ => {}
    }
    job.install_state = Some(InstallState::Installing);
    job.install_error = None;
    job.pid = Some(std::process::id());
    job.updated_unix = now()?;
    save_job(data_dir, &job)?;
    let result = install(&plan, &job.target);
    job.pid = None;
    job.updated_unix = now()?;
    match &result {
        Ok(InstallFinish::Complete) => job.install_state = Some(InstallState::Complete),
        Ok(InstallFinish::NeedsSelection) => job.install_state = Some(InstallState::NeedsSelection),
        Err(error) => {
            job.install_state = Some(InstallState::Failed);
            job.install_error = Some(error.to_string());
        }
    }
    save_job(data_dir, &job)?;
    result
}

/// Reconcile an executable chosen after an archive or setup install needed selection.
pub fn mark_install_registered(data_dir: &Path, game_id: &str) -> Result<()> {
    let library = load_library(data_dir)?;
    let Some(game) = library.games.get(game_id) else {
        return Ok(());
    };
    for listed in list_jobs(data_dir)? {
        if listed.install_state != Some(InstallState::NeedsSelection)
            || listed
                .install
                .as_ref()
                .is_none_or(|plan| plan.game_id != game_id)
        {
            continue;
        }
        let plan = listed.install.as_ref().ok_or("install plan missing")?;
        let expected = match plan.kind {
            InstallKind::File | InstallKind::Archive => data_dir.join("games").join(game_id),
            InstallKind::Setup | InstallKind::SetupArchive => {
                data_dir.join("prefixes").join(game_id)
            }
        };
        if plan.title != game.title || !game.install_dir.starts_with(expected) {
            continue;
        }
        let _lock = acquire_lock(data_dir, &listed.id)?;
        let mut job = load_job(data_dir, &listed.id)?;
        if job.install_state == Some(InstallState::NeedsSelection) {
            job.install_state = Some(InstallState::Complete);
            job.install_error = None;
            job.updated_unix = now()?;
            save_job(data_dir, &job)?;
        }
    }
    Ok(())
}

pub fn start_background(data_dir: &Path, id: &str) -> Result<u32> {
    let job = load_job(data_dir, id)?;
    if job.status == JobStatus::Cancelled {
        return Err("cancelled download jobs cannot be resumed".into());
    }
    if job.effective_status() == JobStatus::Running {
        return Err("download job is already running".into());
    }
    if job.effective_install_state() == Some(InstallState::Installing) {
        return Err("game installation is already running".into());
    }
    if job.target.is_file()
        && !(job.status == JobStatus::Complete
            && job.install.is_some()
            && !matches!(
                job.effective_install_state(),
                Some(InstallState::Complete | InstallState::NeedsSelection)
            ))
    {
        return Err("download target already exists".into());
    }
    clear_pause(data_dir, id)?;
    let log = File::create(job_path(data_dir, id)?.with_extension("log"))?;
    let program = std::env::current_exe()?;
    let child = clean_command(program)
        .args(["jobs", "work", id])
        .env("AZDL_DATA_DIR", fs::canonicalize(data_dir)?)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    Ok(child.id())
}

pub fn pause_job(data_dir: &Path, id: &str) -> Result<()> {
    let mut job = load_job(data_dir, id)?;
    if job.kind == JobKind::Dlc {
        return Err(
            "DLC depot jobs cannot be paused; cancel and restart from the game page".into(),
        );
    }
    if matches!(job.status, JobStatus::Complete | JobStatus::Cancelled) {
        return Err("download job is already finished".into());
    }
    fs::write(pause_path(data_dir, id)?, b"pause\n")?;
    if job.effective_status() != JobStatus::Running {
        job.status = JobStatus::Paused;
        job.pid = None;
        job.updated_unix = now()?;
        save_job(data_dir, &job)?;
    }
    Ok(())
}

pub fn cancel_job(data_dir: &Path, id: &str) -> Result<()> {
    let job = load_job(data_dir, id)?;
    if job.kind == JobKind::Dlc {
        if !dlc_worker_active(data_dir, id)? {
            return Err("DLC download was interrupted; restart it from the game page".into());
        }
        let _lock = lock_dlc_phase(data_dir, id)?;
        let current = load_job(data_dir, id)?;
        if current.status != JobStatus::Running {
            return Err("DLC download job is no longer running".into());
        }
        if current.install_state == Some(InstallState::Installing) {
            return Err(
                "DLC files are being committed; cancellation is no longer available".into(),
            );
        }
        fs::write(cancel_path(data_dir, id)?, b"cancel\n")?;
        return Ok(());
    }
    if matches!(job.status, JobStatus::Complete | JobStatus::Cancelled) {
        return Err("download job is already finished".into());
    }
    fs::write(cancel_path(data_dir, id)?, b"cancel\n")?;
    // A running worker owns the lock and observes the marker. Wait briefly for it to
    // finish so a completion race cannot leave a stale cancellation marker behind.
    let _lock = {
        let mut lock = None;
        for _ in 0..30 {
            match acquire_lock(data_dir, id) {
                Ok(acquired) => {
                    lock = Some(acquired);
                    break;
                }
                Err(error)
                    if matches!(
                        error.to_string().as_str(),
                        "download job is already running" | "download job is starting"
                    ) =>
                {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(error),
            }
        }
        let Some(lock) = lock else {
            return Ok(());
        };
        lock
    };
    let mut job = load_job(data_dir, id)?;
    if job.status == JobStatus::Complete {
        clear_cancel(data_dir, id)?;
        return Err("download job completed before cancellation".into());
    }
    remove_partial(&job)?;
    job.status = JobStatus::Cancelled;
    job.pid = None;
    job.error = None;
    job.updated_unix = now()?;
    save_job(data_dir, &job)?;
    clear_pause(data_dir, id)?;
    clear_cancel(data_dir, id)?;
    Ok(())
}

pub fn clear_job(data_dir: &Path, id: &str) -> Result<()> {
    let job = load_job(data_dir, id)?;
    if job.kind == JobKind::Dlc && dlc_worker_active(data_dir, id)? {
        return Err("cannot delete an actively running job".into());
    }
    if job.kind != JobKind::Dlc && job.effective_status() == JobStatus::Running {
        return Err("cannot delete an actively running job".into());
    }
    if job.kind == JobKind::Dlc {
        clear_retained_dlc_staging(data_dir, &job)?;
    }
    let job_file = job_path(data_dir, id)?;
    if job_file.exists() {
        fs::remove_file(job_file)?;
    }
    let _ = fs::remove_file(pause_path(data_dir, id)?);
    let _ = fs::remove_file(cancel_path(data_dir, id)?);
    let _ = fs::remove_file(job_path(data_dir, id)?.with_extension("phase-lock"));
    let _ = fs::remove_file(dlc_active_lock_path(data_dir, id)?);
    Ok(())
}

fn clear_retained_dlc_staging(data_dir: &Path, job: &DownloadJob) -> Result<()> {
    let base = data_dir.join("dlc-downloads");
    let Ok(relative) = job.target.strip_prefix(&base) else {
        return Ok(());
    };
    let parts = relative.components().collect::<Vec<_>>();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| !matches!(part, Component::Normal(_)))
        || parts[2].as_os_str() != "payload"
    {
        return Ok(());
    }
    let game_id = parts[0].as_os_str().to_string_lossy();
    let dlc_id = parts[1].as_os_str().to_string_lossy();
    validate_id(&game_id)?;
    validate_id(&dlc_id)?;
    if job
        .install
        .as_ref()
        .is_none_or(|plan| plan.game_id != game_id.as_ref())
    {
        return Ok(());
    }
    let mut path = base;
    for part in &parts[..2] {
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err("Retained DLC staging contains a symbolic link".into());
        }
        path.push(part.as_os_str());
    }
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => fs::remove_dir_all(path)?,
        Ok(_) => return Err("Retained DLC staging is not a regular directory".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn clear_job_if_present(data_dir: &Path, id: &str) -> Result<()> {
    if maybe_load_job(data_dir, id)?.is_some() {
        clear_job(data_dir, id)?;
    }
    Ok(())
}

pub fn clear_completed_jobs(data_dir: &Path) -> Result<usize> {
    let jobs = list_jobs(data_dir)?;
    let mut count = 0;
    for job in jobs {
        if matches!(
            job.effective_status(),
            JobStatus::Complete | JobStatus::Cancelled
        ) && clear_job(data_dir, &job.id).is_ok()
        {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_failed_dlc_job_discards_only_owned_resume_data() {
        let root = std::env::temp_dir().join(format!("azdl-dlc-retained-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let staging = root.join("dlc-downloads/base/200");
        fs::create_dir_all(staging.join("group-100")).unwrap();
        fs::write(staging.join("group-100/partial.bin"), b"partial").unwrap();
        let base = root.join("base.bin");
        fs::write(&base, b"base").unwrap();
        let target = staging.join("payload");
        let mut job = DownloadJob {
            id: job_id_for_target(&target),
            kind: JobKind::Dlc,
            url: "steam://100/dlc/200".into(),
            target,
            status: JobStatus::Failed,
            pid: None,
            error: Some("network failure".into()),
            updated_unix: now().unwrap(),
            expected_sha256: None,
            install: Some(InstallPlan {
                game_id: "base".into(),
                title: "DLC".into(),
                kind: InstallKind::Archive,
            }),
            install_state: Some(InstallState::Failed),
            install_error: None,
            progress_bytes: Some(7),
            total_bytes: Some(10),
        };
        save_job(&root, &job).unwrap();
        clear_job(&root, &job.id).unwrap();
        assert!(!staging.exists());
        assert_eq!(fs::read(&base).unwrap(), b"base");
        job.target = root.join("payload");
        fs::create_dir_all(&job.target).unwrap();
        fs::write(job.target.join("unowned.bin"), b"keep").unwrap();
        save_job(&root, &job).unwrap();
        clear_job(&root, &job.id).unwrap();
        assert_eq!(fs::read(job.target.join("unowned.bin")).unwrap(), b"keep");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn user_stopped_downloads_keep_typed_outcomes_and_job_status() {
        let root = std::env::temp_dir().join(format!(
            "azdl-stopped-transfer-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install_dir = root.join("game");
        fs::create_dir_all(&install_dir).unwrap();
        fs::create_dir_all(root.join("jobs")).unwrap();
        let game = Game {
            id: "base".into(),
            title: "Base".into(),
            install_dir: fs::canonicalize(&install_dir).unwrap(),
            executable: PathBuf::new(),
            runner: crate::Runner::Auto,
            arguments: Vec::new(),
            environment: Default::default(),
            last_played_unix: None,
            origin: None,
            steam_app_id: None,
            launch_options: Vec::new(),
            selected_launch_option_id: None,
            cover_url: None,
            banner_url: None,
        };
        for (name, status) in [
            ("paused.zip", JobStatus::Paused),
            ("cancelled.zip", JobStatus::Cancelled),
        ] {
            let id = job_id_for_target(&game.install_dir.join(name));
            let marker = if status == JobStatus::Paused {
                pause_path(&root, &id)
            } else {
                cancel_path(&root, &id)
            }
            .unwrap();
            fs::write(marker, b"stop").unwrap();
            let error = recorded_download(
                &root,
                &game,
                "https://example.invalid/archive",
                Path::new(name),
                JobKind::Archive,
            )
            .unwrap_err();
            assert!(matches!(
                (status, error.downcast_ref::<TransferStopped>()),
                (JobStatus::Paused, Some(TransferStopped::Paused(_)))
                    | (
                        JobStatus::Cancelled,
                        Some(TransferStopped::Cancelled(Some(_)))
                    )
            ));
            assert_eq!(load_job(&root, &id).unwrap().status, status);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_worker_liveness_distinguishes_running_and_missing_processes() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(0));
        assert!(!process_alive(u32::MAX));
    }

    #[test]
    fn dlc_cancellation_stops_before_commit_and_rejects_after_commit_begins() {
        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-job-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cancel_id = "00000000000000a1";
        let commit_id = "00000000000000a2";
        for id in [cancel_id, commit_id] {
            let job = DownloadJob {
                id: id.into(),
                kind: JobKind::Dlc,
                url: "steam://100/dlc/200".into(),
                target: root.join(id).join("payload"),
                status: JobStatus::Running,
                pid: Some(std::process::id()),
                error: None,
                updated_unix: now().unwrap(),
                expected_sha256: None,
                install: None,
                install_state: Some(InstallState::Pending),
                install_error: None,
                progress_bytes: Some(0),
                total_bytes: None,
            };
            save_job(&root, &job).unwrap();
        }
        let cancel_worker = lock_dlc_worker(&root, cancel_id).unwrap();
        let commit_worker = lock_dlc_worker(&root, commit_id).unwrap();
        assert!(lock_dlc_worker(&root, commit_id).is_err());
        assert_eq!(
            list_jobs(&root)
                .unwrap()
                .into_iter()
                .find(|job| job.id == commit_id)
                .unwrap()
                .status,
            JobStatus::Running
        );
        assert!(pause_job(&root, cancel_id).is_err());

        cancel_job(&root, cancel_id).unwrap();
        assert!(cancel_path(&root, cancel_id).unwrap().exists());
        assert!(begin_dlc_commit(&root, cancel_id).is_err());
        assert_eq!(
            load_job(&root, cancel_id).unwrap().status,
            JobStatus::Cancelled
        );
        assert!(!cancel_path(&root, cancel_id).unwrap().exists());

        begin_dlc_commit(&root, commit_id).unwrap();
        assert_eq!(
            load_job(&root, commit_id).unwrap().install_state,
            Some(InstallState::Installing)
        );
        assert!(cancel_job(&root, commit_id).is_err());
        assert!(!cancel_path(&root, commit_id).unwrap().exists());
        assert!(clear_job(&root, commit_id).is_err());
        drop(cancel_worker);
        drop(commit_worker);
        assert_eq!(
            list_jobs(&root)
                .unwrap()
                .into_iter()
                .find(|job| job.id == commit_id)
                .unwrap()
                .status,
            JobStatus::Interrupted
        );
        assert!(cancel_job(&root, commit_id).is_err());
        clear_job(&root, commit_id).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dlc_worker_records_early_failure_and_cancellation() {
        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-early-job-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for id in ["00000000000000b1", "00000000000000b2"] {
            save_job(
                &root,
                &DownloadJob {
                    id: id.into(),
                    kind: JobKind::Dlc,
                    url: "steam://100/dlc/200".into(),
                    target: root.join(id).join("payload"),
                    status: JobStatus::Running,
                    pid: Some(std::process::id()),
                    error: None,
                    updated_unix: now().unwrap(),
                    expected_sha256: None,
                    install: None,
                    install_state: Some(InstallState::Pending),
                    install_error: None,
                    progress_bytes: Some(0),
                    total_bytes: None,
                },
            )
            .unwrap();
        }
        let failed_id = "00000000000000b1";
        let cancelled_id = "00000000000000b2";
        let failed_worker = lock_dlc_worker(&root, failed_id).unwrap();
        let cancelled_worker = lock_dlc_worker(&root, cancelled_id).unwrap();

        assert!(!finish_dlc_job(&root, failed_id, Some("metadata unavailable")).unwrap());
        let failed = load_job(&root, failed_id).unwrap();
        assert_eq!(failed.status, JobStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("metadata unavailable"));
        assert!(failed.pid.is_none());

        cancel_job(&root, cancelled_id).unwrap();
        assert!(check_dlc_cancelled(&root, cancelled_id).is_err());
        assert!(finish_dlc_job(&root, cancelled_id, None).unwrap());
        let cancelled = load_job(&root, cancelled_id).unwrap();
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(!cancel_path(&root, cancelled_id).unwrap().exists());
        assert!(cancel_job(&root, cancelled_id).is_err());

        drop(failed_worker);
        drop(cancelled_worker);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_job_verifies_digest_and_quarantines_modified_file() {
        let root = std::env::temp_dir().join(format!(
            "azdl-jobs-hash-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let id = queue_job_with_sha256(
            &root,
            "https://example.org/game.zip",
            Path::new("game.zip"),
            Some(expected),
        )
        .unwrap();
        let mut job = load_job(&root, &id).unwrap();
        fs::write(&job.target, b"abc").unwrap();
        job.status = JobStatus::Complete;
        save_job(&root, &job).unwrap();
        let parent = fs::canonicalize(job.target.parent().unwrap()).unwrap();
        let placeholder = Game {
            id: "background".into(),
            title: "Background".into(),
            install_dir: parent,
            executable: PathBuf::new(),
            runner: crate::Runner::Auto,
            arguments: Vec::new(),
            environment: Default::default(),
            last_played_unix: None,
            origin: None,
            steam_app_id: None,
            launch_options: Vec::new(),
            selected_launch_option_id: None,
            cover_url: None,
            banner_url: None,
        };
        assert_eq!(
            recorded_download(
                &root,
                &placeholder,
                &job.url,
                Path::new("game.zip"),
                JobKind::AdditionalFile,
            )
            .unwrap(),
            job.target
        );
        fs::write(&job.target, b"modified").unwrap();
        let error = recorded_download(
            &root,
            &placeholder,
            &job.url,
            Path::new("game.zip"),
            JobKind::AdditionalFile,
        )
        .unwrap_err();
        assert!(error.to_string().contains("SHA-256 mismatch"));
        assert_eq!(load_job(&root, &id).unwrap().status, JobStatus::Failed);
        assert!(!job.target.exists());
        assert!(
            fs::read_dir(job.target.parent().unwrap())
                .unwrap()
                .filter_map(std::result::Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains("bad-sha256"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deferred_install_checks_digest_again_before_running() {
        let root = std::env::temp_dir().join(format!(
            "azdl-install-hash-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let id = queue_install_job_with_sha256(
            &root,
            "demo",
            "Demo",
            InstallKind::Archive,
            "https://example.org/demo.zip",
            Path::new("demo.zip"),
            Some(expected),
        )
        .unwrap();
        let mut job = load_job(&root, &id).unwrap();
        fs::write(&job.target, b"modified").unwrap();
        job.status = JobStatus::Complete;
        save_job(&root, &job).unwrap();
        assert!(finish_job_install(&root, &id, |_, _| panic!("bad artifact installed")).is_err());
        let failed = load_job(&root, &id).unwrap();
        assert_eq!(failed.status, JobStatus::Failed);
        assert_eq!(failed.install_state, Some(InstallState::Failed));
        assert!(!job.target.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn records_and_lists_interrupted_job_without_touching_network() {
        let root = std::env::temp_dir().join(format!(
            "azdl-jobs-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("game.zip");
        let job = DownloadJob {
            id: job_id(&target),
            kind: JobKind::Archive,
            url: "https://example.org/game.zip".into(),
            target: target.clone(),
            status: JobStatus::Running,
            pid: Some(u32::MAX),
            error: None,
            updated_unix: now().unwrap(),
            expected_sha256: None,
            install: None,
            install_state: None,
            install_error: None,
            progress_bytes: None,
            total_bytes: None,
        };
        save_job(&root, &job).unwrap();
        fs::write(job.partial_path(), b"partial").unwrap();
        let listed = list_jobs(&root).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].effective_status(), JobStatus::Interrupted);
        assert_eq!(listed[0].downloaded_bytes(), 7);
        fs::write(&target, b"complete file").unwrap();
        let mut completed = listed[0].clone();
        completed.status = JobStatus::Complete;
        assert_eq!(completed.downloaded_bytes(), 13);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancels_queued_job_and_discards_partial_file() {
        let root = std::env::temp_dir().join(format!(
            "azdl-jobs-cancel-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let id = queue_job(&root, "https://example.org/game.zip", Path::new("game.zip")).unwrap();
        let job = load_job(&root, &id).unwrap();
        fs::write(job.partial_path(), b"partial").unwrap();
        cancel_job(&root, &id).unwrap();
        let cancelled = load_job(&root, &id).unwrap();
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert_eq!(cancelled.downloaded_bytes(), 0);
        assert!(!cancelled.partial_path().exists());
        assert!(!cancel_path(&root, &id).unwrap().exists());
        assert!(retry_job(&root, &id).is_err());
        assert!(start_background(&root, &id).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_plan_survives_failed_install_and_retries_without_download() {
        let root = std::env::temp_dir().join(format!(
            "azdl-install-job-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let id = queue_install_job(
            &root,
            "demo",
            "Demo",
            InstallKind::Archive,
            "https://example.org/demo.zip",
            Path::new("demo.zip"),
        )
        .unwrap();
        assert!(
            queue_install_job(
                &root,
                "demo",
                "Demo",
                InstallKind::Archive,
                "https://example.org/other.zip",
                Path::new("other.zip"),
            )
            .is_err()
        );
        let mut job = load_job(&root, &id).unwrap();
        assert_eq!(job.install_state, Some(InstallState::Pending));
        fs::write(&job.target, b"invalid archive").unwrap();
        job.status = JobStatus::Complete;
        save_job(&root, &job).unwrap();
        assert!(finish_job_install(&root, &id, |_, _| Err("bad archive".into())).is_err());
        let failed = load_job(&root, &id).unwrap();
        assert_eq!(failed.status, JobStatus::Complete);
        assert_eq!(failed.install_state, Some(InstallState::Failed));
        assert_eq!(failed.install_error.as_deref(), Some("bad archive"));
        assert_eq!(
            finish_job_install(&root, &id, |plan, path| {
                assert_eq!(plan.game_id, "demo");
                assert_eq!(fs::read(path).unwrap(), b"invalid archive");
                Ok(InstallFinish::Complete)
            })
            .unwrap(),
            InstallFinish::Complete
        );
        assert_eq!(
            load_job(&root, &id).unwrap().install_state,
            Some(InstallState::Complete)
        );
        fs::remove_dir_all(root).unwrap();
    }
}
