use crate::overlay::{MANAGED_APP_ID_MARKER, overlay_status};
use crate::{Game, Result, Runner};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixKind {
    None,
    OnlineFix,
    SteamFix,
}

#[derive(Default)]
pub struct LaunchFixPreparation {
    pub environment: BTreeMap<String, String>,
    pub warnings: Vec<String>,
    pub notice: Option<String>,
}

#[derive(Default)]
struct MarkerScan {
    online: bool,
    steam: bool,
    active_dll: bool,
    disabled_dll: bool,
}

fn markers(directory: &Path) -> Result<MarkerScan> {
    let mut found = MarkerScan::default();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !fs::symlink_metadata(entry.path())?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        found.active_dll |= matches!(
            name.as_str(),
            "winmm.dll"
                | "steamfix64.dll"
                | "steamfix32.dll"
                | "onlinefix.dll"
                | "onlinefix64.dll"
                | "onlinefix32.dll"
                | "steamoverlay32.dll"
                | "steamoverlay64.dll"
                | "epicfix64.dll"
                | "stubdrm32.dll"
                | "stubdrm64.dll"
        );
        found.disabled_dll |= name.ends_with(".dll.arachnel-off");
        found.online |= matches!(
            name.as_str(),
            "onlinefix.ini"
                | "onlinefix.dll"
                | "onlinefix64.dll"
                | "onlinefix32.dll"
                | "epicfix64.dll"
                | "steamoverlay32.dll"
                | "steamoverlay64.dll"
                | "stubdrm32.dll"
                | "stubdrm64.dll"
                | "winmm.txt"
                | "dlllist.txt"
        );
        found.steam |= matches!(
            name.as_str(),
            "steamfix.ini" | "steamfix64.dll" | "steamfix32.dll"
        );
    }
    Ok(found)
}

fn fake_app_id(directory: &Path) -> Option<String> {
    for name in ["SteamFix.ini", "OnlineFix.ini"] {
        let Some(path) = local_file(directory, name).ok().flatten() else {
            continue;
        };
        if !fs::symlink_metadata(&path).is_ok_and(|metadata| {
            metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 256 * 1024
        }) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        for line in contents.lines() {
            let Some((key, value)) = line.trim().split_once('=') else {
                continue;
            };
            let value = value.trim();
            if key.trim().eq_ignore_ascii_case("FakeAppId")
                && !value.is_empty()
                && value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Some(value.to_string());
            }
        }
    }
    None
}

struct FixLocations {
    directories: Vec<PathBuf>,
    overlay_directories: Vec<PathBuf>,
    disabled_overlay: bool,
}

fn fix_locations(game: &Game) -> Result<FixLocations> {
    let root = fs::canonicalize(&game.install_dir)?;
    let executable = fs::canonicalize(&game.executable)?;
    if !executable.starts_with(&root) {
        return Err("game executable is outside its install directory".into());
    }
    let mut directories = BTreeSet::new();
    let mut directory = executable.parent().ok_or("game executable has no parent")?;
    loop {
        directories.insert(directory.to_path_buf());
        if directory == root {
            break;
        }
        directory = directory.parent().ok_or("invalid game directory")?;
    }
    // Large installs may exceed the bounded overlay scan; the executable's ancestors
    // still provide the same launch behavior in that case.
    let mut disabled_overlay = false;
    let mut overlay_directories = Vec::new();
    if let Ok(status) = overlay_status(game) {
        disabled_overlay = status.can_toggle && !status.enabled;
        overlay_directories = status.directories;
        directories.extend(overlay_directories.iter().cloned());
    }
    for candidate in crate::steamless::collect_exe_candidates(&root) {
        let mut curr = candidate.parent();
        while let Some(dir) = curr {
            if dir.starts_with(&root) {
                directories.insert(dir.to_path_buf());
                if dir == root {
                    break;
                }
                curr = dir.parent();
            } else {
                break;
            }
        }
    }
    Ok(FixLocations {
        directories: directories.into_iter().collect(),
        overlay_directories,
        disabled_overlay,
    })
}

