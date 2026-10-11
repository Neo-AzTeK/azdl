use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod archive;
pub mod bundled_fix;
pub mod catalog;
pub mod discovery;
pub mod doctor;
pub mod fixes;
pub mod jobs;
mod marker;
pub mod multiplayer;
pub mod overlay;
pub mod patch;
pub mod plugin_store;
pub mod plugins;
pub mod runtime;
pub mod settings;
pub mod setup;
mod sha256;
pub mod shortcuts;
pub mod steam;
pub mod steam_depot;
pub mod steam_discovery;
pub mod steam_shortcuts;
pub mod steamless;
pub mod torrent;
pub mod updates;

pub use archive::normalize_windows_paths;

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "path", rename_all = "lowercase")]
pub enum Runner {
    Auto,
    Native,
    Wine,
    Proton(PathBuf),
    ProtonAuto,
    Steam(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogOrigin {
    pub qualified_id: String,
    pub upload_date: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchOption {
    pub id: String,
    pub title: String,
    pub executable: PathBuf,
    #[serde(default)]
    pub working_directory: Option<PathBuf>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub id: String,
    pub title: String,
    pub install_dir: PathBuf,
    pub executable: PathBuf,
    pub runner: Runner,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub last_played_unix: Option<u64>,
    #[serde(default)]
    pub origin: Option<CatalogOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam_app_id: Option<u32>,
    #[serde(default)]
    pub launch_options: Vec<LaunchOption>,
    #[serde(default)]
    pub selected_launch_option_id: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub banner_url: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub games: BTreeMap<String, Game>,
}

pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("AZDL_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    #[cfg(windows)]
    {
        return Ok(PathBuf::from(env::var_os("APPDATA").ok_or("APPDATA is not set")?).join("azdl"));
    }
    #[cfg(not(windows))]
    {
        if let Some(dir) = env::var_os("XDG_DATA_HOME") {
            return Ok(PathBuf::from(dir).join("azdl"));
        }
        Ok(PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?).join(".local/share/azdl"))
    }
}

pub fn load_library(dir: &Path) -> Result<Library> {
    let path = dir.join("library.json");
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Library::default()),
        Err(error) => Err(error.into()),
    }
}

pub fn save_library(dir: &Path, library: &Library) -> Result<()> {
    fs::create_dir_all(dir)?;
    let target = dir.join("library.json");
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = dir.join(format!("library.json.{}.{nonce}.tmp", std::process::id()));
    let mut file = File::create(&temp)?;
    serde_json::to_writer_pretty(&mut file, library)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(&target)?;
    }
    fs::rename(temp, target)?;
    Ok(())
}

pub fn add_game(
    library: &mut Library,
    id: &str,
    title: &str,
    dir: &Path,
    exe: &Path,
) -> Result<()> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    let dir = fs::canonicalize(dir)?;
    if !dir.is_dir() {
        return Err("install path is not a directory".into());
    }
    let executable = fs::canonicalize(if exe.is_absolute() {
        exe.to_path_buf()
    } else {
        dir.join(exe)
    })?;
    if !executable.is_file() || !executable.starts_with(&dir) {
        return Err("executable must be a file within the install directory".into());
    }
    let launch_options = marker::read_install_marker(&dir)
        .map(|marker| marker.options)
        .unwrap_or_default();
    library.games.insert(
        id.to_string(),
        Game {
            id: id.to_string(),
            title: title.to_string(),
            install_dir: dir,
            executable,
            runner: Runner::Auto,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            last_played_unix: None,
            origin: None,
            steam_app_id: None,
            launch_options,
            selected_launch_option_id: None,
            cover_url: None,
            banner_url: None,
        },
    );
    Ok(())
}

pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("game ID must be 1–64 ASCII letters, digits, hyphens or underscores".into());
    }
    Ok(())
}

