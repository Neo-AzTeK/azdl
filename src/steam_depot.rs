use crate::archive::{extract_checked, extract_checked_with_passphrase};
use crate::catalog::{CatalogGame, InstallOutcome, save_pending_setup_origin, stamp_origin};
use crate::jobs::{
    DownloadJob, InstallKind, InstallPlan, InstallState, JobKind, JobStatus, clear_cancel,
    job_id_for_target, load_job, now, save_job,
};
use crate::steam::{import_steam, scan_steam, steam_root};
use crate::{Game, Library, Result, clean_command, discovery, validate_id};
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
    if !manifest_dir.is_dir() {
        return Err("manifest directory does not exist".into());
    }

    let mut raw_depots: Vec<(u32, String)> = Vec::new();
    let mut manifest_ids: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();

    // Scan for all .lua scripts in manifest_dir
    for entry in fs::read_dir(manifest_dir)?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "lua")
            && let Ok(content) = fs::read_to_string(&path)
        {
            parse_lua_content(&content, &mut raw_depots, &mut manifest_ids);
        }
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

/// Download selected Steam DLC depots and commit additive files to the managed game.
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
    let receipt = crate::steam_content::load_base_receipt(data_dir, &game.id)?
        .ok_or("Base game content revision is unknown; a managed base install is required")?;
    let metadata = crate::steam_metadata::fetch_appinfo(app_id)?;
    crate::steam_content::validate_base_revision(&receipt, &metadata)?;

    let root = data_dir
        .join("dlc-downloads")
        .join(&game.id)
        .join(dlc_id.to_string());
    let run = (|| -> Result<usize> {
        let (content_app_id, depots) = if metadata
            .depots
            .iter()
            .any(|depot| depot.dlc_app_id == Some(dlc_id))
        {
            let unpacked = fetch_provider_bundle(&root.join("base-provider"), app_id)?;
            let available = parse_lua_manifests(&unpacked, app_id)?;
            let selected = crate::steam_content::select_dlc_depots(
                &available, &metadata, dlc_id, "windows", "english",
            )?;
            (app_id, selected)
        } else {
            let child = crate::steam_metadata::fetch_appinfo(dlc_id)?;
            let unpacked = fetch_provider_bundle(&root.join("dlc-provider"), dlc_id)?;
            let available = parse_lua_manifests(&unpacked, dlc_id)?;
            let selected = crate::steam_content::select_separate_dlc_depots(
                &available, &child, app_id, "windows", "english",
            )?;
            (dlc_id, selected)
        };
        let lib_path = find_depotkit_library(data_dir).map_err(
            |_| "The selected download plugin does not provide a usable depot downloader",
        )?;
        let payload = root.join("payload");
        fs::create_dir_all(&payload)?;
        let job_id = job_id_for_target(&payload);
        let mut job = DownloadJob {
            id: job_id.clone(),
            kind: JobKind::Dlc,
            url: format!("steam://{content_app_id}/dlc/{dlc_id}"),
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
        let result = (|| -> Result<usize> {
            download_steam_depots(
                &lib_path,
                &payload,
                content_app_id,
                &depots,
                data_dir,
                &job_id,
            )?;
            normalize_windows_paths(&payload)?;
            crate::jobs::begin_dlc_commit(data_dir, &job_id)?;
            crate::dlc::install_staged(game, data_dir, &dlc_id.to_string(), &payload)
        })();
        job.updated_unix = now()?;
        match &result {
            Ok(_) => {
                job.status = JobStatus::Complete;
                job.install_state = Some(InstallState::Complete);
            }
            Err(error) => {
                let cancelled = load_job(data_dir, &job_id)
                    .is_ok_and(|current| current.status == JobStatus::Cancelled);
                job.status = if cancelled {
                    JobStatus::Cancelled
                } else {
                    JobStatus::Failed
                };
                job.install_state = Some(InstallState::Failed);
                job.error = Some(error.to_string());
            }
        }
        if let Ok(current) = load_job(data_dir, &job_id) {
            job.progress_bytes = current.progress_bytes;
            job.total_bytes = current.total_bytes;
        }
        save_job(data_dir, &job)?;
        result
    })();
    if run.is_ok() {
        let _ = fs::remove_dir_all(&root);
    }
    run
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
    let provider_depots = parse_lua_manifests(&unpack_dir, app_id)?;
    if provider_depots.is_empty() {
        return Err(format!("No usable depot manifest was found for Steam App ID {app_id}").into());
    }
    let appinfo = crate::steam_metadata::fetch_appinfo(app_id)?;
    let depots =
        crate::steam_content::select_base_depots(&provider_depots, &appinfo, "windows", "english")?;

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
