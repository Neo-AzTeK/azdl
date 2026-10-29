use super::*;
use azdl::discovery::discover_executables;
use azdl::doctor::diagnose_game;
use azdl::jobs::list_jobs;
use azdl::overlay::{overlay_status, set_overlay_enabled};
use azdl::patch::{active_patch, disable_patch, enable_patch, rollback_patch};
use azdl::{
    Library, Runner, add_game, add_launch_option, load_library, remove_launch_option, save_library,
    select_launch_option, set_executable, set_runner, spawn_game, validate_id,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{State, Window};

/// How long a detected Online-Fix/SteamFix result stays cached per game.
const FIX_STATUS_CACHE_TTL: Duration = Duration::from_secs(15);

/// Cached fix detections keyed by game id, invalidated when the install path changes.
pub(crate) type FixStatusCache =
    std::collections::HashMap<String, (PathBuf, azdl::multiplayer::FixKind, Instant)>;

fn scan_steamstub_cached(state: &AppState, game: &azdl::Game) -> azdl::steamless::SteamStubStatus {
    if let Ok(cache) = state.steamstub_cache.lock()
        && let Some((result, instant)) = cache.get(&game.id)
        && instant.elapsed() < Duration::from_secs(60)
    {
        return result.clone();
    }
    let res = azdl::steamless::scan_game_steamstub(game);
    if let Ok(mut cache) = state.steamstub_cache.lock() {
        cache.insert(game.id.clone(), (res.clone(), Instant::now()));
    }
    res
}

pub(super) fn detect_fix_cached(
    state: &AppState,
    game: &azdl::Game,
) -> Option<azdl::multiplayer::FixKind> {
    if let Ok(cache) = state.fix_status_cache.lock()
        && let Some((install_dir, fix, instant)) = cache.get(&game.id)
        && install_dir == &game.install_dir
        && instant.elapsed() < FIX_STATUS_CACHE_TTL
    {
        return Some(*fix);
    }
    let fix = azdl::multiplayer::detect_fix(game).ok()?;
    if let Ok(mut cache) = state.fix_status_cache.lock() {
        cache.insert(
            game.id.clone(),
            (game.install_dir.clone(), fix, Instant::now()),
        );
    }
    Some(fix)
}

#[derive(serde::Deserialize)]
struct SteamDetailsEnvelope {
    success: Option<bool>,
    data: Option<SteamDetailsPayload>,
}

#[derive(serde::Deserialize)]
struct SteamDetailsPayload {
    r#type: Option<Value>,
    fullgame: Option<Value>,
    name: Option<Value>,
    header_image: Option<Value>,
    detailed_description: Option<Value>,
    short_description: Option<Value>,
    developers: Option<Value>,
    movies: Option<Value>,
    screenshots: Option<Value>,
    genres: Option<Value>,
    release_date: Option<Value>,
    pc_requirements: Option<Value>,
}

impl SteamDetailsPayload {
    fn into_value(self) -> Value {
        let mut selected = serde_json::Map::new();
        for (key, value) in [
            ("type", self.r#type),
            ("fullgame", self.fullgame),
            ("name", self.name),
            ("header_image", self.header_image),
            ("detailed_description", self.detailed_description),
            ("short_description", self.short_description),
            ("developers", self.developers),
            ("movies", self.movies),
            ("screenshots", self.screenshots),
            ("genres", self.genres),
            ("release_date", self.release_date),
            ("pc_requirements", self.pc_requirements),
        ] {
            if let Some(value) = value {
                selected.insert(key.to_string(), value);
            }
        }
        Value::Object(selected)
    }
}

fn finish_install_task(state: &AppState, task_id: &str, error: Option<String>) {
    let game_id = state.tasks.lock().ok().and_then(|tasks| {
        tasks
            .iter()
            .find(|task| task.id == task_id)
            .and_then(|task| task.game_id.clone())
    });
    if let Some(game_id) = game_id
        && let Ok(library) = azdl::load_library(&state.data_dir)
        && let Some(game) = library.games.get(&game_id)
    {
        state.dir_size_cache.invalidate(&game.install_dir);
    }
    if let Ok(mut tasks) = state.tasks.lock() {
        if let Some(error) = error {
            if let Some(task) = tasks.iter_mut().find(|task| task.id == task_id) {
                task.status = "failed".into();
                task.log_tail = error;
                task.pid = None;
            }
        } else {
            tasks.retain(|task| task.id != task_id);
        }
    }
    state.notify_state_changed();
}

pub(super) fn finish_dlc_install_task(state: &AppState, task_id: &str, result: AzdlResult<usize>) {
    let failure = result.err().and_then(|error| {
        if error
            .downcast_ref::<azdl::jobs::TransferStopped>()
            .is_some()
        {
            None
        } else {
            Some(error.to_string())
        }
    });
    finish_install_task(state, task_id, failure);
}

fn runner_str(runner: &Runner) -> String {
    match runner {
        Runner::Auto => "auto".into(),
        Runner::Native => "native".into(),
        Runner::Wine => "wine".into(),
        Runner::ProtonAuto => "proton:auto".into(),
        Runner::Proton(p) => format!("proton:{}", p.display()),
        Runner::Steam(id) => format!("steam:{id}"),
    }
}

fn format_file_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn parse_steam_id(raw: &str) -> Option<String> {
    let stripped = raw
        .strip_prefix("steam:")
        .or_else(|| raw.strip_prefix("steam-"))
        .unwrap_or(raw);
    if !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit()) {
        Some(stripped.to_string())
    } else {
        None
    }
}

fn pending_installs(
    data_dir: &Path,
    library: &Library,
    active_game_ids: &[String],
    active_targets: &[PathBuf],
) -> AzdlResult<Vec<Value>> {
    let mut pending = Vec::new();
    let mut roots = vec![data_dir.join("games")];
    for drive in azdl::settings::load_settings(data_dir)?.storage_drives {
        if !roots.contains(&drive.path) {
            roots.push(drive.path);
        }
    }
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if validate_id(&id).is_err()
                || library.games.contains_key(&id)
                || active_game_ids.iter().any(|gid| gid == &id)
                || active_targets.iter().any(|t| t == &entry.path())
            {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            pending.push(json!({
                "id": id,
                "directory": entry.path(),
            }));
        }
    }
    pending.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(pending)
}

