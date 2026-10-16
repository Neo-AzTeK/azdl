use crate::archive::{extract_checked, extract_checked_with_passphrase};
use crate::catalog::{CatalogGame, InstallOutcome, save_pending_setup_origin, stamp_origin};
use crate::jobs::{
    DownloadJob, InstallKind, InstallPlan, InstallState, JobKind, JobStatus, clear_cancel,
    job_id_for_target, load_job, now, save_job,
};
use crate::steam::{import_steam, scan_steam, steam_root};
use crate::steam_metadata::{ResolvedDepotSource, SteamAppMetadata};
use crate::{Game, Library, Result, clean_command, discovery, validate_id};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString, c_char, c_double, c_int, c_void};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[repr(C)]
pub struct DepotkitDepot {
    pub depot_id: u32,
    pub _pad: u32,
    pub manifest_path: *const c_char,
    pub depot_key: *const c_char,
}

#[repr(C)]
pub struct DepotkitRequest {
    pub unused: u64,
    pub depots: *const DepotkitDepot,
    pub depot_count: usize,
    pub target_dir: *const c_char,
    pub max_workers: u32,
    pub verify_checksums: u32,
    pub app_id: u32,
    pub cell_id: u32,
}

#[repr(C)]
pub struct DepotkitProgress {
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub rate_bytes_sec: c_double,
    pub total_written: u64,
    pub unused: u64,
    pub phase: u32,
    pub _pad: u32,
    pub detail: *const c_char,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepotInfo {
    pub depot_id: u32,
    pub manifest_id: u64,
    pub manifest_path: PathBuf,
    pub depot_key: String,
}

fn validate_manifest_archive(path: &Path, app_id: u32) -> Result<()> {
    let mut file = fs::File::open(path)?;
    let mut signature = [0u8; 4];
    if file.read_exact(&mut signature).is_err()
        || !matches!(
            signature,
            [b'P', b'K', 3, 4] | [b'P', b'K', 5, 6] | [b'P', b'K', 7, 8]
        )
    {
        return Err(format!(
            "No usable download manifest is available for Steam App ID {app_id}: the selected plugin's provider did not return a ZIP manifest"
        )
        .into());
    }
    Ok(())
}

fn fetch_provider_bundle(root: &Path, app_id: u32) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    let archive = root.join(format!("{app_id}.zip"));
    if fs::symlink_metadata(&archive).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("Manifest archive path is a symbolic link".into());
    }
    let url = format!("https://ryuu.badkiko.ru/v1/download/{app_id}");
    if !archive.is_file() || archive.metadata()?.len() == 0 {
        let output = clean_command("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--max-time",
                "30",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--write-out",
                "%{http_code}",
                "--output",
            ])
            .arg(&archive)
            .arg(&url)
            .output()?;
        if !output.status.success() {
            let _ = fs::remove_file(&archive);
            let code = String::from_utf8_lossy(&output.stdout);
            if matches!(code.trim(), "404" | "410") {
                return Err(format!(
                    "No download manifest is available for Steam App ID {app_id} from the selected plugin's provider"
                )
                .into());
            }
            return Err(format!(
                "Manifest provider could not be reached for Steam App ID {app_id}"
            )
            .into());
        }
    }
    validate_manifest_archive(&archive, app_id)?;
    let unpacked = root.join("manifest");
    if let Ok(meta) = fs::symlink_metadata(&unpacked) {
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("Manifest staging path is not a directory".into());
        }
        fs::remove_dir_all(&unpacked)?;
    }
    fs::create_dir(&unpacked)?;
    extract_checked(&archive, &unpacked)?;
    Ok(unpacked)
}

/// Resolve each provider depot to exactly one manifest revision.
pub fn parse_lua_manifests(manifest_dir: &Path, app_id: u32) -> Result<Vec<DepotInfo>> {
    parse_lua_manifests_filtered(manifest_dir, app_id, None)
}

fn parse_lua_manifests_filtered(
    manifest_dir: &Path,
    app_id: u32,
    filter: Option<(
        &std::collections::BTreeSet<u32>,
        &std::collections::BTreeSet<u32>,
    )>,
) -> Result<Vec<DepotInfo>> {
    if !manifest_dir.is_dir() {
        return Err("manifest directory does not exist".into());
    }

    let mut raw_depots: Vec<(u32, String)> = Vec::new();
    let mut manifest_ids: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();

    // Scan for all .lua scripts in manifest_dir
    for entry in fs::read_dir(manifest_dir)?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "lua")
            && fs::symlink_metadata(&path)
                .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
            && let Ok(content) = fs::read_to_string(&path)
        {
            parse_lua_content(&content, &mut raw_depots, &mut manifest_ids);
        }
    }

    if let Some((selected, known)) = filter {
        if let Some((unknown, _)) = raw_depots.iter().find(|(id, _)| !known.contains(id)) {
            return Err(format!(
                "Provider depot {unknown} is absent from Steam metadata for App ID {app_id}; its base/DLC role is unknown"
            )
            .into());
        }
        raw_depots.retain(|(id, _)| selected.contains(id));
    }

    let mut depots = Vec::new();

    for (depot_id, depot_key) in raw_depots {
        let requested = manifest_ids.get(&depot_id).copied();
        let mut matches = Vec::new();
        let mut roots = vec![manifest_dir.to_path_buf()];
        if let Some(parent) = manifest_dir.parent() {
            roots.push(parent.join("depotcache"));
        }
        for root in roots {
            let entries = match fs::read_dir(&root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let Some(id) = name
                    .strip_prefix(&format!("{depot_id}_"))
                    .and_then(|tail| tail.strip_suffix(".manifest"))
                    .and_then(|id| id.parse::<u64>().ok())
                else {
                    continue;
                };
                if requested.is_none_or(|expected| expected == id) {
                    let path = entry.path();
                    let meta = fs::symlink_metadata(&path)?;
                    if !meta.is_file() || meta.file_type().is_symlink() {
                        return Err(
                            format!("Depot {depot_id} manifest is not a regular file").into()
                        );
                    }
                    matches.push((id, path));
                }
            }
        }
        matches.sort_by(|a, b| a.1.cmp(&b.1));
        matches.dedup_by(|a, b| a.0 == b.0);
        let (manifest_id, manifest_path) = match matches.len() {
            1 => matches.remove(0),
            0 => return Err(format!(
                "No exact manifest for Steam App ID {app_id}, depot {depot_id}{}",
                requested.map_or(String::new(), |id| format!(", revision {id}"))
            ).into()),
            _ => return Err(format!(
                "Multiple manifest revisions for Steam App ID {app_id}, depot {depot_id}; the provider must specify one"
            ).into()),
        };
        depots.push(DepotInfo {
            depot_id,
            manifest_id,
            manifest_path,
            depot_key,
        });
    }

    depots.sort_by_key(|depot| depot.depot_id);
    Ok(depots)
}

