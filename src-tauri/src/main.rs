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
const FIX_STATUS_CACHE_TTL: Duration = Duration::from_secs(15);

fn steam_dlc_provider_status(
    data_dir: &Path,
    source: Option<&str>,
    app_id: u32,
) -> Result<(), String> {
    let source_id = source.ok_or("This game has no selected download source")?;
    if source_id != "steamidra"
        && (!azdl::plugins::plugin_has_capability(data_dir, source_id, "steam_depot_download")
            || !azdl::plugins::plugin_has_capability(data_dir, source_id, "manifest_provider"))
    {
        return Err("This game's source does not provide Steam depot DLC".to_string());
    }
    azdl::plugins::manifest_download_url_for_plugin(data_dir, source_id, app_id)
        .map_err(|error| error.to_string())?;
    azdl::steam_depot::find_depotkit_library_for(data_dir, source_id)
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

type FixStatusCache = HashMap<String, (PathBuf, azdl::multiplayer::FixKind, Instant)>;

#[derive(Clone)]
pub struct AppState {
    pub data_dir: PathBuf,
    pub running_games: Arc<Mutex<HashMap<String, RunningGameInfo>>>,
    pub tasks: Arc<Mutex<Vec<TaskInfo>>>,
    pub mutating: Arc<AtomicBool>,
    pub cached_diagnostics: Arc<Mutex<Option<Vec<azdl::doctor::SystemCheck>>>>,
    library_cache: Arc<Mutex<Option<CachedLibrary>>>,
    pub dir_size_cache: Arc<sizes::SizeCache>,
    pub steamstub_cache: Arc<Mutex<HashMap<String, (azdl::steamless::SteamStubStatus, Instant)>>>,
    pub fix_status_cache: Arc<Mutex<FixStatusCache>>,
    pub metadata_cache: Arc<Mutex<HashMap<String, MetadataCacheEntry>>>,
    pub metadata_pending: Arc<Mutex<HashSet<String>>>,
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
        #[serde(default)]
        resume_job_id: Option<String>,
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
    GameImport {
        id: String,
        title: String,
        install_dir: String,
        #[serde(default)]
        executable: Option<String>,
    },
    PendingAdd {
        id: String,
        #[serde(default)]
        title: Option<String>,
        install_dir: String,
        executable: String,
        #[serde(default)]
        install_kind: Option<String>,
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
        plugin_id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        banner_url: Option<String>,
        #[serde(default)]
        cover_url: Option<String>,
        #[serde(default)]
        location: Option<String>,
    },
    PatchToggle {
        id: String,
    },
    PatchRollback {
        id: String,
    },
    PluginRemove {
        id: String,
    },
    PluginFavoriteSet {
        id: Option<String>,
    },
    PluginStoreRefresh,
    PluginStoreAdd {
        location: String,
    },
    PluginStoreInstall {
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
    ScanInstalledGames,
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
    data_dir: &Path,
    qualified_id: &str,
    plugin_id: Option<&str>,
    title: Option<&str>,
    banner_url: Option<&str>,
    cover_url: Option<&str>,
) -> AzdlResult<azdl::catalog::CatalogGame> {
    if let Some(app_id) = qualified_id
        .strip_prefix("steam:")
        .and_then(|value| value.parse::<u32>().ok())
    {
        let plugin_id = plugin_id.ok_or("choose an installed download plugin")?;
        let title = title
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .ok_or("Steam game title is missing")?;
        let mut game = azdl::plugins::game_for_steam_app(data_dir, plugin_id, app_id, title)?;
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

mod commands {
    use super::*;
    use azdl::discovery::discover_executables;
    use azdl::doctor::diagnose_game;
    use azdl::jobs::list_jobs;
    use azdl::overlay::{overlay_status, set_overlay_enabled};
    use azdl::patch::{active_patch, disable_patch, enable_patch, rollback_patch};
    use azdl::plugin_store::cached_store;
    use azdl::plugins::list_plugins;
    use azdl::setup::scan_setup;
    use azdl::{
        Library, Runner, add_game, add_launch_option, load_library, remove_launch_option,
        save_library, select_launch_option, set_executable, set_runner, spawn_game, validate_id,
    };
    use serde_json::{Value, json};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tauri::{State, Window};

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

    pub(super) fn finish_dlc_install_task(
        state: &AppState,
        task_id: &str,
        result: AzdlResult<usize>,
    ) {
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

    fn scan_steamstub_cached(
        state: &AppState,
        game: &azdl::Game,
    ) -> azdl::steamless::SteamStubStatus {
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
        let mut roots = vec![
            (data_dir.join("games"), "portable"),
            (data_dir.join("prefixes"), "setup"),
        ];
        for drive in azdl::settings::load_settings(data_dir)?.storage_drives {
            if !roots.iter().any(|(root, _)| root == &drive.path) {
                roots.push((drive.path, "portable"));
            }
        }
        for (root, kind) in roots {
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
                if kind == "setup"
                    && !fs::symlink_metadata(entry.path().join("drive_c"))
                        .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
                {
                    continue;
                }
                pending.push(json!({
                    "id": id,
                    "kind": kind,
                    "directory": entry.path(),
                }));
            }
        }
        pending.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Ok(pending)
    }

    fn progress_jobs(state: &AppState, download_jobs: Vec<azdl::jobs::DownloadJob>) -> Vec<Value> {
        let mut jobs_list: Vec<Value> = download_jobs
            .into_iter()
            .filter_map(|j| {
                if j.kind == azdl::jobs::JobKind::PluginPackage
                    && matches!(
                        j.effective_status(),
                        azdl::jobs::JobStatus::Complete | azdl::jobs::JobStatus::Cancelled
                    )
                {
                    let _ = azdl::jobs::clear_job(&state.data_dir, &j.id);
                    return None;
                }
                Some(j)
            })
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
                        "kind": format!("{:?}", p.kind),
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
            .collect();

        if let Ok(torrents) = azdl::torrent::list_torrent_jobs(&state.data_dir) {
            for t in torrents {
                let status_str = match t.status {
                    azdl::jobs::JobStatus::Running => {
                        if t.install_state.is_some() {
                            "Installing".to_string()
                        } else {
                            "Downloading".to_string()
                        }
                    }
                    s => format!("{:?}", s),
                };

                let meta = state.metadata_cache.lock().ok().and_then(|c| {
                    c.get(&t.title.to_lowercase())
                        .or_else(|| c.get(&t.id.to_lowercase()))
                        .cloned()
                });
                let steam_id = meta
                    .as_ref()
                    .and_then(|m| m.steam_app_id.map(|id| id.to_string()))
                    .or_else(|| parse_steam_id(&t.id));
                let banner = meta
                    .as_ref()
                    .and_then(|m| m.banner_url.clone())
                    .or_else(|| steam_id.as_ref().map(|id| format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{id}/header.jpg")));
                let cover = meta
                    .as_ref()
                    .and_then(|m| m.cover_url.clone())
                    .or_else(|| steam_id.as_ref().map(|id| format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{id}/library_600x900.jpg")));

                let total_bytes = None::<u64>;

                jobs_list.push(json!({
                    "id": t.id,
                    "kind": "Torrent",
                    "status": status_str,
                    "bytes": t.downloaded_bytes(&state.data_dir),
                    "total_bytes": total_bytes,
                    "total": total_bytes,
                    "estimated_bytes": None::<u64>,
                    "target": t.download_dir(&state.data_dir),
                    "error": t.error,
                    "install": Some(json!({
                        "id": t.id,
                        "title": t.title,
                        "kind": "Archive",
                        "banner_url": banner,
                        "cover_url": cover,
                        "file_size": None::<String>,
                    })),
                    "install_state": t.install_state.map(|s| format!("{:?}", s)),
                    "install_error": None::<String>,
                }));
            }
        }

        jobs_list
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
                let dir_bytes = state.dir_size_cache.get(&game.install_dir, move || updated.notify_state_changed());
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

        let plugins_list: Vec<Value> = list_plugins(&state.data_dir)
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "name": p.name,
                    "version": p.version,
                    "catalog_url": p.catalog_url,
                })
            })
            .collect();

        let store = cached_store(&state.data_dir).unwrap_or_default();

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
            "plugins": plugins_list,
            "plugin_store": store,
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

    /// Transfer updates exclude library scans, plugin catalogs, settings, and diagnostics.
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
                name: "Auto (Resolve based on OS/Game)".into(),
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
        let library = load_library(&state.data_dir).map_err(|e| e.to_string())?;
        let discovered =
            azdl::steam_discovery::discover_games(&query).map_err(|e| e.to_string())?;
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
        let library = load_library(&state.data_dir).map_err(|e| e.to_string())?;
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
        let library = load_library(&state.data_dir).map_err(|e| e.to_string())?;
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

        let portable_dir = state.data_dir.join("games").join(&id);
        if portable_dir.is_dir() {
            let candidates = discover_executables(&portable_dir, &id).map_err(|e| e.to_string())?;
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

        let prefix_dir = state.data_dir.join("prefixes").join(&id);
        if prefix_dir.is_dir() {
            let candidates = scan_setup(&state.data_dir, &id, &id).map_err(|e| e.to_string())?;
            let list: Vec<Value> = candidates
                .into_iter()
                .map(|c| {
                    let rel = c
                        .executable
                        .strip_prefix(&c.install_dir)
                        .unwrap_or(&c.executable);
                    json!({
                        "path": c.executable,
                        "relative_path": rel,
                        "score": c.score,
                    })
                })
                .collect();
            return Ok(json!(list));
        }

        let direct_path = Path::new(&id);
        if direct_path.is_dir() {
            let candidates = discover_executables(direct_path, &id).map_err(|e| e.to_string())?;
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

    pub fn handle_scan_game_setup(
        state: &AppState,
        id: String,
        title: Option<String>,
    ) -> Result<Value, String> {
        let title_str = title.unwrap_or_else(|| id.clone());
        let candidates = scan_setup(&state.data_dir, &id, &title_str).map_err(|e| e.to_string())?;
        let list: Vec<Value> = candidates
            .into_iter()
            .map(|cand| {
                json!({
                    "install_dir": cand.install_dir,
                    "executable": cand.executable,
                    "score": cand.score,
                })
            })
            .collect();
        Ok(json!(list))
    }

    #[tauri::command]
    pub async fn scan_game_setup(
        state: State<'_, AppState>,
        id: String,
        title: Option<String>,
    ) -> Result<Value, String> {
        let state = state.inner().clone();
        tauri::async_runtime::spawn_blocking(move || handle_scan_game_setup(&state, id, title))
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

    pub fn handle_run_action(
        state: &AppState,
        action: ActionRequest,
    ) -> Result<Value, LauncherError> {
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
                if let Ok(mut cache) = state.steamstub_cache.lock() {
                    cache.clear();
                }
                if let Ok(mut cache) = state.fix_status_cache.lock() {
                    cache.remove(&id);
                }
                Ok(json!({ "success": true, "removed": id }))
            }
            Action::DlcInstall {
                id,
                dlc_id,
                resume_job_id,
            } => {
                let content_lock =
                    azdl::dlc::lock_game(data_dir, &id).map_err(|error| error.to_string())?;
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
                let addon = azdl::dlc::available_for_game(game, data_dir)?
                    .into_iter()
                    .find(|addon| addon.id == dlc_id && addon.content_available);
                let steam_dlc_id = dlc_id.parse::<u32>().ok();
                if addon.is_none() && steam_dlc_id.is_none() {
                    return Err("DLC is not offered by this game's source plugin".into());
                }
                if let Some(job_id) = &resume_job_id {
                    if addon.is_none() {
                        return Err(
                            "Only an offered archive DLC can resume a paused transfer".into()
                        );
                    }
                    azdl::dlc::validate_catalog_dlc_resume(data_dir, job_id, &id, &dlc_id)?;
                }
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
                        let result = if let Some(job_id) = resume_job_id {
                            azdl::dlc::validate_catalog_dlc_resume(
                                &state_clone.data_dir,
                                &job_id,
                                &game.id,
                                &dlc_id,
                            )
                            .and_then(|()| {
                                azdl::dlc::resume_catalog_dlc_job(&state_clone.data_dir, &job_id)
                            })
                            .and_then(|result| {
                                result.ok_or_else(|| "Paused DLC transfer is unavailable".into())
                            })
                        } else if let Some(addon) = addon {
                            azdl::dlc::install_catalog_dlc(&game, &state_clone.data_dir, &addon)
                        } else {
                            azdl::steam_depot::install_steam_dlc(
                                &game,
                                &state_clone.data_dir,
                                steam_dlc_id.unwrap_or_default(),
                            )
                        };
                        finish_dlc_install_task(&state_clone, &task_id_clone, result);
                    })
                    .map_err(|error| {
                        finish_install_task(state, &task_id, Some(error.to_string()));
                        error.to_string()
                    })?;
                Ok(json!({ "success": true, "started": true, "task_id": task_id }))
            }
            Action::DlcRemove { id, dlc_id } => {
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
            Action::GameImport {
                id,
                title,
                install_dir,
                executable,
            } => {
                let canonical_install =
                    fs::canonicalize(&install_dir).map_err(|e| e.to_string())?;
                let settings = azdl::settings::load_settings(data_dir)
                    .unwrap_or_else(|_| azdl::settings::LauncherSettings::default_for(data_dir));
                let is_inside_storage = settings.storage_drives.iter().any(|drive| {
                    if let Ok(canon_drive) = fs::canonicalize(&drive.path) {
                        canonical_install.starts_with(canon_drive)
                    } else {
                        false
                    }
                }) || canonical_install.starts_with(data_dir);
                if !is_inside_storage {
                    return Err(
                        "External game directories cannot be added. Games must reside within launcher storage locations."
                            .into(),
                    );
                }
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                if let Some(exe) = executable.filter(|e| !e.is_empty()) {
                    add_game(
                        &mut library,
                        &id,
                        &title,
                        &canonical_install,
                        Path::new(&exe),
                    )
                    .map_err(|e| e.to_string())?;
                } else {
                    azdl::discovery::import_game(&mut library, &id, &title, &canonical_install)
                        .map_err(|e| e.to_string())?;
                }
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "imported": id }))
            }
            Action::PendingAdd {
                id,
                title,
                install_dir,
                executable,
                install_kind,
            } => {
                let game_title = title.unwrap_or_else(|| id.clone());
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                let install_path = Path::new(&install_dir);
                let exe_path = Path::new(&executable);
                let is_setup = install_kind.as_deref() == Some("setup");
                let final_install_dir =
                    if is_setup && exe_path.is_absolute() && !exe_path.starts_with(install_path) {
                        exe_path.parent().unwrap_or(install_path)
                    } else {
                        install_path
                    };
                add_game(&mut library, &id, &game_title, final_install_dir, exe_path)
                    .map_err(|e| e.to_string())?;
                if is_setup && let Some(game) = library.games.get_mut(&id) {
                    game.runner = Runner::Wine;
                }
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
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
                let prefix_dir = data_dir.join("prefixes").join(&id);
                if prefix_dir.is_dir() {
                    let _ = fs::remove_dir_all(&prefix_dir);
                }
                let _ = azdl::cleanup_leftovers(data_dir, Some(&id));
                Ok(json!({ "success": true, "removed": id }))
            }
            Action::ShortcutCreate { id } => {
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                azdl::steam_shortcuts::create_shortcut(game, None).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true }))
            }
            Action::SteamShortcutRemove { id } => {
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                azdl::steam_shortcuts::remove_shortcut(game, None).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true }))
            }
            Action::OverlayToggle { id } => {
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                let count =
                    azdl::steamless::unpack_game(game, data_dir).map_err(|e| e.to_string())?;
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
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                let msg = azdl::fixes::heal_ea_activation(&game.install_dir)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "message": msg }))
            }
            Action::EaRollback { id } => {
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                let msg = azdl::fixes::rollback_ea_activation(&game.install_dir)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "message": msg }))
            }
            Action::DlcLicensePatch { id, dlc_ids } => {
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                let target_ids = match dlc_ids {
                    Some(ids) => ids,
                    None => {
                        let installed =
                            azdl::dlc::list_installed(game, data_dir).unwrap_or_default();
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
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
            Action::InstallVcredist { id } => {
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                let prefix_dir = data_dir.join("prefixes").join(&game.id);
                let ok =
                    azdl::dependencies::install_vcredist(&prefix_dir, &game.runner, data_dir, true)
                        .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "installed": ok }))
            }
            Action::InstallDirectx { id } => {
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                let prefix_dir = data_dir.join("prefixes").join(&game.id);
                let ok = azdl::dependencies::install_directx(
                    &prefix_dir,
                    &game.runner,
                    data_dir,
                    &game.install_dir,
                )
                .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "installed": ok }))
            }
            Action::JobPause { id } => {
                if azdl::jobs::load_job(data_dir, &id).is_ok() {
                    azdl::jobs::pause_job(data_dir, &id)?;
                } else if azdl::torrent::load_torrent_job(data_dir, &id).is_ok() {
                    azdl::torrent::pause_magnet(data_dir, &id)?;
                } else {
                    return Err(LauncherError::general(format!("Job {id} not found")));
                }
                Ok(json!({ "success": true }))
            }
            Action::JobResume { id } => {
                let mut dlc_installed_files = None;
                if azdl::jobs::load_job(data_dir, &id).is_ok() {
                    if let Some(count) = azdl::dlc::resume_catalog_dlc_job(data_dir, &id)? {
                        dlc_installed_files = Some(count);
                    } else {
                        azdl::jobs::retry_job(data_dir, &id)?;
                    }
                } else if azdl::torrent::load_torrent_job(data_dir, &id).is_ok() {
                    let mut library = load_library(data_dir)?;
                    azdl::torrent::resume_magnet(&mut library, data_dir, &id)?;
                } else {
                    return Err(LauncherError::general(format!("Job {id} not found")));
                }
                Ok(json!({ "success": true, "dlc_installed_files": dlc_installed_files }))
            }
            Action::JobCancel { id } => {
                if azdl::jobs::load_job(data_dir, &id).is_ok() {
                    azdl::jobs::cancel_job(data_dir, &id)?;
                } else if azdl::torrent::load_torrent_job(data_dir, &id).is_ok() {
                    azdl::torrent::cancel_magnet(data_dir, &id)?;
                } else {
                    return Err(LauncherError::general(format!("Job {id} not found")));
                }
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
                plugin_id,
                title,
                banner_url,
                cover_url,
                location,
            } => {
                let games_root = install_root_for(data_dir, location.as_deref())?;
                let cat_game = resolve_install_game(
                    data_dir,
                    &id,
                    plugin_id.as_deref(),
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
                        .arg(plugin_id.as_deref().unwrap_or(""))
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
                                                format!(
                                                    "Download worker failed ({})",
                                                    output.status
                                                )
                                            } else {
                                                detail.trim().to_string()
                                            })
                                        }
                                        Err(error) => {
                                            Some(format!("Download worker failed: {error}"))
                                        }
                                    };
                                    finish_install_task(&wait_state, &wait_task_id, error);
                                })
                                .ok();
                        }
                        Err(e) => {
                            eprintln!(
                                "Failed to spawn worker process: {e}, falling back to thread"
                            );
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

                            let outcome = azdl::catalog::install_game_in(
                                &mut library,
                                data_dir,
                                &games_root_for_thread,
                                &cat_game_clone,
                            );
                            let mut failure = None;
                            match outcome {
                                Ok(azdl::catalog::InstallOutcome::Installed(inst_id)) => {
                                    if let Ok(mut fresh_library) = load_library(data_dir) {
                                        if let Some(installed_game) = library.games.remove(&inst_id)
                                        {
                                            fresh_library.games.insert(inst_id, installed_game);
                                            let _ = save_library(data_dir, &fresh_library);
                                        } else {
                                            let _ = save_library(data_dir, &library);
                                        }
                                    } else {
                                        let _ = save_library(data_dir, &library);
                                    }
                                    let _ =
                                        azdl::cleanup_leftovers(data_dir, Some(&cat_game_clone.id));
                                }
                                Ok(azdl::catalog::InstallOutcome::NeedsSelection(_)) => {
                                    let _ = save_library(data_dir, &library);
                                }
                                Ok(azdl::catalog::InstallOutcome::SteamRequested(_)) => {}
                                Err(e) => {
                                    eprintln!(
                                        "Background install failed for {}: {e}",
                                        cat_game_clone.title
                                    );
                                    failure = Some(e.to_string());
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
            Action::PatchToggle { id } => {
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
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
            Action::PluginRemove { id } => {
                azdl::plugins::remove_plugin(data_dir, &id).map_err(|e| e.to_string())?;
                let mut settings = azdl::settings::load_settings(data_dir)?;
                if settings.favorite_plugin_id.as_deref() == Some(&id) {
                    settings.favorite_plugin_id = None;
                    azdl::settings::save_settings(data_dir, &settings)?;
                }
                Ok(json!({ "success": true, "removed": id }))
            }
            Action::PluginFavoriteSet { id } => {
                if let Some(ref id) = id
                    && !azdl::plugins::list_plugins(data_dir)?
                        .iter()
                        .any(|plugin| &plugin.id == id)
                {
                    return Err(LauncherError::plugin_not_found(id));
                }
                let mut settings = azdl::settings::load_settings(data_dir)?;
                settings.favorite_plugin_id = id.clone();
                azdl::settings::save_settings(data_dir, &settings)?;
                Ok(json!({ "success": true, "favorite_plugin_id": id }))
            }
            Action::PluginStoreRefresh => {
                let task_id = "refresh-plugin-store".to_string();
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
                        label: "Refreshing plugin store".into(),
                        status: "running".into(),
                        pid: None,
                        game_id: None,
                        log_tail: String::new(),
                    });
                }
                let state_clone = state.clone();
                let task_id_clone = task_id.clone();
                std::thread::Builder::new()
                    .name("azdl-store-refresh".into())
                    .spawn(move || {
                        let data_dir = &state_clone.data_dir;
                        let _ = azdl::plugin_store::refresh_store(data_dir);
                        if let Ok(mut tasks) = state_clone.tasks.lock() {
                            tasks.retain(|t| t.id != task_id_clone);
                        }
                    })
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "started": true }))
            }
            Action::PluginStoreAdd { location } => {
                let count = azdl::plugin_store::add_store(data_dir, &location)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "store": location, "count": count }))
            }
            Action::PluginStoreInstall { id } => {
                let task_id = format!("install-plugin-{id}");
                {
                    let mut tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
                    tasks.retain(|t| t.id != task_id);
                    tasks.push(TaskInfo {
                        id: task_id.clone(),
                        label: format!("Installing plugin {id}"),
                        status: "running".into(),
                        pid: None,
                        game_id: None,
                        log_tail: String::new(),
                    });
                }
                let state_clone = state.clone();
                let id_clone = id.clone();
                let task_id_clone = task_id.clone();
                std::thread::Builder::new()
                    .name(format!("azdl-plug-{id}"))
                    .spawn(move || {
                        let data_dir = &state_clone.data_dir;
                        let result = azdl::plugin_store::install_store_plugin(data_dir, &id_clone)
                            .and_then(|_| {
                                azdl::plugins::refresh_plugin_catalog(data_dir, &id_clone)
                            });
                        finish_install_task(
                            &state_clone,
                            &task_id_clone,
                            result.err().map(|e| e.to_string()),
                        );
                    })
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "started": true, "plugin": id }))
            }
            Action::SettingsUpdate { settings } => {
                azdl::settings::save_settings(data_dir, &settings).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "settings": settings }))
            }
            Action::StorageAddDrive {
                drive_path,
                drive_label,
            } => {
                let label = drive_label.unwrap_or_default();
                let drive =
                    azdl::settings::add_storage_drive(data_dir, &label, Path::new(&drive_path))
                        .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "drive": drive }))
            }
            Action::StorageRemoveDrive { drive_id } => {
                azdl::settings::remove_storage_drive(data_dir, &drive_id)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "removed": drive_id }))
            }
            Action::StorageSetDefault { drive_id } => {
                azdl::settings::set_default_drive(data_dir, &drive_id)
                    .map_err(|e| e.to_string())?;
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
            Action::ScanInstalledGames => {
                let task_id = "scan-games".to_string();
                {
                    let mut tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
                    tasks.retain(|t| t.id != task_id);
                    tasks.push(TaskInfo {
                        id: task_id.clone(),
                        label: "Scanning storage drives for games".into(),
                        status: "running".into(),
                        pid: None,
                        game_id: None,
                        log_tail: String::new(),
                    });
                }
                let state_clone = state.clone();
                let task_id_clone = task_id.clone();
                std::thread::Builder::new()
                    .name("azdl-scan-games".into())
                    .spawn(move || {
                        let data_dir = &state_clone.data_dir;
                        let result = load_library(data_dir).and_then(|mut library| {
                            azdl::settings::scan_drives_for_games(data_dir, &mut library)
                        });
                        finish_install_task(
                            &state_clone,
                            &task_id_clone,
                            result.err().map(|e| e.to_string()),
                        );
                    })
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "started": true }))
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
                let mut cleared = false;
                if azdl::jobs::clear_job(data_dir, &id).is_ok() {
                    cleared = true;
                }
                let torrent_file = data_dir.join("torrents").join(format!("{id}.json"));
                if torrent_file.is_file() {
                    let _ = fs::remove_file(torrent_file);
                    cleared = true;
                }
                if cleared {
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
                if let Ok(torrents) = azdl::torrent::list_torrent_jobs(data_dir) {
                    for t in torrents {
                        if matches!(
                            t.status,
                            azdl::jobs::JobStatus::Complete | azdl::jobs::JobStatus::Cancelled
                        ) || (include_failed && t.status == azdl::jobs::JobStatus::Failed)
                        {
                            let torrent_file =
                                data_dir.join("torrents").join(format!("{}.json", t.id));
                            if fs::remove_file(torrent_file).is_ok() {
                                count += 1;
                            }
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
        let mut response: HashMap<String, SteamDetailsEnvelope> = serde_json::from_slice(response)
            .map_err(|error| format!("json parse error: {error}"))?;
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
        let url = format!(
            "https://store.steampowered.com/api/appdetails?appids={app_id}&l=english&cc=US"
        );
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
            let mut available = azdl::dlc::available_for_game(game, &data_dir)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|addon| {
                    json!({
                        "id": addon.id,
                        "title": addon.title,
                        "content_available": addon.content_available,
                        "installed": installed.contains(&addon.id),
                        "interrupted": interrupted.contains(&addon.id),
                        "reason": if addon.content_available { None } else { Some("The source has no downloadable DLC archive") },
                    })
                })
                .collect::<Vec<_>>();
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
                let source = game
                    .origin
                    .as_ref()
                    .and_then(|origin| origin.qualified_id.split_once(':'))
                    .map(|(source, _)| source);
                let provider_status = super::steam_dlc_provider_status(&data_dir, source, app_id);
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

    #[tauri::command]
    pub fn is_steam_running() -> bool {
        azdl::multiplayer::steam_running()
    }
}

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
    plugin_id: Option<&str>,
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
    let cat_game = match resolve_install_game(
        &data_dir,
        qualified_id,
        plugin_id,
        title,
        banner_url,
        cover_url,
    ) {
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
    let outcome = azdl::catalog::install_game_in(&mut library, &data_dir, &games_root, &cat_game);
    match outcome {
        Ok(azdl::catalog::InstallOutcome::Installed(inst_id)) => {
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
        Ok(azdl::catalog::InstallOutcome::NeedsSelection(_)) => {
            let _ = azdl::save_library(&data_dir, &library);
            std::process::exit(0);
        }
        Ok(azdl::catalog::InstallOutcome::SteamRequested(_)) => {
            std::process::exit(0);
        }
        Err(e) => {
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
            args.get(7).map(String::as_str),
        );
        return;
    }

    #[cfg(target_os = "linux")]
    ensure_webkit_environment();

    #[cfg(target_os = "linux")]
    ensure_desktop_integration();

    let data_dir = data_dir().expect("Failed to resolve azdl data directory");
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
        steamstub_cache: Arc::new(Mutex::new(HashMap::new())),
        fix_status_cache: Arc::new(Mutex::new(HashMap::new())),
        metadata_cache: Arc::new(Mutex::new(metadata_cache)),
        metadata_pending: Arc::new(Mutex::new(HashSet::new())),
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
            commands::search_steam_games,
            commands::resolve_game_artwork,
            commands::diagnose,
            commands::discover_exes,
            commands::scan_game_setup,
            commands::launch_game,
            commands::stop_game,
            commands::run_action,
            commands::window_minimize,
            commands::window_toggle_maximize,
            commands::window_close,
            commands::is_steam_running,
        ])
        .run(tauri::generate_context!())
        .expect("error while running azdl tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_dlc_requires_its_installed_download_provider() {
        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-provider-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let wrong_source = steam_dlc_provider_status(&root, Some("other"), 100).unwrap_err();
        assert!(wrong_source.contains("does not provide Steam depot DLC"));
        let missing_provider =
            steam_dlc_provider_status(&root, Some("steamidra"), 100).unwrap_err();
        assert!(missing_provider.contains("missing or has an invalid manifest"));
        assert!(!root.exists());
    }

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
            steamstub_cache: Arc::new(Mutex::new(HashMap::new())),
            fix_status_cache: Arc::new(Mutex::new(HashMap::new())),
            metadata_cache: Arc::new(Mutex::new(HashMap::new())),
            metadata_pending: Arc::new(Mutex::new(HashSet::new())),
            app_handle: Arc::new(Mutex::new(None)),
        }
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
    fn fix_status_cache_reuses_results_and_refreshes_after_invalidation() {
        let root = scratch();
        let install_dir = root.join("game");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(install_dir.join("game.exe"), b"game").unwrap();
        let mut library = azdl::Library::default();
        azdl::add_game(
            &mut library,
            "game",
            "Game",
            &install_dir,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = library.games.get("game").unwrap();
        let state = test_state(&root);

        assert_eq!(
            commands::detect_fix_cached(&state, game),
            Some(azdl::multiplayer::FixKind::None)
        );
        fs::write(install_dir.join("OnlineFix.ini"), b"[OnlineFix]").unwrap();
        assert_eq!(
            commands::detect_fix_cached(&state, game),
            Some(azdl::multiplayer::FixKind::None)
        );

        state.fix_status_cache.lock().unwrap().remove("game");
        assert_eq!(
            commands::detect_fix_cached(&state, game),
            Some(azdl::multiplayer::FixKind::OnlineFix)
        );
        fs::remove_dir_all(root).unwrap();
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
            serde_json::from_str(r#"{"action":"dlc-install","id":"base","dlc_id":"bonus"}"#)
                .unwrap();
        assert!(matches!(
            install,
            Action::DlcInstall {
                resume_job_id: None,
                ..
            }
        ));
        let resume: ActionRequest = serde_json::from_str(
            r#"{"action":"dlc-install","id":"base","dlc_id":"bonus","resume_job_id":"0123456789abcdef"}"#,
        ).unwrap();
        assert!(
            matches!(resume, Action::DlcInstall { resume_job_id: Some(id), .. } if id == "0123456789abcdef")
        );
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
    fn test_handle_get_state_with_plugin_store_cache() {
        let root = scratch();
        let cache_path = root.join("plugin-store.json");
        let sample = serde_json::json!([
            {
                "id": "steamidra",
                "name": "Steamidra",
                "description": "Depot downloader plugin",
                "version": "1.0.0",
                "url": "https://example.com/steamidra.arach",
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "filename": "steamidra.arach",
                "recommended": true
            }
        ]);
        std::fs::write(&cache_path, serde_json::to_vec(&sample).unwrap()).unwrap();
        let state = test_state(&root);
        let val = handle_get_state(&state).unwrap();
        let store = val["plugin_store"]
            .as_array()
            .expect("plugin_store should be array");
        assert_eq!(store.len(), 1);
        assert_eq!(store[0]["id"], "steamidra");
        assert_eq!(store[0]["name"], "Steamidra");
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
    fn test_handle_run_action_game_import_and_remove() {
        let root = scratch();
        let state = test_state(&root);
        let game_dir = root.join("my-test-game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("game.bin"), b"test").unwrap();

        // 1. Import game
        let import_req = Action::GameImport {
            id: "test-game".into(),
            title: "My Test Game".into(),
            install_dir: game_dir.to_string_lossy().into(),
            executable: Some("game.bin".into()),
        };
        let res = handle_run_action(&state, import_req).unwrap();
        assert_eq!(res["imported"], "test-game");

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

        let import_req = Action::GameImport {
            id: "runner-game".into(),
            title: "Runner Game".into(),
            install_dir: game_dir.to_string_lossy().into(),
            executable: Some("game.exe".into()),
        };
        handle_run_action(&state, import_req).unwrap();

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

        handle_run_action(
            &state,
            Action::GameImport {
                id: "args-env-game".into(),
                title: "Args Env Game".into(),
                install_dir: game_dir.to_string_lossy().into(),
                executable: Some("main.bin".into()),
            },
        )
        .unwrap();

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

        handle_run_action(
            &state,
            Action::GameImport {
                id: "opt-game".into(),
                title: "Option Game".into(),
                install_dir: game_dir.to_string_lossy().into(),
                executable: Some("game.exe".into()),
            },
        )
        .unwrap();

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
                install_kind: None,
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

        handle_run_action(
            &state,
            Action::GameImport {
                id: "diag-game".into(),
                title: "Diag Game".into(),
                install_dir: game_dir.to_string_lossy().into(),
                executable: Some("main.exe".into()),
            },
        )
        .unwrap();

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
    fn test_handle_scan_game_setup() {
        let root = scratch();
        let state = test_state(&root);
        let prefix_dir = root
            .join("prefixes")
            .join("prefix-game")
            .join("drive_c")
            .join("Program Files")
            .join("Game");
        fs::create_dir_all(&prefix_dir).unwrap();
        fs::write(prefix_dir.join("game.exe"), b"game").unwrap();

        let scanned = handle_scan_game_setup(&state, "prefix-game".into(), None).unwrap();
        let arr = scanned.as_array().unwrap();
        assert!(!arr.is_empty());
        assert!(
            arr.iter()
                .any(|c| c["executable"].as_str().unwrap().contains("game.exe"))
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
    fn test_handle_run_action_overlay_toggle() {
        let root = scratch();
        let state = test_state(&root);
        let game_dir = root.join("fix-game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("game.exe"), b"test").unwrap();
        // Create an online-fix DLL
        fs::write(game_dir.join("OnlineFix.ini"), b"[Main]\nFakeAppId=480").unwrap();
        fs::write(game_dir.join("OnlineFix64.dll"), b"dll").unwrap();

        handle_run_action(
            &state,
            Action::GameImport {
                id: "fix-game".into(),
                title: "Fix Game".into(),
                install_dir: game_dir.to_string_lossy().into(),
                executable: Some("game.exe".into()),
            },
        )
        .unwrap();

        // Toggle overlay: disables OnlineFix64.dll -> OnlineFix64.dll.arachnel-off
        let res = handle_run_action(
            &state,
            Action::OverlayToggle {
                id: "fix-game".into(),
            },
        )
        .unwrap();
        assert_eq!(res["enabled"], false);
        assert!(game_dir.join("OnlineFix64.dll.arachnel-off").exists());

        // Toggle back: re-enables
        let res2 = handle_run_action(
            &state,
            Action::OverlayToggle {
                id: "fix-game".into(),
            },
        )
        .unwrap();
        assert_eq!(res2["enabled"], true);
        assert!(game_dir.join("OnlineFix64.dll").exists());
    }

    #[test]
    fn test_handle_run_action_steamstub_unpack_and_rollback() {
        let root = scratch();
        let state = test_state(&root);
        let game_dir = root.join("stub-game");
        fs::create_dir_all(&game_dir).unwrap();

        let exe_path = game_dir.join("game.exe");
        let mut data = vec![0u8; 512];
        data[0] = b'M';
        data[1] = b'Z';
        data[0x3C] = 0x80;
        data[128] = b'P';
        data[129] = b'E';
        data[130] = 0;
        data[131] = 0;
        data[134] = 1;
        data[148] = 0;
        for (i, b) in b".bind\0\0\0".iter().enumerate() {
            data[152 + i] = *b;
        }
        fs::write(&exe_path, &data).unwrap();

        handle_run_action(
            &state,
            Action::GameImport {
                id: "stub-game".into(),
                title: "Stub Game".into(),
                install_dir: game_dir.to_string_lossy().into(),
                executable: Some("game.exe".into()),
            },
        )
        .unwrap();

        let lib_state = handle_get_state(&state).unwrap();
        assert_eq!(lib_state["library"][0]["steamstub"]["stub_present"], true);
        assert_eq!(lib_state["library"][0]["steamstub"]["applied"], false);

        // Simulate unpack by creating backup and clean exe
        let backup_path = game_dir.join("game.exe.steamstub.bak");
        fs::copy(&exe_path, &backup_path).unwrap();
        let mut clean_data = data.clone();
        for (i, b) in b".text\0\0\0".iter().enumerate() {
            clean_data[152 + i] = *b;
        }
        fs::write(&exe_path, &clean_data).unwrap();

        let res_rollback = handle_run_action(
            &state,
            Action::SteamstubRollback {
                id: "stub-game".into(),
            },
        )
        .unwrap();

        assert_eq!(res_rollback["success"], true);
        assert_eq!(res_rollback["restored_count"], 1);
        assert_eq!(res_rollback["steamstub"]["stub_present"], true);
        assert_eq!(res_rollback["steamstub"]["applied"], false);

        let cached_status = {
            let cache = state.steamstub_cache.lock().unwrap();
            cache.get("stub-game").map(|(s, _)| s.clone()).unwrap()
        };
        assert!(cached_status.stub_present);
        assert!(!cached_status.applied);

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

        handle_run_action(
            &state,
            Action::GameImport {
                id: "proc-game".into(),
                title: "Proc Game".into(),
                install_dir: game_dir.to_string_lossy().into(),
                executable: Some(exe_name.into()),
            },
        )
        .unwrap();

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
    fn favorite_plugin_persists_and_is_cleared_on_removal() {
        let root = scratch();
        let plugin = root.join("plugins/provider-a");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            br#"{"id":"provider-a","name":"Provider A","version":"1","apiVersion":4,"library":"provider.so"}"#,
        )
        .unwrap();
        let state = test_state(&root);
        handle_run_action(
            &state,
            Action::PluginFavoriteSet {
                id: Some("provider-a".into()),
            },
        )
        .unwrap();
        assert_eq!(
            azdl::settings::load_settings(&root)
                .unwrap()
                .favorite_plugin_id
                .as_deref(),
            Some("provider-a")
        );
        assert!(
            handle_run_action(
                &state,
                Action::PluginFavoriteSet {
                    id: Some("missing".into()),
                },
            )
            .is_err()
        );
        handle_run_action(
            &state,
            Action::PluginRemove {
                id: "provider-a".into(),
            },
        )
        .unwrap();
        assert!(
            azdl::settings::load_settings(&root)
                .unwrap()
                .favorite_plugin_id
                .is_none()
        );
        fs::remove_dir_all(root).unwrap();
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
            expected_sha256: None,
            install: Some(azdl::jobs::InstallPlan {
                game_id: "steam-480".to_string(),
                title: "Spacewar".to_string(),
                kind: azdl::jobs::InstallKind::File,
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
            expected_sha256: None,
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
        let provider = root.join("plugins/steamidra");
        fs::create_dir_all(&provider).unwrap();
        fs::write(
            provider.join("plugin.json"),
            br#"{"id":"steamidra","name":"SteaMidra","version":"1","apiVersion":4,"library":"plugin"}"#,
        )
        .unwrap();
        let cached = root.join("downloads/steam-620/620.zip");
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::write(&cached, b"invalid archive").unwrap();

        let start_time = std::time::Instant::now();
        let res = handle_run_action(
            &state,
            Action::SteamInstall {
                id: "steam:620".into(),
                title: Some("Portal 2".into()),
                plugin_id: Some("steamidra".into()),
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
                plugin_id: Some("steamidra".into()),
                banner_url: None,
                cover_url: None,
                location: None,
            },
        )
        .unwrap();
        assert_eq!(duplicate["already_working"], true);
    }
}