fn progress_jobs(state: &AppState, download_jobs: Vec<azdl::jobs::DownloadJob>) -> Vec<Value> {
    download_jobs
        .into_iter()
        .take(100)
        .map(|j| {
            let status_str = match j.effective_status() {
                azdl::jobs::JobStatus::Running => {
                    if j.effective_install_state() == Some(azdl::jobs::InstallState::Installing) {
                        "Installing".to_string()
                    } else {
                        "Downloading".to_string()
                    }
                }
                s => format!("{:?}", s),
            };

            let install_json = j.install.as_ref().map(|p| {
                let meta = state.metadata_cache.lock().ok().and_then(|c| {
                    c.get(&p.title.to_lowercase())
                        .or_else(|| c.get(&p.game_id.to_lowercase()))
                        .cloned()
                });
                let steam_id = meta
                    .as_ref()
                    .and_then(|m| m.steam_app_id.map(|id| id.to_string()))
                    .or_else(|| parse_steam_id(&p.game_id));
                let banner = meta
                    .as_ref()
                    .and_then(|m| m.banner_url.clone())
                    .or_else(|| steam_id.as_ref().map(|id| format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{id}/header.jpg")));
                let cover = meta
                    .as_ref()
                    .and_then(|m| m.cover_url.clone())
                    .or_else(|| steam_id.as_ref().map(|id| format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{id}/library_600x900.jpg")));

                json!({
                    "id": p.game_id,
                    "title": p.title,
                    "banner_url": banner,
                    "cover_url": cover,
                    "file_size": None::<String>,
                })
            });

            let total_bytes = j.total_bytes;

            json!({
                "id": j.id,
                "kind": format!("{:?}", j.kind),
                "status": status_str,
                "bytes": j.downloaded_bytes(),
                "total_bytes": total_bytes,
                "total": total_bytes,
                "estimated_bytes": None::<u64>,
                "target": j.target,
                "error": j.error,
                "install": install_json,
                "install_state": j.effective_install_state().map(|s| format!("{:?}", s)),
                "install_error": j.install_error,
            })
        })
        .collect()
}

pub fn handle_get_state(state: &AppState) -> Result<Value, String> {
    let mut library = load_library_snapshot(&state.data_dir, &state.library_cache)?;
    if library
        .games
        .values()
        .any(|game| !game.install_dir.exists())
    {
        let pruned_library = azdl::Library {
            games: library
                .games
                .iter()
                .filter(|(_, game)| game.install_dir.exists())
                .map(|(id, game)| (id.clone(), game.clone()))
                .collect(),
        };
        let saved = save_library(&state.data_dir, &pruned_library).is_ok();
        let library_snapshot = Arc::new(pruned_library);
        if saved
            && let Ok(fingerprint) = library_fingerprint(&state.data_dir)
            && let Ok(mut cache) = state.library_cache.lock()
        {
            *cache = Some(CachedLibrary {
                fingerprint,
                library: Arc::clone(&library_snapshot),
            });
        }
        library = library_snapshot;
    }
    state
        .dir_size_cache
        .retain(library.games.values().map(|game| game.install_dir.clone()));
    let running = state
        .running_games
        .lock()
        .map_err(|_| "State lock poisoned")?
        .clone();
    let tasks = state
        .tasks
        .lock()
        .map_err(|_| "State lock poisoned")?
        .clone();
    let active_ids: Vec<String> = tasks
        .iter()
        .filter(|t| t.status == "running" || t.status == "working")
        .filter_map(|t| t.game_id.clone())
        .collect();
    let download_jobs = list_jobs(&state.data_dir).unwrap_or_default();
    let active_targets: Vec<PathBuf> = download_jobs
        .iter()
        .filter(|job| {
            matches!(
                job.effective_status(),
                azdl::jobs::JobStatus::Running | azdl::jobs::JobStatus::Queued
            )
        })
        .map(|job| job.target.clone())
        .collect();
    let pending = pending_installs(&state.data_dir, &library, &active_ids, &active_targets)
        .unwrap_or_default();

    let games: Vec<Value> = library
        .games
        .values()
        .map(|game| {
            let is_running = running.contains_key(&game.id);

            let active_op = if is_running {
                Some("running")
            } else if tasks
                .iter()
                .any(|t| t.game_id.as_deref() == Some(&game.id) && t.status == "running")
            {
                Some("working")
            } else {
                None
            };

            let source_id = game
                .origin
                .as_ref()
                .and_then(|o| o.qualified_id.split_once(':').map(|(s, _)| s.to_string()))
                .or_else(|| game.id.split_once(':').map(|(s, _)| s.to_string()));

            let version = game.origin.as_ref().and_then(|o| o.version.clone());

            let genres: Vec<String> = Vec::new();
            let addons_count = 0;

            let updated = state.clone();
            let dir_bytes = state
                .dir_size_cache
                .get(&game.install_dir, move || updated.notify_state_changed());
            let file_size = dir_bytes.map(format_file_size);

            let meta_entry = {
                let lock = state.metadata_cache.lock().ok();
                lock.and_then(|cache| {
                    cache
                        .get(&game.title.to_lowercase())
                        .or_else(|| cache.get(&game.id.to_lowercase()))
                        .cloned()
                })
                .filter(|entry| !is_guessed_steam_artwork(entry))
            };

            let steam_id = game.steam_app_id.map(|id| id.to_string()).or_else(|| {
                meta_entry
                    .as_ref()
                    .and_then(|m| m.steam_app_id.map(|id| id.to_string()))
                    .or_else(|| {
                        game.origin
                            .as_ref()
                            .and_then(|o| parse_steam_id(&o.qualified_id))
                    })
                    .or_else(|| parse_steam_id(&game.id))
            });

            let raw_banner_url = game
                .banner_url
                .clone()
                .or_else(|| meta_entry.as_ref().and_then(|m| m.banner_url.clone()));

            let raw_cover_url = game
                .cover_url
                .clone()
                .or_else(|| meta_entry.as_ref().and_then(|m| m.cover_url.clone()));

            let disk_banner = steam_id
                .as_deref()
                .and_then(|sid| get_cached_artwork_uri(&state.data_dir, sid, "banner"))
                .or_else(|| get_cached_artwork_uri(&state.data_dir, &game.id, "banner"));

            let disk_cover = steam_id
                .as_deref()
                .and_then(|sid| get_cached_artwork_uri(&state.data_dir, sid, "cover"))
                .or_else(|| get_cached_artwork_uri(&state.data_dir, &game.id, "cover"));

            let banner_url = disk_banner.or(raw_banner_url.clone());
            let cover_url = disk_cover.or(raw_cover_url.clone());

            if let Some(url) = raw_banner_url.clone()
                && get_cached_artwork_path(
                    &state.data_dir,
                    steam_id.as_deref().unwrap_or(&game.id),
                    "banner",
                )
                .is_none()
            {
                let data_dir = state.data_dir.clone();
                let key = steam_id.clone().unwrap_or_else(|| game.id.clone());
                std::thread::spawn(move || {
                    cache_artwork_image(&data_dir, &key, "banner", &url);
                });
            }
            if let Some(url) = raw_cover_url.clone()
                && get_cached_artwork_path(
                    &state.data_dir,
                    steam_id.as_deref().unwrap_or(&game.id),
                    "cover",
                )
                .is_none()
            {
                let data_dir = state.data_dir.clone();
                let key = steam_id.clone().unwrap_or_else(|| game.id.clone());
                std::thread::spawn(move || {
                    cache_artwork_image(&data_dir, &key, "cover", &url);
                });
            }

            if (raw_banner_url.is_none() || raw_cover_url.is_none()) && meta_entry.is_none() {
                let title_clone = game.title.clone();
                let lookup_query = steam_id.clone().unwrap_or_else(|| title_clone.clone());
                let pending_key = title_clone.to_lowercase();
                let should_spawn = state.metadata_pending.lock().is_ok_and(|mut pending| {
                    reserve_metadata_lookup(&mut pending, pending_key.clone())
                });
                if should_spawn {
                    let state_clone = state.clone();
                    let task_key = pending_key.clone();
                    let spawn_result = std::thread::Builder::new()
                        .name(format!("azdl-art-{}", game.id))
                        .spawn(move || {
                            let entry = resolve_steam_metadata(&lookup_query).unwrap_or(
                                MetadataCacheEntry {
                                    steam_app_id: None,
                                    banner_url: None,
                                    cover_url: None,
                                },
                            );
                            if let Some(app_id) = entry.steam_app_id {
                                if let Some(ref b) = entry.banner_url {
                                    cache_artwork_image(
                                        &state_clone.data_dir,
                                        &app_id.to_string(),
                                        "banner",
                                        b,
                                    );
                                }
                                if let Some(ref c) = entry.cover_url {
                                    cache_artwork_image(
                                        &state_clone.data_dir,
                                        &app_id.to_string(),
                                        "cover",
                                        c,
                                    );
                                }
                            }
                            if let Ok(mut cache) = state_clone.metadata_cache.lock() {
                                insert_metadata_cache(&mut cache, task_key.clone(), entry);
                                save_metadata_cache(&state_clone.data_dir, &cache);
                            }
                            if let Ok(mut pending) = state_clone.metadata_pending.lock() {
                                pending.remove(&task_key);
                            }
                        });
                    if spawn_result.is_err()
                        && let Ok(mut pending) = state.metadata_pending.lock()
                    {
                        pending.remove(&pending_key);
                    }
                }
            }

            let prefix = state.data_dir.join("prefixes").join(&game.id);
            let dependencies = if prefix.is_dir() {
                Some(azdl::dependencies::diagnose_prefix_dependencies(&prefix))
            } else {
                None
            };

            // A bundled multiplayer fix is staged through the same patch records
            // as a user patch, but should not surface as a user-managed patch.
            let patch = active_patch(&state.data_dir, &game.id).ok().flatten();
            let custom_patch = patch.filter(|p| {
                !p.entries
                    .iter()
                    .any(|e| e.path == Path::new(".azdl-bundled-fix"))
            });
            let fix = detect_fix_cached(state, game).map(|kind| match kind {
                azdl::multiplayer::FixKind::OnlineFix => "Online-Fix",
                azdl::multiplayer::FixKind::SteamFix => "SteamFix",
                azdl::multiplayer::FixKind::None => "None",
            });

            json!({
                "id": game.id,
                "title": game.title,
                "install_dir": game.install_dir,
                "executable": game.executable,
                "runner": runner_str(&game.runner),
                "arguments": game.arguments,
                "launch_options": game.launch_options,
                "selected_launch_option_id": game.selected_launch_option_id,
                "environment": game.environment,
                "origin": game.origin.as_ref().map(|o| &o.qualified_id),
                "source_id": source_id,
                "steam_app_id": game.steam_app_id.or_else(|| {
                    steam_id.as_ref().and_then(|id| id.parse::<u32>().ok())
                }),
                "version": version,
                "file_size": file_size,
                "size_bytes": dir_bytes,
                "genres": genres,
                "addons_count": addons_count,
                "last_played": game.last_played_unix,
                "patch": custom_patch.map(|p| if p.enabled { "enabled" } else { "disabled" }),
                "cover_url": cover_url,
                "banner_url": banner_url,
                "fix": fix,
                "steamstub": scan_steamstub_cached(state, game),
                "ea_heal_applied": azdl::fixes::has_ea_backup(&game.install_dir),
                "ea_heal_available": azdl::fixes::has_ea_target(&game.install_dir),
                "dlc_patch_applied": azdl::fixes::has_dlc_installed_patch(&game.install_dir),
                "dlc_patch_available": azdl::fixes::has_dlc_installed_target(&game.install_dir),
                "goldberg_compatible": azdl::goldberg::is_goldberg_compatible(&game.install_dir),
                "goldberg_has_settings": azdl::goldberg::has_goldberg_settings(&game.install_dir),
                "dependencies": dependencies,
                "is_running": is_running,
                "active_operation": active_op,
            })
        })
        .collect();

    let jobs_list = progress_jobs(state, download_jobs);
    let running_map: HashMap<String, RunningGameInfo> = running;
    let tasks_list: Vec<TaskInfo> = tasks;
    let settings = azdl::settings::load_settings(&state.data_dir)
        .unwrap_or_else(|_| azdl::settings::LauncherSettings::default_for(&state.data_dir));
    let diagnostics = {
        let mut lock = state
            .cached_diagnostics
            .lock()
            .map_err(|_| "State lock poisoned")?;
        if let Some(cached) = &*lock {
            cached.clone()
        } else {
            let diag = azdl::doctor::system_diagnostics();
            *lock = Some(diag.clone());
            diag
        }
    };

    Ok(json!({
        "library": games,
        "jobs": jobs_list,
        "running_games": running_map,
        "tasks": tasks_list,
        "pending": pending,
        "settings": settings,
        "diagnostics": diagnostics,
        "data_dir": state.data_dir,
        "platform": if cfg!(windows) { "windows" } else { "linux" },
    }))
}

/// Transfer updates exclude library scans, settings, and diagnostics.
pub fn handle_get_progress(state: &AppState) -> Result<Value, String> {
    let jobs = list_jobs(&state.data_dir).map_err(|error| error.to_string())?;
    let tasks = state
        .tasks
        .lock()
        .map_err(|_| "State lock poisoned")?
        .clone();
    let running = state
        .running_games
        .lock()
        .map_err(|_| "State lock poisoned")?
        .clone();
    Ok(json!({
        "jobs": progress_jobs(state, jobs),
        "tasks": tasks,
        "running_games": running,
    }))
}

#[tauri::command]
pub async fn get_progress(state: State<'_, AppState>) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_get_progress(&state))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn get_state(state: State<'_, AppState>) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_get_state(&state))
        .await
        .map_err(|e| e.to_string())?
}

pub fn handle_get_runners(state: &AppState) -> Result<Vec<RunnerEntry>, String> {
    #[allow(unused_mut)]
    let mut list = vec![
        RunnerEntry {
            id: "auto".into(),
            name: "Auto (Settings default)".into(),
            runner_type: "auto".into(),
            path: None,
            source: None,
        },
        RunnerEntry {
            id: "native".into(),
            name: "Native Host Execution".into(),
            runner_type: "native".into(),
            path: None,
            source: None,
        },
        RunnerEntry {
            id: "wine".into(),
            name: "System Wine".into(),
            runner_type: "wine".into(),
            path: None,
            source: None,
        },
    ];

    #[cfg(unix)]
    {
        list.push(RunnerEntry {
            id: "proton:auto".into(),
            name: "Proton Auto (Preferred GE/Proton)".into(),
            runner_type: "proton_auto".into(),
            path: None,
            source: None,
        });

        if let Ok(protons) = azdl::runtime::discover_proton(&state.data_dir) {
            for p in protons {
                list.push(RunnerEntry {
                    id: format!("proton:{}", p.path.display()),
                    name: format!("{} ({})", p.name, p.source),
                    runner_type: "proton".into(),
                    path: Some(p.path),
                    source: Some(p.source),
                });
            }
        }
    }

    #[cfg(not(unix))]
    let _ = state;

    Ok(list)
}

#[tauri::command]
pub async fn get_runners(state: State<'_, AppState>) -> Result<Vec<RunnerEntry>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_get_runners(&state))
        .await
        .map_err(|e| e.to_string())?
}

pub(crate) fn steam_games_to_values(
    library: &Library,
    discovered: Vec<azdl::steam_discovery::SteamStoreGame>,
) -> Value {
    let games: Vec<Value> = discovered
        .into_iter()
        .map(|g| {
            let is_inst = library.games.values().any(|installed| {
                installed.origin.as_ref().is_some_and(|origin| {
                    origin.qualified_id.ends_with(&format!(":steam-{}", g.app_id))
                }) || installed.id == format!("steam-{}", g.app_id)
            });
            let default_banner = format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{}/header.jpg", g.app_id);
            let banner = g.header_image.clone().or_else(|| g.large_capsule_image.clone()).unwrap_or(default_banner);
            json!({
                "source_id": "steam",
                "id": format!("steam-{}", g.app_id),
                "qualified_id": format!("steam:{}", g.app_id),
                "title": g.title,
                "description": "",
                "cover_url": format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{}/library_600x900.jpg", g.app_id),
                "banner_url": banner,
                "header_image": g.header_image,
                "large_capsule_image": g.large_capsule_image,
                "capsule_url": g.large_capsule_image,
                "small_capsule_image": g.small_capsule_image,
                "tiny_image": g.tiny_image,
                "genres": [],
                "file_size": g.storage_size,
                "version": null,
                "upload_date": null,
                "addons_count": 0,
                "steam_app_id": g.app_id,
                "is_installed": is_inst,
            })
        })
        .collect();
    json!(games)
}

pub fn handle_search_steam_games(state: &AppState, query: String) -> Result<Value, String> {
    let library = load_library_snapshot(&state.data_dir, &state.library_cache)?;
    let discovered = azdl::steam_discovery::discover_games(&query).map_err(|e| e.to_string())?;
    let mut games = steam_games_to_values(&library, discovered);
    if !query.trim().is_empty()
        && let Some(items) = games.as_array_mut()
    {
        for batch in items.chunks_mut(4).take(3) {
            let identities = std::thread::scope(|scope| {
                let handles = batch
                    .iter()
                    .filter_map(|item| item["steam_app_id"].as_u64())
                    .filter_map(|id| u32::try_from(id).ok())
                    .map(|id| scope.spawn(move || azdl::steam_metadata::fetch_appinfo(id).ok()))
                    .collect::<Vec<_>>();
                handles
                    .into_iter()
                    .map(|handle| handle.join().ok().flatten())
                    .collect::<Vec<_>>()
            });
            for (item, identity) in batch.iter_mut().zip(identities) {
                if let Some(identity) = identity {
                    if identity
                        .app_type
                        .as_deref()
                        .is_some_and(|kind| kind.eq_ignore_ascii_case("game"))
                    {
                        item["kind"] = json!("game");
                    } else if identity
                        .app_type
                        .as_deref()
                        .is_some_and(|kind| kind.eq_ignore_ascii_case("dlc"))
                        && let Some(parent_id) = identity.parent_app_id
                    {
                        item["kind"] = json!("dlc");
                        item["parent_app_id"] = json!(parent_id);
                        item["parent_title"] = json!(
                            azdl::steam_metadata::fetch_appinfo(parent_id)
                                .ok()
                                .and_then(|metadata| metadata.name)
                        );
                    }
                }
            }
        }
    }
    Ok(games)
}

#[tauri::command]
pub async fn search_steam_games(
    state: State<'_, AppState>,
    query: String,
) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_search_steam_games(&state, query))
        .await
        .map_err(|e| e.to_string())?
}

