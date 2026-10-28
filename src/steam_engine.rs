//! Steam CDN depot transfers through the pure-Rust `steamroom` pipeline.
//!
//! Manifests and depot keys always come from the provider; Steam is only used
//! anonymously to list CDN servers and to obtain CDN auth tokens on demand.

use crate::Result;
use crate::jobs::{JobStatus, clear_cancel, load_job, now, save_job};
use crate::steam_depot::DepotInfo;
use bytes::Bytes;
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use steamroom::cdn::{CdnClient, CdnServer, CdnServerPool};
use steamroom::client::{LoggedIn, SteamClient};
use steamroom::depot::manifest::DepotManifest;
use steamroom::depot::{AppId, CellId, ChunkId, DepotId, DepotKey};
use steamroom::enums::DepotFileFlags;
use steamroom_client::download::{BoxError, ChunkFetcher, DepotJob};
use steamroom_client::event::DownloadEvent;
use steamroom_client::login::LoginBuilder;
use tokio::sync::{Mutex, mpsc};

const MAX_CONCURRENT_CHUNKS: usize = 16;
const MAX_CDN_SERVERS: u32 = 40;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);
/// Bookkeeping directory steamroom creates for atomic staging; never part of a depot.
const STEAMROOM_STATE_DIR: &str = ".DepotDownloader";

pub(crate) struct DownloadProgressSpan {
    pub(crate) completed_bytes: u64,
    pub(crate) overall_bytes: Option<u64>,
}

/// One validated depot revision ready for transfer.
struct DepotPlan {
    depot_id: DepotId,
    key: DepotKey,
    manifest: DepotManifest,
}

/// Download every depot into `target_dir`, recording progress on the job and
/// honouring its pause and cancel markers.
pub(crate) fn download_steam_depots_with_progress(
    target_dir: &Path,
    app_id: u32,
    depots: &[DepotInfo],
    data_dir: &Path,
    job_id: &str,
    span: DownloadProgressSpan,
) -> Result<()> {
    let plans = depots.iter().map(plan_depot).collect::<Result<Vec<_>>>()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let fetcher = match runtime.block_on(CdnFetcher::connect(AppId(app_id))) {
        Ok(fetcher) => Arc::new(fetcher),
        Err(error) => {
            return Err(fail_job(
                data_dir,
                job_id,
                format!("Steam CDN unavailable: {error}"),
            ));
        }
    };
    let result = transfer(
        &runtime, &plans, target_dir, fetcher, data_dir, job_id, &span,
    );
    runtime.shutdown_background();
    result
}

/// Validate a provider manifest with azdl's own parser, then load it for steamroom.
fn plan_depot(depot: &DepotInfo) -> Result<DepotPlan> {
    crate::steam_manifest::read_manifest_with_key(
        &depot.manifest_path,
        depot.depot_id,
        depot.manifest_id,
        &depot.depot_key,
    )?;
    let key = parse_depot_key(&depot.depot_key)?;
    let mut manifest = DepotManifest::parse(&fs::read(&depot.manifest_path)?)
        .map_err(|error| format!("Invalid depot manifest {}: {error}", depot.depot_id))?;
    manifest
        .decrypt_filenames(&key)
        .map_err(|error| format!("Cannot decrypt depot manifest {}: {error}", depot.depot_id))?;
    check_manifest_entries(&manifest)?;
    Ok(DepotPlan {
        depot_id: DepotId(depot.depot_id),
        key,
        manifest,
    })
}

fn parse_depot_key(hex: &str) -> Result<DepotKey> {
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Depot key must contain 64 hexadecimal characters".into());
    }
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)?;
    }
    Ok(DepotKey(bytes))
}

/// Reject links and paths that could leave the target directory before any write.
fn check_manifest_entries(manifest: &DepotManifest) -> Result<()> {
    for file in &manifest.files {
        if DepotFileFlags::from_bits_retain(file.flags).is_symlink() || file.link_target.is_some() {
            return Err("Depot manifest contains a symbolic link".into());
        }
        let path = file.normalized_path();
        let mut components = Path::new(&path).components().peekable();
        if components.peek().is_none()
            || !components.all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(format!("Depot manifest contains an unsafe path: {path}").into());
        }
    }
    Ok(())
}