/// Safely remove a game from the library and remove its installed files from disk,
/// along with any associated prefixes, shortcuts, and leftover download/staging files.
pub fn remove_game(library: &mut Library, data_dir: &Path, id: &str) -> Result<()> {
    validate_id(id)?;
    let game = library
        .games
        .remove(id)
        .ok_or_else(|| format!("Game {id} not found"))?;

    // Safety checks before deleting install_dir
    let install_dir = &game.install_dir;
    if install_dir.exists()
        && let Ok(canon) = fs::canonicalize(install_dir)
    {
        let is_root = canon.parent().is_none();
        let is_data_dir = fs::canonicalize(data_dir).is_ok_and(|d| canon == d);
        let is_home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .is_some_and(|h| canon.as_os_str() == h);

        let components_count = canon.components().count();
        if !is_root && !is_data_dir && !is_home && components_count >= 2 {
            let _ = fs::remove_dir_all(&canon);
        }
    }

    // Remove dedicated Wine/Proton prefix if present
    let prefix = data_dir.join("prefixes").join(id);
    if prefix.is_dir() {
        let _ = fs::remove_dir_all(&prefix);
    }

    // Clean up shortcuts
    #[cfg(target_os = "linux")]
    {
        let _ = shortcuts::remove_shortcut(&game, data_dir, None);
    }
    let _ = steam_shortcuts::remove_shortcut(&game, None);

    // Clean up leftover download files / staging for this game ID
    let _ = cleanup_leftovers(data_dir, Some(id));

    // Clean up any jobs associated with this game
    if let Ok(jobs) = jobs::list_jobs(data_dir) {
        for job in jobs {
            if job.install.as_ref().is_some_and(|plan| plan.game_id == id) {
                let _ = jobs::clear_job(data_dir, &job.id);
            }
        }
    }

    save_library(data_dir, library)?;
    Ok(())
}

/// Clean up leftover download archives, partial extractions, and temporary files.
pub fn cleanup_leftovers(data_dir: &Path, specific_id: Option<&str>) -> Result<()> {
    // 1. Clean downloads / setup-downloads / patch-downloads
    for folder in ["downloads", "setup-downloads", "patch-downloads"] {
        let base = data_dir.join(folder);
        if !base.is_dir() {
            continue;
        }
        if let Some(id) = specific_id {
            let target = base.join(id);
            if target.is_dir() {
                let _ = fs::remove_dir_all(target);
            }
        } else if let Ok(library) = load_library(data_dir) {
            for id in library.games.keys() {
                let target = base.join(id);
                if target.is_dir() {
                    let _ = fs::remove_dir_all(target);
                }
            }
        }
    }

    // 2. Clean plugin downloads
    let plugin_dl = data_dir.join("plugin-downloads");
    if plugin_dl.is_dir() {
        if let Some(id) = specific_id {
            let target = plugin_dl.join(id);
            if target.is_dir() {
                let _ = fs::remove_dir_all(target);
            }
        } else if let Ok(plugins) = plugins::list_plugins(data_dir) {
            for p in plugins {
                let target = plugin_dl.join(&p.id);
                if target.is_dir() {
                    let _ = fs::remove_dir_all(target);
                }
            }
        }
    }

    // 3. Clean stale staging directories (prefixed with '.') in games and plugins
    for folder in ["games", "plugins"] {
        let base = data_dir.join(folder);
        if let Ok(entries) = fs::read_dir(&base) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') && entry.path().is_dir() {
                    if let Some(id) = specific_id {
                        if name.starts_with(&format!(".{id}."))
                            || name.starts_with(&format!(".{id}-"))
                        {
                            let _ = fs::remove_dir_all(entry.path());
                        }
                    } else {
                        let _ = fs::remove_dir_all(entry.path());
                    }
                }
            }
        }
    }

    Ok(())
}