pub fn handle_resolve_game_artwork(state: &AppState, query: String) -> Result<Value, String> {
    let key = query.trim().to_lowercase();
    if key.is_empty() {
        return Ok(json!({ "steam_app_id": null, "banner_url": null, "cover_url": null }));
    }
    if let Ok(cache) = state.metadata_cache.lock()
        && let Some(entry) = cache.get(&key)
        && !is_guessed_steam_artwork(entry)
    {
        return serde_json::to_value(entry).map_err(|e| e.to_string());
    }
    let entry = resolve_steam_metadata(&query).unwrap_or(MetadataCacheEntry {
        steam_app_id: None,
        banner_url: None,
        cover_url: None,
    });
    if let Ok(mut cache) = state.metadata_cache.lock() {
        insert_metadata_cache(&mut cache, key, entry.clone());
        save_metadata_cache(&state.data_dir, &cache);
    }
    if let Some(app_id) = entry.steam_app_id {
        let data_dir = state.data_dir.clone();
        let b_url = entry.banner_url.clone();
        let c_url = entry.cover_url.clone();
        std::thread::spawn(move || {
            if let Some(url) = b_url {
                cache_artwork_image(&data_dir, &app_id.to_string(), "banner", &url);
            }
            if let Some(url) = c_url {
                cache_artwork_image(&data_dir, &app_id.to_string(), "cover", &url);
            }
        });
    }
    serde_json::to_value(&entry).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn resolve_game_artwork(
    state: State<'_, AppState>,
    query: String,
) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_resolve_game_artwork(&state, query))
        .await
        .map_err(|e| e.to_string())?
}

