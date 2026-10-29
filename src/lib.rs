use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod archive;
pub mod cmd;
pub mod dependencies;
pub mod discovery;
pub mod dlc;
pub mod doctor;
pub mod error;
pub mod game_controls;
mod http;
pub mod installscript;
pub mod jobs;
pub mod migration;
pub mod runtime;
pub mod settings;
mod sha256;
pub mod shortcuts;
pub mod steam;
pub mod steam_access;
pub mod steam_content;
pub mod steam_depot;
pub mod steam_discovery;
mod steam_engine;
pub mod steam_install;
pub mod steam_manifest;
pub mod steam_metadata;
pub mod steam_shortcuts;
pub mod wine_probe;

pub use archive::normalize_windows_paths;
pub use cmd::{clean_command, sanitize_command};
pub use error::LauncherError;

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
        Ok(PathBuf::from(env::var_os("APPDATA").ok_or("APPDATA is not set")?).join("azdl"))
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
    migration::migrate(dir)?;
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
    let launch_options = Vec::new();
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
    remove_game_with_guard(library, data_dir, id, || Ok(()))
}

/// Remove a game after checking desktop activity under its content lock.
pub fn remove_game_with_guard(
    library: &mut Library,
    data_dir: &Path,
    id: &str,
    guard: impl FnOnce() -> Result<()>,
) -> Result<()> {
    validate_id(id)?;
    let _lock = dlc::lock_game(data_dir, id)?;
    guard()?;
    if jobs::has_active_dlc_download(data_dir, id)? {
        return Err("Wait for the DLC download to finish before removing this game".into());
    }
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
    for root in ["prefixes", "compatdata"] {
        let prefix = data_dir.join(root).join(id);
        if prefix.is_dir() {
            let _ = fs::remove_dir_all(&prefix);
        }
    }

    // Clean up shortcuts
    #[cfg(target_os = "linux")]
    {
        let _ = shortcuts::remove_shortcut(&game, data_dir, None);
    }
    let _ = steam_shortcuts::remove_shortcut(&game, None);

    // Clean up leftover download files / staging for this game ID
    let _ = cleanup_leftovers(data_dir, Some(id));
    let _ = fs::remove_dir_all(data_dir.join("dlc").join(id));
    let _ = fs::remove_dir_all(data_dir.join("dlc-downloads").join(id));
    let _ = fs::remove_dir_all(data_dir.join("dlc-observations").join(id));
    let _ = fs::remove_file(data_dir.join("content-receipts").join(format!("{id}.json")));
    let _ = fs::remove_file(data_dir.join("content-sources").join(format!("{id}.json")));

    // Clean up any jobs associated with this game
    if let Ok(jobs) = jobs::list_jobs(data_dir) {
        for job in jobs {
            if job.install.as_ref().is_some_and(|plan| plan.game_id == id)
                || job
                    .target
                    .starts_with(data_dir.join("dlc-downloads").join(id))
            {
                let _ = jobs::clear_job(data_dir, &job.id);
            }
        }
    }

    save_library(data_dir, library)?;
    Ok(())
}

