use super::*;

#[test]
fn selected_catalog_dlc_survives_missing_parent_lists() {
    let ids = dlc_ids_for_display(100, &[], None, None, Some(200));
    assert_eq!(ids, vec![200]);
    assert!(dlc_ids_for_display(100, &[], None, None, Some(0)).is_empty());

    let observation = azdl::steam_content::ObservedDlcReceipt {
        game_id: "game".into(),
        app_id: 100,
        dlc_id: 200,
        depots: Vec::new(),
        verified_unix: 1,
    };
    assert_eq!(
        dlc_ids_for_display(100, &[observation], None, Some(&[200, 201]), Some(201)),
        vec![200, 201]
    );
}

#[test]
fn steam_app_details_use_store_header_image() {
    let response = serde_json::json!({
        "3892270": {
            "success": true,
            "data": {
                "header_image": "https://cdn.example/real-header.jpg",
                "capsule_image": "https://cdn.example/capsule.jpg"
            }
        }
    });
    let artwork = steam_artwork_from_appdetails(3892270, &response).unwrap();
    assert_eq!(artwork.steam_app_id, Some(3892270));
    assert_eq!(
        artwork.banner_url.as_deref(),
        Some("https://cdn.example/real-header.jpg")
    );
    assert_eq!(
        artwork.cover_url.as_deref(),
        Some("https://cdn.example/capsule.jpg")
    );
}

#[test]
fn guessed_steam_artwork_is_marked_for_refresh() {
    let guessed = MetadataCacheEntry {
        steam_app_id: Some(3892270),
        banner_url: Some("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/3892270/header.jpg".into()),
        cover_url: Some("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/3892270/library_600x900.jpg".into()),
    };
    assert!(is_guessed_steam_artwork(&guessed));
}

#[test]
fn test_disk_artwork_caching_and_uri() {
    let root = scratch();
    let target_dir = root.join("artwork-cache/361420");
    fs::create_dir_all(&target_dir).unwrap();
    let banner_file = target_dir.join("banner.jpg");
    fs::write(&banner_file, b"fake image bytes exceeding one hundred bytes padding padding padding padding padding padding padding padding padding padding padding padding padding padding").unwrap();

    assert_eq!(
        get_cached_artwork_path(&root, "361420", "banner"),
        Some(banner_file)
    );
    assert_eq!(
        get_cached_artwork_uri(&root, "361420", "banner"),
        Some("azdl-art://localhost/361420/banner.jpg".to_string())
    );
    assert_eq!(get_cached_artwork_uri(&root, "361420", "cover"), None);

    let _ = fs::remove_dir_all(&root);
}
use commands::*;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "azdl-tauri-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    fs::canonicalize(path).unwrap()
}

fn test_state(root: &Path) -> AppState {
    AppState {
        data_dir: root.to_path_buf(),
        running_games: Arc::new(Mutex::new(HashMap::new())),
        tasks: Arc::new(Mutex::new(Vec::new())),
        mutating: Arc::new(AtomicBool::new(false)),
        cached_diagnostics: Arc::new(Mutex::new(None)),
        library_cache: Arc::new(Mutex::new(None)),
        dir_size_cache: Arc::new(sizes::SizeCache::new(root)),
        metadata_cache: Arc::new(Mutex::new(HashMap::new())),
        metadata_pending: Arc::new(Mutex::new(HashSet::new())),
        steamstub_cache: Arc::new(Mutex::new(HashMap::new())),
        fix_status_cache: Arc::new(Mutex::new(HashMap::new())),
        app_handle: Arc::new(Mutex::new(None)),
    }
}

/// Register an installed game directly, as a completed Steam install would.
fn register_fixture(state: &AppState, id: &str, title: &str, install_dir: &Path, executable: &str) {
    let mut library = azdl::load_library(&state.data_dir).unwrap();
    azdl::add_game(&mut library, id, title, install_dir, Path::new(executable)).unwrap();
    azdl::save_library(&state.data_dir, &library).unwrap();
}