fn parse_lua_content(
    content: &str,
    depots: &mut Vec<(u32, String)>,
    manifest_ids: &mut std::collections::HashMap<u32, u64>,
) {
    for line in content.lines() {
        let line = line.trim();
        // Match addappid(depot_id, [flag], "key")
        if let Some(start) = line.find("addappid(") {
            let args_part = &line[start + "addappid(".len()..];
            if let Some(end) = args_part.find(')') {
                let args_str = &args_part[..end];
                let tokens: Vec<&str> = args_str.split(',').map(str::trim).collect();
                if tokens.len() >= 2
                    && let Ok(depot_id) = tokens[0].parse::<u32>()
                {
                    // The key is the last token or 3rd token
                    let key_token =
                        tokens[tokens.len() - 1].trim_matches(|c| c == '"' || c == '\'');
                    if key_token.len() >= 32
                        && key_token.chars().all(|c| c.is_ascii_hexdigit())
                        && !depots.iter().any(|(id, _)| *id == depot_id)
                    {
                        depots.push((depot_id, key_token.to_string()));
                    }
                }
            }
        }

        // Match setManifestid(depot_id, "manifest_id")
        if let Some(start) = line.find("setManifestid(") {
            let args_part = &line[start + "setManifestid(".len()..];
            if let Some(end) = args_part.find(')') {
                let args_str = &args_part[..end];
                let tokens: Vec<&str> = args_str.split(',').map(str::trim).collect();
                if tokens.len() >= 2
                    && let Ok(depot_id) = tokens[0].parse::<u32>()
                {
                    let man_id = tokens[1].trim_matches(|c| c == '"' || c == '\'');
                    if let Ok(man_id) = man_id.parse::<u64>() {
                        manifest_ids.insert(depot_id, man_id);
                    }
                }
            }
        }
    }
}

pub use crate::archive::normalize_windows_paths;

fn has_depotkit_symbol(path: &Path) -> bool {
    unsafe {
        if let Ok(lib) = libloading::Library::new(path) {
            lib.get::<unsafe extern "C" fn()>(b"depotkit_download")
                .is_ok()
        } else {
            false
        }
    }
}

/// Find installed native Steam depot engine library (such as steamidra plugin).
pub fn find_depotkit_library(data_dir: &Path) -> Result<PathBuf> {
    // 1. Check data_dir/plugins/steamidra
    let steamidra_dir = data_dir.join("plugins").join("steamidra");
    if steamidra_dir.is_dir()
        && let Ok(entries) = fs::read_dir(&steamidra_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if (name.starts_with("lib")
                || name.ends_with(".so")
                || name.ends_with(".dll")
                || name.ends_with(".dylib"))
                && has_depotkit_symbol(&path)
            {
                return Ok(path);
            }
        }
    }

    // 2. Search all plugin subdirectories
    let plugins_dir = data_dir.join("plugins");
    if plugins_dir.is_dir()
        && let Ok(plugins) = fs::read_dir(&plugins_dir)
    {
        for plugin in plugins.flatten() {
            if plugin.path().is_dir()
                && let Ok(entries) = fs::read_dir(plugin.path())
            {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if (name.contains("steamidra")
                        || name.ends_with(".so")
                        || name.ends_with(".dll")
                        || name.ends_with(".dylib"))
                        && has_depotkit_symbol(&path)
                    {
                        return Ok(path);
                    }
                }
            }
        }
    }

    // 3. Fallback to executable directory
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        for cand in [
            "libsteamidra_plugin.so",
            "steamidra_plugin.dll",
            "libsteamidra.so",
        ] {
            let p = parent.join(cand);
            if p.is_file() && has_depotkit_symbol(&p) {
                return Ok(p);
            }
        }
    }

    Err("Steam depot downloader engine not found. Ensure the steamidra plugin is installed.".into())
}

struct ProgressContext<'a> {
    data_dir: &'a Path,
    job_id: &'a str,
    ctrl: *mut c_void,
    last_update: Instant,
    cancel_fn: Option<unsafe extern "C" fn(*mut c_void)>,
}

extern "C" fn depotkit_progress_cb(prog: *const DepotkitProgress, user_data: *mut c_void) {
    if prog.is_null() || user_data.is_null() {
        return;
    }
    unsafe {
        let ctx = &mut *(user_data as *mut ProgressContext);
        let p = &*prog;

        // Throttle progress writes and cancellation checks to every 400ms
        if ctx.last_update.elapsed() >= Duration::from_millis(400) {
            ctx.last_update = Instant::now();

            // Check if user requested job cancellation
            let cancel_marker = ctx
                .data_dir
                .join("jobs")
                .join(format!("{}.cancel", ctx.job_id));
            if cancel_marker.exists()
                && let Some(cancel_fn) = ctx.cancel_fn
                && !ctx.ctrl.is_null()
            {
                cancel_fn(ctx.ctrl);
            }

            if let Ok(mut job) = load_job(ctx.data_dir, ctx.job_id) {
                job.updated_unix = now().unwrap_or_default();
                job.progress_bytes = Some(p.bytes_downloaded);
                job.total_bytes = Some(p.total_bytes);
                let _ = save_job(ctx.data_dir, &job);
            }
        }
    }
}