fn transfer<F: ChunkFetcher + 'static>(
    runtime: &tokio::runtime::Runtime,
    plans: &[DepotPlan],
    target_dir: &Path,
    fetcher: Arc<F>,
    data_dir: &Path,
    job_id: &str,
    span: &DownloadProgressSpan,
) -> Result<()> {
    let total_bytes = plans
        .iter()
        .flat_map(|plan| &plan.manifest.files)
        .map(|file| file.size)
        .sum::<u64>();
    let progress = Progress {
        data_dir,
        job_id,
        written: AtomicU64::new(0),
        completed_bytes: span.completed_bytes,
        overall_bytes: span.overall_bytes.or(Some(total_bytes)),
    };
    let (sender, mut events) = mpsc::unbounded_channel();
    let outcome = runtime.block_on(async {
        let downloads = async {
            for plan in plans {
                let job = DepotJob::builder()
                    .depot_id(plan.depot_id)
                    .depot_key(plan.key.clone())
                    .install_dir(target_dir.to_path_buf())
                    .max_downloads(MAX_CONCURRENT_CHUNKS)
                    .verify(true)
                    .non_atomic(true)
                    .event_sender(sender.clone())
                    .build()
                    .map_err(|error| error.to_string())?;
                job.download(&plan.manifest, Arc::clone(&fetcher))
                    .await
                    .map_err(|error| format!("depot {}: {error}", plan.depot_id))?;
            }
            Ok::<_, String>(())
        };
        let watch = async {
            let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
            loop {
                ticker.tick().await;
                progress.drain(&mut events);
                progress.record();
                if progress.stop_requested() {
                    return;
                }
            }
        };
        tokio::select! {
            result = downloads => Some(result),
            () = watch => None,
        }
    });
    progress.drain(&mut events);
    progress.record();
    remove_steamroom_state(target_dir);

    let pause_marker = data_dir.join("jobs").join(format!("{job_id}.pause"));
    let cancel_marker = data_dir.join("jobs").join(format!("{job_id}.cancel"));
    if pause_marker.exists() && !cancel_marker.exists() {
        if let Ok(mut job) = load_job(data_dir, job_id) {
            job.status = JobStatus::Paused;
            job.pid = None;
            job.install_state = Some(crate::jobs::InstallState::Pending);
            save_job(data_dir, &job)?;
        }
        crate::jobs::clear_pause(data_dir, job_id)?;
        return Err(crate::jobs::TransferStopped::Paused(job_id.into()).into());
    }
    if cancel_marker.exists() {
        if let Ok(mut job) = load_job(data_dir, job_id) {
            job.status = JobStatus::Cancelled;
            let _ = save_job(data_dir, &job);
        }
        let _ = clear_cancel(data_dir, job_id);
        return Err(crate::jobs::TransferStopped::Cancelled(Some(job_id.into())).into());
    }
    match outcome {
        Some(Ok(())) => Ok(()),
        Some(Err(error)) => Err(fail_job(
            data_dir,
            job_id,
            format!("Depot download error: {error}"),
        )),
        None => Err("Depot download stopped without a pause or cancel marker".into()),
    }
}

fn fail_job(data_dir: &Path, job_id: &str, message: String) -> Box<dyn std::error::Error> {
    if let Ok(mut job) = load_job(data_dir, job_id) {
        job.status = JobStatus::Failed;
        job.error = Some(message.clone());
        let _ = save_job(data_dir, &job);
    }
    message.into()
}

fn remove_steamroom_state(target_dir: &Path) {
    let state = target_dir.join(STEAMROOM_STATE_DIR);
    if fs::symlink_metadata(&state).is_ok_and(|metadata| metadata.is_dir()) {
        let _ = fs::remove_dir_all(state);
    }
}

struct Progress<'a> {
    data_dir: &'a Path,
    job_id: &'a str,
    written: AtomicU64,
    completed_bytes: u64,
    overall_bytes: Option<u64>,
}