fn selected_fake_app_id(locations: &FixLocations) -> String {
    // A root marker can coexist with a nested active overlay. Use the active
    // overlay's INI before falling back to an ancestor's settings.
    for directory in &locations.overlay_directories {
        if markers(directory).is_ok_and(|found| found.active_dll)
            && let Some(id) = fake_app_id(directory)
        {
            return id;
        }
    }
    for directory in &locations.overlay_directories {
        if directory.join("steam_appid.txt").is_file()
            && let Some(id) = fake_app_id(directory)
        {
            return id;
        }
    }
    locations
        .directories
        .iter()
        .find_map(|directory| fake_app_id(directory))
        .unwrap_or_else(|| "480".into())
}

pub fn pe_image_bits(path: &Path) -> Option<u8> {
    let mut file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    if size < 0x40 {
        return None;
    }
    file.seek(SeekFrom::Start(0x3c)).ok()?;
    let mut offset = [0u8; 4];
    file.read_exact(&mut offset).ok()?;
    let offset = u64::from(u32::from_le_bytes(offset));
    if offset < 0x40 || offset + 6 > size {
        return None;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut header = [0u8; 6];
    file.read_exact(&mut header).ok()?;
    if header[..4] != *b"PE\0\0" {
        return None;
    }
    match u16::from_le_bytes([header[4], header[5]]) {
        0x14c => Some(32),
        0x8664 => Some(64),
        _ => None,
    }
}

fn local_file(directory: &Path, wanted: &str) -> Result<Option<PathBuf>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(wanted)
        {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

fn active_regular_file(directory: &Path, wanted: &str) -> Option<PathBuf> {
    let path = local_file(directory, wanted).ok().flatten()?;
    let metadata = fs::symlink_metadata(&path).ok()?;
    (metadata.is_file() && !metadata.file_type().is_symlink()).then_some(path)
}

fn write_new(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

fn ensure_app_id(directory: &Path, app_id: &str, warnings: &mut Vec<String>) {
    let path = match local_file(directory, "steam_appid.txt") {
        Ok(Some(path)) => path,
        Ok(None) => {
            let path = directory.join("steam_appid.txt");
            if local_file(directory, "steam_appid.txt.arachnel-off")
                .ok()
                .flatten()
                .is_some()
            {
                warnings.push(format!(
                    "Disabled {} exists; active App ID file was not created",
                    path.display()
                ));
                return;
            }
            let marker = directory.join(MANAGED_APP_ID_MARKER);
            match fs::symlink_metadata(&marker) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    warnings.push(format!(
                        "Managed App ID marker is not a regular file: {}",
                        marker.display()
                    ));
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if let Err(error) = write_new(&marker, b"azdl\n") {
                        warnings.push(format!("Could not create {}: {error}", marker.display()));
                        return;
                    }
                }
                Err(error) => {
                    warnings.push(format!("Could not inspect {}: {error}", marker.display()));
                    return;
                }
            }
            if let Err(error) = write_new(&path, format!("{app_id}\n").as_bytes()) {
                warnings.push(format!("Could not create {}: {error}", path.display()));
            }
            return;
        }
        Err(error) => {
            warnings.push(format!(
                "Could not inspect {}: {error}",
                directory.display()
            ));
            return;
        }
    };
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return;
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 128 {
        warnings.push(format!(
            "Existing {} is not a small regular file",
            path.display()
        ));
        return;
    }
    match fs::read_to_string(&path) {
        Ok(current) if current.trim() == app_id => {}
        Ok(current) => warnings.push(format!(
            "Existing {} contains {:?}; expected {app_id}. Its value was kept",
            path.display(),
            current.trim()
        )),
        Err(error) => warnings.push(format!("Could not read {}: {error}", path.display())),
    }
}

fn ensure_winmm_config(directory: &Path, bits: Option<u8>, warnings: &mut Vec<String>) {
    let has32 = active_regular_file(directory, "SteamFix32.dll");
    let has64 = active_regular_file(directory, "SteamFix64.dll");
    let wanted = match (bits, has32, has64) {
        (Some(32), Some(path), _) | (_, Some(path), None) => path,
        (Some(64), _, Some(path)) | (_, None, Some(path)) => path,
        (_, Some(path), Some(_)) => path,
        (_, None, None) => return,
    };
    let wanted = wanted.file_name().unwrap_or_default().to_string_lossy();
    let contents = format!("{wanted}\n");
    let path = match local_file(directory, "winmm.txt") {
        Ok(Some(path)) => path,
        Ok(None) => {
            let path = directory.join("winmm.txt");
            if let Err(error) = write_new(&path, contents.as_bytes()) {
                warnings.push(format!("Could not create {}: {error}", path.display()));
            }
            return;
        }
        Err(error) => {
            warnings.push(format!(
                "Could not inspect {}: {error}",
                directory.display()
            ));
            return;
        }
    };
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return;
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        warnings.push(format!(
            "Existing {} is not a small regular file",
            path.display()
        ));
        return;
    }
    let Ok(current) = fs::read_to_string(&path) else {
        warnings.push(format!("Could not read {}", path.display()));
        return;
    };
    if current.trim().eq_ignore_ascii_case(&wanted) {
        return;
    }
    if !["SteamFix32.dll", "SteamFix64.dll"]
        .iter()
        .any(|name| current.trim().eq_ignore_ascii_case(name))
    {
        warnings.push(format!(
            "Existing {} has custom content and was kept",
            path.display()
        ));
        return;
    }
    if let Err(error) = fs::write(&path, contents) {
        warnings.push(format!("Could not update {}: {error}", path.display()));
    }
}