/// Clean up leftover download archives, partial extractions, and temporary files.
pub fn cleanup_leftovers(data_dir: &Path, specific_id: Option<&str>) -> Result<()> {
    // 1. Clean downloads
    for folder in ["downloads"] {
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

    // Clean stale staging directories in managed games.
    for folder in ["games"] {
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
        _ => {
            return Err(
                "runner must be auto, native, wine, proton:auto, or proton:/path/to/proton".into(),
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
    if matches!(game.runner, Runner::Steam(_)) {
        return Err("Steam client launches are outside this standalone launcher; choose a local executable and runner".into());
    }
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
    let mut cmd = match runner {
        Runner::Native => clean_command(executable),
        Runner::Wine => {
            let mut cmd = clean_command("wine");
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
            let mut cmd = clean_command(path);
            cmd.env_remove("LD_LIBRARY_PATH").env_remove("LD_PRELOAD");
            cmd.env("STEAM_COMPAT_CLIENT_INSTALL_PATH", steam);
            cmd.env("STEAM_COMPAT_DATA_PATH", compat);
            cmd.args([
                "run",
                executable.to_str().ok_or("non-UTF-8 executable path")?,
            ]);
            cmd
        }
        Runner::Steam(_) => unreachable!(),
        Runner::Auto | Runner::ProtonAuto => unreachable!(),
    };
    if let Ok(installed_deps) =
        dependencies::ensure_game_dependencies(&effective_game, &runner, data_dir)
    {
        for action in installed_deps {
            eprintln!("Dependency provisioning: {action}");
        }
    }
    let prefix_dir = match &runner {
        Runner::Proton(_) => {
            let pfx = data_dir
                .join("compatdata")
                .join(&effective_game.id)
                .join("pfx");
            if pfx.is_dir() {
                pfx
            } else {
                data_dir.join("prefixes").join(&effective_game.id)
            }
        }
        _ => data_dir.join("prefixes").join(&effective_game.id),
    };
    if let Ok(true) = installscript::apply_installscript_for_game(&effective_game, &prefix_dir) {
        eprintln!("Applied Steamworks installscript & registry configuration");
    }
    if let Some(launch) = &launch {
        cmd.current_dir(&launch.working_directory);
    }
    cmd.args(launch.as_ref().map_or(game.arguments.as_slice(), |launch| {
        launch.arguments.as_slice()
    }))
    .args(extra_args);
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
        let selected_executable = fs::canonicalize(game_dir.join("game.exe")).unwrap();
        assert_eq!(game.executable, selected_executable);
        assert!(set_executable(game, Path::new("../outside.exe")).is_err());
        assert_eq!(game.executable, selected_executable);
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

        let compat_dir = root.join("compatdata/removable-game/pfx");
        fs::create_dir_all(&compat_dir).unwrap();
        fs::write(compat_dir.join("user.reg"), b"proton registry").unwrap();
        let source_record = root.join("content-sources/removable-game.json");
        fs::create_dir_all(source_record.parent().unwrap()).unwrap();
        fs::write(&source_record, b"[]").unwrap();

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
        assert!(!compat_dir.exists());
        assert!(!source_record.exists());
        assert!(!prefix_dir.exists());
        assert!(!downloads_dir.exists());

        let reloaded = load_library(&root).unwrap();
        assert!(!reloaded.games.contains_key("removable-game"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remove_game_waits_for_dlc_file_mutation() {
        use std::sync::mpsc;
        use std::time::Duration;

        let root = scratch();
        let game_dir = root.join("games").join("locked-game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("game.bin"), b"binary").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "locked-game",
            "Locked Game",
            &game_dir,
            Path::new("game.bin"),
        )
        .unwrap();
        let lock = dlc::lock_game(&root, "locked-game").unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker_root = root.clone();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx
                .send(remove_game(&mut library, &worker_root, "locked-game").is_ok())
                .unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(game_dir.exists());
        drop(lock);
        assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap();
        assert!(!game_dir.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remove_game_rejects_active_dlc_download() {
        let root = scratch();
        let game_dir = root.join("games").join("active-dlc-game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("game.bin"), b"binary").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "active-dlc-game",
            "Active DLC Game",
            &game_dir,
            Path::new("game.bin"),
        )
        .unwrap();
        let job_id = "00000000000000b1";
        let worker = jobs::lock_dlc_worker(&root, job_id).unwrap();
        jobs::save_job(
            &root,
            &jobs::DownloadJob {
                id: job_id.into(),
                kind: jobs::JobKind::Dlc,
                url: "steam://100/dlc/200".into(),
                target: root.join("payload"),
                status: jobs::JobStatus::Running,
                pid: Some(std::process::id()),
                error: None,
                updated_unix: 0,
                install: Some(jobs::InstallPlan {
                    game_id: "active-dlc-game".into(),
                    title: "Active DLC Game".into(),
                }),
                install_state: Some(jobs::InstallState::Pending),
                install_error: None,
                progress_bytes: Some(0),
                total_bytes: None,
            },
        )
        .unwrap();
        assert!(remove_game(&mut library, &root, "active-dlc-game").is_err());
        assert!(game_dir.exists());
        assert!(library.games.contains_key("active-dlc-game"));
        drop(worker);
        remove_game(&mut library, &root, "active-dlc-game").unwrap();
        assert!(!game_dir.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_cleanup_leftovers_cleans_staging_and_downloads() {
        let root = scratch();
        let games_staging = root.join("games").join(".test.extract-123-456");
        fs::create_dir_all(&games_staging).unwrap();
        fs::write(games_staging.join("temp.txt"), b"temp").unwrap();

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
        assert!(!dl.exists());
        assert!(install_dir.exists());

        fs::remove_dir_all(root).unwrap();
    }
}
