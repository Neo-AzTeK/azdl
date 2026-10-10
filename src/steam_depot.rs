use crate::archive::{extract_checked, extract_checked_with_passphrase};
use crate::catalog::{CatalogGame, InstallOutcome, save_pending_setup_origin, stamp_origin};
use crate::jobs::{
    DownloadJob, InstallKind, InstallPlan, InstallState, JobKind, JobStatus, clear_cancel,
    job_id_for_target, load_job, now, save_job,
};
use crate::steam::{import_steam, scan_steam, steam_root};
use crate::{Library, Result, discovery, validate_id};
use std::ffi::{CStr, CString, c_char, c_double, c_int, c_void};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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
    pub manifest_path: PathBuf,
    pub depot_key: String,
}

/// Parse depot IDs, decryption keys, and manifest paths from unpacked Lua scripts and manifest files.
pub fn parse_lua_manifests(manifest_dir: &Path, _app_id: u32) -> Result<Vec<DepotInfo>> {
    if !manifest_dir.is_dir() {
        return Err("manifest directory does not exist".into());
    }

    let mut raw_depots: Vec<(u32, String)> = Vec::new();
    let mut manifest_ids: std::collections::HashMap<u32, String> = std::collections::HashMap::new();

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
        // Try to locate the matching .manifest file
        let mut manifest_path = None;

        // 1. Check with manifest_id if present: {depot_id}_{manifest_id}.manifest
        if let Some(man_id) = manifest_ids.get(&depot_id) {
            let candidate = manifest_dir.join(format!("{depot_id}_{man_id}.manifest"));
            if candidate.is_file() {
                manifest_path = Some(candidate);
            }
        }

        // 2. Search manifest_dir for any matching {depot_id}_*.manifest or {depot_id}.manifest
        if manifest_path.is_none() {
            let prefix = format!("{depot_id}_");
            if let Ok(entries) = fs::read_dir(manifest_dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.extension().is_some_and(|ext| ext == "manifest") {
                        let name = p.file_name().unwrap_or_default().to_string_lossy();
                        if name.starts_with(&prefix) || name == format!("{depot_id}.manifest") {
                            manifest_path = Some(p);
                            break;
                        }
                    }
                }
            }
        }

        // 3. Fallback to depotcache/ directory
        if manifest_path.is_none()
            && let Some(parent) = manifest_dir.parent()
        {
            let depotcache = parent.join("depotcache");
            if depotcache.is_dir() {
                let prefix = format!("{depot_id}_");
                if let Ok(entries) = fs::read_dir(&depotcache) {
                    for e in entries.flatten() {
                        let p = e.path();
                        if p.extension().is_some_and(|ext| ext == "manifest") {
                            let name = p.file_name().unwrap_or_default().to_string_lossy();
                            if name.starts_with(&prefix) || name == format!("{depot_id}.manifest") {
                                manifest_path = Some(p);
                                break;
                            }
                        }
                    }
                }
            }
        }

        if let Some(path) = manifest_path {
            depots.push(DepotInfo {
                depot_id,
                manifest_path: path,
                depot_key,
            });
        }
    }

    Ok(depots)
}

fn parse_lua_content(
    content: &str,
    depots: &mut Vec<(u32, String)>,
    manifest_ids: &mut std::collections::HashMap<u32, String>,
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
                    if !man_id.is_empty() {
                        manifest_ids.insert(depot_id, man_id.to_string());
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

/// Perform end-to-end installation of a Steam manifest-based catalog game:
/// downloads manifest and addons, invokes native depotkit engine, normalizes paths,
/// overlays addons, registers into library, and tracks full progress in the transfers drawer.
pub fn install_steam_manifest_game(
    library: &mut Library,
    data_dir: &Path,
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
        let output = Command::new("curl")
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
            if http_code.trim() == "404" {
                return Err(format!("No manifest is available for Steam App ID {app_id}").into());
            }
            return Err(format!(
                "Manifest provider could not be reached for Steam App ID {app_id} ({})",
                output.status
            )
            .into());
        }
    }

    if manifest_zip.metadata()?.len() == 0 {
        return Err("Manifest provider returned an empty download".into());
    }

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
                let mut cmd = Command::new("curl");
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
    let depots = parse_lua_manifests(&unpack_dir, app_id)?;
    if depots.is_empty() {
        return Err(format!("No usable depot manifest was found for Steam App ID {app_id}").into());
    }

    // 6. Target installation directory
    let games_dir = data_dir.join("games");
    fs::create_dir_all(&games_dir)?;
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
        assert_eq!(
            manifest_ids.get(&3892271).map(|s| s.as_str()),
            Some("4322338465787944513")
        );
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
