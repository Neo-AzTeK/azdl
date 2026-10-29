use azdl::doctor::diagnose_game;
use azdl::jobs::{
    cancel_job, list_jobs, load_job, mark_install_registered, pause_job, start_background,
};
use azdl::runtime::discover_proton;
use azdl::shortcuts::{create_shortcut, remove_shortcut, shortcut_status};
use azdl::steam::{import_steam, scan_steam, steam_root};
use azdl::steam_discovery::discover_games;
use azdl::steam_install::{InstallOutcome, attach_pending_origin, clear_pending_origin};
use azdl::steam_shortcuts::{
    create_shortcut as create_steam_shortcut, remove_shortcut as remove_steam_shortcut,
    shortcut_status as steam_shortcut_status,
};
use azdl::{
    Library, Result, add_game, add_launch_option, data_dir, load_library, remove_launch_option,
    save_library, select_launch_option, set_executable, set_runner, spawn_game,
};
use std::env;
use std::fs;
use std::path::Path;

fn usage() -> &'static str {
    "Usage:\n  azdl list\n  azdl runners\n  azdl steam featured|search <query>\n  azdl steam install <appid>\n  azdl steam scan|import <appid>\n  azdl jobs list|show <job-id>|retry <job-id>\n  azdl jobs pause|resume|cancel <job-id>\n  azdl pending add <id> <title> <install-dir> <executable>\n  azdl shortcut create|status|remove <id> [desktop-directory]\n  azdl steam shortcut create|status|remove <id> [shortcuts.vdf]\n  azdl doctor <id>\n  azdl executable <id> <path-inside-install-dir>\n  azdl runner <id> <auto|native|wine|proton:auto|proton:/path/to/proton>\n  azdl option list <id>\n  azdl option add <id> <option-id> <title> <executable> <working-dir|-> [arguments...]\n  azdl option select <id> <option-id|default>\n  azdl option remove <id> <option-id>\n  azdl args <id> [launch arguments...]\n  azdl env <id> <name> <value>\n  azdl env-remove <id> <name>\n  azdl launch <id> [extra arguments...]\n  azdl dependencies <id> [status|install-vcredist|install-directx]\n  azdl installscript scan|apply <id>\n  azdl remove <id>\n\nUse AZDL_DATA_DIR to choose a library and prefix directory."
}

fn save_pending_result(library: &mut Library, data_dir: &Path, id: &str) -> Result<()> {
    let game = library.games.get_mut(id).ok_or("pending game missing")?;
    attach_pending_origin(data_dir, game)?;
    save_library(data_dir, library)?;
    if let Err(error) = clear_pending_origin(data_dir, id) {
        eprintln!("Game saved; pending install cleanup failed: {error}");
    }
    Ok(())
}

fn retry_queued_job(data_dir: &Path, id: &str) -> Result<std::path::PathBuf> {
    azdl::steam_install::resume_job(data_dir, id)
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
        Some("jobs") if args.len() == 3 && args[1] == "resume" => {
            let pid = start_background(&data, &args[2])?;
            println!("Resumed download job {} (worker {pid})", args[2]);
        }
        Some("jobs") if args.len() == 3 && args[1] == "work" => {
            let target = retry_queued_job(&data, &args[2])?;
            println!("Finished download job {} at {}", args[2], target.display());
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
        Some("pending") if args.len() == 6 && args[1] == "add" => {
            let id = &args[2];
            azdl::validate_id(id)?;
            let install = fs::canonicalize(&args[4])?;
            let pending = data.join("games").join(id);
            let metadata = fs::symlink_metadata(&pending)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("pending install is not a directory".into());
            }
            if install != fs::canonicalize(pending)? {
                return Err("pending install directory does not match game ID".into());
            }
            add_game(&mut library, id, &args[3], &install, Path::new(&args[5]))?;
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
            let games_dir = data.join("games");
            match azdl::steam_install::install_game_in(&mut library, &data, &games_dir, &game)? {
                InstallOutcome::Installed(id) => {
                    save_library(&data, &library)?;
                    println!("Installed Steam app {app_id} as {id}");
                }
                InstallOutcome::NeedsSelection(id) => {
                    println!("Files for {id} need an executable selection in the desktop launcher");
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
        Some("remove") if args.len() == 2 => {
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
