use azdl::discovery::discover_executables;
use azdl::doctor::diagnose_game;
use azdl::multiplayer::{detect_fix, prepare_game_launch};
use azdl::overlay::{overlay_status, set_overlay_enabled};
use azdl::patch::{active_patch, apply_patch, disable_patch, enable_patch, rollback_patch};
use azdl::settings::{
    add_storage_drive, load_settings, move_game_to_drive, remove_storage_drive, save_settings,
    set_default_drive,
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
    fs::canonicalize(dir).unwrap()
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
fn test_move_game_across_drives_migrates_tree_and_remaps_executable() {
    let ws = temp_workspace("drive-migration");
    let drive_a = ws.join("drive_a");
    let drive_b = ws.join("drive_b");
    fs::create_dir_all(&drive_a).unwrap();
    fs::create_dir_all(&drive_b).unwrap();

    let mut settings =
        load_settings(&ws).unwrap_or_else(|_| azdl::settings::LauncherSettings::default_for(&ws));
    settings.storage_drives = vec![
        azdl::settings::StorageDrive {
            id: "drive-a".into(),
            label: "Drive A".into(),
            path: drive_a.clone(),
            is_default: true,
        },
        azdl::settings::StorageDrive {
            id: "drive-b".into(),
            label: "Drive B".into(),
            path: drive_b.clone(),
            is_default: false,
        },
    ];
    save_settings(&ws, &settings).unwrap();

    // Setup game tree in drive A with nested shipping binary and assets
    let game_dir = drive_a.join("test-game");
    let shipping_dir = game_dir.join("Binaries").join("Win64");
    fs::create_dir_all(&shipping_dir).unwrap();
    let exe = shipping_dir.join("game.exe");
    fs::write(&exe, b"MZ_GAME_BIN").unwrap();
    let asset = game_dir.join("Data").join("assets.pak");
    fs::create_dir_all(asset.parent().unwrap()).unwrap();
    fs::write(&asset, b"PAK_DATA").unwrap();

    let mut library = azdl::load_library(&ws).unwrap();
    add_game(&mut library, "test-game", "Test Game", &game_dir, &exe).unwrap();
    save_library(&ws, &library).unwrap();

    // Migrate to drive B
    let new_path = move_game_to_drive(&ws, &mut library, "test-game", "drive-b").unwrap();
    assert_eq!(new_path, drive_b.join("test-game"));

    // Verify files on destination drive
    assert!(drive_b.join("test-game/Binaries/Win64/game.exe").is_file());
    assert!(drive_b.join("test-game/Data/assets.pak").is_file());

    // Verify source files purged
    assert!(!game_dir.exists());

    // Verify library metadata remapped
    let moved_game = &library.games["test-game"];
    assert_eq!(moved_game.install_dir, drive_b.join("test-game"));
    assert_eq!(
        moved_game.executable,
        drive_b.join("test-game/Binaries/Win64/game.exe")
    );

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_native_runner_prepares_online_fix_and_propagates_payload() {
    let ws = temp_workspace("native-fix-prep");
    let game_dir = ws.join("space-game");
    let shipping_dir = game_dir.join("Space").join("Binaries").join("Win64");
    fs::create_dir_all(&shipping_dir).unwrap();

    let exe = shipping_dir.join("SpaceGame-Win64-Shipping.exe");
    fs::write(&exe, b"MZ_SPACE_GAME").unwrap();

    // Plant OnlineFix files in the root
    fs::write(game_dir.join("OnlineFix64.dll"), b"DLL_PAYLOAD").unwrap();
    fs::write(
        game_dir.join("OnlineFix.ini"),
        b"[Main]\r\nFakeAppId=480\r\n",
    )
    .unwrap();

    let game = Game {
        id: "space-game".into(),
        title: "Space Game".into(),
        install_dir: game_dir.clone(),
        executable: exe.clone(),
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

    assert_eq!(
        detect_fix(&game).unwrap(),
        azdl::multiplayer::FixKind::OnlineFix
    );

    // Call prepare_game_launch with Runner::Native for the shipping working directory
    let prep = prepare_game_launch(&game, &Runner::Native, &shipping_dir).unwrap();

    assert!(prep.notice.is_some());
    assert!(prep.notice.unwrap().contains("Online Fix"));

    // Verify steam_appid.txt is placed beside shipping exe
    let appid_file = shipping_dir.join("steam_appid.txt");
    assert!(appid_file.is_file());
    assert_eq!(fs::read_to_string(&appid_file).unwrap().trim(), "480");

    // Verify OnlineFix64.dll and OnlineFix.ini were propagated downward to the shipping binary folder
    assert!(shipping_dir.join("OnlineFix64.dll").is_file());
    assert!(shipping_dir.join("OnlineFix.ini").is_file());

    let _ = fs::remove_dir_all(&ws);
}