#[test]
fn progress_snapshot_does_not_read_library_or_settings() {
    let root = scratch();
    fs::write(root.join("library.json"), b"invalid library JSON").unwrap();
    fs::write(root.join("settings.json"), b"invalid settings JSON").unwrap();
    let state = test_state(&root);
    let progress = commands::handle_get_progress(&state).unwrap();
    assert_eq!(progress["jobs"], serde_json::json!([]));
    assert_eq!(progress["tasks"], serde_json::json!([]));
    assert!(progress.get("library").is_none());
    assert!(!root.join("directory-sizes.json").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stopped_dlc_tasks_do_not_report_user_actions_as_failures() {
    let root = scratch();
    let state = test_state(&root);
    for stopped in [
        azdl::jobs::TransferStopped::Paused("0123456789abcdef".into()),
        azdl::jobs::TransferStopped::Cancelled(None),
    ] {
        state.tasks.lock().unwrap().push(TaskInfo {
            id: "dlc-base-bonus".into(),
            label: "Installing DLC".into(),
            status: "running".into(),
            pid: None,
            game_id: Some("base".into()),
            log_tail: String::new(),
        });
        finish_dlc_install_task(&state, "dlc-base-bonus", Err(stopped.into()));
        assert!(state.tasks.lock().unwrap().is_empty());
    }
    state.tasks.lock().unwrap().push(TaskInfo {
        id: "dlc-base-bonus".into(),
        label: "Installing DLC".into(),
        status: "running".into(),
        pid: None,
        game_id: Some("base".into()),
        log_tail: String::new(),
    });
    finish_dlc_install_task(&state, "dlc-base-bonus", Err("network failure".into()));
    let tasks = state.tasks.lock().unwrap();
    assert_eq!(tasks[0].status, "failed");
    assert_eq!(tasks[0].log_tail, "network failure");
    drop(tasks);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn metadata_cache_stays_bounded() {
    let mut cache = HashMap::new();
    let entry = MetadataCacheEntry {
        steam_app_id: None,
        banner_url: None,
        cover_url: None,
    };

    for index in 0..=MAX_METADATA_CACHE_ENTRIES {
        insert_metadata_cache(&mut cache, format!("game-{index}"), entry.clone());
    }

    assert_eq!(cache.len(), MAX_METADATA_CACHE_ENTRIES);
    insert_metadata_cache(&mut cache, "game-0".to_string(), entry);
    assert_eq!(cache.len(), MAX_METADATA_CACHE_ENTRIES);
}

#[test]
fn metadata_cache_loader_stops_at_entry_limit() {
    let root = scratch();
    let entry = MetadataCacheEntry {
        steam_app_id: None,
        banner_url: None,
        cover_url: None,
    };
    let oversized = (0..MAX_METADATA_CACHE_ENTRIES + 100)
        .map(|index| (format!("legacy-{index}"), entry.clone()))
        .collect::<HashMap<_, _>>();
    fs::write(
        metadata_cache_path(&root),
        serde_json::to_vec(&oversized).unwrap(),
    )
    .unwrap();

    assert_eq!(load_metadata_cache(&root).len(), MAX_METADATA_CACHE_ENTRIES);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn library_snapshot_reuses_unchanged_file_and_reloads_after_change() {
    let root = scratch();
    let cache = Mutex::new(None);

    let first = load_library_snapshot(&root, &cache).unwrap();
    let unchanged = load_library_snapshot(&root, &cache).unwrap();
    assert!(Arc::ptr_eq(&first, &unchanged));

    fs::write(root.join("library.json"), b"{\"games\":{}}\n").unwrap();
    let refreshed = load_library_snapshot(&root, &cache).unwrap();
    assert!(!Arc::ptr_eq(&first, &refreshed));
    let unchanged_again = load_library_snapshot(&root, &cache).unwrap();
    assert!(Arc::ptr_eq(&refreshed, &unchanged_again));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn steam_details_keep_ui_fields_and_drop_unused_payload() {
    let details = serde_json::json!({
        "name": "Example Game",
        "header_image": "header.jpg",
        "detailed_description": "<p>Details</p>",
        "short_description": "Summary",
        "developers": ["Studio"],
        "release_date": { "date": "Jan 1, 2020", "coming_soon": false },
        "genres": [{ "id": "1", "description": "Action", "extra": true }],
        "pc_requirements": { "minimum": "<strong>Storage:</strong> 20 GB available space" },
        "screenshots": [{
            "path_thumbnail": "thumb.jpg",
            "path_full": "full.jpg",
            "extra": "discard"
        }],
        "movies": [{
            "name": "Trailer",
            "thumbnail": "movie.jpg",
            "mp4": { "max": "max.mp4", "480": "480.mp4", "1080": "unused.mp4" },
            "webm": "trailer.webm",
            "hls_h264": "trailer.m3u8",
            "hls": "fallback.m3u8",
            "dash_h264": "trailer.mpd",
            "extra": "discard"
        }],
        "dlc": [1, 2, 3],
        "recommendations": { "total": 999 },
        "unused_payload": "x".repeat(256 * 1024)
    });

    let response = serde_json::json!({
        "123": { "success": true, "data": details.clone() }
    });
    let response = serde_json::to_vec(&response).unwrap();
    let selected = commands::steam_details_for_ui(&response, 123).unwrap();
    assert_eq!(selected["name"], "Example Game");
    assert_eq!(selected["storage_size"], "20 GB");
    assert!(selected.get("pc_requirements").is_none());
    assert_eq!(
        selected["release_date"],
        serde_json::json!({ "date": "Jan 1, 2020" })
    );
    assert_eq!(
        selected["genres"][0],
        serde_json::json!({ "description": "Action" })
    );
    assert_eq!(
        selected["movies"][0]["mp4"],
        serde_json::json!({ "max": "max.mp4", "480": "480.mp4" })
    );
    assert_eq!(selected["screenshots"][0]["path_full"], "full.jpg");
    assert!(selected.get("dlc").is_none());
    assert!(selected.get("recommendations").is_none());
    assert!(selected["movies"][0].get("extra").is_none());
    let selected_size = serde_json::to_vec(&selected).unwrap().len();
    let full_size = serde_json::to_vec(&details).unwrap().len();
    assert!(selected_size * 10 < full_size);
}

#[test]
fn steam_details_keep_dlc_parent_for_catalog_routing() {
    let response = serde_json::json!({
        "200": {
            "success": true,
            "data": {
                "type": "dlc",
                "name": "Sample DLC",
                "fullgame": { "appid": "100", "name": "Sample Game", "extra": "discard" }
            }
        }
    });
    let selected =
        commands::steam_details_for_ui(&serde_json::to_vec(&response).unwrap(), 200).unwrap();
    assert_eq!(selected["type"], "dlc");
    assert_eq!(selected["fullgame"]["appid"], "100");
    assert!(selected["fullgame"].get("extra").is_none());
}

#[test]
fn metadata_lookups_are_deduplicated_and_bounded() {
    let mut pending = HashSet::new();
    assert!(reserve_metadata_lookup(&mut pending, "game 0".to_string()));
    assert!(!reserve_metadata_lookup(&mut pending, "game 0".to_string()));

    for index in 1..MAX_CONCURRENT_METADATA_LOOKUPS {
        assert!(reserve_metadata_lookup(
            &mut pending,
            format!("game {index}")
        ));
    }
    assert_eq!(pending.len(), MAX_CONCURRENT_METADATA_LOOKUPS);
    assert!(!reserve_metadata_lookup(
        &mut pending,
        "one more game".to_string()
    ));
}

#[test]
fn test_app_state_initialization() {
    let temp = scratch();
    let state = test_state(&temp);
    assert_eq!(state.data_dir, temp);
    assert!(state.running_games.lock().unwrap().is_empty());
}

#[test]
fn test_action_request_deserialization() {
    let json = r#"{"action":"steam-install","id":"steam:620","banner_url":"https://cdn.example/header.jpg","cover_url":"https://cdn.example/cover.jpg"}"#;
    let req: ActionRequest = serde_json::from_str(json).unwrap();
    match req {
        Action::SteamInstall {
            id,
            banner_url,
            cover_url,
            ..
        } => {
            assert_eq!(id, "steam:620");
            assert_eq!(
                banner_url.as_deref(),
                Some("https://cdn.example/header.jpg")
            );
            assert_eq!(cover_url.as_deref(), Some("https://cdn.example/cover.jpg"));
        }
        _ => panic!("Expected SteamInstall variant"),
    }

    // Test StorageAddDrive with alias "install_dir" and "title"
    let json_drive =
        r#"{"action":"storage-add-drive","install_dir":"/mnt/fast","title":"Fast NVMe"}"#;
    let req_drive: ActionRequest = serde_json::from_str(json_drive).unwrap();
    match req_drive {
        Action::StorageAddDrive {
            drive_path,
            drive_label,
        } => {
            assert_eq!(drive_path, "/mnt/fast");
            assert_eq!(drive_label.as_deref(), Some("Fast NVMe"));
        }
        _ => panic!("Expected StorageAddDrive variant"),
    }

    // Test StorageRemoveDrive with alias "id"
    let json_remove = r#"{"action":"storage-remove-drive","id":"drive-1"}"#;
    let req_remove: ActionRequest = serde_json::from_str(json_remove).unwrap();
    match req_remove {
        Action::StorageRemoveDrive { drive_id } => {
            assert_eq!(drive_id, "drive-1");
        }
        _ => panic!("Expected StorageRemoveDrive variant"),
    }

    // Test unknown action fails deserialization at compile/serde level
    let json_unknown = r#"{"action":"nonexistent-action","id":"123"}"#;
    assert!(serde_json::from_str::<ActionRequest>(json_unknown).is_err());

    // Test missing required field fails deserialization
    let json_missing = r#"{"action":"runner-set","id":"game-1"}"#;
    assert!(serde_json::from_str::<ActionRequest>(json_missing).is_err());

    let install: ActionRequest =
        serde_json::from_str(r#"{"action":"dlc-install","id":"base","dlc_id":"bonus"}"#).unwrap();
    assert!(matches!(install, Action::DlcInstall { dlc_id, .. } if dlc_id == "bonus"));
}

#[test]
fn test_handle_get_state_empty() {
    let root = scratch();
    let state = test_state(&root);
    let val = handle_get_state(&state).unwrap();
    assert_eq!(val["library"], serde_json::json!([]));
    assert_eq!(val["pending"], serde_json::json!([]));
    assert_eq!(
        val["platform"],
        if cfg!(windows) { "windows" } else { "linux" }
    );
}

#[test]
fn test_handle_get_runners() {
    let root = scratch();
    let state = test_state(&root);
    let runners = handle_get_runners(&state).unwrap();
    assert!(runners.iter().any(|r| r.id == "auto"));
    assert!(runners.iter().any(|r| r.id == "native"));
    let wine_runner = runners.iter().find(|r| r.id == "wine").unwrap();
    assert_eq!(wine_runner.name, "System Wine");
    #[cfg(unix)]
    assert!(runners.iter().any(|r| r.id == "proton:auto"));
}

#[test]
fn test_handle_run_action_game_remove() {
    let root = scratch();
    let state = test_state(&root);
    let game_dir = root.join("my-test-game");
    fs::create_dir_all(&game_dir).unwrap();
    fs::write(game_dir.join("game.bin"), b"test").unwrap();

    // 1. Import game
    register_fixture(&state, "test-game", "My Test Game", &game_dir, "game.bin");

    // Verify it is in state
    let current = handle_get_state(&state).unwrap();
    let lib = current["library"].as_array().unwrap();
    assert_eq!(lib.len(), 1);
    assert_eq!(lib[0]["id"], "test-game");
    assert_eq!(lib[0]["title"], "My Test Game");

    // 2. Remove game
    let remove_req = Action::GameRemove {
        id: "test-game".into(),
    };
    for (task_id, expected) in [
        ("dlc-test-game-200", "DLC install"),
        ("move-test-game", "game move"),
    ] {
        state.tasks.lock().unwrap().push(TaskInfo {
            id: task_id.into(),
            label: "Changing game content".into(),
            status: "running".into(),
            pid: None,
            game_id: Some("test-game".into()),
            log_tail: String::new(),
        });
        let error = handle_run_action(&state, remove_req.clone())
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(game_dir.join("game.bin").exists());
        state.tasks.lock().unwrap().clear();
    }
    let res_rem = handle_run_action(&state, remove_req).unwrap();
    assert_eq!(res_rem["removed"], "test-game");

    let current_after = handle_get_state(&state).unwrap();
    assert_eq!(current_after["library"].as_array().unwrap().len(), 0);
    assert!(!game_dir.exists());
}

#[test]
fn test_handle_run_action_runner_and_executable_set() {
    let root = scratch();
    let state = test_state(&root);
    let game_dir = root.join("game-runner-test");
    fs::create_dir_all(&game_dir).unwrap();
    fs::write(game_dir.join("game.exe"), b"test").unwrap();
    fs::write(game_dir.join("game2.exe"), b"test2").unwrap();

    register_fixture(&state, "runner-game", "Runner Game", &game_dir, "game.exe");

    // Change runner to wine
    let runner_req = Action::RunnerSet {
        id: "runner-game".into(),
        runner: "wine".into(),
    };
    handle_run_action(&state, runner_req).unwrap();

    // Change executable to game2.exe
    let exe_req = Action::ExecutableSet {
        id: "runner-game".into(),
        executable: "game2.exe".into(),
    };
    handle_run_action(&state, exe_req).unwrap();

    let s = handle_get_state(&state).unwrap();
    let g = &s["library"][0];
    assert_eq!(g["runner"], "wine");
    assert!(g["executable"].as_str().unwrap().ends_with("game2.exe"));
}

#[test]
fn test_handle_run_action_args_and_env() {
    let root = scratch();
    let state = test_state(&root);
    let game_dir = root.join("args-env-test");
    fs::create_dir_all(&game_dir).unwrap();
    fs::write(game_dir.join("main.bin"), b"test").unwrap();

    register_fixture(
        &state,
        "args-env-game",
        "Args Env Game",
        &game_dir,
        "main.bin",
    );

    // Set launch arguments
    handle_run_action(
        &state,
        Action::ArgsSet {
            id: "args-env-game".into(),
            arguments: Some(vec!["--fullscreen".into(), "-v".into()]),
        },
    )
    .unwrap();

    // Set env variable
    handle_run_action(
        &state,
        Action::EnvSet {
            id: "args-env-game".into(),
            key: "DXVK_HUD".into(),
            value: Some("fps".into()),
        },
    )
    .unwrap();

    let s = handle_get_state(&state).unwrap();
    let g = &s["library"][0];
    assert_eq!(g["arguments"], serde_json::json!(["--fullscreen", "-v"]));
    assert_eq!(g["environment"]["DXVK_HUD"], "fps");

    // Remove env variable
    handle_run_action(
        &state,
        Action::EnvRemove {
            id: "args-env-game".into(),
            key: "DXVK_HUD".into(),
        },
    )
    .unwrap();

    let s2 = handle_get_state(&state).unwrap();
    assert!(s2["library"][0]["environment"].get("DXVK_HUD").is_none());
}

#[test]
fn test_handle_run_action_launch_options() {
    let root = scratch();
    let state = test_state(&root);
    let game_dir = root.join("opts-test");
    fs::create_dir_all(&game_dir).unwrap();
    fs::write(game_dir.join("game.exe"), b"test").unwrap();
    fs::write(game_dir.join("mp.exe"), b"mp").unwrap();

    register_fixture(&state, "opt-game", "Option Game", &game_dir, "game.exe");

    // Add launch option
    handle_run_action(
        &state,
        Action::OptionAdd {
            id: "opt-game".into(),
            option_id: "mp".into(),
            title: "Multiplayer".into(),
            executable: "mp.exe".into(),
            working_directory: None,
            arguments: Some(vec!["--mp".into()]),
        },
    )
    .unwrap();

    // Select launch option
    handle_run_action(
        &state,
        Action::OptionSelect {
            id: "opt-game".into(),
            option_id: "mp".into(),
        },
    )
    .unwrap();

    let s = handle_get_state(&state).unwrap();
    assert_eq!(s["library"][0]["selected_launch_option_id"], "mp");

    // Remove launch option
    handle_run_action(
        &state,
        Action::OptionRemove {
            id: "opt-game".into(),
            option_id: "mp".into(),
        },
    )
    .unwrap();

    let s2 = handle_get_state(&state).unwrap();
    assert!(
        s2["library"][0]["launch_options"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_handle_run_action_pending_add() {
    let root = scratch();
    let state = test_state(&root);
    let pending_dir = root.join("games").join("unregistered");
    fs::create_dir_all(&pending_dir).unwrap();
    fs::write(pending_dir.join("app.exe"), b"app").unwrap();

    // Verify it is discovered in pending
    let s = handle_get_state(&state).unwrap();
    let pending = s["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["id"], "unregistered");

    // Discovers executables for pending item before it is in the library
    let exes = handle_discover_exes(&state, "unregistered".into()).unwrap();
    let exe_list = exes.as_array().unwrap();
    assert_eq!(exe_list.len(), 1);
    assert_eq!(exe_list[0]["relative_path"], "app.exe");

    // Register it via pending-add (omitting title defaults to id)
    handle_run_action(
        &state,
        Action::PendingAdd {
            id: "unregistered".into(),
            title: None, // defaults to "unregistered"
            install_dir: pending_dir.to_string_lossy().into(),
            executable: "app.exe".into(),
        },
    )
    .unwrap();

    let s2 = handle_get_state(&state).unwrap();
    assert_eq!(s2["pending"].as_array().unwrap().len(), 0);
    assert_eq!(s2["library"].as_array().unwrap().len(), 1);
    assert_eq!(s2["library"][0]["title"], "unregistered");
}

#[test]
fn test_handle_run_action_pending_remove() {
    let root = scratch();
    let state = test_state(&root);
    let pending_dir = root.join("games").join("to-delete");
    fs::create_dir_all(&pending_dir).unwrap();
    fs::write(pending_dir.join("waste.bin"), b"waste").unwrap();

    let s = handle_get_state(&state).unwrap();
    let pending = s["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["id"], "to-delete");

    // Remove via pending-remove
    handle_run_action(
        &state,
        Action::PendingRemove {
            id: "to-delete".into(),
            install_dir: Some(pending_dir.to_string_lossy().into()),
        },
    )
    .unwrap();

    assert!(!pending_dir.exists());
    let s2 = handle_get_state(&state).unwrap();
    assert_eq!(s2["pending"].as_array().unwrap().len(), 0);
}

#[test]
fn test_handle_diagnose_and_discover_exes() {
    let root = scratch();
    let state = test_state(&root);
    let game_dir = root.join("diag-game");
    fs::create_dir_all(&game_dir).unwrap();
    fs::write(game_dir.join("main.exe"), b"exe").unwrap();

    register_fixture(&state, "diag-game", "Diag Game", &game_dir, "main.exe");

    let diag = handle_diagnose(&state, "diag-game".into()).unwrap();
    assert!(diag.get("runner").is_some());

    let candidates = handle_discover_exes(&state, "diag-game".into()).unwrap();
    let arr = candidates.as_array().unwrap();
    assert!(!arr.is_empty());
    assert!(
        arr.iter()
            .any(|c| c["relative_path"].as_str().unwrap() == "main.exe")
    );
}

#[test]
fn test_steam_discovery_results_are_mapped_by_app_id() {
    let root = scratch();
    let state = test_state(&root);
    let library = azdl::load_library(&state.data_dir).unwrap();
    let mut discovered = azdl::steam_discovery::SteamStoreGame::new(620, "Portal 2");
    discovered.storage_size = Some("10 GB".into());
    let games = commands::steam_games_to_values(&library, vec![discovered]);
    assert_eq!(games.as_array().unwrap().len(), 1);
    assert_eq!(games[0]["qualified_id"], "steam:620");
    assert_eq!(games[0]["steam_app_id"], 620);
    assert_eq!(games[0]["file_size"], "10 GB");
}

#[test]
fn get_state_returns_persisted_steam_artwork_and_app_id() {
    let root = scratch();
    let state = test_state(&root);
    let game_dir = root.join("games").join("steam-620");
    fs::create_dir_all(&game_dir).unwrap();
    fs::write(game_dir.join("game.exe"), b"game").unwrap();
    let mut library = azdl::Library::default();
    library.games.insert(
        "steam-620".into(),
        azdl::Game {
            id: "steam-620".into(),
            title: "Portal 2".into(),
            install_dir: game_dir.clone(),
            executable: game_dir.join("game.exe"),
            runner: azdl::Runner::Native,
            arguments: vec![],
            environment: std::collections::BTreeMap::new(),
            origin: None,
            steam_app_id: Some(620),
            last_played_unix: None,
            launch_options: vec![],
            selected_launch_option_id: None,
            cover_url: Some("https://cdn.example/portal-cover.jpg".into()),
            banner_url: Some("https://cdn.example/portal-header.jpg".into()),
        },
    );
    azdl::save_library(&state.data_dir, &library).unwrap();

    let current = handle_get_state(&state).unwrap();
    let card = &current["library"][0];
    assert_eq!(card["steam_app_id"], 620);
    assert_eq!(card["banner_url"], "https://cdn.example/portal-header.jpg");
    assert_eq!(card["cover_url"], "https://cdn.example/portal-cover.jpg");
    // First state response does not wait for a recursive directory scan.
    assert!(card["size_bytes"].is_null() || card["size_bytes"] == 4);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_handle_launch_and_stop_game() {
    let root = scratch();
    let state = test_state(&root);
    let game_dir = root.join("proc-game");
    fs::create_dir_all(&game_dir).unwrap();

    #[cfg(unix)]
    let exe_name = "mock_game.sh";
    #[cfg(windows)]
    let exe_name = "mock_game.bat";

    let exe_path = game_dir.join(exe_name);
    #[cfg(unix)]
    {
        fs::write(&exe_path, b"#!/bin/sh\nsleep 30\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    #[cfg(windows)]
    fs::write(&exe_path, b"@echo off\r\nping -n 31 127.0.0.1 >nul\r\n").unwrap();

    register_fixture(&state, "proc-game", "Proc Game", &game_dir, exe_name);

    state.tasks.lock().unwrap().push(TaskInfo {
        id: "dlc-proc-game-123".into(),
        label: "Installing DLC".into(),
        status: "running".into(),
        pid: None,
        game_id: Some("proc-game".into()),
        log_tail: String::new(),
    });
    let blocked = handle_launch_game(&state, "proc-game".into(), None)
        .unwrap_err()
        .to_string();
    assert!(blocked.contains("DLC operation"), "{blocked}");
    state.tasks.lock().unwrap().clear();

    // Launch game
    let launch_res = handle_launch_game(&state, "proc-game".into(), None).unwrap();
    assert_eq!(launch_res["status"], "launched");
    let pid = launch_res["pid"].as_u64().unwrap() as u32;

    // Check it is in running_games
    {
        let running = state.running_games.lock().unwrap();
        assert!(running.contains_key("proc-game"));
        assert_eq!(running.get("proc-game").unwrap().pid, pid);
    }
    for action in [
        Action::DlcRemove {
            id: "proc-game".into(),
            dlc_id: "123".into(),
        },
        Action::DlcRecover {
            id: "proc-game".into(),
            dlc_id: "123".into(),
        },
    ] {
        let blocked = handle_run_action(&state, action).unwrap_err().to_string();
        assert!(blocked.contains("Stop the game"), "{blocked}");
    }

    // Stop game
    let stop_res = handle_stop_game(&state, "proc-game".into()).unwrap();
    assert_eq!(stop_res["status"], "stopped");

    // Verify it is removed from running_games
    {
        let running = state.running_games.lock().unwrap();
        assert!(!running.contains_key("proc-game"));
    }
}

#[test]
fn game_move_waits_for_registered_dlc_task() {
    let root = scratch();
    let state = test_state(&root);
    state.tasks.lock().unwrap().push(TaskInfo {
        id: "dlc-game-200".into(),
        label: "Installing DLC".into(),
        status: "running".into(),
        pid: None,
        game_id: Some("game".into()),
        log_tail: String::new(),
    });
    let error = handle_run_action(
        &state,
        Action::StorageMoveGame {
            id: "game".into(),
            drive_id: "default".into(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("DLC operation"), "{error}");
    assert_eq!(state.tasks.lock().unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_handle_storage_and_settings_actions() {
    let root = scratch();
    let state = test_state(&root);
    let custom_drive = root.join("custom-drive");

    // 1. Add storage drive
    let add_res = handle_run_action(
        &state,
        Action::StorageAddDrive {
            drive_label: Some("External SSD".into()),
            drive_path: custom_drive.to_string_lossy().into(),
        },
    )
    .unwrap();
    let drive_id = add_res["drive"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        install_root_for(&root, Some(&drive_id)).unwrap(),
        custom_drive
    );
    assert!(install_root_for(&root, Some("unconfigured-drive")).is_err());
    fs::create_dir_all(custom_drive.join("unfinished-game")).unwrap();
    let pending_state = handle_get_state(&state).unwrap();
    assert!(
        pending_state["pending"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == "unfinished-game")
    );

    // 2. Set default drive
    handle_run_action(
        &state,
        Action::StorageSetDefault {
            drive_id: drive_id.clone(),
        },
    )
    .unwrap();

    // Check settings in state
    let s = handle_get_state(&state).unwrap();
    let settings = &s["settings"];
    let drives = settings["storage_drives"].as_array().unwrap();
    let ext = drives.iter().find(|d| d["id"] == drive_id).unwrap();
    assert_eq!(ext["is_default"], true);

    // 3. Update settings
    let mut new_settings: azdl::settings::LauncherSettings =
        serde_json::from_value(settings.clone()).unwrap();
    new_settings.seed_color = "#FF00FF".into();
    new_settings.theme_mode = "amoled".into();

    handle_run_action(
        &state,
        Action::SettingsUpdate {
            settings: new_settings,
        },
    )
    .unwrap();

    let s2 = handle_get_state(&state).unwrap();
    assert_eq!(s2["settings"]["seed_color"], "#FF00FF");
    assert_eq!(s2["settings"]["theme_mode"], "amoled");

    // 4. Remove storage drive
    handle_run_action(
        &state,
        Action::StorageRemoveDrive {
            drive_id: drive_id.clone(),
        },
    )
    .unwrap();

    let s3 = handle_get_state(&state).unwrap();
    let drives3 = s3["settings"]["storage_drives"].as_array().unwrap();
    assert!(!drives3.iter().any(|d| d["id"] == drive_id));
}

#[test]
fn test_legacy_source_actions_are_not_exposed() {
    let result: Result<Action, _> = serde_json::from_value(serde_json::json!({
        "action": "source-add",
        "id": "old-feed",
        "location": "/tmp/old-feed.json"
    }));
    assert!(result.is_err());
}

#[test]
fn test_handle_get_state_diagnostics_cached_and_no_steam() {
    let temp = scratch();
    let state = test_state(&temp);

    // First call populates cache and never hangs
    let start = std::time::Instant::now();
    let s1 = handle_get_state(&state).unwrap();
    let first_elapsed = start.elapsed();

    assert!(first_elapsed < std::time::Duration::from_secs(3));
    let diags = s1["diagnostics"].as_array().expect("diagnostics array");
    assert!(!diags.iter().any(|d| d["name"].as_str() == Some("steam")));

    // Second call must use in-memory cache and return virtually instantaneously
    let start2 = std::time::Instant::now();
    let s2 = handle_get_state(&state).unwrap();
    let second_elapsed = start2.elapsed();
    assert!(
        second_elapsed < std::time::Duration::from_millis(50),
        "cached get_state took {second_elapsed:?}"
    );
    assert_eq!(s1["diagnostics"], s2["diagnostics"]);
}

#[test]
fn test_handle_open_folder() {
    let temp = scratch();
    let state = test_state(&temp);
    let game_dir = temp.join("MyGame");
    fs::create_dir_all(&game_dir).unwrap();

    // 1. Direct location
    let res = handle_run_action(
        &state,
        Action::OpenFolder {
            id: None,
            location: Some(game_dir.display().to_string()),
        },
    )
    .unwrap();
    assert_eq!(res["success"], true);

    // 2. Missing path returns error
    let err = handle_run_action(
        &state,
        Action::OpenFolder {
            id: None,
            location: Some(temp.join("NonExistent").display().to_string()),
        },
    );
    assert!(err.is_err());
}

#[test]
fn test_download_status_and_thumbnail_enrichment_and_clear_completed() {
    let temp = scratch();
    let state = test_state(&temp);

    // 1. Game with steam id derives header/cover thumbnail URLs
    let mut library = azdl::load_library(&state.data_dir).unwrap();
    let game_dir = temp.join("Spacewar");
    fs::create_dir_all(&game_dir).unwrap();
    library.games.insert(
        "steam-480".to_string(),
        azdl::Game {
            id: "steam-480".to_string(),
            title: "Spacewar".to_string(),
            install_dir: game_dir.clone(),
            executable: game_dir.join("game.exe"),
            runner: azdl::Runner::Native,
            arguments: vec![],
            environment: std::collections::BTreeMap::new(),
            origin: None,
            steam_app_id: None,
            last_played_unix: None,
            launch_options: vec![],
            selected_launch_option_id: None,
            cover_url: None,
            banner_url: None,
        },
    );
    azdl::save_library(&state.data_dir, &library).unwrap();

    // 2. Add an active downloading job with 16-hex ID
    let jobs_dir = state.data_dir.join("jobs");
    fs::create_dir_all(&jobs_dir).unwrap();
    let running_job = azdl::jobs::DownloadJob {
        id: "0123456789abcdef".to_string(),
        kind: azdl::jobs::JobKind::Game,
        url: "http://example.com/spacewar.zip".to_string(),
        target: game_dir.clone(),
        status: azdl::jobs::JobStatus::Running,
        pid: Some(std::process::id()),
        error: None,
        updated_unix: 1700000000,
        install: Some(azdl::jobs::InstallPlan {
            game_id: "steam-480".to_string(),
            title: "Spacewar".to_string(),
        }),
        install_state: None,
        install_error: None,
        progress_bytes: Some(1024),
        total_bytes: Some(4096),
    };
    let failed_job = azdl::jobs::DownloadJob {
        id: "fedcba9876543210".to_string(),
        kind: azdl::jobs::JobKind::Game,
        url: "http://example.com/failed.zip".to_string(),
        target: temp.join("other"),
        status: azdl::jobs::JobStatus::Failed,
        pid: None,
        error: Some("network error".to_string()),
        updated_unix: 1700000000,
        install: None,
        install_state: None,
        install_error: None,
        progress_bytes: Some(0),
        total_bytes: None,
    };
    fs::write(
        jobs_dir.join("0123456789abcdef.json"),
        serde_json::to_string(&running_job).unwrap(),
    )
    .unwrap();
    fs::write(
        jobs_dir.join("fedcba9876543210.json"),
        serde_json::to_string(&failed_job).unwrap(),
    )
    .unwrap();
    let complete_job = azdl::jobs::DownloadJob {
        id: "abcdef0123456789".to_string(),
        status: azdl::jobs::JobStatus::Complete,
        error: None,
        ..failed_job.clone()
    };
    fs::write(
        jobs_dir.join("abcdef0123456789.json"),
        serde_json::to_string(&complete_job).unwrap(),
    )
    .unwrap();

    // Query state
    let res = handle_get_state(&state).unwrap();

    // Check library game has enriched thumbnails
    let games = res["library"].as_array().expect("library array");
    let spacewar = games
        .iter()
        .find(|g| g["id"].as_str() == Some("steam-480"))
        .expect("spacewar game in library");
    assert!(spacewar["banner_url"].is_null());
    assert!(spacewar["cover_url"].is_null());

    // Check active job status is mapped to "Downloading" and NOT "Running"
    let jobs = res["jobs"].as_array().expect("jobs array");
    let active_job = jobs
        .iter()
        .find(|j| j["id"].as_str() == Some("0123456789abcdef"))
        .expect("active job in list");
    assert_eq!(
        active_job["status"].as_str(),
        Some("Downloading"),
        "active downloading job must be labeled Downloading, not Running"
    );
    let job_install = &active_job["install"];
    assert!(
        job_install["banner_url"]
            .as_str()
            .unwrap_or("")
            .contains("steam/apps/480/header.jpg")
    );

    // Automatic cleanup keeps failed transfers visible.
    let auto_clear = handle_run_action(&state, Action::JobClearSuccessful).unwrap();
    assert_eq!(auto_clear["success"], true);
    assert!(!jobs_dir.join("abcdef0123456789.json").exists());
    assert!(jobs_dir.join("fedcba9876543210.json").exists());

    // Manual cleanup clears failed transfers.
    let clear_res = handle_run_action(&state, Action::JobClearCompleted).unwrap();
    assert_eq!(clear_res["success"], true);

    // Failed job is cleared from disk
    assert!(!jobs_dir.join("fedcba9876543210.json").exists());
    // Active downloading job is retained
    assert!(jobs_dir.join("0123456789abcdef.json").exists());
}

#[test]
fn test_steam_install_background_spawning_and_deduplication() {
    let root = scratch();
    let state = test_state(&root);
    let cached = root.join("downloads/steam-620/620.zip");
    fs::create_dir_all(cached.parent().unwrap()).unwrap();
    fs::write(&cached, b"invalid archive").unwrap();

    let start_time = std::time::Instant::now();
    let res = handle_run_action(
        &state,
        Action::SteamInstall {
            id: "steam:620".into(),
            title: Some("Portal 2".into()),
            banner_url: None,
            cover_url: None,
            location: None,
        },
    )
    .unwrap();
    assert!(start_time.elapsed() < std::time::Duration::from_millis(500));
    assert_eq!(res["started"], true);
    assert_eq!(res["id"], "steam:620");
    assert!(
        state
            .tasks
            .lock()
            .unwrap()
            .iter()
            .any(|task| task.id == "install-steam-620")
    );

    state.tasks.lock().unwrap().push(TaskInfo {
        id: "install-steam-480".into(),
        label: "Downloading Spacewar".into(),
        status: "running".into(),
        pid: None,
        game_id: Some("steam-480".into()),
        log_tail: String::new(),
    });
    let duplicate = handle_run_action(
        &state,
        Action::SteamInstall {
            id: "steam:480".into(),
            title: Some("Spacewar".into()),
            banner_url: None,
            cover_url: None,
            location: None,
        },
    )
    .unwrap();
    assert_eq!(duplicate["already_working"], true);
}