/// Prepare files read directly by Steamworks and the SteamFix winmm proxy.
/// Existing App ID values and custom winmm configurations are preserved and reported.
pub fn prepare_fix_files(
    game: &Game,
    runner: &Runner,
    working_directory: &Path,
) -> Result<Vec<String>> {
    if !matches!(runner, Runner::Wine | Runner::Proton(_)) {
        return Ok(Vec::new());
    }
    let locations = fix_locations(game)?;
    let kind = detected_fix(&locations)?;
    prepare_fix_files_for_locations(game, working_directory, &locations, kind)
}

fn prepare_fix_files_for_locations(
    game: &Game,
    working_directory: &Path,
    locations: &FixLocations,
    kind: FixKind,
) -> Result<Vec<String>> {
    if kind == FixKind::None {
        return Ok(Vec::new());
    }
    let root = fs::canonicalize(&game.install_dir)?;
    let working = fs::canonicalize(working_directory)?;
    if !working.starts_with(&root) {
        return Err("game working directory is outside its install directory".into());
    }
    let app_id = game
        .environment
        .get("SteamAppId")
        .cloned()
        .unwrap_or_else(|| selected_fake_app_id(locations));
    let mut directories: BTreeSet<PathBuf> =
        locations.overlay_directories.iter().cloned().collect();
    directories.insert(working);
    directories.insert(
        fs::canonicalize(&game.executable)?
            .parent()
            .ok_or("game executable has no parent")?
            .to_path_buf(),
    );
    for candidate in crate::steamless::collect_exe_candidates(&root) {
        if let Some(parent) = candidate.parent() {
            directories.insert(parent.to_path_buf());
        }
    }
    for directory in &locations.directories {
        let marker = markers(directory)?;
        if marker.online || marker.steam {
            directories.insert(directory.clone());
        }
    }
    let mut warnings = Vec::new();
    if app_id.is_empty() || !app_id.bytes().all(|byte| byte.is_ascii_digit()) {
        warnings.push(format!(
            "SteamAppId {app_id:?} is not numeric; steam_appid.txt was not written"
        ));
    } else {
        for directory in &directories {
            ensure_app_id(directory, &app_id, &mut warnings);
        }
    }
    let bits = pe_image_bits(&game.executable);
    for directory in &directories {
        ensure_winmm_config(directory, bits, &mut warnings);
        plant_freetp_promo_block(directory);
    }
    ensure_fix_payload_files(locations, &directories, &mut warnings);
    Ok(warnings)
}