pub fn handle_diagnose(state: &AppState, id: String) -> Result<Value, String> {
    let library = load_library_snapshot(&state.data_dir, &state.library_cache)?;
    let game = library
        .games
        .get(&id)
        .ok_or_else(|| format!("Game {id} not found"))?;
    let report = diagnose_game(game, &state.data_dir);
    serde_json::to_value(&report).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn diagnose(state: State<'_, AppState>, id: String) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_diagnose(&state, id))
        .await
        .map_err(|e| e.to_string())?
}

pub fn handle_discover_exes(state: &AppState, id: String) -> Result<Value, String> {
    let library = load_library_snapshot(&state.data_dir, &state.library_cache)?;
    if let Some(game) = library.games.get(&id) {
        let candidates =
            discover_executables(&game.install_dir, &game.title).map_err(|e| e.to_string())?;
        let list: Vec<Value> = candidates
            .into_iter()
            .map(|c| {
                json!({
                    "path": c.path,
                    "relative_path": c.relative_path,
                    "score": c.score,
                })
            })
            .collect();
        return Ok(json!(list));
    }

    // Installed files waiting for an executable choice live under a storage drive root.
    validate_id(&id).map_err(|e| e.to_string())?;
    let mut roots = vec![state.data_dir.join("games")];
    if let Ok(settings) = azdl::settings::load_settings(&state.data_dir) {
        roots.extend(settings.storage_drives.into_iter().map(|drive| drive.path));
    }
    if let Some(pending_dir) = roots
        .into_iter()
        .map(|root| root.join(&id))
        .find(|dir| dir.is_dir())
    {
        let candidates = discover_executables(&pending_dir, &id).map_err(|e| e.to_string())?;
        let list: Vec<Value> = candidates
            .into_iter()
            .map(|c| {
                json!({
                    "path": c.path,
                    "relative_path": c.relative_path,
                    "score": c.score,
                })
            })
            .collect();
        return Ok(json!(list));
    }

    Err(format!("Game {id} not found"))
}

#[tauri::command]
pub async fn discover_exes(state: State<'_, AppState>, id: String) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_discover_exes(&state, id))
        .await
        .map_err(|e| e.to_string())?
}

pub fn handle_launch_game(
    state: &AppState,
    id: String,
    extra_args: Option<Vec<String>>,
) -> Result<Value, LauncherError> {
    validate_id(&id).map_err(|e| LauncherError::invalid_id(e.to_string()))?;
    let _content_lock = azdl::dlc::lock_game(&state.data_dir, &id)
        .map_err(|e| LauncherError::general(e.to_string()))?;

    let mut library =
        load_library(&state.data_dir).map_err(|e| LauncherError::general(e.to_string()))?;
    let game = library
        .games
        .get_mut(&id)
        .ok_or_else(|| LauncherError::game_not_found(&id))?;

    {
        let running = state
            .running_games
            .lock()
            .map_err(|_| LauncherError::lock_poisoned("running_games state lock poisoned"))?;
        if running.contains_key(&id) {
            return Err(LauncherError::game_already_running(&id));
        }
    }
    if state
        .tasks
        .lock()
        .map_err(|_| LauncherError::lock_poisoned("tasks state lock poisoned"))?
        .iter()
        .any(|task| {
            task.status == "running"
                && task.game_id.as_deref() == Some(id.as_str())
                && task.id.starts_with("dlc-")
        })
    {
        return Err(LauncherError::general(
            "Wait for the DLC operation to finish before launching this game",
        ));
    }

    let extra = extra_args.unwrap_or_default();
    let mut child = spawn_game(game, &state.data_dir, &extra).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("Compatibility tool") || msg.contains("runner") {
            LauncherError::runner_not_found(msg)
        } else if msg.contains("executable") || msg.contains("not found") {
            LauncherError::executable_not_found(game.executable.display().to_string())
        } else if msg.contains("Steam client directory is missing") {
            LauncherError::steam_not_running(&game.title)
        } else {
            LauncherError::general(msg)
        }
    })?;
    let pid = child.id();
    let game_title = game.title.clone();
    let probe_hints = azdl::wine_probe::WineProbeHints {
        install_dir: Some(game.install_dir.clone()),
        executable_name: game
            .executable
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string),
        prefix_dir: Some(state.data_dir.join("prefixes").join(&id)),
        fake_app_id: None,
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let _ = save_library(&state.data_dir, &library);

    {
        let mut running = state
            .running_games
            .lock()
            .map_err(|_| LauncherError::lock_poisoned("running_games state lock poisoned"))?;
        running.insert(
            id.clone(),
            RunningGameInfo {
                id: id.clone(),
                title: game_title.clone(),
                pid,
                started_at: now,
            },
        );
    }

    let running_map = Arc::clone(&state.running_games);
    let monitored_id = id.clone();
    let data_dir = state.data_dir.clone();
    let state_exit = state.clone();
    std::thread::spawn(move || {
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    let status = azdl::wine_probe::check_game_status(pid, &probe_hints);
                    if let azdl::wine_probe::GameRunStatus::Crashed {
                        ref reason,
                        culprit_pid: _,
                    } = status
                    {
                        eprintln!(
                            "Crash detected for game {monitored_id}: {reason}. Terminating hanging process tree."
                        );
                        azdl::wine_probe::terminate_game_tree(pid, &probe_hints);
                        let _ = child.wait();
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                }
                Err(_) => break,
            }
        }
        if let Ok(mut map) = running_map.lock() {
            map.remove(&monitored_id);
        }
        if let Ok(mut lib) = load_library(&data_dir)
            && let Some(g) = lib.games.get_mut(&monitored_id)
        {
            let exit_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .ok();
            g.last_played_unix = exit_time;
            let _ = save_library(&data_dir, &lib);
        }
        state_exit.notify_state_changed();
    });

    state.notify_state_changed();

    Ok(json!({
        "status": "launched",
        "id": id,
        "title": game_title,
        "pid": pid,
    }))
}

#[tauri::command]
pub async fn launch_game(
    state: State<'_, AppState>,
    id: String,
    extra_args: Option<Vec<String>>,
) -> Result<Value, LauncherError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_launch_game(&state, id, extra_args))
        .await
        .map_err(|e| LauncherError::general(e.to_string()))?
}

#[cfg(unix)]
fn signal_process_group(pid: u32) -> Result<(), String> {
    if let Ok(status) = azdl::clean_command("kill")
        .args(["-TERM", "--"])
        .arg(format!("-{pid}"))
        .status()
        && status.success()
    {
        return Ok(());
    }

    let status = azdl::clean_command("kill")
        .args(["-TERM", "--"])
        .arg(pid.to_string())
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        let check = azdl::clean_command("kill")
            .args(["-0", "--"])
            .arg(pid.to_string())
            .status();
        if let Ok(c) = check
            && !c.success()
        {
            return Ok(());
        }
        return Err(format!("could not stop process {pid}: {status}"));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_process_has_exited(pid: u32) -> bool {
    use std::ffi::c_void;

    if pid == 0 {
        return false;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    const SYNCHRONIZE: u32 = 0x0010_0000;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const WAIT_OBJECT_0: u32 = 0;
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return unsafe { GetLastError() == ERROR_INVALID_PARAMETER };
    }
    let exited = unsafe { WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 };
    unsafe { CloseHandle(handle) };
    exited
}

#[cfg(windows)]
fn signal_process_group(pid: u32) -> Result<(), String> {
    let status = azdl::clean_command("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() && !windows_process_has_exited(pid) {
        return Err(format!("could not stop process group {pid}: {status}"));
    }
    Ok(())
}

#[cfg(all(test, windows))]
#[test]
fn windows_stop_accepts_an_exited_process_without_treating_live_processes_as_exited() {
    assert!(!windows_process_has_exited(0));
    assert!(!windows_process_has_exited(std::process::id()));
    assert!(windows_process_has_exited(u32::MAX));
    signal_process_group(u32::MAX).unwrap();
}

#[cfg(not(any(unix, windows)))]
fn signal_process_group(_pid: u32) -> Result<(), String> {
    Err("stopping a launch is unsupported on this platform".into())
}

pub fn handle_stop_game(state: &AppState, id: String) -> Result<Value, LauncherError> {
    validate_id(&id).map_err(|e| LauncherError::invalid_id(e.to_string()))?;

    let pid = {
        let running = state
            .running_games
            .lock()
            .map_err(|_| LauncherError::lock_poisoned("running_games state lock poisoned"))?;
        let info = running
            .get(&id)
            .ok_or_else(|| LauncherError::game_not_found(&id))?;
        info.pid
    };

    signal_process_group(pid).map_err(|e| LauncherError::general(e.to_string()))?;

    if let Ok(lib) = load_library(&state.data_dir)
        && let Some(game) = lib.games.get(&id)
    {
        let hints = azdl::wine_probe::WineProbeHints {
            install_dir: Some(game.install_dir.clone()),
            executable_name: game
                .executable
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string),
            prefix_dir: Some(state.data_dir.join("prefixes").join(&id)),
            fake_app_id: None,
        };
        azdl::wine_probe::terminate_game_tree(pid, &hints);
    }

    if let Ok(mut map) = state.running_games.lock() {
        map.remove(&id);
    }

    state.notify_state_changed();

    Ok(json!({
        "status": "stopped",
        "id": id,
        "pid": pid,
    }))
}

#[tauri::command]
pub async fn stop_game(state: State<'_, AppState>, id: String) -> Result<Value, LauncherError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_stop_game(&state, id))
        .await
        .map_err(|e| LauncherError::general(e.to_string()))?
}

fn ensure_dlc_mutation_allowed(state: &AppState, game_id: &str) -> azdl::Result<()> {
    if state
        .running_games
        .lock()
        .map_err(|_| "Running game state lock poisoned")?
        .contains_key(game_id)
    {
        return Err("Stop the game before changing DLC".into());
    }
    if state
        .tasks
        .lock()
        .map_err(|_| "Tasks state lock poisoned")?
        .iter()
        .any(|task| {
            task.status == "running"
                && task.game_id.as_deref() == Some(game_id)
                && task.id.starts_with("dlc-")
        })
    {
        return Err("Wait for the DLC operation to finish before changing DLC".into());
    }
    Ok(())
}