/// Synchronize the library with actual directories on disk:
/// 1. Prunes ghost entries whose installation directories no longer exist on disk.
/// 2. Automatically scans `data_dir/games` and any additional storage roots to discover
///    and import unrecorded game directories directly into the library.
pub fn sync_installed_games(
    library: &mut Library,
    data_dir: &Path,
    additional_roots: &[PathBuf],
) -> Result<bool> {
    let mut changed = false;

    // 1. Prune ghost entries
    let initial_count = library.games.len();
    library.games.retain(|_, game| game.install_dir.exists());
    if library.games.len() != initial_count {
        changed = true;
    }

    // 2. Discover unrecorded game folders
    let mut search_roots = vec![data_dir.join("games")];
    search_roots.extend(additional_roots.iter().cloned());

    for root in search_roots {
        if !root.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let folder_name = entry.file_name().to_string_lossy().to_string();
            if folder_name.starts_with('.') {
                continue;
            }

            let already_tracked = library.games.values().any(|g| {
                g.install_dir == path
                    || fs::canonicalize(&g.install_dir)
                        .ok()
                        .is_some_and(|c| fs::canonicalize(&path).ok().as_ref() == Some(&c))
            });

            if !already_tracked {
                // Generate safe ID
                let mut safe_id: String = folder_name
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '_' {
                            c
                        } else {
                            '-'
                        }
                    })
                    .take(60)
                    .collect();
                if safe_id.is_empty() {
                    safe_id = "imported-game".to_string();
                }
                let mut candidate_id = safe_id.clone();
                let mut counter = 1;
                while library.games.contains_key(&candidate_id) {
                    candidate_id = format!("{safe_id}-{counter}");
                    counter += 1;
                }

                let title = folder_name.replace(['-', '_'], " ");
                let imported = discovery::import_game(library, &candidate_id, &title, &path);
                if imported.is_ok() {
                    changed = true;
                } else if let Ok(candidates) = discovery::discover_executables(&path, &title)
                    && let Some(first) = candidates.first()
                    && add_game(library, &candidate_id, &title, &path, &first.path).is_ok()
                {
                    changed = true;
                }
            }
        }
    }

    if changed {
        save_library(data_dir, library)?;
    }

    Ok(changed)
}

/// Download a single directly runnable file into an azdl managed install and register it.
pub fn fetch_game(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    url: &str,
    filename: &Path,
) -> Result<()> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    let install_dir = data_dir.join("games").join(id);
    fs::create_dir_all(&install_dir)?;
    let game = Game {
        id: id.to_string(),
        title: title.to_string(),
        install_dir: fs::canonicalize(&install_dir)?,
        executable: PathBuf::new(),
        runner: Runner::Auto,
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
    let path = jobs::recorded_download(data_dir, &game, url, filename, jobs::JobKind::Game)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(&path, permissions)?;
    }
    add_game(library, id, title, &install_dir, &path)?;
    Ok(())
}

/// Copy a local runnable file into a managed game directory and register it.
pub fn install_file(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    title: &str,
    source: &Path,
) -> Result<PathBuf> {
    validate_id(id)?;
    if title.trim().is_empty() || library.games.contains_key(id) {
        return Err("title is empty or game ID already exists".into());
    }
    let source = fs::canonicalize(source)?;
    if !source.is_file() {
        return Err("source path is not a file".into());
    }
    let filename = source.file_name().ok_or("source file has no name")?;
    let games = data_dir.join("games");
    fs::create_dir_all(&games)?;
    let final_dir = games.join(id);
    if fs::symlink_metadata(&final_dir).is_ok() {
        return Err(format!("install directory already exists: {}", final_dir.display()).into());
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staging = games.join(format!(".{id}.copy-{}-{nonce}", std::process::id()));
    fs::create_dir(&staging)?;
    let result = (|| {
        let target = staging.join(filename);
        fs::copy(&source, &target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&target)?.permissions();
            permissions.set_mode(permissions.mode() | 0o111);
            fs::set_permissions(&target, permissions)?;
        }
        fs::rename(&staging, &final_dir)?;
        add_game(library, id, title, &final_dir, Path::new(filename))?;
        Ok(final_dir)
    })();
    if result.is_err() && staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    result
}

pub fn set_runner(game: &mut Game, value: &str) -> Result<()> {
    game.runner = match value {
        "auto" => Runner::Auto,
        "native" => Runner::Native,
        "wine" => Runner::Wine,
        "proton:auto" => Runner::ProtonAuto,
        path if path.starts_with("proton:") => {
            let path = PathBuf::from(&path[7..]);
            let path = fs::canonicalize(path)?;
            if !path.is_file() {
                return Err("Proton path must point to a file".into());
            }
            Runner::Proton(path)
        }
        path if path.starts_with("steam:") => {
            let app_id = path[6..].parse()?;
            if app_id == 0 {
                return Err("Steam app ID must be positive".into());
            }
            Runner::Steam(app_id)
        }
        _ => {
            return Err(
                "runner must be auto, native, wine, proton:auto, proton:/path/to/proton, or steam:<appid>"
                    .into(),
            );
        }
    };
    Ok(())
}