const FIX_PAYLOAD_FILES: &[&str] = &[
    "OnlineFix64.dll",
    "OnlineFix.dll",
    "SteamOverlay64.dll",
    "SteamOverlay32.dll",
    "StubDRM64.dll",
    "StubDRM32.dll",
    "dlllist.txt",
    "SteamFix64.dll",
    "SteamFix32.dll",
    "winmm.dll",
    "OnlineFix.ini",
];

fn ensure_fix_payload_files(
    locations: &FixLocations,
    directories: &BTreeSet<PathBuf>,
    warnings: &mut Vec<String>,
) {
    if locations.disabled_overlay {
        return;
    }
    for file_name in FIX_PAYLOAD_FILES {
        let mut source_file = None;
        for dir in &locations.directories {
            if let Some(path) = active_regular_file(dir, file_name) {
                source_file = Some(path);
                break;
            }
        }
        let Some(source_path) = source_file else {
            continue;
        };
        let Some(source_dir) = source_path.parent() else {
            continue;
        };

        for target_dir in directories {
            if !target_dir.is_dir() {
                continue;
            }
            if !target_dir.starts_with(source_dir) || target_dir == source_dir {
                continue;
            }
            let target_path = target_dir.join(file_name);
            if target_path.exists() {
                continue;
            }
            let disabled_target = target_dir.join(format!("{file_name}.arachnel-off"));
            if disabled_target.exists() {
                continue;
            }
            if let Err(error) = fs::copy(&source_path, &target_path) {
                warnings.push(format!(
                    "Could not propagate {file_name} to {}: {error}",
                    target_dir.display()
                ));
            }
        }
    }
}

fn plant_freetp_promo_block(directory: &Path) {
    let freetp_data = directory.join("FreeTP").join("UserData");
    if fs::create_dir_all(&freetp_data).is_ok() {
        let anon = freetp_data.join("AnonFolderSave.txt");
        if !anon.exists() {
            let _ = fs::write(anon, b"");
        }
    }
}

fn detected_fix(locations: &FixLocations) -> Result<FixKind> {
    if locations.disabled_overlay {
        return Ok(FixKind::None);
    }
    let mut online = false;
    let mut steam = false;
    let mut active_dll = false;
    let mut disabled_dll = false;
    for directory in &locations.directories {
        let found = markers(directory)?;
        online |= found.online;
        steam |= found.steam;
        active_dll |= found.active_dll;
        disabled_dll |= found.disabled_dll;
    }
    Ok(if disabled_dll && !active_dll {
        FixKind::None
    } else if steam {
        FixKind::SteamFix
    } else if online {
        FixKind::OnlineFix
    } else {
        FixKind::None
    })
}

/// Environment required by detected local fix DLLs for Wine and Proton launches.
pub fn fix_environment(game: &Game, runner: &Runner) -> Result<BTreeMap<String, String>> {
    if !matches!(runner, Runner::Wine | Runner::Proton(_)) {
        return Ok(BTreeMap::new());
    }
    let locations = fix_locations(game)?;
    let kind = detected_fix(&locations)?;
    fix_environment_for_locations(game, &locations, kind)
}

