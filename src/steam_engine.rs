//! Internal adapter for the pinned Qt-free depotkit C ABI.

use crate::Result;
use crate::jobs::{JobStatus, clear_cancel, load_job, now, save_job};
use crate::steam_depot::DepotInfo;
use std::ffi::{CStr, CString, c_char, c_double, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[repr(C)]
pub struct DepotkitDepot {
    pub depot_id: u32,
    pub _pad: u32,
    pub manifest_path: *const c_char,
    pub depot_key: *const c_char,
}

/// Public C request layout for the separately bundled, pinned depotkit engine.
#[repr(C)]
pub(crate) struct StandaloneDepotkitRequest {
    app_id: u32,
    depots: *const DepotkitDepot,
    depot_count: usize,
    target_dir: *const c_char,
    max_workers: u32,
    verify_checksums: c_int,
    cell_id: u32,
}

#[repr(C)]
pub struct DepotkitProgress {
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub rate_bytes_sec: c_double,
    pub files_done: u32,
    pub files_total: u32,
    pub depot_index: u32,
    pub depot_count: u32,
    pub phase: u32,
    pub _pad: u32,
    pub detail: *const c_char,
}

fn check_depotkit_library(path: &Path) -> Result<()> {
    unsafe {
        let library = libloading::Library::new(path)
            .map_err(|error| format!("Cannot load {}: {error}", path.display()))?;
        for name in [
            b"depotkit_download".as_slice(),
            b"depotkit_control_create",
            b"depotkit_control_destroy",
            b"depotkit_cancel",
            b"depotkit_strerror",
        ] {
            library
                .get::<unsafe extern "C" fn()>(name)
                .map_err(|error| format!("Invalid Steam engine {}: {error}", path.display()))?;
        }
        let version = library
            .get::<unsafe extern "C" fn() -> *const c_char>(b"depotkit_version")
            .map_err(|error| format!("Invalid Steam engine {}: {error}", path.display()))?;
        let pointer = version();
        if pointer.is_null() || CStr::from_ptr(pointer).to_bytes() != b"0.2.0" {
            return Err(format!(
                "Unsupported depotkit ABI in {}. Install the pinned azdl runtime package",
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

/// Locate only the bundled Qt-free engine; never search user content or extension trees.
pub fn find_depotkit_library(_data_dir: &Path) -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    find_engine_beside(&executable)
}

fn find_engine_beside(executable: &Path) -> Result<PathBuf> {
    let directory = executable
        .parent()
        .ok_or("Cannot locate launcher directory")?;
    let name = if cfg!(windows) {
        "depotkit.dll"
    } else {
        "libdepotkit.so"
    };
    let candidates = [
        directory.join(name),
        directory.join("../lib/azdl").join(name),
    ];
    for path in candidates {
        if path.is_file() {
            check_depotkit_library(&path)?;
            return Ok(path);
        }
    }
    Err(format!("Steam download engine {name} is missing. Reinstall the complete azdl package or place the pinned engine beside the launcher.").into())
}

struct ProgressContext<'a> {
    data_dir: &'a Path,
    job_id: &'a str,
    ctrl: usize,
    last_update: Instant,
    cancel_fn: Option<unsafe extern "C" fn(*mut c_void)>,
    completed_bytes: u64,
    overall_bytes: Option<u64>,
}

extern "C" fn depotkit_progress_cb(prog: *const DepotkitProgress, user_data: *mut c_void) {
    if prog.is_null() || user_data.is_null() {
        return;
    }
    unsafe {
        let context = &*(user_data as *const Mutex<ProgressContext>);
        let mut ctx = context
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let p = &*prog;

        // Throttle progress writes and cancellation checks to every 400ms
        if ctx.last_update.elapsed() >= Duration::from_millis(400) {
            ctx.last_update = Instant::now();

            // Check if user requested job cancellation
            let cancel_marker = ctx
                .data_dir
                .join("jobs")
                .join(format!("{}.cancel", ctx.job_id));
            let pause_marker = ctx
                .data_dir
                .join("jobs")
                .join(format!("{}.pause", ctx.job_id));
            let cancel = (cancel_marker.exists() || pause_marker.exists())
                && ctx.cancel_fn.is_some()
                && ctx.ctrl != 0;
            let cancel_fn = ctx.cancel_fn;
            let ctrl = ctx.ctrl;

            if let Ok(mut job) = load_job(ctx.data_dir, ctx.job_id) {
                job.updated_unix = now().unwrap_or_default();
                let bytes = ctx.completed_bytes.saturating_add(p.bytes_downloaded);
                job.progress_bytes = Some(job.progress_bytes.unwrap_or(0).max(bytes));
                job.total_bytes = ctx.overall_bytes.or(Some(p.total_bytes));
                let _ = save_job(ctx.data_dir, &job);
            }
            drop(ctx);
            if cancel && let Some(cancel_fn) = cancel_fn {
                cancel_fn(ctrl as *mut c_void);
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
    download_steam_depots_with_progress(
        lib_path,
        target_dir,
        app_id,
        depots,
        data_dir,
        job_id,
        DownloadProgressSpan {
            completed_bytes: 0,
            overall_bytes: None,
        },
    )
}

pub(crate) struct DownloadProgressSpan {
    pub(crate) completed_bytes: u64,
    pub(crate) overall_bytes: Option<u64>,
}

pub(crate) fn download_steam_depots_with_progress(
    lib_path: &Path,
    target_dir: &Path,
    app_id: u32,
    depots: &[DepotInfo],
    data_dir: &Path,
    job_id: &str,
    span: DownloadProgressSpan,
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
    let standalone_req = StandaloneDepotkitRequest {
        app_id,
        depots: c_depots.as_ptr(),
        depot_count: c_depots.len(),
        target_dir: target_dir_c.as_ptr(),
        max_workers: 16,
        verify_checksums: 1,
        cell_id: 0,
    };
    let request = (&standalone_req as *const StandaloneDepotkitRequest).cast::<c_void>();

    check_depotkit_library(lib_path)?;
    let lib = unsafe { libloading::Library::new(lib_path) }?;
    type CreateControlFn = unsafe extern "C" fn() -> *mut c_void;
    type DestroyControlFn = unsafe extern "C" fn(*mut c_void);
    type CancelControlFn = unsafe extern "C" fn(*mut c_void);
    type DownloadFn = unsafe extern "C" fn(
        *const c_void,
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

    if ctrl.is_null() {
        return Err("Steam download engine could not allocate cancellation control".into());
    }

    let cancel_fn_ptr = cancel_ctrl.as_ref().map(|s| **s);
    let ctx = Mutex::new(ProgressContext {
        data_dir,
        job_id,
        ctrl: ctrl as usize,
        last_update: Instant::now()
            .checked_sub(Duration::from_millis(400))
            .unwrap_or_else(Instant::now),
        cancel_fn: cancel_fn_ptr,
        completed_bytes: span.completed_bytes,
        overall_bytes: span.overall_bytes,
    });

    let res = unsafe {
        download(
            request,
            Some(depotkit_progress_cb),
            (&ctx as *const Mutex<ProgressContext>).cast_mut().cast(),
            ctrl,
        )
    };

    if !ctrl.is_null()
        && let Some(destroy) = destroy_ctrl
    {
        unsafe { destroy(ctrl) };
    }

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
    let cancel_marker = data_dir.join("jobs").join(format!("{job_id}.cancel"));
    if cancel_marker.exists() {
        if let Ok(mut job) = load_job(data_dir, job_id) {
            job.status = JobStatus::Cancelled;
            let _ = save_job(data_dir, &job);
        }
        let _ = clear_cancel(data_dir, job_id);
        return Err(crate::jobs::TransferStopped::Cancelled(Some(job_id.into())).into());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{DownloadJob, InstallState, JobKind, job_id_for_target};
    use std::fs;
    #[test]
    fn standalone_request_matches_public_depotkit_layout() {
        assert_eq!(std::mem::offset_of!(StandaloneDepotkitRequest, app_id), 0);
        if cfg!(target_pointer_width = "64") {
            assert_eq!(std::mem::offset_of!(StandaloneDepotkitRequest, depots), 8);
            assert_eq!(std::mem::offset_of!(StandaloneDepotkitRequest, cell_id), 40);
            assert_eq!(std::mem::size_of::<StandaloneDepotkitRequest>(), 48);
        }
    }

    #[test]
    fn depot_loader_errors_retain_library_path_and_loader_diagnostics() {
        let path = std::env::temp_dir().join("azdl-nonexistent-depot-engine");
        let error = check_depotkit_library(&path).unwrap_err().to_string();
        assert!(error.contains(&path.display().to_string()));
        assert!(error.starts_with("Cannot load "));
        assert!(error.len() > format!("Cannot load {}: ", path.display()).len());
    }

    #[cfg(unix)]
    #[test]
    fn bundled_engine_receives_public_request_fields() {
        use std::process::Command;

        if Command::new("cc").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-depot-abi-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let engine = root.join(if cfg!(windows) {
            "depotkit.dll"
        } else {
            "libdepotkit.so"
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/depotkit_abi.c");
        assert!(
            Command::new("cc")
                .args(["-shared", "-fPIC", "-o"])
                .arg(&engine)
                .arg(fixture)
                .status()
                .unwrap()
                .success()
        );
        let depot = DepotInfo {
            depot_id: 381,
            manifest_id: 42,
            manifest_path: root.join("manifest-\u{e9}.bin"),
            depot_key: "test-key".into(),
        };
        let job_id = "0000000000000000";
        save_job(
            &root,
            &DownloadJob {
                id: job_id.into(),
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
        let inspector = unsafe { libloading::Library::new(&engine) }.unwrap();
        download_steam_depots(
            &engine,
            &root,
            480,
            std::slice::from_ref(&depot),
            &root,
            job_id,
        )
        .unwrap();
        let job = load_job(&root, job_id).unwrap();
        assert_eq!(job.progress_bytes, Some(256));
        assert_eq!(job.total_bytes, Some(1024));
        fs::write(
            root.join("jobs").join(format!("{job_id}.cancel")),
            b"cancel",
        )
        .unwrap();
        let error = download_steam_depots(
            &engine,
            &root,
            480,
            std::slice::from_ref(&depot),
            &root,
            job_id,
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(
            load_job(&root, job_id).unwrap().status,
            JobStatus::Cancelled
        );
        let cancel_count = unsafe {
            inspector.get::<unsafe extern "C" fn() -> u32>(b"depotkit_fixture_cancel_count")
        }
        .unwrap();
        assert_eq!(unsafe { cancel_count() }, 1);
        fs::write(root.join("jobs").join(format!("{job_id}.pause")), b"pause").unwrap();
        let error =
            download_steam_depots(&engine, &root, 480, &[depot], &root, job_id).unwrap_err();
        assert!(
            matches!(error.downcast_ref::<crate::jobs::TransferStopped>(), Some(crate::jobs::TransferStopped::Paused(id)) if id == job_id)
        );
        assert_eq!(load_job(&root, job_id).unwrap().status, JobStatus::Paused);
        assert_eq!(unsafe { cancel_count() }, 2);
        drop(inspector);
        let unsupported = root.join("unsupported-engine.so");
        assert!(
            Command::new("cc")
                .args([
                    "-shared",
                    "-fPIC",
                    "-DAZDL_DEPOTKIT_VERSION=\"9.9.9\"",
                    "-o"
                ])
                .arg(&unsupported)
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/depotkit_abi.c"))
                .status()
                .unwrap()
                .success()
        );
        assert!(
            check_depotkit_library(&unsupported)
                .unwrap_err()
                .to_string()
                .contains("Unsupported depotkit ABI")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires scripts/build-depotkit.ps1 and AZDL_TEST_DEPOTKIT"]
    fn bundled_engine_loads_without_qt_and_validates_requests() {
        let engine = PathBuf::from(std::env::var_os("AZDL_TEST_DEPOTKIT").unwrap());
        let executable = std::env::current_exe().unwrap();
        let bundled = executable.parent().unwrap().join(if cfg!(windows) {
            "depotkit.dll"
        } else {
            "libdepotkit.so"
        });
        fs::copy(engine, &bundled).unwrap();
        let root = std::env::temp_dir().join(format!("azdl-windows-engine-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        assert_eq!(find_depotkit_library(&root).unwrap(), bundled);
        unsafe {
            let library = libloading::Library::new(&bundled).unwrap();
            let download = library
                .get::<unsafe extern "C" fn(
                    *const StandaloneDepotkitRequest,
                    *const c_void,
                    *mut c_void,
                    *mut c_void,
                ) -> c_int>(b"depotkit_download")
                .unwrap();
            let request = StandaloneDepotkitRequest {
                app_id: 480,
                depots: std::ptr::null(),
                depot_count: 0,
                target_dir: std::ptr::null(),
                max_workers: 16,
                verify_checksums: 1,
                cell_id: 0,
            };
            assert_eq!(
                download(
                    &request,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                1
            );
            for symbol in [
                b"depotkit_control_create".as_slice(),
                b"depotkit_control_destroy",
                b"depotkit_cancel",
                b"depotkit_strerror",
            ] {
                library.get::<unsafe extern "C" fn()>(symbol).unwrap();
            }
        }
        let error = download_steam_depots(&bundled, &root, 480, &[], &root, "0000000000000000")
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid argument"), "{error}");
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(bundled).unwrap();
    }

    #[test]
    fn dlc_progress_accumulates_across_content_app_groups() {
        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-progress-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = root.join("dlc-downloads/game/200/payload");
        let id = job_id_for_target(&target);
        save_job(
            &root,
            &DownloadJob {
                id: id.clone(),
                kind: JobKind::Dlc,
                url: "steam://100/dlc/200".into(),
                target,
                status: JobStatus::Running,
                pid: Some(std::process::id()),
                error: None,
                updated_unix: now().unwrap(),
                expected_sha256: None,
                install: None,
                install_state: Some(InstallState::Pending),
                install_error: None,
                progress_bytes: Some(0),
                total_bytes: Some(300),
            },
        )
        .unwrap();
        let report = DepotkitProgress {
            bytes_downloaded: 50,
            total_bytes: 100,
            rate_bytes_sec: 0.0,
            files_done: 0,
            files_total: 0,
            depot_index: 0,
            depot_count: 0,
            phase: 0,
            _pad: 0,
            detail: std::ptr::null(),
        };
        let context = Mutex::new(ProgressContext {
            data_dir: &root,
            job_id: &id,
            ctrl: 0,
            last_update: Instant::now() - Duration::from_secs(1),
            cancel_fn: None,
            completed_bytes: 0,
            overall_bytes: Some(300),
        });
        depotkit_progress_cb(
            &report,
            (&context as *const Mutex<ProgressContext>)
                .cast_mut()
                .cast(),
        );
        assert_eq!(load_job(&root, &id).unwrap().progress_bytes, Some(50));
        {
            let mut state = context.lock().unwrap();
            state.completed_bytes = 100;
            state.last_update = Instant::now() - Duration::from_secs(1);
        }
        let second = DepotkitProgress {
            bytes_downloaded: 10,
            total_bytes: 200,
            ..report
        };
        depotkit_progress_cb(
            &second,
            (&context as *const Mutex<ProgressContext>)
                .cast_mut()
                .cast(),
        );
        let job = load_job(&root, &id).unwrap();
        assert_eq!(job.progress_bytes, Some(110));
        assert_eq!(job.total_bytes, Some(300));
        context.lock().unwrap().last_update = Instant::now() - Duration::from_secs(1);
        std::thread::scope(|scope| {
            for bytes in 11..=16 {
                let context = &context;
                scope.spawn(move || {
                    let report = DepotkitProgress {
                        bytes_downloaded: bytes,
                        total_bytes: 200,
                        rate_bytes_sec: 0.0,
                        files_done: 0,
                        files_total: 0,
                        depot_index: 0,
                        depot_count: 0,
                        phase: 0,
                        _pad: 0,
                        detail: std::ptr::null(),
                    };
                    depotkit_progress_cb(
                        &report,
                        (context as *const Mutex<ProgressContext>).cast_mut().cast(),
                    );
                });
            }
        });
        let job = load_job(&root, &id).unwrap();
        assert!(job.progress_bytes.unwrap() >= 111);
        assert!(job.progress_bytes.unwrap() <= 116);
        fs::remove_dir_all(root).unwrap();
    }
}
