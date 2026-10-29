#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod sizes;

use azdl::LauncherError;
use azdl::Result as AzdlResult;
use azdl::data_dir;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataCacheEntry {
    pub steam_app_id: Option<u32>,
    pub banner_url: Option<String>,
    pub cover_url: Option<String>,
}

fn is_guessed_steam_artwork(entry: &MetadataCacheEntry) -> bool {
    let urls = [entry.banner_url.as_deref(), entry.cover_url.as_deref()];
    let present: Vec<&str> = urls.into_iter().flatten().collect();
    !present.is_empty()
        && present.iter().all(|url| {
            url.contains("steamstatic.com/store_item_assets/steam/apps/")
                && (url.ends_with("/header.jpg") || url.ends_with("/library_600x900.jpg"))
        })
}

const MAX_CONCURRENT_METADATA_LOOKUPS: usize = 8;
const MAX_METADATA_CACHE_ENTRIES: usize = 2048;

fn steam_dlc_provider_status(app_id: u32) -> Result<(), String> {
    azdl::steam_access::manifest_url(app_id)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn dlc_ids_for_display(
    app_id: u32,
    observations: &[azdl::steam_content::ObservedDlcReceipt],
    appinfo: Option<&azdl::steam_metadata::SteamAppMetadata>,
    store_ids: Option<&[u32]>,
    candidate_dlc_id: Option<u32>,
) -> Vec<u32> {
    let mut ids = observations
        .iter()
        .filter(|receipt| receipt.app_id == app_id)
        .map(|receipt| receipt.dlc_id)
        .collect::<BTreeSet<_>>();
    if let Some(appinfo) = appinfo {
        ids.extend(appinfo.dlc_ids.iter().copied());
    }
    if let Some(store_ids) = store_ids {
        ids.extend(store_ids.iter().copied());
    }
    if let Some(candidate) = candidate_dlc_id.filter(|id| *id > 0) {
        ids.insert(candidate);
    }
    ids.into_iter().collect()
}

fn reserve_metadata_lookup(pending: &mut HashSet<String>, key: String) -> bool {
    if pending.len() >= MAX_CONCURRENT_METADATA_LOOKUPS || pending.contains(&key) {
        return false;
    }
    pending.insert(key)
}

fn insert_metadata_cache(
    cache: &mut HashMap<String, MetadataCacheEntry>,
    key: String,
    entry: MetadataCacheEntry,
) {
    if !cache.contains_key(&key)
        && cache.len() >= MAX_METADATA_CACHE_ENTRIES
        && let Some(evicted_key) = cache.keys().next().cloned()
    {
        cache.remove(&evicted_key);
    }
    cache.insert(key, entry);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LibraryFingerprint {
    modified: Option<SystemTime>,
    len: u64,
}

struct CachedLibrary {
    fingerprint: Option<LibraryFingerprint>,
    library: Arc<azdl::Library>,
}

fn library_fingerprint(data_dir: &Path) -> Result<Option<LibraryFingerprint>, String> {
    let path = data_dir.join("library.json");
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(LibraryFingerprint {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn load_library_snapshot(
    data_dir: &Path,
    cache: &Mutex<Option<CachedLibrary>>,
) -> Result<Arc<azdl::Library>, String> {
    let fingerprint = library_fingerprint(data_dir)?;
    if let Ok(snapshot) = cache.lock()
        && let Some(snapshot) = snapshot.as_ref()
        && snapshot.fingerprint == fingerprint
    {
        return Ok(Arc::clone(&snapshot.library));
    }

    let library = Arc::new(azdl::load_library(data_dir).map_err(|error| error.to_string())?);
    let fingerprint = library_fingerprint(data_dir)?;
    if let Ok(mut snapshot) = cache.lock() {
        *snapshot = Some(CachedLibrary {
            fingerprint,
            library: Arc::clone(&library),
        });
    }
    Ok(library)
}

pub fn metadata_cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("metadata-cache.json")
}

struct BoundedMetadataCache(HashMap<String, MetadataCacheEntry>);

impl<'de> Deserialize<'de> for BoundedMetadataCache {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct CacheVisitor;

        impl<'de> Visitor<'de> for CacheVisitor {
            type Value = BoundedMetadataCache;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an artwork metadata cache object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut entries = HashMap::with_capacity(MAX_METADATA_CACHE_ENTRIES);
                while entries.len() < MAX_METADATA_CACHE_ENTRIES {
                    let Some((key, value)) = map.next_entry()? else {
                        return Ok(BoundedMetadataCache(entries));
                    };
                    entries.insert(key, value);
                }
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(BoundedMetadataCache(entries))
            }
        }

        deserializer.deserialize_map(CacheVisitor)
    }
}

pub fn load_metadata_cache(data_dir: &Path) -> HashMap<String, MetadataCacheEntry> {
    let path = metadata_cache_path(data_dir);
    if let Ok(file) = std::fs::File::open(path) {
        let mut deserializer = serde_json::Deserializer::from_reader(BufReader::new(file));
        if let Ok(cache) = BoundedMetadataCache::deserialize(&mut deserializer) {
            return cache.0;
        }
    }
    HashMap::new()
}

pub fn save_metadata_cache(data_dir: &Path, cache: &HashMap<String, MetadataCacheEntry>) {
    let path = metadata_cache_path(data_dir);
    if let Ok(bytes) = serde_json::to_vec_pretty(cache) {
        let _ = std::fs::write(path, bytes);
    }
}

pub fn urlencoding_helper(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

pub fn resolve_steam_metadata(query: &str) -> Option<MetadataCacheEntry> {
    let clean = query.trim();
    if clean.is_empty() {
        return None;
    }
    let stripped = clean
        .strip_prefix("steam:")
        .or_else(|| clean.strip_prefix("steam-"))
        .unwrap_or(clean);
    if let Ok(app_id) = stripped.parse::<u32>() {
        return resolve_steam_app_artwork(app_id);
    }
    let url = format!(
        "https://store.steampowered.com/api/storesearch/?term={}&l=english&cc=US",
        urlencoding_helper(clean)
    );
    let output = azdl::clean_command("curl")
        .args(["-s", "--max-time", "5", "--proto", "=https", &url])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let items = value.get("items")?.as_array()?;
    let first = items.first()?;
    let app_id = first.get("id")?.as_u64()? as u32;
    resolve_steam_app_artwork(app_id)
}

fn resolve_steam_app_artwork(app_id: u32) -> Option<MetadataCacheEntry> {
    let url =
        format!("https://store.steampowered.com/api/appdetails?appids={app_id}&cc=US&l=english");
    let output = azdl::clean_command("curl")
        .args(["-s", "--max-time", "5", "--proto", "=https", &url])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    steam_artwork_from_appdetails(app_id, &value)
}

fn steam_artwork_from_appdetails(
    app_id: u32,
    value: &serde_json::Value,
) -> Option<MetadataCacheEntry> {
    let data = value.get(app_id.to_string())?.get("data")?;
    let banner_url = data
        .get("header_image")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let cover_url = data
        .get("library_assets")
        .and_then(|assets| assets.get("library"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            data.get("capsule_image")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    Some(MetadataCacheEntry {
        steam_app_id: Some(app_id),
        banner_url,
        cover_url,
    })
}

pub fn cache_artwork_image(
    data_dir: &Path,
    app_id_or_key: &str,
    kind: &str,
    url: &str,
) -> Option<PathBuf> {
    if url.starts_with("azdl-art://") {
        return None;
    }
    let safe_key: String = app_id_or_key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe_key.is_empty() {
        return None;
    }
    let target_dir = data_dir.join("artwork-cache").join(&safe_key);
    let extension = if url.to_ascii_lowercase().contains(".png") {
        "png"
    } else {
        "jpg"
    };
    let target_file = target_dir.join(format!("{kind}.{extension}"));
    if target_file.is_file()
        && target_file
            .metadata()
            .map(|m| m.len() > 100)
            .unwrap_or(false)
    {
        return Some(target_file);
    }
    let _ = std::fs::create_dir_all(&target_dir);
    let temp_file = target_dir.join(format!(".{kind}.tmp-{}", std::process::id()));
    let status = azdl::clean_command("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "10",
            "--proto",
            "=https",
            "--output",
        ])
        .arg(&temp_file)
        .arg(url)
        .status()
        .ok()?;
    if status.success()
        && temp_file.is_file()
        && temp_file.metadata().map(|m| m.len() > 100).unwrap_or(false)
    {
        let _ = std::fs::rename(&temp_file, &target_file);
        Some(target_file)
    } else {
        let _ = std::fs::remove_file(&temp_file);
        None
    }
}

pub fn get_cached_artwork_path(data_dir: &Path, key: &str, kind: &str) -> Option<PathBuf> {
    let safe_key: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe_key.is_empty() {
        return None;
    }
    let dir = data_dir.join("artwork-cache").join(&safe_key);
    for ext in ["jpg", "png", "webp", "jpeg"] {
        let file = dir.join(format!("{kind}.{ext}"));
        if file.is_file() && file.metadata().map(|m| m.len() > 100).unwrap_or(false) {
            return Some(file);
        }
    }
    None
}

pub fn get_cached_artwork_uri(data_dir: &Path, key: &str, kind: &str) -> Option<String> {
    let path = get_cached_artwork_path(data_dir, key, kind)?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("jpg");
    let safe_key: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(format!("azdl-art://localhost/{safe_key}/{kind}.{ext}"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningGameInfo {
    pub id: String,
    pub title: String,
    pub pid: u32,
    pub started_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: String,
    pub label: String,
    pub status: String,
    pub pid: Option<u32>,
    pub game_id: Option<String>,
    pub log_tail: String,
}

#[derive(Clone)]
pub struct AppState {
    pub data_dir: PathBuf,
    pub running_games: Arc<Mutex<HashMap<String, RunningGameInfo>>>,
    pub tasks: Arc<Mutex<Vec<TaskInfo>>>,
    pub mutating: Arc<AtomicBool>,
    pub cached_diagnostics: Arc<Mutex<Option<Vec<azdl::doctor::SystemCheck>>>>,
    library_cache: Arc<Mutex<Option<CachedLibrary>>>,
    pub dir_size_cache: Arc<sizes::SizeCache>,
    pub metadata_cache: Arc<Mutex<HashMap<String, MetadataCacheEntry>>>,
    pub metadata_pending: Arc<Mutex<HashSet<String>>>,
    pub steamstub_cache: Arc<Mutex<HashMap<String, (azdl::steamless::SteamStubStatus, Instant)>>>,
    pub fix_status_cache: Arc<Mutex<commands::FixStatusCache>>,
    pub app_handle: Arc<Mutex<Option<tauri::AppHandle>>>,
}

impl AppState {
    pub fn notify_state_changed(&self) {
        if let Ok(guard) = self.app_handle.lock()
            && let Some(handle) = guard.as_ref()
        {
            use tauri::Emitter;
            let _ = handle.emit("state-changed", ());
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Action {
    Launch {
        id: String,
        #[serde(default)]
        arguments: Option<Vec<String>>,
    },
    Stop {
        id: String,
    },
    RunnerSet {
        id: String,
        runner: String,
    },
    ExecutableSet {
        id: String,
        executable: String,
    },
    GameRemove {
        id: String,
    },
    DlcInstall {
        id: String,
        dlc_id: String,
    },
    DlcRemove {
        id: String,
        dlc_id: String,
    },
    DlcRecover {
        id: String,
        dlc_id: String,
    },
    OpenFolder {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        location: Option<String>,
    },
    PendingAdd {
        id: String,
        #[serde(default)]
        title: Option<String>,
        install_dir: String,
        executable: String,
    },
    PendingRemove {
        id: String,
        #[serde(default)]
        install_dir: Option<String>,
    },
    ShortcutCreate {
        id: String,
    },
    ShortcutRemove {
        id: String,
    },
    SteamShortcutCreate {
        id: String,
    },
    SteamShortcutRemove {
        id: String,
    },
    InstallVcredist {
        id: String,
    },
    InstallDirectx {
        id: String,
    },
    JobPause {
        id: String,
    },
    JobResume {
        id: String,
    },
    JobCancel {
        id: String,
    },
    ArgsSet {
        id: String,
        #[serde(default)]
        arguments: Option<Vec<String>>,
    },
    EnvSet {
        id: String,
        key: String,
        #[serde(default)]
        value: Option<String>,
    },
    EnvRemove {
        id: String,
        key: String,
    },
    OptionAdd {
        id: String,
        option_id: String,
        title: String,
        executable: String,
        #[serde(default)]
        working_directory: Option<String>,
        #[serde(default)]
        arguments: Option<Vec<String>>,
    },
    OptionSelect {
        id: String,
        option_id: String,
    },
    OptionRemove {
        id: String,
        option_id: String,
    },
    SteamInstall {
        id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        banner_url: Option<String>,
        #[serde(default)]
        cover_url: Option<String>,
        #[serde(default)]
        location: Option<String>,
    },
    OverlayToggle {
        id: String,
    },
    SteamstubUnpack {
        id: String,
    },
    SteamstubRollback {
        id: String,
    },
    EaHeal {
        id: String,
    },
    EaRollback {
        id: String,
    },
    DlcLicensePatch {
        id: String,
        #[serde(default)]
        dlc_ids: Option<Vec<u32>>,
    },
    DlcLicenseRollback {
        id: String,
    },
    GoldbergSync {
        id: String,
    },
    GoldbergRollback {
        id: String,
    },
    PatchToggle {
        id: String,
    },
    PatchRollback {
        id: String,
    },
    SettingsUpdate {
        settings: azdl::settings::LauncherSettings,
    },
    StorageAddDrive {
        #[serde(alias = "location", alias = "install_dir")]
        drive_path: String,
        #[serde(default, alias = "title")]
        drive_label: Option<String>,
    },
    StorageRemoveDrive {
        #[serde(alias = "id")]
        drive_id: String,
    },
    StorageSetDefault {
        #[serde(alias = "id")]
        drive_id: String,
    },
    StorageMoveGame {
        id: String,
        drive_id: String,
    },
    ProtonDownload,
    SteamShortcutsSync,
    JobClear {
        id: String,
    },
    JobClearSuccessful,
    JobClearCompleted,
}

pub type ActionRequest = Action;

fn resolve_install_game(
    _data_dir: &Path,
    qualified_id: &str,
    title: Option<&str>,
    banner_url: Option<&str>,
    cover_url: Option<&str>,
) -> AzdlResult<azdl::steam_install::SteamGame> {
    if let Some(app_id) = qualified_id
        .strip_prefix("steam:")
        .and_then(|value| value.parse::<u32>().ok())
    {
        let title = title
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .ok_or("Steam game title is missing")?;
        let mut game = azdl::steam_install::game_for_app(app_id, title)?;
        if let Some(url) = banner_url.filter(|url| url.starts_with("https://")) {
            game.banner_url = Some(url.to_string());
        }
        if let Some(url) = cover_url.filter(|url| url.starts_with("https://")) {
            game.cover_url = Some(url.to_string());
        }
        return Ok(game);
    }
    Err("install requests must use steam:<appid>".into())
}

fn install_root_for(data_dir: &Path, location: Option<&str>) -> AzdlResult<PathBuf> {
    let settings = azdl::settings::load_settings(data_dir)?;
    let drive = match location.filter(|value| !value.is_empty()) {
        Some(value) => settings
            .storage_drives
            .iter()
            .find(|drive| drive.id == value || drive.path.to_string_lossy() == value),
        None => settings.default_drive(),
    }
    .ok_or("Selected installation drive is no longer configured")?;
    if let Ok(metadata) = std::fs::symlink_metadata(&drive.path)
        && (!metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return Err("Selected installation drive is not a regular directory".into());
    }
    Ok(drive.path.clone())
}

#[derive(Debug, Serialize)]
pub struct RunnerEntry {
    pub id: String,
    pub name: String,
    pub runner_type: String,
    pub path: Option<PathBuf>,
    pub source: Option<String>,
}

mod commands;

#[cfg(target_os = "linux")]
fn ensure_webkit_environment() {
    if let Ok(home) = std::env::var("HOME") {
        let home_path = std::path::PathBuf::from(home);
        let local_webkit = home_path.join(".local/lib/webkit2gtk-4.1");
        if local_webkit.is_dir() {
            let tmp_link = std::path::Path::new("/tmp/webkit2gtk-4.1");
            if !tmp_link.exists() {
                let _ = std::os::unix::fs::symlink(&local_webkit, tmp_link);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn ensure_desktop_integration() {
    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => return,
    };

    let data_home = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".local/share"));

    let icon_512: &[u8] = include_bytes!("../icons/icon.png");
    let icon_256: &[u8] = include_bytes!("../icons/128x128@2x.png");
    let icon_128: &[u8] = include_bytes!("../icons/128x128.png");
    let icon_32: &[u8] = include_bytes!("../icons/32x32.png");
    let icon_svg: &[u8] = include_bytes!("../icons/icon.svg");

    let hicolor_root = data_home.join("icons/hicolor");
    let sizes: [(&str, &[u8]); 4] = [
        ("512x512", icon_512),
        ("256x256", icon_256),
        ("128x128", icon_128),
        ("32x32", icon_32),
    ];

    let icon_names = ["azdl", "azdl-desktop", "io.azdl.launcher"];

    for (size, bytes) in sizes {
        let app_dir = hicolor_root.join(size).join("apps");
        let _ = std::fs::create_dir_all(&app_dir);
        for name in icon_names {
            let target = app_dir.join(format!("{name}.png"));
            let needs_write = match std::fs::metadata(&target) {
                Ok(m) => m.len() != bytes.len() as u64,
                Err(_) => true,
            };
            if needs_write {
                let _ = std::fs::write(&target, bytes);
            }
        }
    }

    let scalable_dir = hicolor_root.join("scalable/apps");
    let _ = std::fs::create_dir_all(&scalable_dir);
    for name in icon_names {
        let target = scalable_dir.join(format!("{name}.svg"));
        let needs_write = match std::fs::metadata(&target) {
            Ok(m) => m.len() != icon_svg.len() as u64,
            Err(_) => true,
        };
        if needs_write {
            let _ = std::fs::write(&target, icon_svg);
        }
    }

    let pixmaps_dir = data_home.join("pixmaps");
    let _ = std::fs::create_dir_all(&pixmaps_dir);
    for name in icon_names {
        let target_png = pixmaps_dir.join(format!("{name}.png"));
        if !target_png.exists() {
            let _ = std::fs::write(&target_png, icon_256);
        }
        let target_svg = pixmaps_dir.join(format!("{name}.svg"));
        if !target_svg.exists() {
            let _ = std::fs::write(&target_svg, icon_svg);
        }
    }

    let index_theme = hicolor_root.join("index.theme");
    if !index_theme.exists() {
        let system_index = Path::new("/usr/share/icons/hicolor/index.theme");
        if system_index.exists() {
            let _ = std::fs::copy(system_index, &index_theme);
        }
    }

    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("azdl-desktop"));
    let exec_str = if let Ok(appimage) = std::env::var("APPIMAGE") {
        appimage
    } else {
        current_exe.to_string_lossy().to_string()
    };

    let apps_dir = data_home.join("applications");
    let _ = std::fs::create_dir_all(&apps_dir);

    let desktop_configs = [
        ("azdl-desktop.desktop", "azdl-desktop", "azdl-desktop"),
        ("azdl.desktop", "azdl", "azdl"),
        (
            "io.azdl.launcher.desktop",
            "io.azdl.launcher",
            "io.azdl.launcher",
        ),
    ];

    for (file_name, wm_class, icon_name) in desktop_configs {
        let content = format!(
            "[Desktop Entry]\n\
            Name=azdl\n\
            GenericName=Game Launcher\n\
            Comment=Modern game launcher and runtime orchestrator\n\
            Exec={exec_str} %U\n\
            Icon={icon_name}\n\
            Terminal=false\n\
            Type=Application\n\
            Categories=Game;Utility;\n\
            StartupWMClass={wm_class}\n\
            MimeType=x-scheme-handler/azdl;\n"
        );
        let target = apps_dir.join(file_name);
        let _ = std::fs::write(&target, &content);
    }

    let desktop_content = format!(
        "[Desktop Entry]\n\
        Name=azdl\n\
        GenericName=Game Launcher\n\
        Comment=Modern game launcher and runtime orchestrator\n\
        Exec={exec_str} %U\n\
        Icon=azdl\n\
        Terminal=false\n\
        Type=Application\n\
        Categories=Game;Utility;\n\
        StartupWMClass=azdl-desktop\n\
        MimeType=x-scheme-handler/azdl;\n"
    );

    let desktop_dir = azdl::clean_command("xdg-user-dir")
        .arg("DESKTOP")
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(PathBuf::from(s));
                }
            }
            None
        })
        .unwrap_or_else(|| {
            let bureau = home.join("Bureau");
            if bureau.is_dir() {
                bureau
            } else {
                home.join("Desktop")
            }
        });

    if desktop_dir.is_dir() {
        let desktop_file = desktop_dir.join("azdl.desktop");
        let _ = std::fs::write(&desktop_file, &desktop_content);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&desktop_file, std::fs::Permissions::from_mode(0o755));
        }
    }

    // Also update desktop file in repository root if executable is running from target/
    if let Some(target_dir) = current_exe.parent().and_then(|p| p.parent())
        && target_dir.file_name().and_then(|n| n.to_str()) == Some("target")
        && let Some(repo_root) = target_dir.parent()
        && repo_root.join("Cargo.toml").is_file()
    {
        let repo_desktop = repo_root.join("azdl.desktop");
        let _ = std::fs::write(&repo_desktop, &desktop_content);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&repo_desktop, std::fs::Permissions::from_mode(0o755));
        }
    }

    let _ = azdl::clean_command("update-desktop-database")
        .arg(&apps_dir)
        .status();

    let _ = azdl::clean_command("kbuildsycoca6").status();

    let _ = azdl::clean_command("gtk-update-icon-cache")
        .args(["-f", "-q"])
        .arg(&hicolor_root)
        .status();
}

fn run_worker_install(
    qualified_id: &str,
    title: Option<&str>,
    banner_url: Option<&str>,
    cover_url: Option<&str>,
    location: Option<&str>,
) {
    let data_dir = match data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Worker error: failed to resolve data dir: {e}");
            std::process::exit(1);
        }
    };
    let cat_game = match resolve_install_game(&data_dir, qualified_id, title, banner_url, cover_url)
    {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Worker error: failed to find game '{qualified_id}': {e}");
            std::process::exit(1);
        }
    };
    let mut library = match azdl::load_library(&data_dir) {
        Ok(lib) => lib,
        Err(e) => {
            eprintln!("Worker error: failed to load library: {e}");
            std::process::exit(1);
        }
    };
    let games_root = match install_root_for(&data_dir, location) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("Worker error: {error}");
            std::process::exit(1);
        }
    };
    let outcome =
        azdl::steam_install::install_game_in(&mut library, &data_dir, &games_root, &cat_game);
    match outcome {
        Ok(azdl::steam_install::InstallOutcome::Installed(inst_id)) => {
            if let Ok(mut fresh_library) = azdl::load_library(&data_dir) {
                if let Some(installed_game) = library.games.remove(&inst_id) {
                    fresh_library.games.insert(inst_id, installed_game);
                    let _ = azdl::save_library(&data_dir, &fresh_library);
                } else {
                    let _ = azdl::save_library(&data_dir, &library);
                }
            } else {
                let _ = azdl::save_library(&data_dir, &library);
            }
            let _ = azdl::cleanup_leftovers(&data_dir, Some(&cat_game.id));
            std::process::exit(0);
        }
        Ok(azdl::steam_install::InstallOutcome::NeedsSelection(_)) => {
            let _ = azdl::save_library(&data_dir, &library);
            std::process::exit(0);
        }
        Err(e) => {
            if e.downcast_ref::<azdl::jobs::TransferStopped>().is_some() {
                std::process::exit(0);
            }
            eprintln!("Worker error: install failed for {}: {e}", cat_game.title);
            std::process::exit(1);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "worker-install" {
        run_worker_install(
            &args[2],
            args.get(3).map(String::as_str),
            args.get(4).map(String::as_str),
            args.get(5).map(String::as_str),
            args.get(6).map(String::as_str),
        );
        return;
    }

    #[cfg(target_os = "linux")]
    ensure_webkit_environment();

    #[cfg(target_os = "linux")]
    ensure_desktop_integration();

    let data_dir = data_dir().expect("Failed to resolve azdl data directory");
    azdl::migration::migrate(&data_dir).expect("Failed to migrate launcher data");
    let metadata_cache = load_metadata_cache(&data_dir);
    let dir_size_cache = Arc::new(sizes::SizeCache::new(&data_dir));
    let state = AppState {
        data_dir,
        running_games: Arc::new(Mutex::new(HashMap::new())),
        tasks: Arc::new(Mutex::new(Vec::new())),
        mutating: Arc::new(AtomicBool::new(false)),
        cached_diagnostics: Arc::new(Mutex::new(None)),
        library_cache: Arc::new(Mutex::new(None)),
        dir_size_cache,
        metadata_cache: Arc::new(Mutex::new(metadata_cache)),
        metadata_pending: Arc::new(Mutex::new(HashSet::new())),
        steamstub_cache: Arc::new(Mutex::new(HashMap::new())),
        fix_status_cache: Arc::new(Mutex::new(HashMap::new())),
        app_handle: Arc::new(Mutex::new(None)),
    };

    tauri::Builder::default()
        .register_uri_scheme_protocol("azdl-art", move |_app, request| {
            let path_str = request.uri().path().trim_start_matches('/');
            let data_dir = match azdl::data_dir() {
                Ok(d) => d,
                Err(_) => {
                    return tauri::http::Response::builder()
                        .status(500)
                        .header("Access-Control-Allow-Origin", "*")
                        .body(Vec::new())
                        .unwrap();
                }
            };
            let cache_root = data_dir.join("artwork-cache");
            let _ = std::fs::create_dir_all(&cache_root);
            let target_file = cache_root.join(path_str);
            let canonical_target = match std::fs::canonicalize(&target_file) {
                Ok(p) => p,
                Err(_) => {
                    return tauri::http::Response::builder()
                        .status(404)
                        .header("Access-Control-Allow-Origin", "*")
                        .body(Vec::new())
                        .unwrap();
                }
            };
            let canonical_root = match std::fs::canonicalize(&cache_root) {
                Ok(p) => p,
                Err(_) => {
                    return tauri::http::Response::builder()
                        .status(404)
                        .header("Access-Control-Allow-Origin", "*")
                        .body(Vec::new())
                        .unwrap();
                }
            };
            if !canonical_target.starts_with(&canonical_root) || !canonical_target.is_file() {
                return tauri::http::Response::builder()
                    .status(403)
                    .header("Access-Control-Allow-Origin", "*")
                    .body(Vec::new())
                    .unwrap();
            }
            let content_type = if canonical_target
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("png"))
                .unwrap_or(false)
            {
                "image/png"
            } else if canonical_target
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("webp"))
                .unwrap_or(false)
            {
                "image/webp"
            } else {
                "image/jpeg"
            };
            match std::fs::read(&canonical_target) {
                Ok(bytes) => tauri::http::Response::builder()
                    .status(200)
                    .header("Content-Type", content_type)
                    .header("Access-Control-Allow-Origin", "*")
                    .header("Cache-Control", "public, max-age=31536000, immutable")
                    .body(bytes)
                    .unwrap(),
                Err(_) => tauri::http::Response::builder()
                    .status(500)
                    .header("Access-Control-Allow-Origin", "*")
                    .body(Vec::new())
                    .unwrap(),
            }
        })
        .manage(state)
        .setup(|app| {
            use tauri::Manager;
            let state = app.state::<AppState>();
            if let Ok(mut handle) = state.app_handle.lock() {
                *handle = Some(app.handle().clone());
            }
            let progress_state = state.inner().clone();
            std::thread::Builder::new()
                .name("azdl-progress-events".into())
                .spawn(move || {
                    use tauri::Emitter;
                    let mut previous = serde_json::Value::Null;
                    let mut interval = Duration::from_secs(1);
                    loop {
                        std::thread::sleep(interval);
                        if let Ok(progress) = commands::handle_get_progress(&progress_state) {
                            let active = progress["tasks"].as_array().is_some_and(|tasks| {
                                tasks.iter().any(|task| {
                                    task["status"] == "running" || task["status"] == "working"
                                })
                            }) || progress["jobs"].as_array().is_some_and(|jobs| {
                                jobs.iter().any(|job| {
                                    matches!(
                                        job["status"].as_str(),
                                        Some("Downloading" | "Installing" | "Queued")
                                    )
                                })
                            });
                            interval = Duration::from_secs(if active { 1 } else { 15 });
                            if progress == previous {
                                continue;
                            }
                            if let Ok(handle) = progress_state.app_handle.lock()
                                && let Some(handle) = handle.as_ref()
                            {
                                let _ = handle.emit("progress-changed", &progress);
                            }
                            previous = progress;
                        }
                    }
                })?;
            if let Some(window) = app.get_webview_window("main") {
                let icon_bytes = include_bytes!("../icons/icon.png");
                if let Ok(icon) = tauri::image::Image::from_bytes(icon_bytes) {
                    let _ = window.set_icon(icon);
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::get_progress,
            commands::get_runners,
            commands::get_steam_details,
            commands::get_game_dlc,
            commands::get_game_controls,
            commands::search_steam_games,
            commands::resolve_game_artwork,
            commands::diagnose,
            commands::discover_exes,
            commands::launch_game,
            commands::stop_game,
            commands::run_action,
            commands::window_minimize,
            commands::window_toggle_maximize,
            commands::window_close,
        ])
        .run(tauri::generate_context!())
        .expect("error while running azdl tauri application");
}

#[cfg(test)]
mod tests;