/// Download Steam depot chunks via the native depotkit C ABI.
pub fn download_steam_depots(
    lib_path: &Path,
    target_dir: &Path,
    app_id: u32,
    depots: &[DepotInfo],
    data_dir: &Path,
    job_id: &str,
) -> Result<()> {
    let mut c_depots = Vec::new();
    let mut c_manifest_paths = Vec::new();
    let mut c_depot_keys = Vec::new();

    for d in depots {
        let manifest_str = CString::new(d.manifest_path.to_string_lossy().as_bytes())?;
        let key_str = CString::new(d.depot_key.as_bytes())?;
        c_depots.push(DepotkitDepot {
            depot_id: d.depot_id,
            _pad: 0,
            manifest_path: manifest_str.as_ptr(),
            depot_key: key_str.as_ptr(),
        });
        c_manifest_paths.push(manifest_str);
        c_depot_keys.push(key_str);
    }

    let target_dir_c = CString::new(target_dir.to_string_lossy().as_bytes())?;
    let req = DepotkitRequest {
        unused: 0,
        depots: c_depots.as_ptr(),
        depot_count: c_depots.len(),
        target_dir: target_dir_c.as_ptr(),
        max_workers: 16, // Cap workers to prevent saturating disk and event loop
        verify_checksums: 1,
        app_id,
        cell_id: 0, // auto detect
    };

    let lib = unsafe { libloading::Library::new(lib_path) }?;
    type CreateControlFn = unsafe extern "C" fn() -> *mut c_void;
    type DestroyControlFn = unsafe extern "C" fn(*mut c_void);
    type CancelControlFn = unsafe extern "C" fn(*mut c_void);
    type DownloadFn = unsafe extern "C" fn(
        *const DepotkitRequest,
        Option<extern "C" fn(*const DepotkitProgress, *mut c_void)>,
        *mut c_void,
        *mut c_void,
    ) -> c_int;
    type StrerrorFn = unsafe extern "C" fn(c_int) -> *const c_char;

    let create_ctrl: Option<libloading::Symbol<CreateControlFn>> =
        unsafe { lib.get(b"depotkit_control_create").ok() };
    let destroy_ctrl: Option<libloading::Symbol<DestroyControlFn>> =
        unsafe { lib.get(b"depotkit_control_destroy").ok() };
    let cancel_ctrl: Option<libloading::Symbol<CancelControlFn>> =
        unsafe { lib.get(b"depotkit_cancel").ok() };
    let download: libloading::Symbol<DownloadFn> = unsafe { lib.get(b"depotkit_download")? };
    let strerror: Option<libloading::Symbol<StrerrorFn>> =
        unsafe { lib.get(b"depotkit_strerror").ok() };

    let ctrl = if let Some(create) = create_ctrl {
        unsafe { create() }
    } else {
        std::ptr::null_mut()
    };

    let cancel_fn_ptr = cancel_ctrl.as_ref().map(|s| **s);
    let mut ctx = ProgressContext {
        data_dir,
        job_id,
        ctrl,
        last_update: Instant::now(),
        cancel_fn: cancel_fn_ptr,
    };

    let res = unsafe {
        download(
            &req,
            Some(depotkit_progress_cb),
            &mut ctx as *mut ProgressContext as *mut c_void,
            ctrl,
        )
    };

    if !ctrl.is_null()
        && let Some(destroy) = destroy_ctrl
    {
        unsafe { destroy(ctrl) };
    }

    let cancel_marker = data_dir.join("jobs").join(format!("{job_id}.cancel"));
    if cancel_marker.exists() {
        if let Ok(mut job) = load_job(data_dir, job_id) {
            job.status = JobStatus::Cancelled;
            let _ = save_job(data_dir, &job);
        }
        let _ = clear_cancel(data_dir, job_id);
        return Err("Download cancelled by user".into());
    }

    if res != 0 {
        let err_desc = if let Some(st_fn) = strerror {
            unsafe {
                let ptr = st_fn(res);
                if !ptr.is_null() {
                    CStr::from_ptr(ptr).to_string_lossy().to_string()
                } else {
                    format!("error code {res}")
                }
            }
        } else {
            format!("error code {res}")
        };

        if let Ok(mut job) = load_job(data_dir, job_id) {
            job.status = JobStatus::Failed;
            job.error = Some(err_desc.clone());
            let _ = save_job(data_dir, &job);
        }
        return Err(format!("Depot download error: {err_desc}").into());
    }

    Ok(())
}

/// Verify every target base depot in an older installation without changing game files.
pub fn verify_legacy_base(
    game: &Game,
    root: &Path,
    metadata: &crate::steam_metadata::SteamAppMetadata,
) -> Result<crate::steam_content::BaseContentReceipt> {
    let app_id = metadata.app_id;
    let unpacked = fetch_provider_bundle(&root.join("base-verification-provider"), app_id)?;
    let known = metadata
        .depots
        .iter()
        .map(|depot| depot.depot_id)
        .collect::<std::collections::BTreeSet<_>>();
    let selected_ids = metadata
        .depots
        .iter()
        .filter(|depot| depot.dlc_app_id.is_none())
        .filter(|depot| crate::steam_content::matches_target(depot, "windows", "english"))
        .map(|depot| depot.depot_id)
        .collect::<std::collections::BTreeSet<_>>();
    let available = parse_lua_manifests_filtered(&unpacked, app_id, Some((&selected_ids, &known)))?;
    if available.len() != selected_ids.len() {
        return Err(format!(
            "Cannot verify the old base game: provider manifests cover {} of {} required depots",
            available.len(),
            selected_ids.len()
        )
        .into());
    }
    let depots =
        crate::steam_content::select_base_depots(&available, metadata, "windows", "english")?;
    for depot in &depots {
        let manifest = crate::steam_manifest::read_manifest(
            &depot.manifest_path,
            depot.depot_id,
            depot.manifest_id,
        )?;
        let result = crate::steam_manifest::verify_installed(&manifest, &game.install_dir)?;
        if !result.complete() {
            return Err(format!(
                "Base depot {} differs from the current provider manifest ({} missing, {} changed files); reinstall the base game before adding DLC",
                depot.depot_id, result.missing, result.changed
            )
            .into());
        }
    }
    Ok(crate::steam_content::BaseContentReceipt {
        game_id: game.id.clone(),
        app_id,
        depots: depots
            .iter()
            .map(|depot| crate::steam_content::DepotRevision {
                depot_id: depot.depot_id,
                manifest_id: depot.manifest_id.to_string(),
            })
            .collect(),
        origin: crate::steam_content::BaseReceiptOrigin::VerifiedLegacy,
    })
}