fn fix_environment_for_locations(
    game: &Game,
    locations: &FixLocations,
    kind: FixKind,
) -> Result<BTreeMap<String, String>> {
    let mut extra = BTreeMap::new();
    if kind == FixKind::None {
        return Ok(extra);
    }
    let mut dlls = BTreeSet::new();
    let mut bundled_overlay = false;
    let id = selected_fake_app_id(locations);
    for directory in &locations.directories {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if !fs::symlink_metadata(entry.path())?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            let Some(stem) = name.strip_suffix(".dll") else {
                continue;
            };
            if matches!(
                stem,
                "winmm"
                    | "steamfix64"
                    | "steamfix32"
                    | "epicfix64"
                    | "onlinefix64"
                    | "onlinefix32"
                    | "onlinefix"
                    | "steamoverlay32"
                    | "steamoverlay64"
                    | "stubdrm32"
                    | "stubdrm64"
                    | "steam_api64"
                    | "steam_api"
                    | "winhttp"
                    | "dnet"
            ) {
                dlls.insert(stem.to_string());
            }
            bundled_overlay |= matches!(
                stem,
                "onlinefix" | "onlinefix64" | "onlinefix32" | "steamoverlay32" | "steamoverlay64"
            );
        }
    }
    for key in ["SteamAppId", "SteamGameId", "SteamOverlayGameId"] {
        if !game.environment.contains_key(key) {
            extra.insert(key.into(), id.clone());
        }
    }
    let mut overrides = game
        .environment
        .get("WINEDLLOVERRIDES")
        .cloned()
        .or_else(|| env::var("WINEDLLOVERRIDES").ok())
        .unwrap_or_default();
    for stem in dlls {
        if !overrides.split(';').any(|entry| {
            entry
                .split_once('=')
                .is_some_and(|(name, _)| name.eq_ignore_ascii_case(&stem))
        }) {
            if !overrides.is_empty() && !overrides.ends_with(';') {
                overrides.push(';');
            }
            overrides.push_str(&format!("{stem}=n,b"));
        }
    }
    if !overrides.is_empty() {
        extra.insert("WINEDLLOVERRIDES".into(), overrides);
    }
    if bundled_overlay {
        if !game.environment.contains_key("LD_PRELOAD") {
            extra.insert("LD_PRELOAD".into(), String::new());
        }
        if !game
            .environment
            .contains_key("ENABLE_VK_LAYER_VALVE_steam_overlay_1")
        {
            extra.insert("ENABLE_VK_LAYER_VALVE_steam_overlay_1".into(), "0".into());
        }
    }
    Ok(extra)
}

/// Inspect the executable's ancestors and recognized nested local fix directories.
pub fn detect_fix(game: &Game) -> Result<FixKind> {
    detected_fix(&fix_locations(game)?)
}

#[cfg(target_os = "linux")]
pub fn steam_running() -> bool {
    let Ok(processes) = fs::read_dir("/proc") else {
        return false;
    };
    processes.filter_map(std::result::Result::ok).any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            && fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim().eq_ignore_ascii_case("steam"))
    })
}

