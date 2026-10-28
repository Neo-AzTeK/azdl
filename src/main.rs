use azdl::archive::{fetch_archive, fetch_archive_parts, install_archive};
use azdl::catalog::{
    InstallOutcome, attach_pending_setup_origin, clear_pending_setup_origin, install_game,
};
use azdl::discovery::{discover_executables, import_game};
use azdl::doctor::diagnose_game;
#[cfg(test)]
use azdl::jobs::queue_install_job;
use azdl::jobs::{
    InstallFinish, InstallKind, JobKind, cancel_job, finish_job_install, list_jobs, load_job,
    mark_install_registered, pause_job, queue_install_job_with_sha256, queue_job_with_sha256,
    recorded_download, retry_job, start_background,
};
use azdl::multiplayer::{FixKind, detect_fix, steam_running};
use azdl::overlay::{overlay_status, set_overlay_enabled};
use azdl::patch::{
    active_patch, apply_patch, apply_patch_archive_with_root, disable_patch, enable_patch,
    fetch_patch_archive, fetch_patch_archive_parts, rollback_patch,
};
use azdl::runtime::discover_proton;
use azdl::setup::{
    SetupOutcome, fetch_setup, fetch_setup_archive, import_setup, run_setup, run_setup_archive,
    scan_setup,
};
use azdl::shortcuts::{create_shortcut, remove_shortcut, shortcut_status};
use azdl::steam::{import_steam, scan_steam, steam_root};
use azdl::steam_discovery::discover_games;
use azdl::steam_shortcuts::{
    create_shortcut as create_steam_shortcut, remove_shortcut as remove_steam_shortcut,
    shortcut_status as steam_shortcut_status,
};
use azdl::torrent::fetch_magnet;
use azdl::{
    Library, Result, add_game, add_launch_option, data_dir, fetch_game, install_file, load_library,
    remove_launch_option, resolve_launch, save_library, select_launch_option, set_executable,
    set_runner, spawn_game,
};
use std::env;
use std::fs;
use std::path::Path;

fn usage() -> &'static str {
    "Usage:\n  azdl list\n  azdl runners\n  azdl shortcut create|status|remove <id> [desktop-directory]\n  azdl steam shortcut create|status|remove <id> [shortcuts.vdf]\n  azdl jobs fetch <https-url> <relative-destination> [sha256]\n  azdl jobs fetch-install <id> <title> <file|archive|setup|setup-archive> <https-url> <filename> [sha256]\n  azdl jobs list|show <job-id>|retry <job-id>\n  azdl jobs pause|resume|cancel <job-id>\n  azdl scan <title> <install-dir>\n  azdl import <id> <title> <install-dir>\n  azdl add <id> <title> <install-dir> <executable>\n  azdl fetch <id> <title> <https-url> <filename>\n  azdl fetch-magnet <id> <title> <magnet-uri>\n  azdl install-file <id> <title> <file-path>\n  azdl install-archive <id> <title> <archive-path>\n  azdl fetch-archive <id> <title> <https-url> <archive-filename>\n  azdl fetch-archive-parts <id> <title> <first-part-filename> <part1-https-url> <part2-https-url> [...]\n  azdl setup run <id> <title> <installer.exe|installer.msi> [installer args...]\n  azdl setup fetch <id> <title> <https-url> <installer-filename> [installer args...]\n  azdl setup archive <id> <title> <archive-path> [installer args...]\n  azdl setup fetch-archive <id> <title> <https-url> <archive-filename> [installer args...]\n  azdl setup scan|import <id> <title>\n  azdl setup add <id> <title> <install-dir> <executable>\n  azdl pending add <id> <title> <portable|setup> <install-dir> <executable>\n  azdl steam featured|search <query>\n  azdl steam install <appid>\n  azdl steam scan|import <appid>\n  azdl patch apply <id> <extracted-patch-dir>\n  azdl patch archive <id> <archive-path> [archive-root]\n  azdl patch fetch <id> <https-url> <archive-filename>\n  azdl patch fetch-parts <id> <first-part-filename> <part1-https-url> <part2-https-url> [...]\n  azdl patch status|enable|disable|rollback <id>\n  azdl overlay status|enable|disable <id>\n  azdl fix-status <id>\n  azdl doctor <id>\n  azdl executable <id> <path-inside-install-dir>\n  azdl runner <id> <auto|native|wine|proton:auto|proton:/path/to/proton>\n  azdl option list <id>\n  azdl option add <id> <option-id> <title> <executable> <working-dir|-> [arguments...]\n  azdl option select <id> <option-id|default>\n  azdl option remove <id> <option-id>\n  azdl args <id> [launch arguments...]\n  azdl env <id> <name> <value>\n  azdl env-remove <id> <name>\n  azdl launch <id> [extra arguments...]\n  azdl download <id> <https-url> <relative-destination>\n  azdl steamstub status|unpack|rollback <id>\n  azdl goldberg sync|status|rollback <id>\n  azdl dependencies <id> [status|install-vcredist|install-directx]\n  azdl installscript scan|apply <id>\n  azdl fixes ea-heal|ea-rollback|provision-steam-api|patch-dlc-installed|patch-dlc-installed-rollback <id> [dlc-ids...]\n  azdl remove <id>\n\nUse AZDL_DATA_DIR to choose a library and prefix directory."
}