/// Verify existing DLC files, or download selected depots into a managed base game.
#[derive(Debug)]
struct ResolvedDlcDownloads {
    groups: Vec<(u32, Vec<DepotInfo>)>,
    depots: Vec<DepotInfo>,
}

fn resolve_dlc_download_groups(
    root: &Path,
    metadata: &SteamAppMetadata,
    selected_ids: &BTreeSet<u32>,
) -> Result<ResolvedDlcDownloads> {
    resolve_dlc_download_groups_with(
        root,
        metadata,
        selected_ids,
        crate::steam_metadata::fetch_appinfo,
        fetch_provider_bundle,
    )
}

fn resolve_dlc_download_groups_with(
    root: &Path,
    metadata: &SteamAppMetadata,
    selected_ids: &BTreeSet<u32>,
    mut load_app: impl FnMut(u32) -> Result<SteamAppMetadata>,
    mut fetch_bundle: impl FnMut(&Path, u32) -> Result<PathBuf>,
) -> Result<ResolvedDlcDownloads> {
    if selected_ids.is_empty() {
        return Err(format!(
            "No matching DLC depots for Steam App ID {}",
            metadata.app_id
        )
        .into());
    }
    let mut by_manifest_app = BTreeMap::<u32, Vec<ResolvedDepotSource>>::new();
    for depot_id in selected_ids {
        let resolved =
            crate::steam_metadata::resolve_depot_source(metadata, *depot_id, &mut load_app)?;
        by_manifest_app
            .entry(resolved.manifest_app_id)
            .or_default()
            .push(resolved);
    }
    let mut download_groups = BTreeMap::<u32, Vec<DepotInfo>>::new();
    let mut selected = Vec::new();
    for (manifest_app_id, sources) in by_manifest_app {
        let manifest_metadata = if manifest_app_id == metadata.app_id {
            metadata.clone()
        } else {
            load_app(manifest_app_id)?
        };
        let known = manifest_metadata
            .depots
            .iter()
            .map(|depot| depot.depot_id)
            .collect::<BTreeSet<_>>();
        let wanted = sources
            .iter()
            .map(|source| source.depot_id)
            .collect::<BTreeSet<_>>();
        let unpacked = fetch_bundle(
            &root.join(format!("provider-{manifest_app_id}")),
            manifest_app_id,
        )?;
        let available =
            parse_lua_manifests_filtered(&unpacked, manifest_app_id, Some((&wanted, &known)))?;
        let artifacts = available
            .iter()
            .map(|artifact| (artifact.depot_id, artifact))
            .collect::<BTreeMap<_, _>>();
        for source in sources {
            let artifact = artifacts.get(&source.depot_id).ok_or_else(|| {
                format!(
                    "The selected source has no provider manifest for DLC depot {} from App ID {manifest_app_id}",
                    source.depot_id
                )
            })?;
            if artifact.manifest_id != source.manifest_id {
                return Err(format!(
                    "DLC depot {} provider revision differs from public manifest {}",
                    source.depot_id, source.manifest_id
                )
                .into());
            }
            download_groups
                .entry(source.download_app_id)
                .or_default()
                .push((*artifact).clone());
            selected.push((*artifact).clone());
        }
    }
    selected.sort_by_key(|depot| depot.depot_id);
    for depots in download_groups.values_mut() {
        depots.sort_by_key(|depot| depot.depot_id);
    }
    Ok(ResolvedDlcDownloads {
        groups: download_groups.into_iter().collect(),
        depots: selected,
    })
}