/// Select a runnable file inside the game's registered install directory.
pub fn set_executable(game: &mut Game, value: &Path) -> Result<()> {
    game.executable = contained_path(&game.install_dir, value, true)?;
    Ok(())
}

fn contained_path(install_dir: &Path, value: &Path, file: bool) -> Result<PathBuf> {
    let install = fs::canonicalize(install_dir)?;
    if !install.is_dir() {
        return Err("game install directory is missing".into());
    }
    let requested = if value.is_absolute() {
        value.to_path_buf()
    } else {
        install.join(value)
    };
    let path = fs::canonicalize(requested)?;
    if !path.starts_with(&install) || (file && !path.is_file()) || (!file && !path.is_dir()) {
        return Err(if file {
            "path must be a file within the install directory"
        } else {
            "working directory must be within the install directory"
        }
        .into());
    }
    Ok(path)
}

pub fn add_launch_option(
    game: &mut Game,
    id: &str,
    title: &str,
    executable: &Path,
    working_directory: Option<&Path>,
    arguments: &[String],
) -> Result<()> {
    validate_id(id)?;
    if matches!(game.runner, Runner::Steam(_)) {
        return Err("Steam client launches do not support local launch options".into());
    }
    if id == "default"
        || title.trim().is_empty()
        || game.launch_options.iter().any(|option| option.id == id)
    {
        return Err("launch option title is empty or ID already exists".into());
    }
    if arguments.len() > 64
        || arguments
            .iter()
            .any(|arg| arg.len() > 4096 || arg.contains('\0'))
    {
        return Err("invalid launch arguments".into());
    }
    let executable = contained_path(&game.install_dir, executable, true)?;
    let working_directory = working_directory
        .map(|path| contained_path(&game.install_dir, path, false))
        .transpose()?;
    game.launch_options.push(LaunchOption {
        id: id.into(),
        title: title.into(),
        executable,
        working_directory,
        arguments: arguments.to_vec(),
        kind: "game".into(),
        is_default: false,
    });
    Ok(())
}

pub fn select_launch_option(game: &mut Game, id: Option<&str>) -> Result<()> {
    if let Some(id) = id
        && !game.launch_options.iter().any(|option| option.id == id)
    {
        return Err("unknown launch option ID".into());
    }
    game.selected_launch_option_id = id.map(str::to_owned);
    Ok(())
}

pub fn remove_launch_option(game: &mut Game, id: &str) -> Result<()> {
    let before = game.launch_options.len();
    game.launch_options.retain(|option| option.id != id);
    if game.launch_options.len() == before {
        return Err("unknown launch option ID".into());
    }
    if game.selected_launch_option_id.as_deref() == Some(id) {
        game.selected_launch_option_id = None;
    }
    Ok(())
}

#[derive(Debug)]
pub struct ResolvedLaunch {
    pub executable: PathBuf,
    pub working_directory: PathBuf,
    pub arguments: Vec<String>,
}

pub fn resolve_launch(game: &Game) -> Result<ResolvedLaunch> {
    let option = game
        .selected_launch_option_id
        .as_ref()
        .map(|id| {
            game.launch_options
                .iter()
                .find(|option| &option.id == id)
                .ok_or("selected launch option is missing")
        })
        .transpose()?;
    let executable = contained_path(
        &game.install_dir,
        option.map_or(game.executable.as_path(), |option| {
            option.executable.as_path()
        }),
        true,
    )?;
    let working_directory = match option.and_then(|option| option.working_directory.as_deref()) {
        Some(path) => contained_path(&game.install_dir, path, false)?,
        None => executable
            .parent()
            .ok_or("executable has no parent")?
            .to_path_buf(),
    };
    let mut arguments = game.arguments.clone();
    if let Some(option) = option {
        arguments.extend(option.arguments.iter().cloned());
    }
    Ok(ResolvedLaunch {
        executable,
        working_directory,
        arguments,
    })
}