pub fn handle_run_action(state: &AppState, action: ActionRequest) -> Result<Value, LauncherError> {
    let data_dir = &state.data_dir;
    let content_path = match &action {
        Action::DlcRemove { id, .. } | Action::DlcRecover { id, .. } => load_library(data_dir)
            .ok()
            .and_then(|library| library.games.get(id).map(|game| game.install_dir.clone())),
        _ => None,
    };
    let res = match action {
        Action::Launch { id, arguments } => handle_launch_game(state, id, arguments),
        Action::Stop { id } => handle_stop_game(state, id),
        Action::RunnerSet { id, runner } => {
            let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
            let game = library
                .games
                .get_mut(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            set_runner(game, &runner).map_err(|e| e.to_string())?;
            save_library(data_dir, &library).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "runner": runner }))
        }
        Action::ExecutableSet { id, executable } => {
            let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
            let game = library
                .games
                .get_mut(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            set_executable(game, Path::new(&executable)).map_err(|e| e.to_string())?;
            save_library(data_dir, &library).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "executable": executable }))
        }
        Action::GameRemove { id } => {
            let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
            azdl::remove_game_with_guard(&mut library, data_dir, &id, || {
                let tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
                if let Some(task) = tasks.iter().find(|task| {
                    task.game_id.as_deref() == Some(&id)
                        && task.status == "running"
                        && (task.id.starts_with("dlc-") || task.id.starts_with("move-"))
                }) {
                    return Err(if task.id.starts_with("dlc-") {
                        "Wait for the DLC install to finish before removing this game"
                    } else {
                        "Wait for the game move to finish before removing this game"
                    }
                    .into());
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
            if let Ok(mut tasks) = state.tasks.lock() {
                tasks.retain(|t| {
                    t.id != format!("install-{id}") && t.game_id.as_deref() != Some(&id)
                });
            }
            state.dir_size_cache.invalidate_all();
            Ok(json!({ "success": true, "removed": id }))
        }
        Action::DlcInstall { id, dlc_id } => {
            let content_lock =
                azdl::dlc::lock_game(data_dir, &id).map_err(|error| error.to_string())?;
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            if state
                .running_games
                .lock()
                .map_err(|_| "Running game state lock poisoned")?
                .contains_key(&id)
            {
                return Err("Stop the game before installing DLC".into());
            }
            let steam_dlc_id = dlc_id
                .parse::<u32>()
                .map_err(|_| "DLC must be identified by its Steam App ID")?;
            let task_id = format!("dlc-{}-{dlc_id}", game.id);
            {
                let mut tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
                if tasks
                    .iter()
                    .any(|task| task.id == format!("move-{id}") && task.status == "running")
                {
                    return Err("Wait for the game move to finish before installing DLC".into());
                }
                if tasks
                    .iter()
                    .any(|task| task.id == task_id && task.status == "running")
                {
                    return Ok(
                        json!({ "success": true, "already_working": true, "task_id": task_id }),
                    );
                }
                tasks.retain(|task| task.id != task_id);
                tasks.push(TaskInfo {
                    id: task_id.clone(),
                    label: format!("Installing DLC for {}", game.title),
                    status: "running".into(),
                    pid: None,
                    game_id: Some(game.id.clone()),
                    log_tail: String::new(),
                });
            }
            drop(content_lock);
            let game = game.clone();
            let state_clone = state.clone();
            let task_id_clone = task_id.clone();
            let thread_name = format!("azdl-dlc-{}", game.id);
            std::thread::Builder::new()
                .name(thread_name)
                .spawn(move || {
                    let result = azdl::steam_depot::install_steam_dlc(
                        &game,
                        &state_clone.data_dir,
                        steam_dlc_id,
                    );
                    finish_dlc_install_task(&state_clone, &task_id_clone, result);
                })
                .map_err(|error| {
                    finish_install_task(state, &task_id, Some(error.to_string()));
                    error.to_string()
                })?;
            Ok(json!({ "success": true, "started": true, "task_id": task_id }))
        }
        Action::DlcRemove { id, dlc_id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            azdl::dlc::remove_with_guard(game, data_dir, &dlc_id, || {
                ensure_dlc_mutation_allowed(state, &id)
            })?;
            Ok(json!({ "success": true }))
        }
        Action::DlcRecover { id, dlc_id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            azdl::dlc::recover_interrupted_with_guard(game, data_dir, &dlc_id, || {
                ensure_dlc_mutation_allowed(state, &id)
            })?;
            Ok(json!({ "success": true }))
        }
        Action::OpenFolder { id, location } => {
            let target_path = if let Some(p) = location {
                PathBuf::from(p)
            } else if let Some(id) = id {
                let library = load_library(data_dir)?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| LauncherError::game_not_found(&id))?;
                game.install_dir.clone()
            } else {
                return Err(LauncherError::general("missing game ID or location"));
            };

            if !target_path.exists() {
                return Err(LauncherError::executable_not_found(
                    target_path.display().to_string(),
                ));
            }

            #[cfg(target_os = "windows")]
            {
                let _ = azdl::clean_command("explorer")
                    .arg(&target_path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = azdl::clean_command("xdg-open")
                    .arg(&target_path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }

            Ok(json!({ "success": true, "path": target_path.display().to_string() }))
        }
        Action::PendingAdd {
            id,
            title,
            install_dir,
            executable,
        } => {
            let game_title = title.unwrap_or_else(|| id.clone());
            let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
            add_game(
                &mut library,
                &id,
                &game_title,
                Path::new(&install_dir),
                Path::new(&executable),
            )
            .map_err(|e| e.to_string())?;
            if let Some(game) = library.games.get_mut(&id) {
                azdl::steam_install::attach_pending_origin(data_dir, game)
                    .map_err(|e| e.to_string())?;
            }
            save_library(data_dir, &library).map_err(|e| e.to_string())?;
            if let Err(error) = azdl::steam_install::clear_pending_origin(data_dir, &id) {
                eprintln!("Game saved; pending install cleanup failed: {error}");
            }
            if let Err(error) = azdl::jobs::mark_install_registered(data_dir, &id) {
                eprintln!("Game added, but install job status could not be updated: {error}");
            }
            Ok(json!({ "success": true, "added": id }))
        }
        Action::PendingRemove { id, install_dir } => {
            validate_id(&id).map_err(|e| e.to_string())?;

            if let Some(dir_str) = install_dir.as_deref() {
                let dir = Path::new(dir_str);
                if dir.exists()
                    && let Ok(canon) = fs::canonicalize(dir)
                {
                    let is_root = canon.parent().is_none();
                    let is_data_dir = fs::canonicalize(data_dir).is_ok_and(|d| canon == d);
                    let is_home = std::env::var_os("HOME")
                        .or_else(|| std::env::var_os("USERPROFILE"))
                        .is_some_and(|h| canon.as_os_str() == h);
                    let components_count = canon.components().count();
                    if !is_root && !is_data_dir && !is_home && components_count >= 2 {
                        let _ = fs::remove_dir_all(&canon);
                    }
                }
            }

            let games_dir = data_dir.join("games").join(&id);
            if games_dir.is_dir() {
                let _ = fs::remove_dir_all(&games_dir);
            }
            let _ = azdl::steam_install::clear_pending_origin(data_dir, &id);
            let _ = azdl::cleanup_leftovers(data_dir, Some(&id));
            Ok(json!({ "success": true, "removed": id }))
        }
        Action::ShortcutCreate { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            #[cfg(target_os = "linux")]
            {
                let launcher = std::env::current_exe().map_err(|e| e.to_string())?;
                azdl::shortcuts::create_shortcut(game, data_dir, &launcher, None)
                    .map_err(|e| e.to_string())?;
            }
            #[cfg(not(target_os = "linux"))]
            let _ = game;
            Ok(json!({ "success": true }))
        }
        Action::ShortcutRemove { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            #[cfg(target_os = "linux")]
            {
                azdl::shortcuts::remove_shortcut(game, data_dir, None)
                    .map_err(|e| e.to_string())?;
            }
            #[cfg(not(target_os = "linux"))]
            let _ = game;
            Ok(json!({ "success": true }))
        }
        Action::SteamShortcutCreate { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            azdl::steam_shortcuts::create_shortcut(game, None).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true }))
        }
        Action::SteamShortcutRemove { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            azdl::steam_shortcuts::remove_shortcut(game, None).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true }))
        }
        Action::InstallVcredist { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let launch = azdl::resolve_launch(game)?;
            let runner = azdl::runtime::resolve_runner(&game.runner, &launch.executable, data_dir)?;
            let (prefix_dir, _) = azdl::dependencies::prefix_paths(data_dir, &game.id, &runner)
                .ok_or("Runtime dependencies are managed by the native host")?;
            let ok = azdl::dependencies::install_vcredist(&prefix_dir, &runner, data_dir, true)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "installed": ok }))
        }
        Action::InstallDirectx { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let launch = azdl::resolve_launch(game)?;
            let runner = azdl::runtime::resolve_runner(&game.runner, &launch.executable, data_dir)?;
            let (prefix_dir, _) = azdl::dependencies::prefix_paths(data_dir, &game.id, &runner)
                .ok_or("Runtime dependencies are managed by the native host")?;
            let ok = azdl::dependencies::install_directx(
                &prefix_dir,
                &runner,
                data_dir,
                &game.install_dir,
            )
            .map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "installed": ok }))
        }
        Action::JobPause { id } => {
            azdl::jobs::pause_job(data_dir, &id)?;
            Ok(json!({ "success": true }))
        }
        Action::JobResume { id } => {
            if let Err(error) = azdl::jobs::retry_job(data_dir, &id) {
                if error
                    .downcast_ref::<azdl::jobs::TransferStopped>()
                    .is_some()
                {
                    return Ok(json!({ "success": true, "stopped": true }));
                }
                return Err(error.into());
            }
            Ok(json!({ "success": true }))
        }
        Action::JobCancel { id } => {
            azdl::jobs::cancel_job(data_dir, &id)?;
            if let Ok(mut tasks) = state.tasks.lock() {
                tasks.retain(|t| t.id != id && t.game_id.as_deref() != Some(&id));
            }
            Ok(json!({ "success": true }))
        }
        Action::ArgsSet { id, arguments } => {
            let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
            let game = library
                .games
                .get_mut(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            game.arguments = arguments.unwrap_or_default();
            save_library(data_dir, &library).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true }))
        }
        Action::EnvSet { id, key, value } => {
            let val = value.unwrap_or_default();
            let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
            let game = library
                .games
                .get_mut(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            game.environment.insert(key, val);
            save_library(data_dir, &library).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true }))
        }
        Action::EnvRemove { id, key } => {
            let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
            let game = library
                .games
                .get_mut(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            game.environment.remove(&key);
            save_library(data_dir, &library).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true }))
        }
        Action::OptionAdd {
            id,
            option_id,
            title,
            executable,
            working_directory,
            arguments,
        } => {
            let working = working_directory.map(PathBuf::from);
            let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
            let game = library
                .games
                .get_mut(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            add_launch_option(
                game,
                &option_id,
                &title,
                Path::new(&executable),
                working.as_deref(),
                &arguments.unwrap_or_default(),
            )
            .map_err(|e| e.to_string())?;
            save_library(data_dir, &library).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true }))
        }
        Action::OptionSelect { id, option_id } => {
            let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
            let game = library
                .games
                .get_mut(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            select_launch_option(game, Some(&option_id)).map_err(|e| e.to_string())?;
            save_library(data_dir, &library).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true }))
        }
        Action::OptionRemove { id, option_id } => {
            let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
            let game = library
                .games
                .get_mut(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            remove_launch_option(game, &option_id).map_err(|e| e.to_string())?;
            save_library(data_dir, &library).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true }))
        }
        Action::SteamInstall {
            id,
            title,
            banner_url,
            cover_url,
            location,
        } => {
            let games_root = install_root_for(data_dir, location.as_deref())?;
            let cat_game = resolve_install_game(
                data_dir,
                &id,
                title.as_deref(),
                banner_url.as_deref(),
                cover_url.as_deref(),
            )
            .map_err(|e| e.to_string())?;

            // Prevent duplicate concurrent downloads for this title.
            let task_id = format!("install-{}", cat_game.id);
            {
                let tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
                if tasks.iter().any(|t| {
                    (t.id == task_id
                        || t.id == format!("install-{id}")
                        || t.game_id.as_deref() == Some(&cat_game.id)
                        || t.game_id.as_deref() == Some(&id))
                        && (t.status == "running" || t.status == "working")
                }) {
                    return Ok(json!({
                        "success": true,
                        "already_working": true,
                        "id": cat_game.id,
                        "title": cat_game.title,
                    }));
                }
            }
            if let Ok(jobs) = azdl::jobs::list_jobs(data_dir)
                && jobs.iter().any(|j| {
                    matches!(
                        j.effective_status(),
                        azdl::jobs::JobStatus::Running | azdl::jobs::JobStatus::Queued
                    ) && (j.install.as_ref().is_some_and(|i| i.game_id == cat_game.id)
                        || j.id == cat_game.id)
                })
            {
                return Ok(json!({
                    "success": true,
                    "already_working": true,
                    "id": cat_game.id,
                    "title": cat_game.title,
                }));
            }

            // Register before spawning so a fast failure cannot leave a stale running task.
            {
                let mut tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
                tasks.retain(|task| task.id != task_id);
                tasks.push(TaskInfo {
                    id: task_id.clone(),
                    label: format!("Downloading {}", cat_game.title),
                    status: "running".into(),
                    pid: None,
                    game_id: Some(cat_game.id.clone()),
                    log_tail: String::new(),
                });
            }

            // Spawn isolated background worker process (with thread fallback in tests or error).
            let state_clone = state.clone();
            let cat_game_clone = cat_game.clone();
            let task_id_clone = task_id.clone();
            let qualified_id = id.clone();
            let games_root_for_thread = games_root.clone();

            let is_test_env = cfg!(test) || std::env::var("AZDL_TEST_IN_PROCESS").is_ok();
            let mut spawned_pid: Option<u32> = None;

            if !is_test_env && let Ok(current_exe) = std::env::current_exe() {
                match azdl::clean_command(&current_exe)
                    .arg("worker-install")
                    .arg(&qualified_id)
                    .arg(title.as_deref().unwrap_or(""))
                    .arg(cat_game.banner_url.as_deref().unwrap_or(""))
                    .arg(cat_game.cover_url.as_deref().unwrap_or(""))
                    .arg(location.as_deref().unwrap_or(""))
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                {
                    Ok(child) => {
                        let pid = child.id();
                        spawned_pid = Some(pid);
                        if let Ok(mut tasks) = state.tasks.lock()
                            && let Some(task) = tasks.iter_mut().find(|task| task.id == task_id)
                        {
                            task.pid = Some(pid);
                        }
                        let wait_state = state_clone.clone();
                        let wait_task_id = task_id_clone.clone();
                        std::thread::Builder::new()
                            .name(format!("azdl-wait-{}", cat_game.id))
                            .spawn(move || {
                                let error = match child.wait_with_output() {
                                    Ok(output) if output.status.success() => None,
                                    Ok(output) => {
                                        let detail = String::from_utf8_lossy(&output.stderr);
                                        Some(if detail.trim().is_empty() {
                                            format!("Download worker failed ({})", output.status)
                                        } else {
                                            detail.trim().to_string()
                                        })
                                    }
                                    Err(error) => Some(format!("Download worker failed: {error}")),
                                };
                                finish_install_task(&wait_state, &wait_task_id, error);
                            })
                            .ok();
                    }
                    Err(e) => {
                        eprintln!("Failed to spawn worker process: {e}, falling back to thread");
                    }
                }
            }

            if spawned_pid.is_none() {
                let thread_name = format!("azdl-dl-{}", cat_game.id);
                std::thread::Builder::new()
                    .name(thread_name)
                    .spawn(move || {
                        let data_dir = &state_clone.data_dir;
                        let mut library = match load_library(data_dir) {
                            Ok(lib) => lib,
                            Err(e) => {
                                eprintln!("Failed to load library in background install: {e}");
                                finish_install_task(
                                    &state_clone,
                                    &task_id_clone,
                                    Some(e.to_string()),
                                );
                                return;
                            }
                        };

                        let outcome = azdl::steam_install::install_game_in(
                            &mut library,
                            data_dir,
                            &games_root_for_thread,
                            &cat_game_clone,
                        );
                        let mut failure = None;
                        match outcome {
                            Ok(azdl::steam_install::InstallOutcome::Installed(inst_id)) => {
                                if let Ok(mut fresh_library) = load_library(data_dir) {
                                    if let Some(installed_game) = library.games.remove(&inst_id) {
                                        fresh_library.games.insert(inst_id, installed_game);
                                        let _ = save_library(data_dir, &fresh_library);
                                    } else {
                                        let _ = save_library(data_dir, &library);
                                    }
                                } else {
                                    let _ = save_library(data_dir, &library);
                                }
                                let _ = azdl::cleanup_leftovers(data_dir, Some(&cat_game_clone.id));
                            }
                            Ok(azdl::steam_install::InstallOutcome::NeedsSelection(_)) => {
                                let _ = save_library(data_dir, &library);
                            }
                            Err(e) => {
                                eprintln!(
                                    "Background install failed for {}: {e}",
                                    cat_game_clone.title
                                );
                                if e.downcast_ref::<azdl::jobs::TransferStopped>().is_none() {
                                    failure = Some(e.to_string());
                                }
                            }
                        }
                        finish_install_task(&state_clone, &task_id_clone, failure);
                    })
                    .map_err(|error| {
                        finish_install_task(state, &task_id, Some(error.to_string()));
                        error.to_string()
                    })?;
            }

            Ok(json!({
                "success": true,
                "started": true,
                "id": id,
                "title": cat_game.title,
            }))
        }
        Action::OverlayToggle { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let status = overlay_status(game).map_err(|e| e.to_string())?;
            set_overlay_enabled(game, !status.enabled).map_err(|e| e.to_string())?;
            if let Ok(mut cache) = state.fix_status_cache.lock() {
                cache.remove(&id);
            }
            Ok(json!({ "success": true, "enabled": !status.enabled }))
        }
        Action::SteamstubUnpack { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let count = azdl::steamless::unpack_game(game, data_dir).map_err(|e| e.to_string())?;
            let new_status = azdl::steamless::scan_game_steamstub(game);
            if let Ok(mut cache) = state.steamstub_cache.lock() {
                cache.insert(id.clone(), (new_status.clone(), Instant::now()));
            }
            Ok(json!({
                "success": true,
                "unpacked_count": count,
                "steamstub": new_status,
            }))
        }
        Action::SteamstubRollback { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let count = azdl::steamless::rollback_game(game).map_err(|e| e.to_string())?;
            let new_status = azdl::steamless::scan_game_steamstub(game);
            if let Ok(mut cache) = state.steamstub_cache.lock() {
                cache.insert(id.clone(), (new_status.clone(), Instant::now()));
            }
            Ok(json!({
                "success": true,
                "restored_count": count,
                "steamstub": new_status,
            }))
        }
        Action::EaHeal { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let msg =
                azdl::fixes::heal_ea_activation(&game.install_dir).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "message": msg }))
        }
        Action::EaRollback { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let msg = azdl::fixes::rollback_ea_activation(&game.install_dir)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "message": msg }))
        }
        Action::DlcLicensePatch { id, dlc_ids } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let target_ids = match dlc_ids {
                Some(ids) => ids,
                None => {
                    let installed = azdl::dlc::list_installed(game, data_dir).unwrap_or_default();
                    installed.iter().filter_map(|s| s.parse().ok()).collect()
                }
            };
            let patched = azdl::fixes::patch_dlc_installed_ids(&game.install_dir, &target_ids)
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "success": true,
                "patched": patched,
                "dlc_patch_applied": azdl::fixes::has_dlc_installed_patch(&game.install_dir),
            }))
        }
        Action::DlcLicenseRollback { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let restored = azdl::fixes::rollback_dlc_installed(&game.install_dir)
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "success": true,
                "restored": restored,
                "dlc_patch_applied": azdl::fixes::has_dlc_installed_patch(&game.install_dir),
            }))
        }
        Action::GoldbergSync { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let updated = azdl::goldberg::prepare_goldberg_for_game(game, data_dir)
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "success": true,
                "updated": updated,
                "goldberg_has_settings": azdl::goldberg::has_goldberg_settings(&game.install_dir),
            }))
        }
        Action::GoldbergRollback { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let restored = azdl::goldberg::rollback_goldberg_config(&game.install_dir)
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "success": true,
                "restored": restored,
                "goldberg_has_settings": azdl::goldberg::has_goldberg_settings(&game.install_dir),
            }))
        }
        Action::PatchToggle { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            let patch = active_patch(data_dir, &id).map_err(|e| e.to_string())?;
            if let Some(p) = patch {
                if p.enabled {
                    disable_patch(game, data_dir).map_err(|e| e.to_string())?;
                } else {
                    enable_patch(game, data_dir).map_err(|e| e.to_string())?;
                }
                if let Ok(mut cache) = state.fix_status_cache.lock() {
                    cache.remove(&id);
                }
                Ok(json!({ "success": true, "enabled": !p.enabled }))
            } else {
                Err("No patch installed for this game".into())
            }
        }
        Action::PatchRollback { id } => {
            let library = load_library_snapshot(data_dir, &state.library_cache)?;
            let game = library
                .games
                .get(&id)
                .ok_or_else(|| format!("Game {id} not found"))?;
            rollback_patch(game, data_dir).map_err(|e| e.to_string())?;
            if let Ok(mut cache) = state.fix_status_cache.lock() {
                cache.remove(&id);
            }
            Ok(json!({ "success": true }))
        }
        Action::SettingsUpdate { settings } => {
            azdl::parse_runner(&settings.default_runner).map_err(|e| e.to_string())?;
            azdl::settings::save_settings(data_dir, &settings).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "settings": settings }))
        }
        Action::StorageAddDrive {
            drive_path,
            drive_label,
        } => {
            let label = drive_label.unwrap_or_default();
            let drive = azdl::settings::add_storage_drive(data_dir, &label, Path::new(&drive_path))
                .map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "drive": drive }))
        }
        Action::StorageRemoveDrive { drive_id } => {
            azdl::settings::remove_storage_drive(data_dir, &drive_id).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "removed": drive_id }))
        }
        Action::StorageSetDefault { drive_id } => {
            azdl::settings::set_default_drive(data_dir, &drive_id).map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "default": drive_id }))
        }
        Action::StorageMoveGame { id, drive_id } => {
            let content_lock =
                azdl::dlc::lock_game(data_dir, &id).map_err(|error| error.to_string())?;
            if state
                .running_games
                .lock()
                .map_err(|_| "Running game state lock poisoned")?
                .contains_key(&id)
            {
                return Err("Stop the game before moving it".into());
            }
            let task_id = format!("move-{id}");
            {
                let mut tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
                if tasks.iter().any(|task| {
                    task.game_id.as_deref() == Some(&id)
                        && task.status == "running"
                        && task.id.starts_with("dlc-")
                }) {
                    return Err(
                        "Wait for the DLC operation to finish before moving this game".into(),
                    );
                }
                if tasks
                    .iter()
                    .any(|t| t.id == task_id && t.status == "running")
                {
                    return Ok(json!({ "success": true, "already_working": true, "id": id }));
                }
                tasks.retain(|t| t.id != task_id);
                tasks.push(TaskInfo {
                    id: task_id.clone(),
                    label: format!("Moving game {id}"),
                    status: "running".into(),
                    pid: None,
                    game_id: Some(id.clone()),
                    log_tail: String::new(),
                });
            }
            drop(content_lock);
            let state_clone = state.clone();
            let id_clone = id.clone();
            let drive_id_clone = drive_id.clone();
            let task_id_clone = task_id.clone();
            std::thread::Builder::new()
                .name(format!("azdl-move-{id}"))
                .spawn(move || {
                    let data_dir = &state_clone.data_dir;
                    let result = (|| -> AzdlResult<()> {
                        let mut library = load_library(data_dir)?;
                        azdl::settings::move_game_to_drive(
                            data_dir,
                            &mut library,
                            &id_clone,
                            &drive_id_clone,
                        )?;
                        save_library(data_dir, &library)?;
                        Ok(())
                    })();
                    state_clone.dir_size_cache.invalidate_all();
                    finish_install_task(
                        &state_clone,
                        &task_id_clone,
                        result.err().map(|e| e.to_string()),
                    );
                })
                .map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "started": true, "id": id }))
        }
        Action::ProtonDownload => {
            let task_id = "download-proton-ge".to_string();
            {
                let mut tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
                if tasks
                    .iter()
                    .any(|t| t.id == task_id && t.status == "running")
                {
                    return Ok(json!({ "success": true, "already_working": true }));
                }
                tasks.retain(|t| t.id != task_id);
                tasks.push(TaskInfo {
                    id: task_id.clone(),
                    label: "Downloading Proton-GE".into(),
                    status: "running".into(),
                    pid: None,
                    game_id: None,
                    log_tail: String::new(),
                });
            }
            let state_clone = state.clone();
            let task_id_clone = task_id.clone();
            std::thread::Builder::new()
                .name("azdl-dl-proton".into())
                .spawn(move || {
                    let result = azdl::runtime::download_proton_ge(&state_clone.data_dir);
                    finish_install_task(
                        &state_clone,
                        &task_id_clone,
                        result.err().map(|e| e.to_string()),
                    );
                })
                .map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "started": true }))
        }
        Action::SteamShortcutsSync => {
            let task_id = "sync-steam-shortcuts".to_string();
            {
                let mut tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
                if tasks
                    .iter()
                    .any(|t| t.id == task_id && t.status == "running")
                {
                    return Ok(json!({ "success": true, "already_working": true }));
                }
                tasks.retain(|t| t.id != task_id);
                tasks.push(TaskInfo {
                    id: task_id.clone(),
                    label: "Syncing Steam shortcuts & artwork".into(),
                    status: "running".into(),
                    pid: None,
                    game_id: None,
                    log_tail: String::new(),
                });
            }
            let state_clone = state.clone();
            let task_id_clone = task_id.clone();
            std::thread::Builder::new()
                .name("azdl-steam-sync".into())
                .spawn(move || {
                    let data_dir = &state_clone.data_dir;
                    if let Ok(library) = load_library(data_dir) {
                        let _ = azdl::steam_shortcuts::sync_all_shortcuts(&library);
                    }
                    if let Ok(mut tasks) = state_clone.tasks.lock() {
                        tasks.retain(|t| t.id != task_id_clone);
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(json!({ "success": true, "started": true }))
        }
        Action::JobClear { id } => {
            if azdl::jobs::clear_job(data_dir, &id).is_ok() {
                Ok(json!({ "success": true, "cleared": id }))
            } else {
                Err(LauncherError::general(format!(
                    "Job {id} not found or could not be cleared"
                )))
            }
        }
        mode @ (Action::JobClearSuccessful | Action::JobClearCompleted) => {
            let include_failed = matches!(mode, Action::JobClearCompleted);
            let mut count =
                azdl::jobs::clear_completed_jobs(data_dir).map_err(|e| e.to_string())?;
            if include_failed && let Ok(jobs) = azdl::jobs::list_jobs(data_dir) {
                for j in jobs {
                    if j.effective_status() == azdl::jobs::JobStatus::Failed
                        && azdl::jobs::clear_job(data_dir, &j.id).is_ok()
                    {
                        count += 1;
                    }
                }
            }
            Ok(json!({ "success": true, "cleared_count": count }))
        }
    };

    if res.is_ok() {
        if let Some(path) = content_path {
            state.dir_size_cache.invalidate(&path);
        }
        state.notify_state_changed();
    }

    res
}

#[tauri::command]
pub async fn run_action(
    state: State<'_, AppState>,
    action: ActionRequest,
) -> Result<Value, LauncherError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_run_action(&state, action))
        .await
        .map_err(|e| LauncherError::general(e.to_string()))?
}