pub fn install_steam_dlc(game: &Game, data_dir: &Path, dlc_id: u32) -> Result<usize> {
    validate_id(&game.id)?;
    if dlc_id == 0 {
        return Err("DLC App ID is missing".into());
    }
    let app_id = game.steam_app_id.ok_or("This game has no Steam App ID")?;
    let source = game
        .origin
        .as_ref()
        .and_then(|origin| origin.qualified_id.split_once(':'))
        .map(|(source, _)| source);
    if source != Some("steamidra") {
        return Err("This game's source does not provide Steam depot DLC".into());
    }
    let root = data_dir
        .join("dlc-downloads")
        .join(&game.id)
        .join(dlc_id.to_string());
    let payload = root.join("payload");
    let job_id = job_id_for_target(&payload);
    let _active_worker = crate::jobs::lock_dlc_worker(data_dir, &job_id)?;
    prepare_dlc_retry(data_dir, &payload, &job_id)?;
    let job = DownloadJob {
        id: job_id.clone(),
        kind: JobKind::Dlc,
        url: format!("steam://{app_id}/dlc/{dlc_id}"),
        target: payload.clone(),
        status: JobStatus::Running,
        pid: Some(std::process::id()),
        error: None,
        updated_unix: now()?,
        expected_sha256: None,
        install: Some(InstallPlan {
            game_id: game.id.clone(),
            title: format!("{} DLC {dlc_id}", game.title),
            kind: InstallKind::Archive,
        }),
        install_state: Some(InstallState::Pending),
        install_error: None,
        progress_bytes: Some(0),
        total_bytes: None,
    };
    save_job(data_dir, &job)?;
    let run = (|| -> Result<usize> {
        let receipt = crate::steam_content::load_base_receipt(data_dir, &game.id)?;
        let metadata = crate::steam_metadata::fetch_appinfo(app_id)?;
        crate::jobs::check_dlc_cancelled(data_dir, &job_id)?;
        let (source_metadata, selected_ids) = if metadata
            .depots
            .iter()
            .any(|depot| depot.dlc_app_id == Some(dlc_id))
        {
            let selected_ids = metadata
                .depots
                .iter()
                .filter(|depot| depot.dlc_app_id == Some(dlc_id))
                .filter(|depot| crate::steam_content::matches_target(depot, "windows", "english"))
                .map(|depot| depot.depot_id)
                .collect::<BTreeSet<_>>();
            (metadata.clone(), selected_ids)
        } else {
            let child = crate::steam_metadata::fetch_appinfo(dlc_id)?;
            if child.parent_app_id != Some(app_id) {
                return Err("DLC metadata does not identify the installed parent game".into());
            }
            let selected_ids = child
                .depots
                .iter()
                .filter(|depot| depot.dlc_app_id.is_none_or(|id| id == dlc_id))
                .filter(|depot| crate::steam_content::matches_target(depot, "windows", "english"))
                .map(|depot| depot.depot_id)
                .collect::<BTreeSet<_>>();
            (child, selected_ids)
        };
        let downloads = resolve_dlc_download_groups(&root, &source_metadata, &selected_ids)?;
        crate::jobs::check_dlc_cancelled(data_dir, &job_id)?;
        let check_lock = crate::dlc::lock_game(data_dir, &game.id)?;
        let mut all_present = true;
        for depot in &downloads.depots {
            let manifest = crate::steam_manifest::read_manifest(
                &depot.manifest_path,
                depot.depot_id,
                depot.manifest_id,
            )?;
            if !crate::steam_manifest::verify_installed(&manifest, &game.install_dir)?.complete() {
                all_present = false;
            }
            crate::jobs::check_dlc_cancelled(data_dir, &job_id)?;
        }
        if all_present {
            crate::steam_content::save_observation(
                data_dir,
                &crate::steam_content::ObservedDlcReceipt {
                    game_id: game.id.clone(),
                    app_id,
                    dlc_id,
                    depots: downloads
                        .depots
                        .iter()
                        .map(|depot| crate::steam_content::DepotRevision {
                            depot_id: depot.depot_id,
                            manifest_id: depot.manifest_id.to_string(),
                        })
                        .collect(),
                    verified_unix: now()?,
                },
            )?;
            return Ok(0);
        }
        crate::steam_content::clear_observation(data_dir, &game.id, dlc_id)?;
        let receipt = match receipt.as_ref() {
            Some(receipt) => receipt.clone(),
            None => {
                let verified = verify_legacy_base(game, &root, &metadata)?;
                crate::steam_content::save_base_receipt(data_dir, &verified)?;
                verified
            }
        };
        crate::steam_content::validate_base_revision(&receipt, &metadata)?;
        crate::jobs::check_dlc_cancelled(data_dir, &job_id)?;
        let lib_path = find_depotkit_library(data_dir).map_err(
            |_| "The selected download plugin does not provide a usable depot downloader",
        )?;
        fs::create_dir_all(&payload)?;
        drop(check_lock);
        (|| -> Result<usize> {
            for (content_app_id, group) in &downloads.groups {
                crate::jobs::check_dlc_cancelled(data_dir, &job_id)?;
                let group_dir = root.join(format!("group-{content_app_id}"));
                remove_staging_dir(&group_dir)?;
                fs::create_dir_all(&group_dir)?;
                download_steam_depots(
                    &lib_path,
                    &group_dir,
                    *content_app_id,
                    group,
                    data_dir,
                    &job_id,
                )?;
                normalize_windows_paths(&group_dir)?;
                let group_manifests = group
                    .iter()
                    .map(|depot| {
                        crate::steam_manifest::read_manifest(
                            &depot.manifest_path,
                            depot.depot_id,
                            depot.manifest_id,
                        )
                    })
                    .collect::<Result<Vec<_>>>()?;
                crate::steam_manifest::verify_staged_exact(&group_manifests, &group_dir)?;
                merge_verified_payload(&group_dir, &payload)?;
            }
            let manifests = downloads
                .depots
                .iter()
                .map(|depot| {
                    crate::steam_manifest::read_manifest(
                        &depot.manifest_path,
                        depot.depot_id,
                        depot.manifest_id,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            crate::steam_manifest::verify_staged_exact(&manifests, &payload)?;
            crate::jobs::begin_dlc_commit(data_dir, &job_id)?;
            crate::dlc::install_staged(game, data_dir, &dlc_id.to_string(), &payload)
        })()
    })();
    let error = run.as_ref().err().map(ToString::to_string);
    let cancelled = crate::jobs::finish_dlc_job(data_dir, &job_id, error.as_deref())?;
    if cancelled {
        return Err("Download cancelled by user".into());
    }
    if run.is_ok() {
        let _ = fs::remove_dir_all(&root);
    }
    run
}

fn prepare_dlc_retry(data_dir: &Path, payload: &Path, job_id: &str) -> Result<()> {
    crate::jobs::clear_cancel(data_dir, job_id)?;
    remove_staging_dir(payload)
}

fn remove_staging_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)?;
        }
        Ok(_) => return Err("DLC staging payload is not a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn merge_verified_payload(source: &Path, target: &Path) -> Result<()> {
    let source_meta = fs::symlink_metadata(source)?;
    if !source_meta.is_dir() || source_meta.file_type().is_symlink() {
        return Err("DLC staging group is not a regular directory".into());
    }
    match fs::symlink_metadata(target) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err("Combined DLC staging path is not a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(target)?,
        Err(error) => return Err(error.into()),
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.file_type().is_symlink() {
            return Err("DLC staging group contains a symbolic link".into());
        }
        if metadata.is_dir() {
            merge_verified_payload(&from, &to)?;
        } else if metadata.is_file() {
            match fs::symlink_metadata(&to) {
                Ok(existing) if !existing.is_file() || existing.file_type().is_symlink() => {
                    return Err("Combined DLC staging path is not a regular file".into());
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            fs::copy(&from, &to)?;
        } else {
            return Err("DLC staging group contains a non-regular file".into());
        }
    }
    Ok(())
}

/// Perform end-to-end installation of a Steam manifest-based catalog game:
/// downloads manifest and addons, invokes native depotkit engine, normalizes paths,
/// overlays addons, registers into library, and tracks full progress in the transfers drawer.
pub fn install_steam_manifest_game(
    library: &mut Library,
    data_dir: &Path,
    game: &CatalogGame,
    app_id: u32,
) -> Result<InstallOutcome> {
    install_steam_manifest_game_in(library, data_dir, &data_dir.join("games"), game, app_id)
}

/// Install a manifest game into a selected library drive.
pub fn install_steam_manifest_game_in(
    library: &mut Library,
    data_dir: &Path,
    games_dir: &Path,
    game: &CatalogGame,
    app_id: u32,
) -> Result<InstallOutcome> {
    validate_id(&game.id)?;

    // 1. If installed in local Steam, import directly
    if let Ok(root) = steam_root()
        && scan_steam(&root)?
            .iter()
            .any(|candidate| candidate.app_id == app_id && candidate.installed)
    {
        let id = import_steam(library, &root, app_id)?;
        stamp_origin(library, &id, game);
        return Ok(InstallOutcome::Installed(id));
    }

    // 2. Prepare downloads directory
    let download_dir = data_dir.join("downloads").join(format!("steam-{app_id}"));
    fs::create_dir_all(&download_dir)?;

    let manifest_url = format!("https://ryuu.badkiko.ru/v1/download/{app_id}");
    let manifest_zip = download_dir.join(format!("{app_id}.zip"));
    if !manifest_zip.is_file() || manifest_zip.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        let output = clean_command("curl")
            .args([
                "--fail",
                "--location",
                "--retry",
                "2",
                "--max-time",
                "30",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--write-out",
                "%{http_code}",
                "--output",
            ])
            .arg(&manifest_zip)
            .arg(&manifest_url)
            .output()?;
        if !output.status.success() {
            let _ = fs::remove_file(&manifest_zip);
            let http_code = String::from_utf8_lossy(&output.stdout);
            if matches!(http_code.trim(), "404" | "410") {
                return Err(format!("No download manifest is available for Steam App ID {app_id} from the selected plugin's provider").into());
            }
            return Err(format!(
                "Manifest provider could not be reached for Steam App ID {app_id} ({})",
                output.status
            )
            .into());
        }
    }

    validate_manifest_archive(&manifest_zip, app_id)?;

    let unpack_dir = download_dir.join("manifest");
    if let Ok(metadata) = fs::symlink_metadata(&unpack_dir) {
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Manifest staging path is not a directory".into());
        }
        fs::remove_dir_all(&unpack_dir)?;
    }
    fs::create_dir_all(&unpack_dir)?;
    extract_checked(&manifest_zip, &unpack_dir)?;

    let depot_cache = download_dir.join("depotcache");
    fs::create_dir_all(&depot_cache)?;
    for entry in fs::read_dir(&unpack_dir)? {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_some_and(|ext| ext == "manifest")
        {
            fs::copy(entry.path(), depot_cache.join(entry.file_name()))?;
        }
    }

    // 3. Download addons (e.g. Online-Fix RAR)
    let mut addon_files = Vec::new();
    for addon in &game.addons {
        if let Some(uri) = &addon.uri {
            let filename = addon
                .filename
                .clone()
                .unwrap_or_else(|| format!("{}.rar", addon.id));
            let dest = download_dir.join(&filename);
            if !dest.is_file() || dest.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
                let mut cmd = clean_command("curl");
                cmd.args([
                    "--fail",
                    "--location",
                    "--retry",
                    "2",
                    "--max-time",
                    "120",
                    "--proto",
                    "=https,http",
                    "--proto-redir",
                    "=https,http",
                ]);
                if let Some(ref referer) = addon.referer {
                    cmd.arg("--referer").arg(referer);
                }
                let status = cmd.arg("--output").arg(&dest).arg(uri).status();
                if let Ok(st) = status
                    && st.success()
                    && dest.is_file()
                {
                    addon_files.push(dest);
                }
            } else {
                addon_files.push(dest);
            }
        }
    }

    // 4. Check if native depotkit engine is available
    let lib_path = find_depotkit_library(data_dir)
        .map_err(|_| "The selected download plugin does not provide a usable depot downloader")?;

    // 5. Parse depots from Lua manifests
    let appinfo = crate::steam_metadata::fetch_appinfo(app_id)?;
    let known = appinfo
        .depots
        .iter()
        .map(|depot| depot.depot_id)
        .collect::<std::collections::BTreeSet<_>>();
    let selected_ids = appinfo
        .depots
        .iter()
        .filter(|depot| depot.dlc_app_id.is_none())
        .filter(|depot| crate::steam_content::matches_target(depot, "windows", "english"))
        .map(|depot| depot.depot_id)
        .collect::<std::collections::BTreeSet<_>>();
    let provider_depots =
        parse_lua_manifests_filtered(&unpack_dir, app_id, Some((&selected_ids, &known)))?;
    if provider_depots.is_empty() {
        return Err(format!("No usable depot manifest was found for Steam App ID {app_id}").into());
    }
    let depots =
        crate::steam_content::select_base_depots(&provider_depots, &appinfo, "windows", "english")?;
    for depot in &depots {
        crate::steam_manifest::read_manifest(
            &depot.manifest_path,
            depot.depot_id,
            depot.manifest_id,
        )?;
    }

    // 6. Target installation directory
    fs::create_dir_all(games_dir)?;
    let target_dir = games_dir.join(&game.id);
    fs::create_dir_all(&target_dir)?;

    // 7. Register and track DownloadJob in transfers drawer
    let job_id = job_id_for_target(&target_dir);
    let mut job = DownloadJob {
        id: job_id.clone(),
        kind: JobKind::Game,
        url: format!("steam://{app_id}"),
        target: target_dir.clone(),
        status: JobStatus::Running,
        pid: Some(std::process::id()),
        error: None,
        updated_unix: now()?,
        expected_sha256: None,
        install: Some(InstallPlan {
            game_id: game.id.clone(),
            title: game.title.clone(),
            kind: InstallKind::Archive,
        }),
        install_state: Some(InstallState::Installing),
        install_error: None,
        progress_bytes: Some(0),
        total_bytes: None,
    };
    save_job(data_dir, &job)?;

    // 8. Execute Steam depot chunk download from Steam CDN
    let dl_result =
        download_steam_depots(&lib_path, &target_dir, app_id, &depots, data_dir, &job_id);
    if let Err(e) = dl_result {
        if let Ok(mut j) = load_job(data_dir, &job_id)
            && j.status == JobStatus::Running
        {
            j.status = JobStatus::Failed;
            j.error = Some(e.to_string());
            let _ = save_job(data_dir, &j);
        }
        return Err(e);
    }
    crate::steam_content::save_base_receipt(
        data_dir,
        &crate::steam_content::BaseContentReceipt {
            game_id: game.id.clone(),
            app_id,
            depots: depots
                .iter()
                .map(|depot| crate::steam_content::DepotRevision {
                    depot_id: depot.depot_id,
                    manifest_id: depot.manifest_id.to_string(),
                })
                .collect(),
            origin: crate::steam_content::BaseReceiptOrigin::ManagedInstall,
        },
    )?;

    // 9. Normalize backslash paths on Linux
    let _ = normalize_windows_paths(&target_dir);

    // 10. Extract addons over target directory
    for addon_file in &addon_files {
        let _ = extract_checked_with_passphrase(addon_file, &target_dir, Some("online-fix.me"));
        let _ = normalize_windows_paths(&target_dir);
    }

    // 11. Import into Library
    let import_result = discovery::import_game(library, &game.id, &game.title, &target_dir);
    match import_result {
        Ok(_) => {
            stamp_origin(library, &game.id, game);
            if game.plugin_install_kind == Some(2)
                && let Some(installed) = library.games.get(&game.id)
                && let Err(error) = crate::bundled_fix::ensure_steamidra_fix(installed, data_dir)
            {
                job.status = JobStatus::Failed;
                job.error = Some(format!("Bundled fix installation failed: {error}"));
                job.updated_unix = now().unwrap_or(0);
                let _ = save_job(data_dir, &job);
                return Err(error);
            }
            job.status = JobStatus::Complete;
            job.install_state = Some(InstallState::Complete);
            job.updated_unix = now().unwrap_or(0);
            let _ = save_job(data_dir, &job);
            Ok(InstallOutcome::Installed(game.id.clone()))
        }
        Err(_) => {
            job.status = JobStatus::Complete;
            job.install_state = Some(InstallState::NeedsSelection);
            job.updated_unix = now().unwrap_or(0);
            let _ = save_job(data_dir, &job);
            save_pending_setup_origin(data_dir, &game.id, game)?;
            Ok(InstallOutcome::NeedsSelection(game.id.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separately_downloaded_groups_merge_for_one_verified_commit() {
        use crate::steam_manifest::{DepotManifest, ManifestFile, verify_staged_exact};
        use sha1::{Digest, Sha1};

        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-groups-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = root.join("first");
        let second = root.join("second");
        let combined = root.join("combined");
        fs::create_dir_all(first.join("data")).unwrap();
        fs::create_dir_all(second.join("data")).unwrap();
        fs::write(first.join("data/base.bin"), b"first").unwrap();
        fs::write(second.join("data/addon.bin"), b"second").unwrap();
        let manifest = |depot_id, path: &str, bytes: &[u8]| DepotManifest {
            depot_id,
            manifest_id: u64::from(depot_id),
            files: vec![ManifestFile {
                path: PathBuf::from(path),
                size: bytes.len() as u64,
                sha1: Sha1::digest(bytes).into(),
            }],
        };
        let base = manifest(201, "data/base.bin", b"first");
        let addon = manifest(202, "data/addon.bin", b"second");
        verify_staged_exact(std::slice::from_ref(&base), &first).unwrap();
        verify_staged_exact(std::slice::from_ref(&addon), &second).unwrap();
        merge_verified_payload(&first, &combined).unwrap();
        merge_verified_payload(&second, &combined).unwrap();
        verify_staged_exact(&[base.clone(), addon], &combined).unwrap();

        let conflict_dir = root.join("conflict");
        fs::create_dir_all(conflict_dir.join("data")).unwrap();
        fs::write(conflict_dir.join("data/base.bin"), b"other").unwrap();
        let conflict = manifest(203, "data/base.bin", b"other");
        verify_staged_exact(std::slice::from_ref(&conflict), &conflict_dir).unwrap();
        merge_verified_payload(&conflict_dir, &combined).unwrap();
        assert!(verify_staged_exact(&[base, conflict], &combined).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shared_dlc_depots_use_manifest_owner_and_download_app_groups() {
        let root = std::env::temp_dir().join(format!(
            "azdl-shared-dlc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let parent_bundle = root.join("parent");
        let shared_bundle = root.join("shared");
        fs::create_dir_all(&parent_bundle).unwrap();
        fs::create_dir_all(&shared_bundle).unwrap();
        let key = "0123456789abcdef0123456789abcdef";
        fs::write(
            parent_bundle.join("100.lua"),
            format!("addappid(201,0,\"{key}\")\nsetManifestid(201,\"11\")"),
        )
        .unwrap();
        fs::write(parent_bundle.join("201_11.manifest"), b"parent").unwrap();
        fs::write(
            shared_bundle.join("200.lua"),
            format!("addappid(202,0,\"{key}\")\nsetManifestid(202,\"22\")"),
        )
        .unwrap();
        fs::write(shared_bundle.join("202_22.manifest"), b"shared").unwrap();
        let parent = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"100":{"depots":{"201":{"dlcappid":"900","manifests":{"public":{"gid":"11"}}},"202":{"dlcappid":"900","depotfromapp":"200"}}}}}"#,
            100,
        )
        .unwrap();
        let shared = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"200":{"depots":{"202":{"manifests":{"public":{"gid":"22"}}}}}}}"#,
            200,
        )
        .unwrap();
        let ids = [201, 202].into_iter().collect();
        let resolve = || {
            resolve_dlc_download_groups_with(
                &root,
                &parent,
                &ids,
                |id| {
                    assert_eq!(id, 200);
                    Ok(shared.clone())
                },
                |_, id| match id {
                    100 => Ok(parent_bundle.clone()),
                    200 => Ok(shared_bundle.clone()),
                    _ => panic!("unexpected provider App ID"),
                },
            )
        };
        let downloads = resolve().unwrap();
        assert_eq!(downloads.groups.len(), 2);
        assert_eq!(downloads.groups[0].0, 100);
        assert_eq!(downloads.groups[0].1[0].depot_id, 201);
        assert_eq!(downloads.groups[1].0, 200);
        assert_eq!(downloads.groups[1].1[0].depot_id, 202);
        assert_eq!(downloads.depots.len(), 2);

        let mut free_parent = parent.clone();
        free_parent.free_to_download = true;
        let free_downloads = resolve_dlc_download_groups_with(
            &root,
            &free_parent,
            &ids,
            |_| Ok(shared.clone()),
            |_, id| match id {
                100 => Ok(parent_bundle.clone()),
                200 => Ok(shared_bundle.clone()),
                _ => panic!("unexpected provider App ID"),
            },
        )
        .unwrap();
        assert_eq!(free_downloads.groups.len(), 1);
        assert_eq!(free_downloads.groups[0].0, 100);
        assert_eq!(free_downloads.groups[0].1.len(), 2);

        fs::write(
            shared_bundle.join("200.lua"),
            format!("addappid(202,0,\"{key}\")\nsetManifestid(202,\"23\")"),
        )
        .unwrap();
        fs::write(shared_bundle.join("202_23.manifest"), b"wrong").unwrap();
        let error = resolve().unwrap_err().to_string();
        assert!(error.contains("provider revision differs"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dlc_retry_discards_old_payload_and_cancel_marker_but_keeps_receipts() {
        let data_dir = std::env::temp_dir().join(format!(
            "azdl-dlc-retry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let payload = data_dir
            .join("dlc-downloads")
            .join("test-game")
            .join("123")
            .join("payload");
        fs::create_dir_all(&payload).unwrap();
        fs::write(payload.join("old-depot.bin"), b"stale").unwrap();
        let receipt = data_dir.join("dlc").join("test-game").join("123");
        fs::create_dir_all(&receipt).unwrap();
        fs::write(receipt.join("state.json"), b"journal").unwrap();
        let id = job_id_for_target(&payload);
        let worker = crate::jobs::lock_dlc_worker(&data_dir, &id).unwrap();
        let cancel = data_dir.join("jobs").join(format!("{id}.cancel"));
        fs::write(&cancel, b"").unwrap();

        prepare_dlc_retry(&data_dir, &payload, &id).unwrap();

        assert!(!payload.exists());
        assert!(!cancel.exists());
        assert_eq!(fs::read(receipt.join("state.json")).unwrap(), b"journal");
        drop(worker);
        fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn provider_error_page_is_reported_as_missing_manifest() {
        let path = std::env::temp_dir().join(format!(
            "azdl-manifest-response-{}-{}.zip",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, br#"{"error":"app not found"}"#).unwrap();
        let error = validate_manifest_archive(&path, 620)
            .unwrap_err()
            .to_string();
        fs::remove_file(&path).unwrap();
        assert!(error.contains("No usable download manifest"));
        assert!(error.contains("620"));
    }

    #[test]
    fn parses_lua_addappid_and_manifest_id() {
        let lua = r#"
            addappid(3892271,0,"90695be6010d5ec2e8890f1d742357316f4d5f74a8a9d22b2ae076b184797167")
            setManifestid(3892271,"4322338465787944513")
            addappid(3892270)
        "#;
        let mut depots = Vec::new();
        let mut manifest_ids = std::collections::HashMap::new();
        parse_lua_content(lua, &mut depots, &mut manifest_ids);

        assert_eq!(depots.len(), 1);
        assert_eq!(depots[0].0, 3892271);
        assert_eq!(
            depots[0].1,
            "90695be6010d5ec2e8890f1d742357316f4d5f74a8a9d22b2ae076b184797167"
        );
        assert_eq!(manifest_ids.get(&3892271), Some(&4322338465787944513));
    }

    #[test]
    fn rejects_another_revision_when_requested_manifest_is_missing() {
        let root = std::env::temp_dir().join(format!(
            "azdl-manifest-selection-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("123.lua"),
            "addappid(456,0,\"0123456789abcdef0123456789abcdef\")\nsetManifestid(456,\"789\")",
        )
        .unwrap();
        fs::write(root.join("456_788.manifest"), b"manifest").unwrap();
        let error = parse_lua_manifests(&root, 123).unwrap_err().to_string();
        assert!(error.contains("depot 456, revision 789"));
        fs::write(root.join("456_789.manifest"), b"manifest").unwrap();
        let selected = parse_lua_manifests(&root, 123).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].manifest_id, 789);
        assert_eq!(selected[0].manifest_path, root.join("456_789.manifest"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unrelated_missing_dlc_manifest_does_not_block_base_selection() {
        let root = std::env::temp_dir().join(format!(
            "azdl-manifest-filter-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let key = "0123456789abcdef0123456789abcdef";
        fs::write(
            root.join("100.lua"),
            format!(
                "addappid(101,0,\"{key}\")\nsetManifestid(101,\"10\")\naddappid(201,0,\"{key}\")\nsetManifestid(201,\"20\")"
            ),
        )
        .unwrap();
        fs::write(root.join("101_10.manifest"), b"base").unwrap();
        let known = [101, 201].into_iter().collect();
        let base = [101].into_iter().collect();
        let selected = parse_lua_manifests_filtered(&root, 100, Some((&base, &known))).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|depot| depot.depot_id)
                .collect::<Vec<_>>(),
            vec![101]
        );
        let dlc = [201].into_iter().collect();
        assert!(
            parse_lua_manifests_filtered(&root, 100, Some((&dlc, &known)))
                .unwrap_err()
                .to_string()
                .contains("depot 201, revision 20")
        );
        fs::write(root.join("100.lua"), format!("addappid(999,0,\"{key}\")")).unwrap();
        assert!(
            parse_lua_manifests_filtered(&root, 100, Some((&base, &known)))
                .unwrap_err()
                .to_string()
                .contains("Provider depot 999 is absent")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalizes_windows_backslashes_into_subdirectories() {
        let temp = std::env::temp_dir().join(format!("azdl-test-norm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let bad_file = temp.join(r"SubFolder\Nested\file.txt");
        let _ = fs::create_dir_all(bad_file.parent().unwrap());
        fs::write(&bad_file, b"content").unwrap();

        normalize_windows_paths(&temp).unwrap();

        let good_file = temp.join("SubFolder").join("Nested").join("file.txt");
        assert!(good_file.is_file());
        assert_eq!(fs::read(&good_file).unwrap(), b"content");

        let _ = fs::remove_dir_all(&temp);
    }
}