#[cfg(windows)]
pub fn steam_running() -> bool {
    crate::clean_command("tasklist")
        .args(["/FI", "IMAGENAME eq steam.exe", "/NH"])
        .output()
        .map(|out| {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.to_ascii_lowercase().contains("steam.exe")
        })
        .unwrap_or(false)
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn steam_running() -> bool {
    false
}

/// Detect and report multiplayer fix markers before game launch without requiring Steam to be running.
pub fn prepare_fix_launch(game: &Game, runner: &Runner) -> Result<Option<String>> {
    if !matches!(runner, Runner::Wine | Runner::Proton(_)) {
        return Ok(None);
    }
    prepare_fix_launch_for_kind(detect_fix(game)?)
}

fn prepare_fix_launch_for_kind(kind: FixKind) -> Result<Option<String>> {
    match kind {
        FixKind::None => Ok(None),
        FixKind::OnlineFix => Ok(Some("Online Fix marker detected".into())),
        FixKind::SteamFix => Ok(Some("Steam Fix marker detected".into())),
    }
}

/// Compute launch settings and prepare local fix files from one game scan.
pub fn prepare_game_launch(
    game: &Game,
    runner: &Runner,
    working_directory: &Path,
) -> Result<LaunchFixPreparation> {
    if !matches!(runner, Runner::Wine | Runner::Proton(_)) {
        return Ok(LaunchFixPreparation::default());
    }
    let locations = fix_locations(game)?;
    let kind = detected_fix(&locations)?;
    let environment = fix_environment_for_locations(game, &locations, kind)?;
    let warnings = prepare_fix_files_for_locations(game, working_directory, &locations, kind)?;
    let notice = prepare_fix_launch_for_kind(kind)?;
    Ok(LaunchFixPreparation {
        environment,
        warnings,
        notice,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::set_overlay_enabled;
    use crate::{Library, add_game};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detects_markers_near_executable_without_disabled_files() {
        let root = std::env::temp_dir().join(format!(
            "azdl-fix-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let executable_dir = root.join("game/bin");
        fs::create_dir_all(&executable_dir).unwrap();
        fs::write(executable_dir.join("game.exe"), b"game").unwrap();
        fs::write(root.join("game/SteamFix.ini.arachnel-off"), b"disabled").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "game",
            "Game",
            &root.join("game"),
            Path::new("bin/game.exe"),
        )
        .unwrap();
        let game = &library.games["game"];
        assert_eq!(detect_fix(game).unwrap(), FixKind::None);
        fs::write(root.join("game/OnlineFix.ini"), b"fix").unwrap();
        assert_eq!(detect_fix(game).unwrap(), FixKind::OnlineFix);
        fs::write(executable_dir.join("SteamFix64.dll"), b"fix").unwrap();
        assert_eq!(detect_fix(game).unwrap(), FixKind::SteamFix);
        fs::rename(
            executable_dir.join("SteamFix64.dll"),
            executable_dir.join("SteamFix64.dll.arachnel-off"),
        )
        .unwrap();
        assert_eq!(detect_fix(game).unwrap(), FixKind::None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn wine_fix_environment_uses_local_dlls_and_fake_app_id() {
        let root = std::env::temp_dir().join(format!(
            "azdl-fix-env-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        let bin = install.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("game.exe"), b"game").unwrap();
        fs::write(bin.join("winmm.dll"), b"dll").unwrap();
        fs::write(bin.join("OnlineFix64.dll"), b"dll").unwrap();
        fs::write(install.join("OnlineFix.ini"), b"[Main]\nFakeAppId=12345\n").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "game",
            "Game",
            &install,
            Path::new("bin/game.exe"),
        )
        .unwrap();
        let game = library.games.get_mut("game").unwrap();
        assert_eq!(detect_fix(game).unwrap(), FixKind::OnlineFix);
        assert!(fix_environment(game, &Runner::Native).unwrap().is_empty());
        let environment = fix_environment(game, &Runner::Wine).unwrap();
        assert_eq!(environment["SteamAppId"], "12345");
        assert_eq!(environment["SteamGameId"], "12345");
        assert!(environment["WINEDLLOVERRIDES"].contains("winmm=n,b"));
        assert!(environment["WINEDLLOVERRIDES"].contains("onlinefix64=n,b"));
        assert_eq!(environment["ENABLE_VK_LAYER_VALVE_steam_overlay_1"], "0");
        game.environment.insert("SteamAppId".into(), "54321".into());
        game.environment
            .insert("WINEDLLOVERRIDES".into(), "winmm=b".into());
        game.environment
            .insert("LD_PRELOAD".into(), "/custom/overlay.so".into());
        let custom = fix_environment(game, &Runner::Wine).unwrap();
        assert!(!custom.contains_key("SteamAppId"));
        assert!(!custom.contains_key("LD_PRELOAD"));
        assert!(custom["WINEDLLOVERRIDES"].starts_with("winmm=b;"));
        assert!(!custom["WINEDLLOVERRIDES"].contains("winmm=n,b"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_fix_is_detected_for_root_executable_and_can_be_disabled() {
        let root = std::env::temp_dir().join(format!(
            "azdl-nested-fix-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let fix_dir = root.join("game/Binaries/Win64");
        fs::create_dir_all(&fix_dir).unwrap();
        fs::write(root.join("game/game.exe"), b"game").unwrap();
        fs::write(fix_dir.join("SteamFix.ini"), b"FakeAppId=12345\n").unwrap();
        fs::write(fix_dir.join("SteamFix64.dll"), b"fix").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "game",
            "Game",
            &root.join("game"),
            Path::new("game.exe"),
        )
        .unwrap();
        let game = &library.games["game"];
        assert_eq!(detect_fix(game).unwrap(), FixKind::SteamFix);
        let environment = fix_environment(game, &Runner::Wine).unwrap();
        assert_eq!(environment["SteamAppId"], "12345");
        assert!(environment["WINEDLLOVERRIDES"].contains("steamfix64=n,b"));
        assert_eq!(set_overlay_enabled(game, false).unwrap(), 1);
        assert_eq!(detect_fix(game).unwrap(), FixKind::None);
        assert!(fix_environment(game, &Runner::Wine).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ini_free_epic_fix_receives_wine_overrides() {
        let root = std::env::temp_dir().join(format!(
            "azdl-epic-fix-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("game.exe"), b"game").unwrap();
        fs::write(root.join("EpicFix64.dll"), b"fix").unwrap();
        let mut library = Library::default();
        add_game(&mut library, "game", "Game", &root, Path::new("game.exe")).unwrap();
        let game = &library.games["game"];
        assert_eq!(detect_fix(game).unwrap(), FixKind::OnlineFix);
        let environment = fix_environment(game, &Runner::Wine).unwrap();
        assert!(environment["WINEDLLOVERRIDES"].contains("epicfix64=n,b"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepares_32_bit_fix_files_and_toggles_generated_app_id() {
        let root = std::env::temp_dir().join(format!(
            "azdl-fix-files-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let overlay = root.join("Binaries/Win64");
        fs::create_dir_all(&overlay).unwrap();
        let mut pe = vec![0u8; 0x86];
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x84..0x86].copy_from_slice(&0x14cu16.to_le_bytes());
        fs::write(root.join("game.exe"), pe).unwrap();
        fs::write(root.join("winmm.dll"), b"unrelated").unwrap();
        fs::write(overlay.join("steamfix.ini"), b"FakeAppId=12345\n").unwrap();
        fs::write(overlay.join("winmm.dll"), b"proxy").unwrap();
        fs::write(overlay.join("SteamFix32.dll"), b"32-bit fix").unwrap();
        fs::write(overlay.join("SteamFix64.dll"), b"64-bit fix").unwrap();
        fs::write(overlay.join("winmm.txt"), b"SteamFix64.dll\n").unwrap();
        let mut library = Library::default();
        add_game(&mut library, "game", "Game", &root, Path::new("game.exe")).unwrap();
        let game = &library.games["game"];
        assert_eq!(pe_image_bits(&game.executable), Some(32));
        assert!(
            prepare_fix_files(game, &Runner::Wine, &root)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fs::read_to_string(overlay.join("winmm.txt")).unwrap(),
            "SteamFix32.dll\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("steam_appid.txt")).unwrap(),
            "12345\n"
        );
        assert_eq!(
            fs::read_to_string(overlay.join("steam_appid.txt")).unwrap(),
            "12345\n"
        );
        assert!(root.join(MANAGED_APP_ID_MARKER).is_file());
        assert_eq!(
            fix_environment(game, &Runner::Wine).unwrap()["SteamAppId"],
            "12345"
        );
        assert_eq!(set_overlay_enabled(game, false).unwrap(), 5);
        assert!(root.join("steam_appid.txt.arachnel-off").is_file());
        assert!(root.join("winmm.dll").is_file());
        assert_eq!(detect_fix(game).unwrap(), FixKind::None);
        assert!(
            prepare_fix_files(game, &Runner::Wine, &root)
                .unwrap()
                .is_empty()
        );
        assert!(!root.join("steam_appid.txt").exists());
        assert_eq!(set_overlay_enabled(game, true).unwrap(), 5);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keeps_conflicting_app_id_and_custom_winmm_config() {
        let root = std::env::temp_dir().join(format!(
            "azdl-fix-conflict-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("game.exe"), b"game").unwrap();
        fs::write(root.join("SteamFix.ini"), b"FakeAppId=12345\n").unwrap();
        fs::write(root.join("SteamFix64.dll"), b"fix").unwrap();
        fs::write(root.join("steam_appid.txt"), b"999\n").unwrap();
        fs::write(root.join("winmm.txt"), b"CustomFix.dll\n").unwrap();
        let mut library = Library::default();
        add_game(&mut library, "game", "Game", &root, Path::new("game.exe")).unwrap();
        let warnings = prepare_fix_files(&library.games["game"], &Runner::Wine, &root).unwrap();
        assert_eq!(warnings.len(), 2);
        assert_eq!(
            fs::read_to_string(root.join("steam_appid.txt")).unwrap(),
            "999\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("winmm.txt")).unwrap(),
            "CustomFix.dll\n"
        );
        assert!(!root.join(MANAGED_APP_ID_MARKER).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_prepare_fix_launch_never_spawns_steam() {
        let root = std::env::temp_dir().join(format!(
            "azdl-fix-nospawn-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("game.exe"), b"game").unwrap();
        fs::write(root.join("SteamFix.ini"), b"FakeAppId=480\n").unwrap();
        fs::write(root.join("SteamFix64.dll"), b"fix").unwrap();

        let mut library = Library::default();
        crate::add_game(&mut library, "game", "Game", &root, Path::new("game.exe")).unwrap();
        let game = &library.games["game"];

        let status = prepare_fix_launch(game, &Runner::Wine).unwrap();
        assert_eq!(status, Some("Steam Fix marker detected".into()));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn propagates_root_fix_to_nested_shipping_binary_folder() {
        let root = std::env::temp_dir().join(format!(
            "azdl-fix-propagate-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let nested = root.join("Astro/Binaries/Win64");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("Astro.exe"), b"launcher").unwrap();
        fs::write(nested.join("Astro-Win64-Shipping.exe"), b"game").unwrap();
        fs::write(root.join("OnlineFix.ini"), b"FakeAppId=480\n").unwrap();
        fs::write(root.join("OnlineFix64.dll"), b"online fix 64").unwrap();
        fs::write(root.join("winmm.dll"), b"winmm proxy").unwrap();

        let mut library = Library::default();
        add_game(
            &mut library,
            "astroneer",
            "ASTRONEER",
            &root,
            Path::new("Astro.exe"),
        )
        .unwrap();
        let game = &library.games["astroneer"];
        assert_eq!(detect_fix(game).unwrap(), FixKind::OnlineFix);

        let warnings = prepare_fix_files(game, &Runner::Wine, &root).unwrap();
        assert!(warnings.is_empty());

        assert!(nested.join("OnlineFix64.dll").is_file());
        assert_eq!(
            fs::read(nested.join("OnlineFix64.dll")).unwrap(),
            b"online fix 64"
        );
        assert!(nested.join("OnlineFix.ini").is_file());
        assert!(nested.join("winmm.dll").is_file());
        assert!(nested.join("steam_appid.txt").is_file());
        assert_eq!(
            fs::read_to_string(nested.join("steam_appid.txt")).unwrap(),
            "480\n"
        );

        fs::remove_dir_all(root).unwrap();
    }
}