fn report_setup(outcome: SetupOutcome, id: &str) {
    match outcome {
        SetupOutcome::Imported(path) => println!("Installed and added {id} ({})", path.display()),
        SetupOutcome::NeedsSelection(candidates) => {
            println!("Setup finished, but {id} needs an executable selection.");
            for candidate in candidates.into_iter().take(10) {
                println!(
                    "{}\t{}\t{}",
                    candidate.score,
                    candidate.install_dir.display(),
                    candidate.executable.display()
                );
            }
            println!(
                "Use `azdl setup add {id} <title> <install-dir> <executable>` after choosing a game executable."
            );
        }
    }
}

fn save_pending_result(library: &mut Library, data_dir: &Path, id: &str) -> Result<()> {
    let game = library.games.get_mut(id).ok_or("setup game missing")?;
    attach_pending_setup_origin(data_dir, game)?;
    save_library(data_dir, library)?;
    if let Err(error) = clear_pending_setup_origin(data_dir, id) {
        eprintln!("Game saved; pending install cleanup failed: {error}");
    }
    Ok(())
}

fn finish_queued_install(data_dir: &Path, job_id: &str) -> Result<InstallFinish> {
    finish_job_install(data_dir, job_id, |plan, download| {
        let mut library = load_library(data_dir)?;
        let managed = data_dir.join("games").join(&plan.game_id);
        let prefix = data_dir.join("prefixes").join(&plan.game_id);
        if let Some(existing) = library.games.get(&plan.game_id) {
            let expected = match plan.kind {
                InstallKind::File | InstallKind::Archive => &managed,
                InstallKind::Setup | InstallKind::SetupArchive => &prefix,
            };
            if existing.title == plan.title && existing.install_dir.starts_with(expected) {
                return Ok(InstallFinish::Complete);
            }
            return Err("game ID was registered by another install".into());
        }
        let outcome = match plan.kind {
            InstallKind::File => {
                install_file(&mut library, data_dir, &plan.game_id, &plan.title, download)?;
                InstallFinish::Complete
            }
            InstallKind::Archive => {
                if fs::symlink_metadata(&managed).is_ok() {
                    return Err("install directory appeared after the job was queued".into());
                }
                let install =
                    install_archive(&mut library, data_dir, &plan.game_id, &plan.title, download);
                match install {
                    Ok(_) => InstallFinish::Complete,
                    Err(error)
                        if managed.is_dir()
                            && discover_executables(&managed, &plan.title).is_ok() =>
                    {
                        eprintln!("Game files need an executable selection: {error}");
                        InstallFinish::NeedsSelection
                    }
                    Err(error) => return Err(error),
                }
            }
            InstallKind::Setup => match run_setup(
                &mut library,
                data_dir,
                &plan.game_id,
                &plan.title,
                download,
                &[],
            )? {
                SetupOutcome::Imported(_) => InstallFinish::Complete,
                SetupOutcome::NeedsSelection(_) => InstallFinish::NeedsSelection,
            },
            InstallKind::SetupArchive => match run_setup_archive(
                &mut library,
                data_dir,
                &plan.game_id,
                &plan.title,
                download,
                &[],
            )? {
                SetupOutcome::Imported(_) => InstallFinish::Complete,
                SetupOutcome::NeedsSelection(_) => InstallFinish::NeedsSelection,
            },
        };
        if outcome == InstallFinish::Complete {
            if matches!(plan.kind, InstallKind::Setup | InstallKind::SetupArchive) {
                save_pending_result(&mut library, data_dir, &plan.game_id)?;
            } else {
                save_library(data_dir, &library)?;
            }
        }
        Ok(outcome)
    })
}

fn retry_queued_job(data_dir: &Path, id: &str) -> Result<std::path::PathBuf> {
    let job = load_job(data_dir, id)?;
    if job.url.starts_with("steam://") {
        return azdl::steam_install::resume_job(data_dir, id);
    }
    let path = if job.target.is_file() {
        job.target
    } else {
        retry_job(data_dir, id)?
    };
    if job.install.is_some() {
        match finish_queued_install(data_dir, id)? {
            InstallFinish::Complete => println!("Installed game from {}", path.display()),
            InstallFinish::NeedsSelection => {
                println!("Downloaded game needs an executable selection")
            }
        }
    }
    Ok(path)
}