impl Progress<'_> {
    fn drain(&self, events: &mut mpsc::UnboundedReceiver<DownloadEvent>) {
        while let Ok(event) = events.try_recv() {
            if let DownloadEvent::ChunkCompleted { bytes } = event {
                self.written.fetch_add(bytes, Ordering::Relaxed);
            }
        }
    }

    /// Progress never moves backwards, so resumed transfers keep earlier totals.
    fn record(&self) {
        if let Ok(mut job) = load_job(self.data_dir, self.job_id) {
            job.updated_unix = now().unwrap_or_default();
            let bytes = self
                .completed_bytes
                .saturating_add(self.written.load(Ordering::Relaxed));
            job.progress_bytes = Some(job.progress_bytes.unwrap_or(0).max(bytes));
            job.total_bytes = self.overall_bytes;
            let _ = save_job(self.data_dir, &job);
        }
    }

    fn stop_requested(&self) -> bool {
        let jobs = self.data_dir.join("jobs");
        jobs.join(format!("{}.cancel", self.job_id)).exists()
            || jobs.join(format!("{}.pause", self.job_id)).exists()
    }
}

/// CDN chunk source with per-host auth tokens fetched from an anonymous Steam session.
struct CdnFetcher {
    cdn: CdnClient,
    pool: CdnServerPool,
    app_id: AppId,
    session: Mutex<Option<SteamClient<LoggedIn>>>,
    tokens: Mutex<HashMap<String, String>>,
}

impl CdnFetcher {
    async fn connect(app_id: AppId) -> std::result::Result<Self, BoxError> {
        let session = LoginBuilder::new().anonymous().login().await?;
        let servers = session
            .get_cdn_servers(CellId(0), Some(MAX_CDN_SERVERS))
            .await?;
        if servers.is_empty() {
            return Err("Steam returned no CDN servers".into());
        }
        Ok(Self {
            cdn: CdnClient::new()?,
            pool: CdnServerPool::new(servers),
            app_id,
            session: Mutex::new(Some(session)),
            tokens: Mutex::new(HashMap::new()),
        })
    }

    /// Request a fresh token, reconnecting once if the CM session has dropped.
    async fn refresh_token(&self, depot_id: DepotId, host: &str) -> Option<String> {
        let mut session = self.session.lock().await;
        for _ in 0..2 {
            if session.is_none() {
                *session = LoginBuilder::new().anonymous().login().await.ok();
            }
            let client = session.as_ref()?;
            match client.get_cdn_auth_token(self.app_id, depot_id, host).await {
                Ok(token) => {
                    let token = token.token.filter(|token| !token.is_empty())?;
                    self.tokens
                        .lock()
                        .await
                        .insert(host.to_owned(), token.clone());
                    return Some(token);
                }
                Err(_) => *session = None,
            }
        }
        None
    }

    async fn fetch_from(
        &self,
        server: &CdnServer,
        depot_id: DepotId,
        chunk_id: &ChunkId,
    ) -> steamroom::Result<Bytes> {
        let token = self.tokens.lock().await.get(&server.host).cloned();
        let result = self
            .cdn
            .download_chunk(server, depot_id, chunk_id, token.as_deref())
            .await;
        if let Err(steamroom::Error::CdnStatus { status, .. }) = &result
            && matches!(status.as_u16(), 401 | 403)
            && let Some(token) = self.refresh_token(depot_id, &server.host).await
        {
            return self
                .cdn
                .download_chunk(server, depot_id, chunk_id, Some(&token))
                .await;
        }
        result
    }
}