pub fn spawn_game(game: &mut Game, data_dir: &Path, extra_args: &[String]) -> Result<Child> {
    let launch = if matches!(game.runner, Runner::Steam(_)) {
        None
    } else {
        Some(resolve_launch(game)?)
    };
    let executable = launch.as_ref().map_or(game.executable.as_path(), |launch| {
        launch.executable.as_path()
    });
    let runner = runtime::resolve_runner(&game.runner, executable, data_dir)?;
    let mut effective_game = game.clone();
    effective_game.executable = executable.to_path_buf();
    bundled_fix::ensure_steamidra_fix(&effective_game, data_dir)?;
    let mut cmd = match runner {
        Runner::Native => Command::new(executable),
        Runner::Wine => {
            let mut cmd = Command::new("wine");
            let prefix = data_dir.join("prefixes").join(&game.id);
            fs::create_dir_all(&prefix)?;
            cmd.env("WINEPREFIX", prefix);
            cmd.arg(executable);
            cmd
        }
        Runner::Proton(ref path) => {
            let compat = data_dir.join("compatdata").join(&game.id);
            fs::create_dir_all(&compat)?;
            let steam = env::var_os("STEAM_COMPAT_CLIENT_INSTALL_PATH")
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
                .or_else(|| steam::steam_root().ok().filter(|path| path.is_dir()))
                .ok_or("Steam client directory is missing; set STEAM_COMPAT_CLIENT_INSTALL_PATH")?;
            let mut cmd = Command::new(path);
            cmd.env_remove("LD_LIBRARY_PATH").env_remove("LD_PRELOAD");
            cmd.env("STEAM_COMPAT_CLIENT_INSTALL_PATH", steam);
            cmd.env("STEAM_COMPAT_DATA_PATH", compat);
            cmd.args([
                "run",
                executable.to_str().ok_or("non-UTF-8 executable path")?,
            ]);
            cmd
        }
        Runner::Steam(app_id) => {
            let mut cmd = Command::new(steam::steam_command());
            cmd.arg("-applaunch").arg(app_id.to_string());
            cmd
        }
        Runner::Auto | Runner::ProtonAuto => unreachable!(),
    };
    if let Ok(Some(msg)) =
        fixes::ensure_steam_api_dll(&effective_game.install_dir, &effective_game.executable)
    {
        eprintln!("{msg}");
    }
    let _ = fixes::heal_ea_activation(&effective_game.install_dir);
    if steamless::scan_game_steamstub(&effective_game).stub_present {
        match steamless::unpack_game(&effective_game, data_dir) {
            Ok(count) if count > 0 => eprintln!("Steamless unpacked {count} executable(s)"),
            Ok(_) => {}
            Err(e) => eprintln!("Steamless unpacking failed: {e}"),
        }
    }
    let fix = if let Some(launch) = &launch {
        cmd.current_dir(&launch.working_directory);
        multiplayer::prepare_game_launch(&effective_game, &runner, &launch.working_directory)?
    } else {
        multiplayer::LaunchFixPreparation::default()
    };
    for warning in &fix.warnings {
        eprintln!("Fix preparation: {warning}");
    }
    if let Some(notice) = &fix.notice {
        eprintln!("{notice}");
    }
    cmd.args(launch.as_ref().map_or(game.arguments.as_slice(), |launch| {
        launch.arguments.as_slice()
    }))
    .args(extra_args);
    cmd.envs(&fix.environment);
    cmd.envs(&game.environment);
    let mut attempts = 0;
    let child = loop {
        match cmd.spawn() {
            Ok(c) => break c,
            Err(err) if err.raw_os_error() == Some(26) && attempts < 5 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            Err(err) => return Err(err.into()),
        }
    };
    game.last_played_unix = Some(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    Ok(child)
}

pub fn launch(game: &mut Game, data_dir: &Path, extra_args: &[String]) -> Result<ExitStatus> {
    Ok(spawn_game(game, data_dir, extra_args)?.wait()?)
}

pub fn download(game: &Game, url: &str, relative: &Path) -> Result<PathBuf> {
    if !url.starts_with("https://") {
        return Err("download URL must use HTTPS".into());
    }
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("destination must be a relative path within the game directory".into());
    }
    let target = game.install_dir.join(relative);
    let parent = target.parent().ok_or("invalid destination")?;
    fs::create_dir_all(parent)?;
    let canonical_parent = fs::canonicalize(parent)?;
    if !canonical_parent.starts_with(&game.install_dir) {
        return Err("destination leaves the game directory".into());
    }
    if target.exists() {
        return Err("destination already exists".into());
    }
    let mut part_name = target
        .file_name()
        .ok_or("invalid destination filename")?
        .to_os_string();
    part_name.push(".part");
    let part = target.with_file_name(part_name);
    if fs::symlink_metadata(&part).is_ok_and(|metadata| !metadata.is_file()) {
        return Err("partial download path is not a regular file".into());
    }
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--retry",
            "3",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--continue-at",
            "-",
        ])
        .arg("--output")
        .arg(&part)
        .arg(url)
        .status()?;
    if !status.success() {
        return Err(format!(
            "download failed ({status}); partial file kept at {}",
            part.display()
        )
        .into());
    }
    fs::rename(&part, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let path = env::temp_dir().join(format!(
            "azdl-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn library_round_trip_and_path_restrictions() {
        let root = scratch();
        let game_dir = root.join("installed");
        fs::create_dir(&game_dir).unwrap();
        fs::write(game_dir.join("game.exe"), b"game").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "good-id",
            "A Game",
            &game_dir,
            Path::new("game.exe"),
        )
        .unwrap();
        assert!(
            add_game(
                &mut library,
                "../bad",
                "Bad",
                &game_dir,
                Path::new("game.exe")
            )
            .is_err()
        );
        fs::write(root.join("other.exe"), b"other").unwrap();
        assert!(
            add_game(
                &mut library,
                "other",
                "Other",
                &game_dir,
                &root.join("other.exe")
            )
            .is_err()
        );
        let game = library.games.get("good-id").unwrap();
        assert!(download(game, "http://example.org/game", Path::new("game.zip")).is_err());
        assert!(download(game, "https://example.org/game", Path::new("../escape")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("other.exe"), game_dir.join("unsafe.part"))
                .unwrap();
            assert!(download(game, "https://example.org/game", Path::new("unsafe")).is_err());
        }
        save_library(&root, &library).unwrap();
        assert_eq!(
            load_library(&root)
                .unwrap()
                .games
                .get("good-id")
                .unwrap()
                .title,
            "A Game"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_library_games_default_steam_app_id_to_none() {
        let game: Game = serde_json::from_value(serde_json::json!({
            "id": "legacy-game",
            "title": "Legacy Game",
            "install_dir": "/games/legacy-game",
            "executable": "/games/legacy-game/game.exe",
            "runner": { "kind": "auto" }
        }))
        .unwrap();
        assert_eq!(game.steam_app_id, None);
    }

    #[test]
    fn executable_selection_stays_inside_game_directory() {
        let root = scratch();
        let game_dir = root.join("installed");
        fs::create_dir(&game_dir).unwrap();
        fs::write(game_dir.join("launcher.exe"), b"launcher").unwrap();
        fs::write(game_dir.join("game.exe"), b"game").unwrap();
        fs::write(root.join("outside.exe"), b"outside").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "game",
            "Game",
            &game_dir,
            Path::new("launcher.exe"),
        )
        .unwrap();
        let game = library.games.get_mut("game").unwrap();
        set_executable(game, Path::new("game.exe")).unwrap();
        assert_eq!(game.executable, game_dir.join("game.exe"));
        assert!(set_executable(game, Path::new("../outside.exe")).is_err());
        assert_eq!(game.executable, game_dir.join("game.exe"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("outside.exe"), game_dir.join("link.exe"))
                .unwrap();
            assert!(set_executable(game, Path::new("link.exe")).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn installs_local_file_into_managed_directory_and_launches_it() {
        let root = scratch();
        let source = root.join("My Game.sh");
        fs::write(&source, "#!/bin/sh\nprintf '%s' \"$1\" > result.txt\n").unwrap();
        let mut library = Library::default();
        let installed = install_file(&mut library, &root, "local", "My Game", &source).unwrap();
        assert_eq!(installed, root.join("games/local"));
        assert!(source.is_file());
        assert!(installed.join("My Game.sh").is_file());
        assert!(install_file(&mut library, &root, "local", "My Game", &source).is_err());
        let game = library.games.get_mut("local").unwrap();
        game.runner = Runner::Native;
        assert!(
            launch(game, &root, &["with spaces".into()])
                .unwrap()
                .success()
        );
        assert_eq!(
            fs::read_to_string(installed.join("result.txt")).unwrap(),
            "with spaces"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_launch_passes_arguments_without_shell() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let script = root.join("game.sh");
        fs::write(&script, "#!/bin/sh\nprintf '%s' \"$1\" > result.txt\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let mut library = Library::default();
        add_game(&mut library, "shell", "Shell", &root, Path::new("game.sh")).unwrap();
        let game = library.games.get_mut("shell").unwrap();
        game.runner = Runner::Native;
        assert!(
            launch(game, &root, &["hello world; exit 99".to_string()])
                .unwrap()
                .success()
        );
        assert_eq!(
            fs::read_to_string(root.join("result.txt")).unwrap(),
            "hello world; exit 99"
        );
        assert!(game.last_played_unix.is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn selected_launch_option_uses_its_executable_working_directory_and_arguments() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let install = root.join("game");
        let work = install.join("bin");
        fs::create_dir_all(&work).unwrap();
        let base = install.join("base.sh");
        let alternate = work.join("alternate.sh");
        fs::write(root.join("outside.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::write(&base, "#!/bin/sh\nprintf base > base-result\n").unwrap();
        fs::write(
            &alternate,
            "#!/bin/sh\nprintf '%s|%s|%s|%s' \"$1\" \"$2\" \"$3\" \"$PWD\" > result\n",
        )
        .unwrap();
        for path in [&base, &alternate] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut library = Library::default();
        add_game(&mut library, "demo", "Demo", &install, Path::new("base.sh")).unwrap();
        let game = library.games.get_mut("demo").unwrap();
        game.runner = Runner::Native;
        game.arguments = vec!["shared argument".into()];
        add_launch_option(
            game,
            "multi",
            "Multiplayer",
            Path::new("bin/alternate.sh"),
            Some(Path::new("bin")),
            &["option argument".into()],
        )
        .unwrap();
        assert!(
            add_launch_option(game, "multi", "Duplicate", Path::new("base.sh"), None, &[]).is_err()
        );
        assert!(
            add_launch_option(game, "default", "Reserved", Path::new("base.sh"), None, &[])
                .is_err()
        );
        assert!(
            add_launch_option(
                game,
                "outside",
                "Outside",
                Path::new("../outside.sh"),
                None,
                &[]
            )
            .is_err()
        );
        assert!(
            add_launch_option(
                game,
                "bad-dir",
                "Bad directory",
                Path::new("base.sh"),
                Some(Path::new("..")),
                &[]
            )
            .is_err()
        );
        select_launch_option(game, Some("multi")).unwrap();
        save_library(&root, &library).unwrap();
        let mut loaded = load_library(&root).unwrap();
        let game = loaded.games.get_mut("demo").unwrap();
        assert_eq!(game.selected_launch_option_id.as_deref(), Some("multi"));
        assert!(
            launch(game, &root, &["extra argument".into()])
                .unwrap()
                .success()
        );
        assert_eq!(
            fs::read_to_string(work.join("result")).unwrap(),
            format!(
                "shared argument|option argument|extra argument|{}",
                work.display()
            )
        );
        assert!(!install.join("base-result").exists());
        remove_launch_option(game, "multi").unwrap();
        assert!(game.selected_launch_option_id.is_none());
        assert!(launch(game, &root, &[]).unwrap().success());
        assert_eq!(
            fs::read_to_string(install.join("base-result")).unwrap(),
            "base"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_game_json_defaults_launch_options() {
        let game: Game = serde_json::from_str(r#"{"id":"old","title":"Old","install_dir":"/tmp/old","executable":"/tmp/old/game","runner":{"kind":"native"}}"#).unwrap();
        assert!(game.launch_options.is_empty());
        assert!(game.selected_launch_option_id.is_none());
    }

    #[test]
    fn test_remove_game_deletes_files_prefixes_and_leftovers() {
        let root = scratch();
        let game_dir = root.join("games").join("removable-game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("game.bin"), b"binary").unwrap();

        let prefix_dir = root.join("prefixes").join("removable-game");
        fs::create_dir_all(&prefix_dir).unwrap();
        fs::write(prefix_dir.join("system.reg"), b"registry").unwrap();

        let downloads_dir = root.join("downloads").join("removable-game");
        fs::create_dir_all(&downloads_dir).unwrap();
        fs::write(downloads_dir.join("game.zip"), b"zip").unwrap();

        let mut library = Library::default();
        add_game(
            &mut library,
            "removable-game",
            "Removable Game",
            &game_dir,
            Path::new("game.bin"),
        )
        .unwrap();
        save_library(&root, &library).unwrap();

        assert!(game_dir.exists());
        assert!(prefix_dir.exists());
        assert!(downloads_dir.exists());
        assert!(library.games.contains_key("removable-game"));

        remove_game(&mut library, &root, "removable-game").unwrap();

        assert!(!library.games.contains_key("removable-game"));
        assert!(!game_dir.exists());
        assert!(!prefix_dir.exists());
        assert!(!downloads_dir.exists());

        let reloaded = load_library(&root).unwrap();
        assert!(!reloaded.games.contains_key("removable-game"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_cleanup_leftovers_cleans_staging_and_downloads() {
        let root = scratch();
        let games_staging = root.join("games").join(".test.extract-123-456");
        fs::create_dir_all(&games_staging).unwrap();
        fs::write(games_staging.join("temp.txt"), b"temp").unwrap();

        let plugins_staging = root.join("plugins").join(".extract-999");
        fs::create_dir_all(&plugins_staging).unwrap();
        fs::write(plugins_staging.join("temp.txt"), b"temp").unwrap();

        let dl = root.join("downloads").join("my-game");
        fs::create_dir_all(&dl).unwrap();
        fs::write(dl.join("archive.zip"), b"archive").unwrap();

        let mut library = Library::default();
        let install_dir = root.join("games").join("my-game");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(install_dir.join("game.bin"), b"run").unwrap();
        add_game(
            &mut library,
            "my-game",
            "My Game",
            &install_dir,
            Path::new("game.bin"),
        )
        .unwrap();
        save_library(&root, &library).unwrap();

        cleanup_leftovers(&root, None).unwrap();

        assert!(!games_staging.exists());
        assert!(!plugins_staging.exists());
        assert!(!dl.exists());
        assert!(install_dir.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_sync_installed_games_prunes_and_auto_discovers() {
        let root = scratch();
        let mut library = Library::default();

        // 1. Ghost entry pointing to non-existent directory
        let ghost_dir = root.join("games").join("ghost-game");
        library.games.insert(
            "ghost-game".to_string(),
            Game {
                id: "ghost-game".to_string(),
                title: "Ghost Game".to_string(),
                install_dir: ghost_dir,
                executable: PathBuf::from("game.exe"),
                runner: Runner::Auto,
                arguments: vec![],
                environment: BTreeMap::new(),
                last_played_unix: None,
                origin: None,
                steam_app_id: None,
                launch_options: vec![],
                selected_launch_option_id: None,
                cover_url: None,
                banner_url: None,
            },
        );

        // 2. Untracked physical game directory in root/games
        let physical_dir = root.join("games").join("discovered-quest");
        fs::create_dir_all(&physical_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let exe_path = physical_dir.join("quest");
            fs::write(&exe_path, b"\x7fELFfakebinary").unwrap();
            let mut perms = fs::metadata(&exe_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&exe_path, perms).unwrap();
        }
        #[cfg(windows)]
        {
            fs::write(physical_dir.join("quest.exe"), b"MZfakebinary").unwrap();
        }

        let changed = sync_installed_games(&mut library, &root, &[]).unwrap();
        assert!(changed);

        // Ghost game must be pruned
        assert!(!library.games.contains_key("ghost-game"));

        // Discovered quest must be added
        assert!(
            library.games.contains_key("discovered-quest")
                || library
                    .games
                    .values()
                    .any(|g| g.install_dir == physical_dir)
        );

        // Saved library.json must match
        let reloaded = load_library(&root).unwrap();
        assert!(!reloaded.games.contains_key("ghost-game"));
        assert!(
            reloaded.games.contains_key("discovered-quest")
                || reloaded
                    .games
                    .values()
                    .any(|g| g.install_dir == physical_dir)
        );

        fs::remove_dir_all(root).unwrap();
    }
}