#[tauri::command]
pub fn window_minimize(window: Window) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn window_toggle_maximize(window: Window) -> Result<(), String> {
    if window.is_maximized().map_err(|e| e.to_string())? {
        window.unmaximize().map_err(|e| e.to_string())
    } else {
        window.maximize().map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn window_close(window: Window) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

pub(super) fn steam_details_for_ui(response: &[u8], app_id: u32) -> Result<Value, String> {
    let mut response: HashMap<String, SteamDetailsEnvelope> =
        serde_json::from_slice(response).map_err(|error| format!("json parse error: {error}"))?;
    let Some(entry) = response.remove(&app_id.to_string()) else {
        return Ok(json!({}));
    };
    if entry.success != Some(true) {
        return Ok(json!({}));
    }
    let Some(data) = entry.data else {
        return Ok(json!({}));
    };
    Ok(steam_details_payload_for_ui(data.into_value()))
}

fn retain_object_fields(value: &mut Value, fields: &[&str]) {
    if let Some(object) = value.as_object_mut() {
        object.retain(|key, _| fields.contains(&key.as_str()));
    }
}

fn steam_details_payload_for_ui(mut data: Value) -> Value {
    let Some(object) = data.as_object_mut() else {
        return data;
    };
    let storage_size = object
        .get("pc_requirements")
        .and_then(azdl::steam_discovery::parse_storage_requirement);
    object.retain(|key, _| {
        matches!(
            key.as_str(),
            "type"
                | "fullgame"
                | "name"
                | "header_image"
                | "detailed_description"
                | "short_description"
                | "developers"
                | "movies"
                | "screenshots"
                | "genres"
                | "release_date"
        )
    });

    if let Some(Value::Array(movies)) = object.get_mut("movies") {
        for movie in movies {
            retain_object_fields(
                movie,
                &[
                    "id",
                    "name",
                    "thumbnail",
                    "hls_h264",
                    "hls",
                    "dash_h264",
                    "mp4",
                    "webm",
                ],
            );
            if let Some(movie) = movie.as_object_mut() {
                for format in ["mp4", "webm"] {
                    if let Some(format) = movie.get_mut(format) {
                        retain_object_fields(format, &["max", "480"]);
                    }
                }
                if !movie.contains_key("mp4")
                    && let Some(id) = movie.get("id").and_then(|v| v.as_u64())
                {
                    movie.insert(
                        "mp4".to_string(),
                        json!({
                            "max": format!("https://cdn.akamai.steamstatic.com/steam/apps/{id}/movie_max.mp4"),
                            "480": format!("https://cdn.akamai.steamstatic.com/steam/apps/{id}/movie480.mp4"),
                        }),
                    );
                }
            }
        }
    }
    if let Some(Value::Array(screenshots)) = object.get_mut("screenshots") {
        for screenshot in screenshots {
            retain_object_fields(screenshot, &["path_thumbnail", "path_full"]);
        }
    }
    if let Some(Value::Array(genres)) = object.get_mut("genres") {
        for genre in genres {
            retain_object_fields(genre, &["description"]);
        }
    }
    if let Some(fullgame) = object.get_mut("fullgame") {
        retain_object_fields(fullgame, &["appid", "name"]);
    }
    if let Some(release_date) = object.get_mut("release_date") {
        retain_object_fields(release_date, &["date"]);
    }

    if let Some(storage_size) = storage_size {
        object.insert("storage_size".into(), Value::String(storage_size));
    }

    data
}

pub fn handle_get_steam_details(app_id: u32) -> Result<Value, String> {
    let url =
        format!("https://store.steampowered.com/api/appdetails?appids={app_id}&l=english&cc=US");
    let output = azdl::clean_command("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
        ])
        .arg(&url)
        .output()
        .map_err(|e| format!("curl error: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() {
            format!("fetch failed with status {}", output.status)
        } else {
            format!(
                "fetch failed with status {}: {}",
                output.status,
                stderr.trim()
            )
        };
        return Err(detail);
    }
    steam_details_for_ui(&output.stdout, app_id)
}

#[tauri::command]
pub async fn get_steam_details(app_id: u32) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || handle_get_steam_details(app_id))
        .await
        .map_err(|e| e.to_string())?
}

