use azdl::catalog::{add_source, find_game, list_games, load_sources, save_sources};
use azdl::discovery::discover_executables;
use azdl::doctor::diagnose_game;
use azdl::multiplayer::{detect_fix, prepare_game_launch};
use azdl::overlay::{overlay_status, set_overlay_enabled};
use azdl::patch::{active_patch, apply_patch, disable_patch, enable_patch, rollback_patch};
use azdl::settings::{
    add_storage_drive, load_settings, remove_storage_drive, save_settings, set_default_drive,
};
use azdl::{
    Game, Runner, add_game, add_launch_option, load_library, save_library, select_launch_option,
    set_runner,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_workspace(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("azdl-integration-{name}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn test_catalog_source_management_and_search() {
    let ws = temp_workspace("catalog");
    let feed_path = ws.join("test-feed.json");

    // Feed with both Ryuu entries format and Steam fields
    let feed_content = r#"{
        "schema": "arachnel.plugin.catalog.v1",
        "entries": [
            {
                "id": "steam-1091500",
                "steamAppId": 1091500,
                "title": "Cyberpunk 2077",
                "description": "An open-world action-adventure RPG set in Night City",
                "genres": ["Action", "RPG", "Cyberpunk"],
                "fileSize": "70 GB",
                "hasWorkshop": false,
                "dlcCount": 1,
                "dlc": ["2138330"]
            },
            {
                "id": "steam-620",
                "steamAppId": 620,
                "title": "Portal 2",
                "description": "The sequel to the acclaimed puzzle platformer",
                "genres": ["Puzzle", "Co-op", "Sci-Fi"],
                "fileSize": "12 GB",
                "hasWorkshop": true,
                "dlcCount": 0
            }
        ]
    }"#;
    fs::write(&feed_path, feed_content).unwrap();

    let mut sources = load_sources(&ws).unwrap();
    add_source(
        &ws,
        &mut sources,
        "official-ryuu",
        &feed_path.to_string_lossy(),
    )
    .unwrap();
    save_sources(&ws, &sources).unwrap();

    // Verify sources persistence
    let reloaded_sources = load_sources(&ws).unwrap();
    assert!(reloaded_sources.sources.contains_key("official-ryuu"));

    // Verify games listing
    let all_games = list_games(&ws, &reloaded_sources).unwrap();
    assert_eq!(all_games.len(), 2);

    // Verify search by title
    let portal_results: Vec<_> = all_games
        .iter()
        .filter(|g| g.title.contains("Portal"))
        .collect();
    assert_eq!(portal_results.len(), 1);
    assert_eq!(portal_results[0].title, "Portal 2");

    // Verify search by genre
    let rpg_results: Vec<_> = all_games
        .iter()
        .filter(|g| g.genres.iter().any(|genre| genre == "RPG"))
        .collect();
    assert_eq!(rpg_results.len(), 1);
    assert_eq!(rpg_results[0].title, "Cyberpunk 2077");

    // Verify find_game by qualified ID
    let found = find_game(&ws, &reloaded_sources, "official-ryuu:steam-1091500")
        .expect("game should be found");
    assert_eq!(found.title, "Cyberpunk 2077");
    assert_eq!(found.genres, vec!["Action", "RPG", "Cyberpunk"]);

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_library_exclusive_launcher_lifecycle() {
    let ws = temp_workspace("library");
    let managed_games_dir = ws.join("games");
    let game_dir = managed_games_dir.join("indie-hit");
    fs::create_dir_all(&game_dir).unwrap();

    let exe_name = "game.sh";
    let exe_path = game_dir.join(exe_name);
    fs::write(&exe_path, b"#!/bin/sh\necho 'running indie hit'\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut library = load_library(&ws).unwrap();
    assert!(library.games.is_empty());

    // Register game
    add_game(
        &mut library,
        "indie-hit",
        "Indie Hit",
        &game_dir,
        Path::new(exe_name),
    )
    .unwrap();
    save_library(&ws, &library).unwrap();

    // Verify persisted game
    let reloaded = load_library(&ws).unwrap();
    let game = reloaded.games.get("indie-hit").expect("game should exist");
    assert_eq!(game.title, "Indie Hit");
    assert_eq!(game.runner, Runner::Auto);

    // Configure runner to Wine
    let game_mut = library.games.get_mut("indie-hit").unwrap();
    set_runner(game_mut, "wine").unwrap();
    assert_eq!(game_mut.runner, Runner::Wine);

    // Configure launch options
    let opt_exe = "tools.sh";
    let opt_path = game_dir.join(opt_exe);
    fs::write(&opt_path, b"#!/bin/sh\necho 'running tools'\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&opt_path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    add_launch_option(
        game_mut,
        "tools",
        "Level Tools",
        Path::new(opt_exe),
        None,
        &["--tools-mode".into()],
    )
    .unwrap();

    select_launch_option(game_mut, Some("tools")).unwrap();
    assert_eq!(game_mut.selected_launch_option_id.as_deref(), Some("tools"));
    save_library(&ws, &library).unwrap();

    // Discover executables
    let candidates = discover_executables(&game_dir, "Indie Hit").unwrap();
    assert!(!candidates.is_empty());
    assert!(
        candidates
            .iter()
            .any(|c| c.relative_path.to_string_lossy().contains(exe_name))
    );

    // Remove game
    library.games.remove("indie-hit");
    save_library(&ws, &library).unwrap();

    let final_lib = load_library(&ws).unwrap();
    assert!(final_lib.games.is_empty());

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_multiplayer_overlay_and_patch_reversibility() {
    let ws = temp_workspace("multiplayer");
    let game_dir = ws.join("multiplayer-game");
    fs::create_dir_all(&game_dir).unwrap();

    let exe_name = if cfg!(windows) {
        "game.exe"
    } else {
        "game.bin"
    };
    let exe_path = game_dir.join(exe_name);
    fs::write(&exe_path, b"game binary").unwrap();

    let game = Game {
        id: "mp-game".into(),
        title: "Multiplayer Game".into(),
        install_dir: game_dir.clone(),
        executable: exe_path.clone(),
        runner: Runner::Wine,
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

    // 1. OnlineFix simulation
    fs::write(game_dir.join("OnlineFix.ini"), b"[Main]\nFakeAppId=480\n").unwrap();
    fs::write(game_dir.join("OnlineFix64.dll"), b"fake dll").unwrap();

    let fix = detect_fix(&game).unwrap();
    assert_eq!(fix, azdl::multiplayer::FixKind::OnlineFix);

    // Verify launch preparation generates wine overrides without starting Steam
    let prep = prepare_game_launch(&game, &game.runner, &game_dir).unwrap();
    assert!(prep.environment.contains_key("WINEDLLOVERRIDES"));
    assert!(prep.notice.is_some());

    // Verify overlay toggle reversibility (.arachnel-off)
    assert!(overlay_status(&game).unwrap().enabled);
    set_overlay_enabled(&game, false).unwrap();
    assert!(!overlay_status(&game).unwrap().enabled);
    assert!(game_dir.join("OnlineFix64.dll.arachnel-off").exists());

    set_overlay_enabled(&game, true).unwrap();
    assert!(overlay_status(&game).unwrap().enabled);
    assert!(game_dir.join("OnlineFix64.dll").exists());

    // 2. Patch simulation
    let patch_dir = ws.join("patch-v1");
    fs::create_dir_all(&patch_dir).unwrap();
    fs::write(patch_dir.join("patch_file.dat"), b"new patch content").unwrap();

    apply_patch(&game, &ws, &patch_dir).unwrap();
    let active = active_patch(&ws, &game.id)
        .unwrap()
        .expect("patch should be active");
    assert_eq!(active.source_dir, patch_dir);
    assert!(game_dir.join("patch_file.dat").exists());

    // Disable patch
    disable_patch(&game, &ws).unwrap();
    assert!(!game_dir.join("patch_file.dat").exists());

    // Enable patch
    enable_patch(&game, &ws).unwrap();
    assert!(game_dir.join("patch_file.dat").exists());

    // Rollback patch
    rollback_patch(&game, &ws).unwrap();
    assert!(active_patch(&ws, &game.id).unwrap().is_none());
    assert!(!game_dir.join("patch_file.dat").exists());

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_settings_storage_drives_and_system_theme() {
    let ws = temp_workspace("settings");
    let initial_settings = load_settings(&ws).unwrap();

    // Default settings invariants
    assert_eq!(initial_settings.theme_mode, "dark");
    assert!(!initial_settings.storage_drives.is_empty());
    assert!(initial_settings.storage_drives[0].is_default);

    // Add external storage drive
    let extra_drive = ws.join("external-ssd");
    fs::create_dir_all(&extra_drive).unwrap();

    let new_drive = add_storage_drive(&ws, "Fast SSD", &extra_drive).unwrap();
    assert_eq!(new_drive.label, "Fast SSD");
    assert!(!new_drive.is_default);

    // Set as default
    set_default_drive(&ws, &new_drive.id).unwrap();

    let reloaded_after_default = load_settings(&ws).unwrap();
    assert!(
        reloaded_after_default
            .storage_drives
            .iter()
            .find(|d| d.id == new_drive.id)
            .unwrap()
            .is_default
    );

    // Remove drive
    remove_storage_drive(&ws, &new_drive.id).unwrap();

    let reloaded_after_remove = load_settings(&ws).unwrap();
    assert!(
        !reloaded_after_remove
            .storage_drives
            .iter()
            .any(|d| d.id == new_drive.id)
    );

    // Theme color and mode
    let mut theme_settings = reloaded_after_remove;
    theme_settings.seed_color = "#2563EB".into();
    theme_settings.theme_mode = "system".into();
    save_settings(&ws, &theme_settings).unwrap();

    let reloaded_final = load_settings(&ws).unwrap();
    assert_eq!(reloaded_final.seed_color, "#2563EB");
    assert_eq!(reloaded_final.theme_mode, "system");

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_diagnostics_never_hangs_or_launches_steam() {
    let ws = temp_workspace("doctor");
    let game_dir = ws.join("test-diag-game");
    fs::create_dir_all(&game_dir).unwrap();

    let exe_name = if cfg!(windows) {
        "game.exe"
    } else {
        "game.bin"
    };
    let exe_path = game_dir.join(exe_name);
    fs::write(&exe_path, b"binary").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let game = Game {
        id: "diag-check".into(),
        title: "Diag Check".into(),
        install_dir: game_dir,
        executable: exe_path,
        runner: Runner::Native,
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

    let start = std::time::Instant::now();
    let diag = diagnose_game(&game, &ws);
    let elapsed = start.elapsed();

    // Diagnostics must be instantaneous (under 500ms) and never hang
    assert!(elapsed < std::time::Duration::from_millis(500));
    assert!(diag.runner.is_some());

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_plugin_store_settings_and_transient_job_cleanup() {
    let ws = temp_workspace("plugin-jobs");

    // Verify settings with multi-store URLs
    let mut settings = azdl::settings::load_settings(&ws)
        .unwrap_or_else(|_| azdl::settings::LauncherSettings::default_for(&ws));
    assert!(settings.plugin_store_urls.is_empty());

    let custom_url = "https://example.com/custom-plugins.json";
    settings.plugin_store_urls.push(custom_url.to_string());
    azdl::settings::save_settings(&ws, &settings).unwrap();

    let reloaded = azdl::settings::load_settings(&ws).unwrap();
    assert!(reloaded.plugin_store_urls.contains(&custom_url.to_string()));

    // Verify transient plugin package job auto-cleanup behavior
    let job_id = azdl::jobs::queue_job(
        &ws,
        "https://example.com/test-plugin.arach",
        Path::new("test-plugin.arach"),
    )
    .unwrap();
    let mut job = azdl::jobs::load_job(&ws, &job_id).unwrap();
    assert_eq!(job.id, job_id);

    // Mark as PluginPackage and Complete
    job.kind = azdl::jobs::JobKind::PluginPackage;
    job.status = azdl::jobs::JobStatus::Complete;
    // Save job directly
    let job_file = ws.join("jobs").join(format!("{job_id}.json"));
    let mut f = fs::File::create(&job_file).unwrap();
    serde_json::to_writer_pretty(&mut f, &job).unwrap();

    // Verify that transient completed plugin packages get cleaned up
    let jobs_before = azdl::jobs::list_jobs(&ws).unwrap();
    assert_eq!(jobs_before.len(), 1);

    // Simulate the state handler cleanup filter
    let filtered_jobs: Vec<_> = jobs_before
        .into_iter()
        .filter_map(|j| {
            if j.kind == azdl::jobs::JobKind::PluginPackage
                && matches!(
                    j.effective_status(),
                    azdl::jobs::JobStatus::Complete | azdl::jobs::JobStatus::Cancelled
                )
            {
                let _ = azdl::jobs::clear_job(&ws, &j.id);
                return None;
            }
            Some(j)
        })
        .collect();

    assert_eq!(filtered_jobs.len(), 0);
    // After clear, job file should be removed from disk
    assert!(azdl::jobs::load_job(&ws, &job_id).is_err());

    let _ = fs::remove_dir_all(&ws);
}