fn remember_launch(data_dir: &Path, id: &str, played_unix: Option<u64>) {
    let result = (|| -> Result<()> {
        let mut current = load_library(data_dir)?;
        let game = current
            .games
            .get_mut(id)
            .ok_or("launched game is no longer in the library")?;
        game.last_played_unix = played_unix;
        save_library(data_dir, &current)
    })();
    if let Err(error) = result {
        eprintln!("Game started, but play time could not be saved: {error}");
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let data = data_dir()?;
    let mut library = load_library(&data)?;
    match args.first().map(String::as_str) {
        Some("list") if args.len() == 1 => {
            for game in library.games.values() {
                println!(
                    "{}\t{}\t{}",
                    game.id,
                    game.title,
                    game.install_dir.display()
                );
            }
        }
        Some("jobs") if args.len() == 2 && args[1] == "list" => {
            for job in list_jobs(&data)? {
                println!(
                    "{}\t{:?}\t{:?}\t{} B\t{}{}",
                    job.id,
                    job.effective_status(),
                    job.kind,
                    job.downloaded_bytes(),
                    job.target.display(),
                    job.effective_install_state()
                        .map(|state| format!("\tinstall={state:?}"))
                        .unwrap_or_default()
                );
            }
        }
        Some("jobs") if args.len() == 3 && args[1] == "show" => {
            let mut job = load_job(&data, &args[2])?;
            job.status = job.effective_status();
            println!("{}", serde_json::to_string_pretty(&job)?);
            println!("Downloaded bytes: {}", job.downloaded_bytes());
        }
        Some("jobs") if args.len() == 3 && args[1] == "retry" => {
            let path = retry_queued_job(&data, &args[2])?;
            println!("Downloaded {}", path.display());
        }
        Some("jobs") if (args.len() == 7 || args.len() == 8) && args[1] == "fetch-install" => {
            let kind = match args[4].as_str() {
                "file" => InstallKind::File,
                "archive" => InstallKind::Archive,
                "setup" => InstallKind::Setup,
                "setup-archive" => InstallKind::SetupArchive,
                _ => return Err("unknown install type".into()),
            };
            let id = queue_install_job_with_sha256(
                &data,
                &args[2],
                &args[3],
                kind,
                &args[5],
                Path::new(&args[6]),
                args.get(7).map(String::as_str),
            )?;
            let pid = start_background(&data, &id)?;
            println!("Started download and install job {id} (worker {pid})");
        }
        Some("jobs") if (args.len() == 4 || args.len() == 5) && args[1] == "fetch" => {
            let id = queue_job_with_sha256(
                &data,
                &args[2],
                Path::new(&args[3]),
                args.get(4).map(String::as_str),
            )?;
            let pid = start_background(&data, &id)?;
            println!("Started download job {id} (worker {pid})");
        }
        Some("jobs") if args.len() == 3 && args[1] == "resume" => {
            let pid = start_background(&data, &args[2])?;
            println!("Resumed download job {} (worker {pid})", args[2]);
        }
        Some("jobs") if args.len() == 3 && args[1] == "pause" => {
            pause_job(&data, &args[2])?;
            println!("Pause requested for download job {}", args[2]);
        }
        Some("jobs") if args.len() == 3 && args[1] == "cancel" => {
            cancel_job(&data, &args[2])?;
            println!("Cancellation requested for download job {}", args[2]);
        }
        Some("runners") if args.len() == 1 => {
            for entry in discover_proton(&data)? {
                println!("{}\t{}\t{}", entry.name, entry.source, entry.path.display());
            }
        }
        Some("shortcut") if (args.len() == 3 || args.len() == 4) => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            let directory = args.get(3).map(Path::new);
            match args[1].as_str() {
                "create" => {
                    let path = create_shortcut(game, &data, &env::current_exe()?, directory)?;
                    println!("Created {}", path.display());
                }
                "status" => println!(
                    "{}",
                    if shortcut_status(game, &data, directory)? {
                        "installed"
                    } else {
                        "absent"
                    }
                ),
                "remove" => println!(
                    "{}",
                    if remove_shortcut(game, &data, directory)? {
                        "removed"
                    } else {
                        "absent"
                    }
                ),
                _ => return Err(usage().into()),
            }
        }
        Some("steamstub") if args.len() == 3 => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            match args[1].as_str() {
                "status" => {
                    let status = azdl::steamless::scan_game_steamstub(game);
                    println!("SteamStub for {}: {}", game.id, status.label);
                }
                "unpack" => {
                    let stripped = azdl::steamless::unpack_game(game, &data)?;
                    println!("Unpacked {} executable(s) for {}", stripped, game.id);
                }
                "rollback" => {
                    let restored = azdl::steamless::rollback_game(game)?;
                    println!("Restored {} executable(s) for {}", restored, game.id);
                }
                _ => return Err(usage().into()),
            }
        }
        Some("fixes") if args.len() >= 3 => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            match args[1].as_str() {
                "ea-heal" => {
                    let patched = azdl::fixes::heal_ea_activation(&game.install_dir)?;
                    println!(
                        "EA activation heal for {}: {}",
                        game.id,
                        if patched {
                            "patched"
                        } else {
                            "not needed / clean"
                        }
                    );
                }
                "ea-rollback" => {
                    let restored = azdl::fixes::rollback_ea_activation(&game.install_dir)?;
                    println!(
                        "EA activation rollback for {}: {}",
                        game.id,
                        if restored {
                            "restored"
                        } else {
                            "no backup found"
                        }
                    );
                }
                "provision-steam-api" => {
                    let res =
                        azdl::fixes::ensure_steam_api_dll(&game.install_dir, &game.executable)?;
                    println!(
                        "Steam API provisioning for {}: {}",
                        game.id,
                        res.unwrap_or_else(|| "already satisfied".into())
                    );
                }
                "patch-dlc-installed" => {
                    let mut dlc_ids = Vec::new();
                    for arg in args.iter().skip(3) {
                        if let Ok(id) = arg.parse::<u32>() {
                            dlc_ids.push(id);
                        }
                    }
                    let patched =
                        azdl::fixes::patch_dlc_installed_ids(&game.install_dir, &dlc_ids)?;
                    println!(
                        "DLC installed patch for {}: {}",
                        game.id,
                        if patched {
                            "patched"
                        } else {
                            "not needed / clean"
                        }
                    );
                }
                "patch-dlc-installed-rollback" => {
                    let restored = azdl::fixes::rollback_dlc_installed(&game.install_dir)?;
                    println!(
                        "DLC installed patch rollback for {}: {}",
                        game.id,
                        if restored {
                            "restored"
                        } else {
                            "no backup found"
                        }
                    );
                }
                _ => return Err(usage().into()),
            }
        }
        Some("goldberg") if args.len() == 3 => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            match args[1].as_str() {
                "sync" => {
                    let updated = azdl::goldberg::prepare_goldberg_for_game(game, &data)?;
                    println!(
                        "Goldberg sync for {}: {}",
                        game.id,
                        if updated {
                            "synchronized"
                        } else {
                            "already up to date / not needed"
                        }
                    );
                }
                "status" => {
                    let compatible = azdl::goldberg::is_goldberg_compatible(&game.install_dir);
                    let has_settings = azdl::goldberg::has_goldberg_settings(&game.install_dir);
                    println!(
                        "Goldberg status for {}: compatible={}, has_settings={}",
                        game.id, compatible, has_settings
                    );
                }
                "rollback" => {
                    let rolled_back = azdl::goldberg::rollback_goldberg_config(&game.install_dir)?;
                    println!(
                        "Goldberg rollback for {}: {}",
                        game.id,
                        if rolled_back {
                            "restored"
                        } else {
                            "no managed config found"
                        }
                    );
                }
                _ => return Err(usage().into()),
            }
        }
        Some("dependencies") if args.len() >= 2 => {
            let game = library.games.get(&args[1]).ok_or("unknown game ID")?;
            let prefix_dir = data.join("prefixes").join(&game.id);
            if args.len() == 2 || args[2] == "status" {
                let status = azdl::dependencies::diagnose_prefix_dependencies(&prefix_dir);
                println!(
                    "Dependencies for {}: vcredist_x64={}, vcredist_x86={}, directx={}",
                    game.id, status.vcredist_x64, status.vcredist_x86, status.directx
                );
                if !status.missing.is_empty() {
                    println!("Missing: {}", status.missing.join(", "));
                }
            } else if args[2] == "install-vcredist" {
                let ok =
                    azdl::dependencies::install_vcredist(&prefix_dir, &game.runner, &data, true)?;
                println!("Installed VC++ 2015-2022 (x64) for {}: {}", game.id, ok);
            } else if args[2] == "install-directx" {
                let ok = azdl::dependencies::install_directx(
                    &prefix_dir,
                    &game.runner,
                    &data,
                    &game.install_dir,
                )?;
                println!("Installed DirectX for {}: {}", game.id, ok);
            } else {
                return Err(usage().into());
            }
        }
        Some("installscript") if args.len() >= 3 => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            let prefix_dir = data.join("prefixes").join(&game.id);
            match args[1].as_str() {
                "scan" => {
                    let files = azdl::installscript::find_installscript_files(&game.install_dir);
                    let ubisoft = azdl::installscript::is_ubisoft_game(&game.install_dir);
                    println!("Install scripts found: {}", files.len());
                    for f in files {
                        println!("  {}", f.display());
                    }
                    println!("Ubisoft DRM detected: {ubisoft}");
                }
                "apply" => {
                    let ok = azdl::installscript::apply_installscript_for_game(game, &prefix_dir)?;
                    println!(
                        "Applied installscript registry entries for {}: {}",
                        game.id, ok
                    );
                }
                _ => return Err(usage().into()),
            }
        }
        Some("scan") if args.len() == 3 => {
            for candidate in discover_executables(Path::new(&args[2]), &args[1])? {
                println!("{}\t{}", candidate.score, candidate.relative_path.display());
            }
        }
        Some("import") if args.len() == 4 => {
            let selected = import_game(&mut library, &args[1], &args[2], Path::new(&args[3]))?;
            save_library(&data, &library)?;
            println!("Added {} ({})", args[1], selected.display());
        }
        Some("add") if args.len() == 5 => {
            add_game(
                &mut library,
                &args[1],
                &args[2],
                Path::new(&args[3]),
                Path::new(&args[4]),
            )?;
            save_library(&data, &library)?;
            if let Err(error) = mark_install_registered(&data, &args[1]) {
                eprintln!("Game added, but install job status could not be updated: {error}");
            }
            println!("Added {}", args[1]);
        }
        Some("fetch") if args.len() == 5 => {
            fetch_game(
                &mut library,
                &data,
                &args[1],
                &args[2],
                &args[3],
                Path::new(&args[4]),
            )?;
            save_library(&data, &library)?;
            println!("Downloaded and added {}", args[1]);
        }
        Some("install-file") if args.len() == 4 => {
            let dir = install_file(&mut library, &data, &args[1], &args[2], Path::new(&args[3]))?;
            save_library(&data, &library)?;
            println!("Installed {} to {}", args[1], dir.display());
        }
        Some("fetch-magnet") if args.len() == 4 => {
            let dir = fetch_magnet(&mut library, &data, &args[1], &args[2], &args[3])?;
            save_library(&data, &library)?;
            println!("Downloaded and installed {} to {}", args[1], dir.display());
        }
        Some("install-archive") if args.len() == 4 => {
            let dir =
                install_archive(&mut library, &data, &args[1], &args[2], Path::new(&args[3]))?;
            save_library(&data, &library)?;
            println!("Installed {} to {}", args[1], dir.display());
        }
        Some("fetch-archive") if args.len() == 5 => {
            let dir = fetch_archive(
                &mut library,
                &data,
                &args[1],
                &args[2],
                &args[3],
                Path::new(&args[4]),
            )?;
            save_library(&data, &library)?;
            println!("Downloaded and installed {} to {}", args[1], dir.display());
        }
        Some("fetch-archive-parts") if args.len() >= 6 => {
            let dir = fetch_archive_parts(
                &mut library,
                &data,
                &args[1],
                &args[2],
                Path::new(&args[3]),
                &args[4..],
            )?;
            save_library(&data, &library)?;
            println!("Downloaded and installed {} to {}", args[1], dir.display());
        }
        Some("setup") if args.len() >= 5 && args[1] == "run" => {
            let outcome = run_setup(
                &mut library,
                &data,
                &args[2],
                &args[3],
                Path::new(&args[4]),
                &args[5..],
            )?;
            if matches!(outcome, SetupOutcome::Imported(_)) {
                save_pending_result(&mut library, &data, &args[2])?;
            }
            report_setup(outcome, &args[2]);
        }
        Some("setup") if args.len() >= 6 && args[1] == "fetch" => {
            let (installer, outcome) = fetch_setup(
                &mut library,
                &data,
                &args[2],
                &args[3],
                &args[4],
                Path::new(&args[5]),
                &args[6..],
            )?;
            if matches!(outcome, SetupOutcome::Imported(_)) {
                save_pending_result(&mut library, &data, &args[2])?;
            }
            println!("Downloaded installer to {}", installer.display());
            report_setup(outcome, &args[2]);
        }
        Some("setup") if args.len() >= 5 && args[1] == "archive" => {
            let outcome = run_setup_archive(
                &mut library,
                &data,
                &args[2],
                &args[3],
                Path::new(&args[4]),
                &args[5..],
            )?;
            if matches!(outcome, SetupOutcome::Imported(_)) {
                save_pending_result(&mut library, &data, &args[2])?;
            }
            report_setup(outcome, &args[2]);
        }
        Some("setup") if args.len() >= 6 && args[1] == "fetch-archive" => {
            let (archive, outcome) = fetch_setup_archive(
                &mut library,
                &data,
                &args[2],
                &args[3],
                &args[4],
                Path::new(&args[5]),
                &args[6..],
            )?;
            if matches!(outcome, SetupOutcome::Imported(_)) {
                save_pending_result(&mut library, &data, &args[2])?;
            }
            println!("Downloaded setup archive to {}", archive.display());
            report_setup(outcome, &args[2]);
        }
        Some("setup") if args.len() == 4 && args[1] == "scan" => {
            for candidate in scan_setup(&data, &args[2], &args[3])? {
                println!(
                    "{}\t{}\t{}",
                    candidate.score,
                    candidate.install_dir.display(),
                    candidate.executable.display()
                );
            }
        }
        Some("setup") if args.len() == 4 && args[1] == "import" => {
            let outcome = import_setup(&mut library, &data, &args[2], &args[3])?;
            if matches!(outcome, SetupOutcome::Imported(_)) {
                save_pending_result(&mut library, &data, &args[2])?;
            }
            report_setup(outcome, &args[2]);
        }
        Some("setup") if args.len() == 6 && args[1] == "add" => {
            add_game(
                &mut library,
                &args[2],
                &args[3],
                Path::new(&args[4]),
                Path::new(&args[5]),
            )?;
            library
                .games
                .get_mut(&args[2])
                .ok_or("added game missing")?
                .runner = azdl::Runner::Wine;
            save_pending_result(&mut library, &data, &args[2])?;
            println!("Added {} with its Wine prefix", args[2]);
        }
        Some("pending") if args.len() == 7 && args[1] == "add" => {
            let id = &args[2];
            azdl::validate_id(id)?;
            let install = fs::canonicalize(&args[5])?;
            match args[4].as_str() {
                "portable" => {
                    let pending = data.join("games").join(id);
                    let metadata = fs::symlink_metadata(&pending)?;
                    if !metadata.is_dir() || metadata.file_type().is_symlink() {
                        return Err("pending portable install is not a directory".into());
                    }
                    let expected = fs::canonicalize(pending)?;
                    if install != expected {
                        return Err(
                            "pending portable install directory does not match game ID".into()
                        );
                    }
                }
                "setup" => {
                    let prefix = data.join("prefixes").join(id);
                    let prefix_metadata = fs::symlink_metadata(&prefix)?;
                    let drive_path = prefix.join("drive_c");
                    let drive_metadata = fs::symlink_metadata(&drive_path)?;
                    if !prefix_metadata.is_dir()
                        || prefix_metadata.file_type().is_symlink()
                        || !drive_metadata.is_dir()
                        || drive_metadata.file_type().is_symlink()
                    {
                        return Err("pending setup prefix is not a directory".into());
                    }
                    let drive = fs::canonicalize(drive_path)?;
                    if !install.starts_with(&drive) {
                        return Err("pending setup directory is outside its Wine prefix".into());
                    }
                }
                _ => return Err("pending install kind must be portable or setup".into()),
            }
            add_game(&mut library, id, &args[3], &install, Path::new(&args[6]))?;
            if args[4] == "setup" {
                library
                    .games
                    .get_mut(id)
                    .ok_or("added game missing")?
                    .runner = azdl::Runner::Wine;
            }
            save_pending_result(&mut library, &data, id)?;
            if let Err(error) = mark_install_registered(&data, id) {
                eprintln!("Game added, but install job status could not be updated: {error}");
            }
            println!("Added pending game {id}");
        }
        Some("steam") if args.len() == 2 && args[1] == "featured" => {
            for game in discover_games("")? {
                println!("{}\t{}", game.app_id, game.title);
            }
        }
        Some("steam") if args.len() == 3 && args[1] == "search" => {
            for game in discover_games(&args[2])? {
                println!("{}\t{}", game.app_id, game.title);
            }
        }
        Some("steam") if args.len() == 2 && args[1] == "scan" => {
            for game in scan_steam(&steam_root()?)? {
                println!(
                    "{}\t{}\t{}\t{}",
                    game.app_id,
                    if game.installed {
                        "installed"
                    } else {
                        "pending"
                    },
                    game.title,
                    game.install_dir.display()
                );
            }
        }
        Some("steam") if args.len() == 3 && args[1] == "install" => {
            let app_id: u32 = args[2].parse()?;
            let title = discover_games(&args[2])?
                .into_iter()
                .find(|game| game.app_id == app_id)
                .map(|game| game.title)
                .ok_or("Steam App ID was not found in the Store")?;
            let game = azdl::steam_install::game_for_app(app_id, &title)?;
            match install_game(&mut library, &data, &game)? {
                InstallOutcome::Installed(id) => {
                    save_library(&data, &library)?;
                    println!("Installed Steam app {app_id} as {id}");
                }
                InstallOutcome::NeedsSelection(id) => {
                    println!("Files for {id} need an executable selection in the desktop launcher");
                }
                InstallOutcome::SteamRequested(_) => {
                    return Err("download provider did not install the game".into());
                }
            }
        }
        Some("steam") if args.len() == 3 && args[1] == "import" => {
            let id = import_steam(&mut library, &steam_root()?, args[2].parse()?)?;
            save_library(&data, &library)?;
            println!("Added {} from Steam", id);
        }
        Some("steam") if (args.len() == 4 || args.len() == 5) && args[1] == "shortcut" => {
            let game = library.games.get(&args[3]).ok_or("unknown game ID")?;
            let path = args.get(4).map(Path::new);
            match args[2].as_str() {
                "create" => {
                    let (path, app_id) = create_steam_shortcut(game, path)?;
                    println!("Created Steam shortcut {app_id} in {}", path.display());
                }
                "status" => println!(
                    "{}",
                    if steam_shortcut_status(game, path)? {
                        "installed"
                    } else {
                        "absent"
                    }
                ),
                "remove" => println!(
                    "{}",
                    if remove_steam_shortcut(game, path)? {
                        "removed"
                    } else {
                        "absent"
                    }
                ),
                _ => return Err(usage().into()),
            }
        }
        Some("patch") if args.len() == 4 && args[1] == "apply" => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            let count = apply_patch(game, &data, Path::new(&args[3]))?;
            println!("Applied {count} patch files to {}", args[2]);
        }
        Some("patch") if (args.len() == 4 || args.len() == 5) && args[1] == "archive" => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            let count = apply_patch_archive_with_root(
                game,
                &data,
                Path::new(&args[3]),
                args.get(4).map(Path::new),
            )?;
            println!("Applied {count} patch files to {}", args[2]);
        }
        Some("patch") if args.len() == 5 && args[1] == "fetch" => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            let (archive, count) = fetch_patch_archive(game, &data, &args[3], Path::new(&args[4]))?;
            println!(
                "Downloaded {} and applied {count} patch files to {}",
                archive.display(),
                args[2]
            );
        }
        Some("patch") if args.len() >= 6 && args[1] == "fetch-parts" => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            let (archive, count) =
                fetch_patch_archive_parts(game, &data, Path::new(&args[3]), &args[4..])?;
            println!(
                "Downloaded {} and applied {count} patch files to {}",
                archive.display(),
                args[2]
            );
        }
        Some("patch") if args.len() == 3 && args[1] == "status" => {
            if let Some(patch) = active_patch(&data, &args[2])? {
                let source = patch.source_archive.as_ref().unwrap_or(&patch.source_dir);
                println!(
                    "{}: {} files from {}",
                    if patch.enabled { "enabled" } else { "disabled" },
                    patch.entries.len(),
                    source.display()
                );
            } else {
                println!("No active patch for {}", args[2]);
            }
        }
        Some("patch") if args.len() == 3 && args[1] == "disable" => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            disable_patch(game, &data)?;
            println!("Disabled patch for {}", args[2]);
        }
        Some("patch") if args.len() == 3 && args[1] == "enable" => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            enable_patch(game, &data)?;
            println!("Enabled patch for {}", args[2]);
        }
        Some("patch") if args.len() == 3 && args[1] == "rollback" => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            rollback_patch(game, &data)?;
            println!("Rolled back patch for {}", args[2]);
        }
        Some("overlay") if args.len() == 3 && args[1] == "status" => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            let status = overlay_status(game)?;
            println!(
                "{}: {} active, {} disabled fix files in {} directories",
                if !status.can_toggle {
                    "absent"
                } else if status.mixed {
                    "mixed"
                } else if status.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                status.active_files,
                status.disabled_files,
                status.directories.len()
            );
            for directory in status.directories {
                println!("{}", directory.display());
            }
        }
        Some("overlay") if args.len() == 3 && matches!(args[1].as_str(), "enable" | "disable") => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            if active_patch(&data, &args[2])?.is_some() {
                return Err(
                    "this game has a staged patch; use patch enable or patch disable".into(),
                );
            }
            let enabled = args[1] == "enable";
            let count = set_overlay_enabled(game, enabled)?;
            println!(
                "{} {count} local fix files for {}",
                if enabled { "Enabled" } else { "Disabled" },
                args[2]
            );
        }
        Some("doctor") if args.len() == 2 => {
            let game = library.games.get(&args[1]).ok_or("unknown game ID")?;
            let report = diagnose_game(game, &data);
            println!(
                "Runner: {}",
                report.runner.as_deref().unwrap_or("unresolved")
            );
            for check in &report.checks {
                println!("{:?}: {}", check.severity, check.message);
            }
            if report.has_errors() {
                return Err("launch prerequisites have errors".into());
            }
        }
        Some("fix-status") if args.len() == 2 => {
            let game = library.games.get(&args[1]).ok_or("unknown game ID")?;
            if matches!(game.runner, azdl::Runner::Steam(_)) {
                println!(
                    "Legacy Steam runner cannot launch here; choose a local executable and runner"
                );
            } else {
                let mut effective = game.clone();
                effective.executable = resolve_launch(game)?.executable;
                match detect_fix(&effective)? {
                    FixKind::None => println!("No Online Fix or Steam Fix marker found"),
                    FixKind::OnlineFix => println!("Online Fix marker found"),
                    FixKind::SteamFix => println!(
                        "Steam Fix marker found; Steam is {}",
                        if steam_running() {
                            "running"
                        } else {
                            "not running"
                        }
                    ),
                }
            }
        }
        Some("runner") if args.len() == 3 => {
            set_runner(
                library.games.get_mut(&args[1]).ok_or("unknown game ID")?,
                &args[2],
            )?;
            save_library(&data, &library)?;
        }
        Some("executable") if args.len() == 3 => {
            set_executable(
                library.games.get_mut(&args[1]).ok_or("unknown game ID")?,
                Path::new(&args[2]),
            )?;
            save_library(&data, &library)?;
        }
        Some("option") if args.len() == 3 && args[1] == "list" => {
            let game = library.games.get(&args[2]).ok_or("unknown game ID")?;
            println!("default\tBase executable\t{}", game.executable.display());
            for option in &game.launch_options {
                println!(
                    "{}\t{}\t{}{}",
                    option.id,
                    option.title,
                    option.executable.display(),
                    if game.selected_launch_option_id.as_deref() == Some(&option.id) {
                        "\tselected"
                    } else {
                        ""
                    }
                );
            }
        }
        Some("option") if args.len() >= 7 && args[1] == "add" => {
            let game = library.games.get_mut(&args[2]).ok_or("unknown game ID")?;
            let working = if args[6] == "-" {
                None
            } else {
                Some(Path::new(&args[6]))
            };
            add_launch_option(
                game,
                &args[3],
                &args[4],
                Path::new(&args[5]),
                working,
                &args[7..],
            )?;
            save_library(&data, &library)?;
        }
        Some("option") if args.len() == 4 && args[1] == "select" => {
            let game = library.games.get_mut(&args[2]).ok_or("unknown game ID")?;
            select_launch_option(
                game,
                if args[3] == "default" {
                    None
                } else {
                    Some(&args[3])
                },
            )?;
            save_library(&data, &library)?;
        }
        Some("option") if args.len() == 4 && args[1] == "remove" => {
            let game = library.games.get_mut(&args[2]).ok_or("unknown game ID")?;
            remove_launch_option(game, &args[3])?;
            save_library(&data, &library)?;
        }
        Some("args") if args.len() >= 2 => {
            library
                .games
                .get_mut(&args[1])
                .ok_or("unknown game ID")?
                .arguments = args[2..].to_vec();
            save_library(&data, &library)?;
        }
        Some("env") if args.len() == 4 => {
            if args[2].is_empty() || args[2].contains('=') || args[2].contains('\0') {
                return Err("invalid environment variable name".into());
            }
            library
                .games
                .get_mut(&args[1])
                .ok_or("unknown game ID")?
                .environment
                .insert(args[2].clone(), args[3].clone());
            save_library(&data, &library)?;
        }
        Some("env-remove") if args.len() == 3 => {
            let game = library.games.get_mut(&args[1]).ok_or("unknown game ID")?;
            if game.environment.remove(&args[2]).is_none() {
                return Err("environment variable is not set for this game".into());
            }
            save_library(&data, &library)?;
        }
        Some("launch") if args.len() >= 2 => {
            let game = library.games.get_mut(&args[1]).ok_or("unknown game ID")?;
            let mut child = spawn_game(game, &data, &args[2..])?;
            remember_launch(&data, &args[1], game.last_played_unix);
            let status = child.wait()?;
            if !status.success() {
                return Err(format!("game exited with {status}").into());
            }
        }
        Some("download") if args.len() == 4 => {
            let game = library.games.get(&args[1]).ok_or("unknown game ID")?;
            println!(
                "Downloaded {}",
                recorded_download(
                    &data,
                    game,
                    &args[2],
                    Path::new(&args[3]),
                    JobKind::AdditionalFile,
                )?
                .display()
            );
        }
        Some("remove") if args.len() == 2 => {
            if active_patch(&data, &args[1])?.is_some() {
                return Err("roll back the active patch before removing this game".into());
            }
            if library.games.remove(&args[1]).is_none() {
                return Err("unknown game ID".into());
            }
            save_library(&data, &library)?;
            println!("Removed {} from library", args[1]);
        }
        _ => return Err(usage().into()),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("azdl: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod queued_install_tests {
    use super::*;
    use azdl::jobs::InstallState;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn completed_archive_job_can_wait_for_executable_choice() {
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-queued-archive-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("mode-a.exe"), b"game").unwrap();
        fs::write(source.join("mode-b.exe"), b"game").unwrap();
        let archive = root.join("package.zip");
        assert!(
            Command::new("bsdtar")
                .args(["-a", "-c", "-f"])
                .arg(&archive)
                .arg("-C")
                .arg(&source)
                .arg(".")
                .status()
                .unwrap()
                .success()
        );
        let id = queue_install_job(
            &root,
            "demo",
            "Unknown Game",
            InstallKind::Archive,
            "https://example.org/package.zip",
            Path::new("package.zip"),
        )
        .unwrap();
        let job = load_job(&root, &id).unwrap();
        fs::copy(archive, &job.target).unwrap();
        let path = root.join("jobs").join(format!("{id}.json"));
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["status"] = "complete".into();
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(
            finish_queued_install(&root, &id).unwrap(),
            InstallFinish::NeedsSelection
        );
        assert_eq!(
            load_job(&root, &id).unwrap().install_state,
            Some(InstallState::NeedsSelection)
        );
        let install = root.join("games/demo");
        assert!(install.join("mode-a.exe").is_file());
        let mut library = load_library(&root).unwrap();
        add_game(
            &mut library,
            "demo",
            "Unknown Game",
            &install,
            Path::new("mode-a.exe"),
        )
        .unwrap();
        save_library(&root, &library).unwrap();
        mark_install_registered(&root, "demo").unwrap();
        assert_eq!(
            load_job(&root, &id).unwrap().install_state,
            Some(InstallState::Complete)
        );
        fs::remove_dir_all(root).unwrap();
    }
}