pub fn handle_get_game_controls(state: &AppState, id: &str) -> Result<Value, String> {
    let library = load_library_snapshot(&state.data_dir, &state.library_cache)?;
    let game = library.games.get(id).ok_or("Game is not installed")?;
    let controls =
        azdl::game_controls::inspect(game, &state.data_dir).map_err(|error| error.to_string())?;
    serde_json::to_value(controls).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_game_controls(state: State<'_, AppState>, id: String) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || handle_get_game_controls(&state, &id))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn get_game_dlc(
    state: State<'_, AppState>,
    id: String,
    candidate_dlc_id: Option<u32>,
) -> Result<Value, String> {
    let data_dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let library = load_library(&data_dir).map_err(|e| e.to_string())?;
        let game = library
            .games
            .get(&id)
            .ok_or_else(|| format!("Game {id} not found"))?;
        let installed =
            azdl::dlc::list_installed(game, &data_dir).map_err(|e| e.to_string())?;
        let interrupted =
            azdl::dlc::list_interrupted(game, &data_dir).map_err(|e| e.to_string())?;
        let mut available = Vec::<Value>::new();
        let mut metadata_error = None;
        if let Some(app_id) = game.steam_app_id {
            let observations = azdl::steam_content::list_observations(&data_dir, &game.id)
                .map_err(|e| e.to_string())?;
            let appinfo = azdl::steam_metadata::fetch_appinfo(app_id);
            let store_ids = azdl::steam_metadata::fetch_store_dlc_ids(app_id);
            let mut depots = std::collections::BTreeMap::new();
            match &appinfo {
                Ok(metadata) => {
                    depots = azdl::steam_metadata::dlc_depots(metadata);
                }
                Err(error) => metadata_error = Some(error.to_string()),
            }
            let ids = super::dlc_ids_for_display(
                app_id,
                &observations,
                appinfo.as_ref().ok(),
                store_ids.as_ref().ok().map(Vec::as_slice),
                candidate_dlc_id,
            );
            let provider_status = super::steam_dlc_provider_status(app_id);
            let base_receipt = azdl::steam_content::load_base_receipt(&data_dir, &game.id);
            let needs_existing_check = provider_status.is_ok() && matches!(&base_receipt, Ok(None));
            let receipt_error = base_receipt.is_err();
            let revision_check = match (&base_receipt, &appinfo) {
                (Ok(Some(receipt)), Ok(metadata)) if provider_status.is_ok() => Some(
                    azdl::steam_content::validate_base_revision(receipt, metadata)
                        .map_err(|error| error.to_string()),
                ),
                _ => None,
            };
            let revision_mismatch = revision_check
                .as_ref()
                .is_some_and(|result| result.is_err());
            let steam_eligibility = if let Err(error) = &provider_status {
                Err(error.clone())
            } else {
                match &base_receipt {
                    Ok(Some(_)) => revision_check.unwrap_or_else(|| {
                        Err(appinfo
                            .as_ref()
                            .err()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "Base revision could not be checked".to_string()))
                    }),
                    Ok(None) => Err(
                        "An older install needs a file check before missing DLC can be downloaded"
                            .to_string(),
                    ),
                    Err(error) => Err(error.to_string()),
                }
            };
            let mut details = ids
                .chunks(4)
                .take(if appinfo.is_ok() || store_ids.is_ok() { 3 } else { 0 })
                .flat_map(|batch| {
                    std::thread::scope(|scope| {
                        let handles = batch
                            .iter()
                            .map(|dlc_id| {
                                scope.spawn(move || {
                                    azdl::steam_metadata::fetch_appinfo(*dlc_id)
                                        .ok()
                                        .map(|details| (*dlc_id, details))
                                })
                            })
                            .collect::<Vec<_>>();
                        handles
                            .into_iter()
                            .filter_map(|handle| handle.join().ok().flatten())
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<std::collections::HashMap<_, _>>();
            if let Some(candidate) = candidate_dlc_id.filter(|id| *id > 0)
                && !details.contains_key(&candidate)
                && let Ok(detail) = azdl::steam_metadata::fetch_appinfo(candidate)
            {
                details.insert(candidate, detail);
            }
            for dlc_id in ids {
                let id = dlc_id.to_string();
                if available.iter().any(|addon| addon["id"] == id) {
                    continue;
                }
                let observed = observations
                    .iter()
                    .find(|receipt| receipt.app_id == app_id && receipt.dlc_id == dlc_id);
                let related = depots.get(&dlc_id);
                let delivery = azdl::steam_content::dlc_depot_availability(
                    appinfo.as_ref().ok(),
                    details.get(&dlc_id),
                    app_id,
                    dlc_id,
                );
                let unavailable_reason = match delivery {
                    azdl::steam_content::DlcDepotAvailability::Unavailable(reason) => {
                        Some(reason)
                    }
                    _ => None,
                };
                let unknown_reason = matches!(
                    delivery,
                    azdl::steam_content::DlcDepotAvailability::Unknown
                )
                .then_some("Steam metadata has not identified downloadable files for this DLC yet.");
                let can_check_content = provider_status.is_ok()
                    && unavailable_reason.is_none()
                    && !receipt_error
                    && (!revision_mismatch || observed.is_some());
                let manifest_count = related
                    .into_iter()
                    .flatten()
                    .filter(|depot| depot.public_manifest_id.is_some())
                    .count();
                available.push(json!({
                    "id": id,
                    "title": details.get(&dlc_id).filter(|details| details.parent_app_id == Some(app_id)).and_then(|details| details.name.clone()).unwrap_or_else(|| format!("Steam DLC {dlc_id}")),
                    "content_available": can_check_content,
                    "needs_provider_check": can_check_content,
                    "needs_existing_check": can_check_content && needs_existing_check,
                    "present_unmanaged": observed.is_some(),
                    "installed": installed.contains(&id),
                    "interrupted": interrupted.contains(&id),
                    "depot_count": related.map_or(0, Vec::len),
                    "public_manifest_count": manifest_count,
                    "reason": if let Err(error) = &provider_status {
                        Some(error.as_str())
                    } else if observed.is_some() {
                        Some("Existing files matched a provider manifest when last checked. They are not managed by azdl and cannot be removed here.")
                    } else {
                        unavailable_reason
                            .or_else(|| steam_eligibility.as_ref().err().map(String::as_str))
                            .or(unknown_reason)
                    },
                }));
            }
        }
        for dlc_id in &installed {
            if !available.iter().any(|addon| addon["id"] == *dlc_id) {
                available.push(json!({
                    "id": dlc_id,
                    "title": dlc_id,
                    "content_available": false,
                    "installed": true,
                    "interrupted": false,
                }));
            }
        }
        for dlc_id in &interrupted {
            if !available.iter().any(|addon| addon["id"] == *dlc_id) {
                available.push(json!({
                    "id": dlc_id,
                    "title": dlc_id,
                    "content_available": false,
                    "installed": false,
                    "interrupted": true,
                }));
            }
        }
        Ok(json!({ "available": available, "installed": installed, "interrupted": interrupted, "metadata_error": metadata_error }))
    })
    .await
    .map_err(|e| e.to_string())?
}