impl ChunkFetcher for CdnFetcher {
    async fn fetch_chunk(
        &self,
        depot_id: DepotId,
        chunk_id: &ChunkId,
    ) -> std::result::Result<Bytes, BoxError> {
        let (server, wait) = self.pool.pick();
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        match self.fetch_from(server, depot_id, chunk_id).await {
            Ok(bytes) => {
                self.pool.report_success(server);
                Ok(bytes)
            }
            Err(error) => {
                let retry_after = match &error {
                    steamroom::Error::CdnStatus { retry_after, .. } => {
                        retry_after.map(Duration::from_secs)
                    }
                    _ => None,
                };
                self.pool.report_failure(server, retry_after);
                Err(Box::new(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{DownloadJob, InstallState, JobKind};
    use steamroom::depot::manifest::{ManifestChunk, ManifestFile};

    /// Chunk source that never answers, so only a stop marker can end the transfer.
    struct StalledFetcher;

    impl ChunkFetcher for StalledFetcher {
        async fn fetch_chunk(
            &self,
            _: DepotId,
            _: &ChunkId,
        ) -> std::result::Result<Bytes, BoxError> {
            std::future::pending().await
        }
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "azdl-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("jobs")).unwrap();
        root
    }

    fn running_job(root: &Path, id: &str) {
        save_job(
            root,
            &DownloadJob {
                id: id.into(),
                kind: JobKind::Dlc,
                url: "steam://480/dlc/381".into(),
                target: root.join("payload"),
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

    fn manifest_file(name: &str, flags: u32) -> ManifestFile {
        let mut file = ManifestFile::new(name.into(), 1024);
        file.flags = flags;
        file.chunks = vec![ManifestChunk::new(ChunkId([7; 20]), 0, 1024)];
        file
    }

    fn plan(files: Vec<ManifestFile>) -> DepotPlan {
        DepotPlan {
            depot_id: DepotId(381),
            key: DepotKey([1; 32]),
            manifest: DepotManifest::new(files),
        }
    }

    fn run_stalled(root: &Path, job_id: &str, marker: &str) -> Box<dyn std::error::Error> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        fs::write(root.join("jobs").join(format!("{job_id}.{marker}")), marker).unwrap();
        let error = transfer(
            &runtime,
            &[plan(vec![manifest_file("bin/game.dll", 0)])],
            &root.join("payload"),
            Arc::new(StalledFetcher),
            root,
            job_id,
            &DownloadProgressSpan {
                completed_bytes: 100,
                overall_bytes: Some(4096),
            },
        )
        .unwrap_err();
        runtime.shutdown_background();
        error
    }

    #[test]
    fn cancel_marker_stops_a_stalled_transfer() {
        let root = temp_root("engine-cancel");
        let job_id = "0000000000000001";
        running_job(&root, job_id);
        let error = run_stalled(&root, job_id, "cancel");
        assert!(matches!(
            error.downcast_ref::<crate::jobs::TransferStopped>(),
            Some(crate::jobs::TransferStopped::Cancelled(Some(id))) if id == job_id
        ));
        let job = load_job(&root, job_id).unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
        assert_eq!(job.progress_bytes, Some(100));
        assert_eq!(job.total_bytes, Some(4096));
        assert!(!root.join("payload").join(STEAMROOM_STATE_DIR).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pause_marker_keeps_the_job_resumable() {
        let root = temp_root("engine-pause");
        let job_id = "0000000000000002";
        running_job(&root, job_id);
        let error = run_stalled(&root, job_id, "pause");
        assert!(matches!(
            error.downcast_ref::<crate::jobs::TransferStopped>(),
            Some(crate::jobs::TransferStopped::Paused(id)) if id == job_id
        ));
        let job = load_job(&root, job_id).unwrap();
        assert_eq!(job.status, JobStatus::Paused);
        assert_eq!(job.pid, None);
        assert!(!root.join("jobs").join(format!("{job_id}.pause")).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_entries_reject_links_and_escaping_paths() {
        let symlink_flag = DepotFileFlags::SYMLINK.bits();
        for (name, flags) in [
            ("bin/link", symlink_flag),
            ("..\\outside.dll", 0),
            ("/etc/passwd", 0),
            ("", 0),
        ] {
            let manifest = DepotManifest::new(vec![manifest_file(name, flags)]);
            assert!(check_manifest_entries(&manifest).is_err(), "{name}");
        }
        let mut linked = manifest_file("bin/linked", 0);
        linked.link_target = Some("../../etc".into());
        assert!(check_manifest_entries(&DepotManifest::new(vec![linked])).is_err());
        let safe = DepotManifest::new(vec![manifest_file("bin\\game.dll", 0)]);
        check_manifest_entries(&safe).unwrap();
    }

    #[test]
    fn depot_keys_must_be_full_hex() {
        assert_eq!(parse_depot_key(&"0a".repeat(32)).unwrap().0, [10; 32]);
        assert!(parse_depot_key("0a").is_err());
        assert!(parse_depot_key(&"zz".repeat(32)).is_err());
    }
}
