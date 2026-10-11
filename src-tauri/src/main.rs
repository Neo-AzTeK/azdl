#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use azdl::Result as AzdlResult;
use azdl::data_dir;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    let output = std::process::Command::new("curl")
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
    let output = std::process::Command::new("curl")
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
    pub cached_catalog: Arc<Mutex<Option<Vec<azdl::catalog::CatalogGame>>>>,
    pub cached_diagnostics: Arc<Mutex<Option<Vec<azdl::doctor::SystemCheck>>>>,
    library_cache: Arc<Mutex<Option<CachedLibrary>>>,
    pub dir_size_cache: Arc<Mutex<HashMap<PathBuf, (u64, Instant)>>>,
    pub steamstub_cache: Arc<Mutex<HashMap<String, (azdl::steamless::SteamStubStatus, Instant)>>>,
    pub fix_status_cache: Arc<Mutex<FixStatusCache>>,
    pub metadata_cache: Arc<Mutex<HashMap<String, MetadataCacheEntry>>>,
    pub metadata_pending: Arc<Mutex<HashSet<String>>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ActionRequest {
    pub action: String,
    pub id: Option<String>,
    pub title: Option<String>,
    pub install_dir: Option<String>,
    pub executable: Option<String>,
    pub destination: Option<String>,
    pub sha256: Option<String>,
    pub install_kind: Option<String>,
    pub patch_kind: Option<String>,
    pub patch_root: Option<String>,
    pub location: Option<String>,
    pub addon_id: Option<String>,
    pub option_id: Option<String>,
    pub working_directory: Option<String>,
    pub runner: Option<String>,
    pub arguments: Option<Vec<String>>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub drive_id: Option<String>,
    pub drive_label: Option<String>,
    pub drive_path: Option<String>,
    pub settings: Option<azdl::settings::LauncherSettings>,
    pub plugin_name: Option<String>,
    pub version: Option<String>,
    pub catalog_url: Option<String>,
    pub plugin_id: Option<String>,
    pub banner_url: Option<String>,
    pub cover_url: Option<String>,
}

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
    use std::process::Command;
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

    fn calc_dir_size(path: &Path) -> u64 {
        let mut total = 0;
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Ok(meta) = p.symlink_metadata() {
                    if meta.is_file() {
                        total += meta.len();
                    } else if meta.is_dir() {
                        total += calc_dir_size(&p);
                    }
                }
            }
        }
        total
    }

    fn calc_dir_size_cached(state: &AppState, path: &Path) -> u64 {
        if let Ok(cache) = state.dir_size_cache.lock()
            && let Some(&(bytes, instant)) = cache.get(path)
            && instant.elapsed() < Duration::from_secs(60)
        {
            return bytes;
        }
        let bytes = calc_dir_size(path);
        if let Ok(mut cache) = state.dir_size_cache.lock() {
            cache.insert(path.to_path_buf(), (bytes, Instant::now()));
        }
        bytes
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

    fn parse_size_str_to_bytes(s: &str) -> Option<u64> {
        let s = s.trim();
        let num_part: String = s
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let num: f64 = num_part.parse().ok()?;
        let rest = s[num_part.len()..].trim().to_ascii_uppercase();
        let multiplier: f64 = if rest.starts_with("TB") {
            1024.0 * 1024.0 * 1024.0 * 1024.0
        } else if rest.starts_with("GB") {
            1024.0 * 1024.0 * 1024.0
        } else if rest.starts_with("MB") {
            1024.0 * 1024.0
        } else if rest.starts_with("KB") {
            1024.0
        } else if rest.starts_with('B') {
            1.0
        } else {
            1024.0 * 1024.0
        };
        Some((num * multiplier) as u64)
    }

    fn pending_installs(
        data_dir: &Path,
        library: &Library,
        active_game_ids: &[String],
        active_targets: &[PathBuf],
    ) -> AzdlResult<Vec<Value>> {
        let mut pending = Vec::new();
        for (folder, kind) in [("games", "portable"), ("prefixes", "setup")] {
            let root = data_dir.join(folder);
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

        let cached_cat = state
            .cached_catalog
            .lock()
            .ok()
            .and_then(|lock| lock.clone());

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

                let catalog_match = cached_cat.as_ref().and_then(|cat| {
                    cat.iter().find(|cg| {
                        cg.id == game.id
                            || game
                                .origin
                                .as_ref()
                                .map(|o| &o.qualified_id)
                                .map(|qid| qid == &format!("{}:{}", cg.source_id, cg.id))
                                .unwrap_or(false)
                            || cg.title.eq_ignore_ascii_case(&game.title)
                    })
                });

                let source_id = catalog_match.map(|cg| cg.source_id.clone()).or_else(|| {
                    game.origin
                        .as_ref()
                        .and_then(|o| o.qualified_id.split_once(':').map(|(s, _)| s.to_string()))
                        .or_else(|| game.id.split_once(':').map(|(s, _)| s.to_string()))
                });

                let version = game
                    .origin
                    .as_ref()
                    .and_then(|o| o.version.clone())
                    .or_else(|| catalog_match.and_then(|cg| cg.version.clone()));

                let genres = catalog_match
                    .map(|cg| cg.genres.clone())
                    .unwrap_or_default();
                let addons_count = catalog_match.map(|cg| cg.addons.len()).unwrap_or(0);

                let dir_bytes = calc_dir_size_cached(state, &game.install_dir);
                let file_size = format_file_size(dir_bytes);

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
                    catalog_match
                        .and_then(|cg| cg.steam_app_id.map(|id| id.to_string()))
                        .or_else(|| {
                            meta_entry
                                .as_ref()
                                .and_then(|m| m.steam_app_id.map(|id| id.to_string()))
                        })
                        .or_else(|| {
                            game.origin
                                .as_ref()
                                .and_then(|o| parse_steam_id(&o.qualified_id))
                        })
                        .or_else(|| parse_steam_id(&game.id))
                });

                let banner_url = game
                    .banner_url
                    .clone()
                    .or_else(|| catalog_match.and_then(|cg| cg.banner_url.clone()))
                    .or_else(|| meta_entry.as_ref().and_then(|m| m.banner_url.clone()));

                let cover_url = game
                    .cover_url
                    .clone()
                    .or_else(|| catalog_match.and_then(|cg| cg.cover_url.clone()))
                    .or_else(|| meta_entry.as_ref().and_then(|m| m.cover_url.clone()));

                if (banner_url.is_none() || cover_url.is_none()) && meta_entry.is_none() {
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
                        if j.effective_install_state().is_some() {
                            "Installing".to_string()
                        } else {
                            "Downloading".to_string()
                        }
                    }
                    s => format!("{:?}", s),
                };

                let cat = j.install.as_ref().and_then(|p| {
                    cached_cat.as_ref().and_then(|cat| {
                        cat.iter().find(|cg| cg.id == p.game_id || cg.title.eq_ignore_ascii_case(&p.title))
                    })
                });
                let estimated_bytes = cat
                    .and_then(|cg| cg.file_size.as_deref())
                    .and_then(parse_size_str_to_bytes);

                let install_json = j.install.as_ref().map(|p| {
                    let meta = state.metadata_cache.lock().ok().and_then(|c| {
                        c.get(&p.title.to_lowercase())
                            .or_else(|| c.get(&p.game_id.to_lowercase()))
                            .cloned()
                    });
                    let steam_id = cat
                        .and_then(|cg| cg.steam_app_id.map(|id| id.to_string()))
                        .or_else(|| meta.as_ref().and_then(|m| m.steam_app_id.map(|id| id.to_string())))
                        .or_else(|| parse_steam_id(&p.game_id));
                    let banner = cat
                        .and_then(|cg| cg.banner_url.clone())
                        .or_else(|| meta.as_ref().and_then(|m| m.banner_url.clone()))
                        .or_else(|| steam_id.as_ref().map(|id| format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{id}/header.jpg")));
                    let cover = cat
                        .and_then(|cg| cg.cover_url.clone())
                        .or_else(|| meta.as_ref().and_then(|m| m.cover_url.clone()))
                        .or_else(|| steam_id.as_ref().map(|id| format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{id}/library_600x900.jpg")));

                    json!({
                        "id": p.game_id,
                        "title": p.title,
                        "kind": format!("{:?}", p.kind),
                        "banner_url": banner,
                        "cover_url": cover,
                        "file_size": cat.and_then(|cg| cg.file_size.clone()),
                    })
                });

                let total_bytes = j.total_bytes.or(estimated_bytes);

                json!({
                    "id": j.id,
                    "kind": format!("{:?}", j.kind),
                    "status": status_str,
                    "bytes": j.downloaded_bytes(),
                    "total_bytes": total_bytes,
                    "total": total_bytes,
                    "estimated_bytes": estimated_bytes,
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

                let cat = cached_cat.as_ref().and_then(|cat| {
                    cat.iter()
                        .find(|cg| cg.id == t.id || cg.title.eq_ignore_ascii_case(&t.title))
                });
                let estimated_bytes = cat
                    .and_then(|cg| cg.file_size.as_deref())
                    .and_then(parse_size_str_to_bytes);
                let total_bytes = estimated_bytes;

                let meta = state.metadata_cache.lock().ok().and_then(|c| {
                    c.get(&t.title.to_lowercase())
                        .or_else(|| c.get(&t.id.to_lowercase()))
                        .cloned()
                });
                let steam_id = cat
                    .and_then(|cg| cg.steam_app_id.map(|id| id.to_string()))
                    .or_else(|| {
                        meta.as_ref()
                            .and_then(|m| m.steam_app_id.map(|id| id.to_string()))
                    })
                    .or_else(|| parse_steam_id(&t.id));
                let banner = cat
                    .and_then(|cg| cg.banner_url.clone())
                    .or_else(|| meta.as_ref().and_then(|m| m.banner_url.clone()))
                    .or_else(|| steam_id.as_ref().map(|id| format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{id}/header.jpg")));
                let cover = cat
                    .and_then(|cg| cg.cover_url.clone())
                    .or_else(|| meta.as_ref().and_then(|m| m.cover_url.clone()))
                    .or_else(|| steam_id.as_ref().map(|id| format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{id}/library_600x900.jpg")));

                jobs_list.push(json!({
                    "id": t.id,
                    "kind": "Torrent",
                    "status": status_str,
                    "bytes": t.downloaded_bytes(&state.data_dir),
                    "total_bytes": total_bytes,
                    "total": total_bytes,
                    "estimated_bytes": estimated_bytes,
                    "target": t.download_dir(&state.data_dir),
                    "error": t.error,
                    "install": Some(json!({
                        "id": t.id,
                        "title": t.title,
                        "kind": "Archive",
                        "banner_url": banner,
                        "cover_url": cover,
                        "file_size": cat.and_then(|cg| cg.file_size.clone()),
                    })),
                    "install_state": t.install_state.map(|s| format!("{:?}", s)),
                    "install_error": None::<String>,
                }));
            }
        }

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

    #[tauri::command]
    pub async fn get_state(state: State<'_, AppState>) -> Result<Value, String> {
        let state = state.inner().clone();
        tauri::async_runtime::spawn_blocking(move || handle_get_state(&state))
            .await
            .map_err(|e| e.to_string())?
    }

    pub fn handle_get_runners(state: &AppState) -> Result<Vec<RunnerEntry>, String> {
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
        Ok(steam_games_to_values(&library, discovered))
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
    ) -> Result<Value, String> {
        validate_id(&id).map_err(|e| e.to_string())?;

        let mut library = load_library(&state.data_dir).map_err(|e| e.to_string())?;
        let game = library
            .games
            .get_mut(&id)
            .ok_or_else(|| format!("Game {id} not found"))?;

        {
            let running = state
                .running_games
                .lock()
                .map_err(|_| "State lock poisoned")?;
            if running.contains_key(&id) {
                return Err(format!("Game '{id}' is already running"));
            }
        }

        let extra = extra_args.unwrap_or_default();
        let mut child = spawn_game(game, &state.data_dir, &extra).map_err(|e| e.to_string())?;
        let pid = child.id();
        let game_title = game.title.clone();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let _ = save_library(&state.data_dir, &library);

        {
            let mut running = state
                .running_games
                .lock()
                .map_err(|_| "State lock poisoned")?;
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
        std::thread::spawn(move || {
            let _ = child.wait();
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
        });

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
    ) -> Result<Value, String> {
        let state = state.inner().clone();
        tauri::async_runtime::spawn_blocking(move || handle_launch_game(&state, id, extra_args))
            .await
            .map_err(|e| e.to_string())?
    }

    #[cfg(unix)]
    fn signal_process_group(pid: u32) -> Result<(), String> {
        if let Ok(status) = Command::new("kill")
            .args(["-TERM", "--"])
            .arg(format!("-{pid}"))
            .status()
            && status.success()
        {
            return Ok(());
        }

        let status = Command::new("kill")
            .args(["-TERM", "--"])
            .arg(pid.to_string())
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            let check = Command::new("kill")
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
    fn signal_process_group(pid: u32) -> Result<(), String> {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("could not stop process group {pid}: {status}"));
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn signal_process_group(_pid: u32) -> Result<(), String> {
        Err("stopping a launch is unsupported on this platform".into())
    }

    pub fn handle_stop_game(state: &AppState, id: String) -> Result<Value, String> {
        validate_id(&id).map_err(|e| e.to_string())?;

        let pid = {
            let running = state
                .running_games
                .lock()
                .map_err(|_| "State lock poisoned")?;
            let info = running
                .get(&id)
                .ok_or_else(|| format!("Game '{id}' is not running"))?;
            info.pid
        };

        signal_process_group(pid).map_err(|e| e.to_string())?;

        if let Ok(mut map) = state.running_games.lock() {
            map.remove(&id);
        }

        Ok(json!({
            "status": "stopped",
            "id": id,
            "pid": pid,
        }))
    }

    #[tauri::command]
    pub async fn stop_game(state: State<'_, AppState>, id: String) -> Result<Value, String> {
        let state = state.inner().clone();
        tauri::async_runtime::spawn_blocking(move || handle_stop_game(&state, id))
            .await
            .map_err(|e| e.to_string())?
    }

    pub fn handle_run_action(state: &AppState, action: ActionRequest) -> Result<Value, String> {
        let data_dir = &state.data_dir;
        let name = action.action.as_str();

        let res = match name {
            "launch" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                handle_launch_game(state, id, action.arguments)
            }
            "stop" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                handle_stop_game(state, id)
            }
            "runner-set" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let runner_val = action.runner.ok_or_else(|| "missing runner".to_string())?;
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get_mut(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                set_runner(game, &runner_val).map_err(|e| e.to_string())?;
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "runner": runner_val }))
            }
            "executable-set" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let exe = action
                    .executable
                    .ok_or_else(|| "missing executable".to_string())?;
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get_mut(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                set_executable(game, Path::new(&exe)).map_err(|e| e.to_string())?;
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "executable": exe }))
            }
            "game-remove" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                azdl::remove_game(&mut library, data_dir, &id).map_err(|e| e.to_string())?;
                if let Ok(mut tasks) = state.tasks.lock() {
                    tasks.retain(|t| {
                        t.id != format!("install-{id}") && t.game_id.as_deref() != Some(&id)
                    });
                }
                if let Ok(mut cache) = state.dir_size_cache.lock() {
                    cache.clear();
                }
                if let Ok(mut cache) = state.steamstub_cache.lock() {
                    cache.clear();
                }
                if let Ok(mut cache) = state.fix_status_cache.lock() {
                    cache.remove(&id);
                }
                Ok(json!({ "success": true, "removed": id }))
            }
            "open-folder" => {
                let target_path = if let Some(p) = action.location {
                    PathBuf::from(p)
                } else if let Some(id) = action.id {
                    let library = load_library(data_dir).map_err(|e| e.to_string())?;
                    let game = library
                        .games
                        .get(&id)
                        .ok_or_else(|| format!("Game {id} not found"))?;
                    game.install_dir.clone()
                } else {
                    return Err("missing game ID or location".to_string());
                };

                if !target_path.exists() {
                    return Err(format!(
                        "Folder path does not exist: {}",
                        target_path.display()
                    ));
                }

                #[cfg(target_os = "windows")]
                {
                    let _ = Command::new("explorer")
                        .arg(&target_path)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = Command::new("xdg-open")
                        .arg(&target_path)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                }

                Ok(json!({ "success": true, "path": target_path.display().to_string() }))
            }
            "game-import" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let title = action.title.ok_or_else(|| "missing title".to_string())?;
                let install_dir = action
                    .install_dir
                    .ok_or_else(|| "missing install_dir".to_string())?;
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
                if let Some(exe) = action.executable.filter(|e| !e.is_empty()) {
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
            "pending-add" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let title = action.title.unwrap_or_else(|| id.clone());
                let install_dir = action
                    .install_dir
                    .ok_or_else(|| "missing install_dir".to_string())?;
                let exe = action
                    .executable
                    .ok_or_else(|| "missing executable".to_string())?;
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                let install_path = Path::new(&install_dir);
                let exe_path = Path::new(&exe);
                let final_install_dir = if action.install_kind.as_deref() == Some("setup")
                    && exe_path.is_absolute()
                    && !exe_path.starts_with(install_path)
                {
                    exe_path.parent().unwrap_or(install_path)
                } else {
                    install_path
                };
                add_game(&mut library, &id, &title, final_install_dir, exe_path)
                    .map_err(|e| e.to_string())?;
                if action.install_kind.as_deref() == Some("setup")
                    && let Some(game) = library.games.get_mut(&id)
                {
                    game.runner = Runner::Wine;
                }
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "added": id }))
            }
            "pending-remove" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                validate_id(&id).map_err(|e| e.to_string())?;

                if let Some(dir_str) = action.install_dir.as_deref() {
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
            "shortcut-create" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
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
            "shortcut-remove" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
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
            "steam-shortcut-create" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                azdl::steam_shortcuts::create_shortcut(game, None).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true }))
            }
            "steam-shortcut-remove" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                azdl::steam_shortcuts::remove_shortcut(game, None).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true }))
            }
            "overlay-toggle" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
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
            "steamstub-unpack" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                let _cli =
                    azdl::steamless::ensure_steamless_cli(data_dir).map_err(|e| e.to_string())?;
                let task_id = format!("steamstub-{id}");
                {
                    let mut tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
                    tasks.retain(|t| t.id != task_id);
                    tasks.push(TaskInfo {
                        id: task_id.clone(),
                        label: format!("Stripping DRM for {}", game.title),
                        status: "running".into(),
                        pid: None,
                        game_id: Some(id.clone()),
                        log_tail: String::new(),
                    });
                }
                let state_clone = state.clone();
                let game_clone = game.clone();
                let data_dir_buf = data_dir.to_path_buf();
                let id_clone = id.clone();
                let task_id_clone = task_id.clone();
                std::thread::Builder::new()
                    .name(format!("azdl-stub-{id}"))
                    .spawn(move || {
                        let _ = azdl::steamless::unpack_game(&game_clone, &data_dir_buf);
                        if let Ok(mut cache) = state_clone.steamstub_cache.lock() {
                            cache.remove(&id_clone);
                        }
                        if let Ok(mut tasks) = state_clone.tasks.lock() {
                            tasks.retain(|t| t.id != task_id_clone);
                        }
                    })
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "started": true, "id": id }))
            }
            "steamstub-rollback" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                azdl::steamless::rollback_game(game).map_err(|e| e.to_string())?;
                if let Ok(mut cache) = state.steamstub_cache.lock() {
                    cache.remove(&id);
                }
                Ok(json!({ "success": true }))
            }
            "ea-heal" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                let msg = azdl::fixes::heal_ea_activation(&game.install_dir)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "message": msg }))
            }
            "ea-rollback" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                let msg = azdl::fixes::rollback_ea_activation(&game.install_dir)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "message": msg }))
            }
            "job-pause" => {
                let id = action.id.ok_or_else(|| "missing job ID".to_string())?;
                if azdl::jobs::load_job(data_dir, &id).is_ok() {
                    azdl::jobs::pause_job(data_dir, &id).map_err(|e| e.to_string())?;
                } else if azdl::torrent::load_torrent_job(data_dir, &id).is_ok() {
                    azdl::torrent::pause_magnet(data_dir, &id).map_err(|e| e.to_string())?;
                } else {
                    return Err(format!("Job {id} not found"));
                }
                Ok(json!({ "success": true }))
            }
            "job-resume" => {
                let id = action.id.ok_or_else(|| "missing job ID".to_string())?;
                if azdl::jobs::load_job(data_dir, &id).is_ok() {
                    azdl::jobs::retry_job(data_dir, &id).map_err(|e| e.to_string())?;
                } else if azdl::torrent::load_torrent_job(data_dir, &id).is_ok() {
                    let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                    azdl::torrent::resume_magnet(&mut library, data_dir, &id)
                        .map_err(|e| e.to_string())?;
                } else {
                    return Err(format!("Job {id} not found"));
                }
                Ok(json!({ "success": true }))
            }
            "job-cancel" => {
                let id = action.id.ok_or_else(|| "missing job ID".to_string())?;
                if azdl::jobs::load_job(data_dir, &id).is_ok() {
                    azdl::jobs::cancel_job(data_dir, &id).map_err(|e| e.to_string())?;
                } else if azdl::torrent::load_torrent_job(data_dir, &id).is_ok() {
                    azdl::torrent::cancel_magnet(data_dir, &id).map_err(|e| e.to_string())?;
                } else {
                    return Err(format!("Job {id} not found"));
                }
                if let Ok(mut tasks) = state.tasks.lock() {
                    tasks.retain(|t| t.id != id && t.game_id.as_deref() != Some(&id));
                }
                Ok(json!({ "success": true }))
            }
            "args-set" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get_mut(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                game.arguments = action.arguments.unwrap_or_default();
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true }))
            }
            "env-set" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let key = action.key.ok_or_else(|| "missing env key".to_string())?;
                let val = action.value.unwrap_or_default();
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get_mut(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                game.environment.insert(key, val);
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true }))
            }
            "env-remove" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let key = action.key.ok_or_else(|| "missing env key".to_string())?;
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get_mut(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                game.environment.remove(&key);
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true }))
            }
            "option-add" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let opt_id = action
                    .option_id
                    .ok_or_else(|| "missing option_id".to_string())?;
                let title = action.title.ok_or_else(|| "missing title".to_string())?;
                let exe = action
                    .executable
                    .ok_or_else(|| "missing executable".to_string())?;
                let working = action.working_directory.map(PathBuf::from);
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get_mut(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                add_launch_option(
                    game,
                    &opt_id,
                    &title,
                    Path::new(&exe),
                    working.as_deref(),
                    &action.arguments.unwrap_or_default(),
                )
                .map_err(|e| e.to_string())?;
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true }))
            }
            "option-select" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let opt_id = action
                    .option_id
                    .ok_or_else(|| "missing option_id".to_string())?;
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get_mut(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                select_launch_option(game, Some(&opt_id)).map_err(|e| e.to_string())?;
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true }))
            }
            "option-remove" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let opt_id = action
                    .option_id
                    .ok_or_else(|| "missing option_id".to_string())?;
                let mut library = load_library(data_dir).map_err(|e| e.to_string())?;
                let game = library
                    .games
                    .get_mut(&id)
                    .ok_or_else(|| format!("Game {id} not found"))?;
                remove_launch_option(game, &opt_id).map_err(|e| e.to_string())?;
                save_library(data_dir, &library).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true }))
            }
            "steam-install" => {
                let id = action.id.ok_or_else(|| "missing catalog ID".to_string())?;
                let plugin_id = action.plugin_id.clone();
                let request_title = action.title.clone();
                let cat_game = resolve_install_game(
                    data_dir,
                    &id,
                    plugin_id.as_deref(),
                    request_title.as_deref(),
                    action.banner_url.as_deref(),
                    action.cover_url.as_deref(),
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

                let is_test_env = cfg!(test) || std::env::var("AZDL_TEST_IN_PROCESS").is_ok();
                let mut spawned_pid: Option<u32> = None;

                if !is_test_env && let Ok(current_exe) = std::env::current_exe() {
                    match std::process::Command::new(&current_exe)
                        .arg("worker-install")
                        .arg(&qualified_id)
                        .arg(plugin_id.as_deref().unwrap_or(""))
                        .arg(request_title.as_deref().unwrap_or(""))
                        .arg(cat_game.banner_url.as_deref().unwrap_or(""))
                        .arg(cat_game.cover_url.as_deref().unwrap_or(""))
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
                                    if let Ok(mut lock) = wait_state.cached_catalog.lock() {
                                        *lock = None;
                                    }
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

                            let outcome = azdl::catalog::install_game(
                                &mut library,
                                data_dir,
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
                                    if let Ok(mut lock) = state_clone.cached_catalog.lock() {
                                        *lock = None;
                                    }
                                }
                                Ok(azdl::catalog::InstallOutcome::NeedsSelection(_)) => {
                                    let _ = save_library(data_dir, &library);
                                    if let Ok(mut lock) = state_clone.cached_catalog.lock() {
                                        *lock = None;
                                    }
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
            "patch-toggle" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
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
            "patch-rollback" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
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
            "plugin-install" => {
                let loc = action
                    .location
                    .ok_or_else(|| "missing location".to_string())?;
                let manifest = azdl::plugins::install_bundle(data_dir, Path::new(&loc))
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "plugin": manifest.id }))
            }
            "plugin-remove" => {
                let id = action.id.ok_or_else(|| "missing plugin ID".to_string())?;
                azdl::plugins::remove_plugin(data_dir, &id).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "removed": id }))
            }
            "plugin-store-refresh" => {
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
            "plugin-store-add" => {
                let loc = action
                    .location
                    .ok_or_else(|| "missing store location/URL".to_string())?;
                let count =
                    azdl::plugin_store::add_store(data_dir, &loc).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "store": loc, "count": count }))
            }
            "plugin-store-install" => {
                let id = action.id.ok_or_else(|| "missing plugin ID".to_string())?;
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
                        if azdl::plugin_store::install_store_plugin(data_dir, &id_clone).is_ok() {
                            let _ = azdl::plugins::refresh_plugin_catalog(data_dir, &id_clone);
                            if let Ok(mut lock) = state_clone.cached_catalog.lock() {
                                *lock = None;
                            }
                        }
                        if let Ok(mut tasks) = state_clone.tasks.lock() {
                            tasks.retain(|t| t.id != task_id_clone);
                        }
                    })
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "started": true, "plugin": id }))
            }
            "plugin-create" => {
                let id = action.id.ok_or_else(|| "missing plugin ID".to_string())?;
                azdl::validate_id(&id).map_err(|e| e.to_string())?;
                let name = action.plugin_name.unwrap_or_else(|| id.clone());
                let version = action.version.unwrap_or_else(|| "1.0.0".to_string());
                let catalog_url = action.catalog_url.or(action.location);
                let plugins_dir = data_dir.join("plugins").join(&id);
                fs::create_dir_all(&plugins_dir).map_err(|e| e.to_string())?;
                let manifest = json!({
                    "id": id,
                    "name": name,
                    "version": version,
                    "apiVersion": 4,
                    "library": "",
                    "catalogUrl": catalog_url
                });
                let manifest_path = plugins_dir.join("plugin.json");
                fs::write(
                    &manifest_path,
                    serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                if let Some(ref _url) = catalog_url {
                    let _ = azdl::plugins::refresh_plugin_catalog(data_dir, &id);
                }
                Ok(json!({ "success": true, "plugin": id, "manifest": manifest }))
            }
            "settings-update" => {
                let settings = action
                    .settings
                    .ok_or_else(|| "missing settings payload".to_string())?;
                azdl::settings::save_settings(data_dir, &settings).map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "settings": settings }))
            }
            "storage-add-drive" => {
                let path_str = action
                    .drive_path
                    .or(action.location)
                    .or(action.install_dir)
                    .ok_or_else(|| "missing drive path".to_string())?;
                let label = action.drive_label.or(action.title).unwrap_or_default();
                let drive =
                    azdl::settings::add_storage_drive(data_dir, &label, Path::new(&path_str))
                        .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "drive": drive }))
            }
            "storage-remove-drive" => {
                let drive_id = action
                    .drive_id
                    .or(action.id)
                    .ok_or_else(|| "missing drive id".to_string())?;
                azdl::settings::remove_storage_drive(data_dir, &drive_id)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "removed": drive_id }))
            }
            "storage-set-default" => {
                let drive_id = action
                    .drive_id
                    .or(action.id)
                    .ok_or_else(|| "missing drive id".to_string())?;
                azdl::settings::set_default_drive(data_dir, &drive_id)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "default": drive_id }))
            }
            "storage-move-game" => {
                let id = action.id.ok_or_else(|| "missing game ID".to_string())?;
                let drive_id = action
                    .drive_id
                    .ok_or_else(|| "missing target drive ID".to_string())?;
                let task_id = format!("move-{id}");
                {
                    let mut tasks = state.tasks.lock().map_err(|_| "State lock poisoned")?;
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
                let state_clone = state.clone();
                let id_clone = id.clone();
                let drive_id_clone = drive_id.clone();
                let task_id_clone = task_id.clone();
                std::thread::Builder::new()
                    .name(format!("azdl-move-{id}"))
                    .spawn(move || {
                        let data_dir = &state_clone.data_dir;
                        if let Ok(mut library) = load_library(data_dir)
                            && let Ok(_new_path) = azdl::settings::move_game_to_drive(
                                data_dir,
                                &mut library,
                                &id_clone,
                                &drive_id_clone,
                            )
                        {
                            let _ = save_library(data_dir, &library);
                        }
                        if let Ok(mut cache) = state_clone.dir_size_cache.lock() {
                            cache.clear();
                        }
                        if let Ok(mut tasks) = state_clone.tasks.lock() {
                            tasks.retain(|t| t.id != task_id_clone);
                        }
                    })
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "started": true, "id": id }))
            }
            "scan-installed-games" => {
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
                        if let Ok(mut library) = load_library(data_dir) {
                            let _ = azdl::settings::scan_drives_for_games(data_dir, &mut library);
                        }
                        if let Ok(mut tasks) = state_clone.tasks.lock() {
                            tasks.retain(|t| t.id != task_id_clone);
                        }
                    })
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "started": true }))
            }
            "proton-download" => {
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
                        let _ = azdl::runtime::download_proton_ge(&state_clone.data_dir);
                        if let Ok(mut tasks) = state_clone.tasks.lock() {
                            tasks.retain(|t| t.id != task_id_clone);
                        }
                    })
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "success": true, "started": true }))
            }
            "steam-shortcuts-sync" => {
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
            "job-clear" => {
                let id = action.id.ok_or_else(|| "missing job ID".to_string())?;
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
                    Err(format!("Job {id} not found or could not be cleared"))
                }
            }
            "job-clear-completed" => {
                let mut count =
                    azdl::jobs::clear_completed_jobs(data_dir).map_err(|e| e.to_string())?;
                if let Ok(jobs) = azdl::jobs::list_jobs(data_dir) {
                    for j in jobs {
                        if matches!(
                            j.effective_status(),
                            azdl::jobs::JobStatus::Complete
                                | azdl::jobs::JobStatus::Cancelled
                                | azdl::jobs::JobStatus::Failed
                        ) && azdl::jobs::clear_job(data_dir, &j.id).is_ok()
                        {
                            count += 1;
                        }
                    }
                }
                if let Ok(torrents) = azdl::torrent::list_torrent_jobs(data_dir) {
                    for t in torrents {
                        if matches!(
                            t.status,
                            azdl::jobs::JobStatus::Complete
                                | azdl::jobs::JobStatus::Cancelled
                                | azdl::jobs::JobStatus::Failed
                        ) {
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
            _ => Err(format!("Unimplemented or unknown action: {name}")),
        };
        if res.is_ok()
            && matches!(
                name,
                "plugin-install"
                    | "plugin-store-add"
                    | "plugin-store-refresh"
                    | "plugin-store-install"
                    | "plugin-remove"
                    | "storage-add-drive"
                    | "storage-remove-drive"
                    | "scan-installed-games"
            )
            && let Ok(mut lock) = state.cached_catalog.lock()
        {
            *lock = None;
        }
        res
    }

    #[tauri::command]
    pub async fn run_action(
        state: State<'_, AppState>,
        action: ActionRequest,
    ) -> Result<Value, String> {
        let state = state.inner().clone();
        tauri::async_runtime::spawn_blocking(move || handle_run_action(&state, action))
            .await
            .map_err(|e| e.to_string())?
    }

    #[tauri::command]
    pub fn set_window_translucency(window: Window, effect: String) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            use tauri::window::{Effect, EffectState, EffectsBuilder};
            let eff = match effect.as_str() {
                "mica" => Some(Effect::Mica),
                "acrylic" => Some(Effect::Acrylic),
                "tabbed" => Some(Effect::Tabbed),
                _ => None,
            };
            if let Some(e) = eff {
                let effects = EffectsBuilder::new()
                    .effect(e)
                    .state(EffectState::Active)
                    .build();
                let _ = window.set_effects(effects);
            } else {
                let effects = EffectsBuilder::new().state(EffectState::Disabled).build();
                let _ = window.set_effects(effects);
            }
        }
        let _ = (window, effect);
        Ok(())
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
                "name"
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
        let output = Command::new("curl")
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
            return Err(format!("fetch failed with status {}", output.status));
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
    pub fn is_steam_running() -> bool {
        azdl::multiplayer::steam_running()
    }

    #[tauri::command]
    pub fn start_steam() -> Result<bool, String> {
        azdl::steam::launch_steam().map_err(|e| e.to_string())?;
        Ok(true)
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
    let exec_str = current_exe.to_string_lossy();

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

    let apps_dir = data_home.join("applications");
    let _ = std::fs::create_dir_all(&apps_dir);
    let app_desktop_files = [
        "azdl-desktop.desktop",
        "azdl.desktop",
        "io.azdl.launcher.desktop",
    ];
    for file_name in app_desktop_files {
        let target = apps_dir.join(file_name);
        let _ = std::fs::write(&target, &desktop_content);
    }

    let desktop_dir = std::process::Command::new("xdg-user-dir")
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

    let _ = std::process::Command::new("update-desktop-database")
        .arg(&apps_dir)
        .status();

    let _ = std::process::Command::new("kbuildsycoca6").status();

    let _ = std::process::Command::new("gtk-update-icon-cache")
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
    let outcome = azdl::catalog::install_game(&mut library, &data_dir, &cat_game);
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
        );
        return;
    }

    #[cfg(target_os = "linux")]
    ensure_webkit_environment();

    let data_dir = data_dir().expect("Failed to resolve azdl data directory");
    let metadata_cache = load_metadata_cache(&data_dir);
    let state = AppState {
        data_dir,
        running_games: Arc::new(Mutex::new(HashMap::new())),
        tasks: Arc::new(Mutex::new(Vec::new())),
        mutating: Arc::new(AtomicBool::new(false)),
        cached_catalog: Arc::new(Mutex::new(None)),
        cached_diagnostics: Arc::new(Mutex::new(None)),
        library_cache: Arc::new(Mutex::new(None)),
        dir_size_cache: Arc::new(Mutex::new(HashMap::new())),
        steamstub_cache: Arc::new(Mutex::new(HashMap::new())),
        fix_status_cache: Arc::new(Mutex::new(HashMap::new())),
        metadata_cache: Arc::new(Mutex::new(metadata_cache)),
        metadata_pending: Arc::new(Mutex::new(HashSet::new())),
    };

    tauri::Builder::default()
        .manage(state)
        .setup(|app| {
            #[cfg(target_os = "linux")]
            ensure_desktop_integration();

            use tauri::Manager;
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
            commands::get_runners,
            commands::get_steam_details,
            commands::search_steam_games,
            commands::resolve_game_artwork,
            commands::diagnose,
            commands::discover_exes,
            commands::scan_game_setup,
            commands::launch_game,
            commands::stop_game,
            commands::run_action,
            commands::set_window_translucency,
            commands::window_minimize,
            commands::window_toggle_maximize,
            commands::window_close,
            commands::is_steam_running,
            commands::start_steam,
        ])
        .run(tauri::generate_context!())
        .expect("error while running azdl tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

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
        path
    }

    fn test_state(root: &Path) -> AppState {
        AppState {
            data_dir: root.to_path_buf(),
            running_games: Arc::new(Mutex::new(HashMap::new())),
            tasks: Arc::new(Mutex::new(Vec::new())),
            mutating: Arc::new(AtomicBool::new(false)),
            cached_catalog: Arc::new(Mutex::new(None)),
            cached_diagnostics: Arc::new(Mutex::new(None)),
            library_cache: Arc::new(Mutex::new(None)),
            dir_size_cache: Arc::new(Mutex::new(HashMap::new())),
            steamstub_cache: Arc::new(Mutex::new(HashMap::new())),
            fix_status_cache: Arc::new(Mutex::new(HashMap::new())),
            metadata_cache: Arc::new(Mutex::new(HashMap::new())),
            metadata_pending: Arc::new(Mutex::new(HashSet::new())),
        }
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
        assert_eq!(req.action, "steam-install");
        assert_eq!(req.id.as_deref(), Some("steam:620"));
        assert_eq!(
            req.banner_url.as_deref(),
            Some("https://cdn.example/header.jpg")
        );
        assert_eq!(
            req.cover_url.as_deref(),
            Some("https://cdn.example/cover.jpg")
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
        let import_req = ActionRequest {
            action: "game-import".into(),
            id: Some("test-game".into()),
            title: Some("My Test Game".into()),
            install_dir: Some(game_dir.to_string_lossy().into()),
            executable: Some("game.bin".into()),
            destination: None,
            sha256: None,
            install_kind: None,
            patch_kind: None,
            patch_root: None,
            location: None,
            addon_id: None,
            option_id: None,
            working_directory: None,
            runner: None,
            arguments: None,
            key: None,
            value: None,
            ..Default::default()
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
        let remove_req = ActionRequest {
            action: "game-remove".into(),
            id: Some("test-game".into()),
            title: None,
            install_dir: None,
            executable: None,
            destination: None,
            sha256: None,
            install_kind: None,
            patch_kind: None,
            patch_root: None,
            location: None,
            addon_id: None,
            option_id: None,
            working_directory: None,
            runner: None,
            arguments: None,
            key: None,
            value: None,
            ..Default::default()
        };
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

        let import_req = ActionRequest {
            action: "game-import".into(),
            id: Some("runner-game".into()),
            title: Some("Runner Game".into()),
            install_dir: Some(game_dir.to_string_lossy().into()),
            executable: Some("game.exe".into()),
            destination: None,
            sha256: None,
            install_kind: None,
            patch_kind: None,
            patch_root: None,
            location: None,
            addon_id: None,
            option_id: None,
            working_directory: None,
            runner: None,
            arguments: None,
            key: None,
            value: None,
            ..Default::default()
        };
        handle_run_action(&state, import_req).unwrap();

        // Change runner to wine
        let runner_req = ActionRequest {
            action: "runner-set".into(),
            id: Some("runner-game".into()),
            runner: Some("wine".into()),
            title: None,
            install_dir: None,
            executable: None,
            destination: None,
            sha256: None,
            install_kind: None,
            patch_kind: None,
            patch_root: None,
            location: None,
            addon_id: None,
            option_id: None,
            working_directory: None,
            arguments: None,
            key: None,
            value: None,
            ..Default::default()
        };
        handle_run_action(&state, runner_req).unwrap();

        // Change executable to game2.exe
        let exe_req = ActionRequest {
            action: "executable-set".into(),
            id: Some("runner-game".into()),
            executable: Some("game2.exe".into()),
            title: None,
            install_dir: None,
            destination: None,
            sha256: None,
            install_kind: None,
            patch_kind: None,
            patch_root: None,
            location: None,
            addon_id: None,
            option_id: None,
            working_directory: None,
            runner: None,
            arguments: None,
            key: None,
            value: None,
            ..Default::default()
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
            ActionRequest {
                action: "game-import".into(),
                id: Some("args-env-game".into()),
                title: Some("Args Env Game".into()),
                install_dir: Some(game_dir.to_string_lossy().into()),
                executable: Some("main.bin".into()),
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                option_id: None,
                working_directory: None,
                runner: None,
                arguments: None,
                key: None,
                value: None,
                ..Default::default()
            },
        )
        .unwrap();

        // Set launch arguments
        handle_run_action(
            &state,
            ActionRequest {
                action: "args-set".into(),
                id: Some("args-env-game".into()),
                arguments: Some(vec!["--fullscreen".into(), "-v".into()]),
                title: None,
                install_dir: None,
                executable: None,
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                option_id: None,
                working_directory: None,
                runner: None,
                key: None,
                value: None,
                ..Default::default()
            },
        )
        .unwrap();

        // Set env variable
        handle_run_action(
            &state,
            ActionRequest {
                action: "env-set".into(),
                id: Some("args-env-game".into()),
                key: Some("DXVK_HUD".into()),
                value: Some("fps".into()),
                title: None,
                install_dir: None,
                executable: None,
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                option_id: None,
                working_directory: None,
                runner: None,
                arguments: None,
                ..Default::default()
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
            ActionRequest {
                action: "env-remove".into(),
                id: Some("args-env-game".into()),
                key: Some("DXVK_HUD".into()),
                title: None,
                install_dir: None,
                executable: None,
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                option_id: None,
                working_directory: None,
                runner: None,
                arguments: None,
                value: None,
                ..Default::default()
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
            ActionRequest {
                action: "game-import".into(),
                id: Some("opt-game".into()),
                title: Some("Option Game".into()),
                install_dir: Some(game_dir.to_string_lossy().into()),
                executable: Some("game.exe".into()),
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                option_id: None,
                working_directory: None,
                runner: None,
                arguments: None,
                key: None,
                value: None,
                ..Default::default()
            },
        )
        .unwrap();

        // Add launch option
        handle_run_action(
            &state,
            ActionRequest {
                action: "option-add".into(),
                id: Some("opt-game".into()),
                option_id: Some("mp".into()),
                title: Some("Multiplayer".into()),
                executable: Some("mp.exe".into()),
                working_directory: None,
                arguments: Some(vec!["--mp".into()]),
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                runner: None,
                key: None,
                value: None,
                install_dir: None,
                ..Default::default()
            },
        )
        .unwrap();

        // Select launch option
        handle_run_action(
            &state,
            ActionRequest {
                action: "option-select".into(),
                id: Some("opt-game".into()),
                option_id: Some("mp".into()),
                title: None,
                executable: None,
                working_directory: None,
                arguments: None,
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                runner: None,
                key: None,
                value: None,
                install_dir: None,
                ..Default::default()
            },
        )
        .unwrap();

        let s = handle_get_state(&state).unwrap();
        assert_eq!(s["library"][0]["selected_launch_option_id"], "mp");

        // Remove launch option
        handle_run_action(
            &state,
            ActionRequest {
                action: "option-remove".into(),
                id: Some("opt-game".into()),
                option_id: Some("mp".into()),
                title: None,
                executable: None,
                working_directory: None,
                arguments: None,
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                runner: None,
                key: None,
                value: None,
                install_dir: None,
                ..Default::default()
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
            ActionRequest {
                action: "pending-add".into(),
                id: Some("unregistered".into()),
                title: None, // defaults to "unregistered"
                install_dir: Some(pending_dir.to_string_lossy().into()),
                executable: Some("app.exe".into()),
                ..Default::default()
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
            ActionRequest {
                action: "pending-remove".into(),
                id: Some("to-delete".into()),
                install_dir: Some(pending_dir.to_string_lossy().into()),
                ..Default::default()
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
            ActionRequest {
                action: "game-import".into(),
                id: Some("diag-game".into()),
                title: Some("Diag Game".into()),
                install_dir: Some(game_dir.to_string_lossy().into()),
                executable: Some("main.exe".into()),
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                option_id: None,
                working_directory: None,
                runner: None,
                arguments: None,
                key: None,
                value: None,
                ..Default::default()
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
        assert_eq!(card["file_size"], "4 B");
        assert_eq!(card["size_bytes"], 4);
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
            ActionRequest {
                action: "game-import".into(),
                id: Some("fix-game".into()),
                title: Some("Fix Game".into()),
                install_dir: Some(game_dir.to_string_lossy().into()),
                executable: Some("game.exe".into()),
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                option_id: None,
                working_directory: None,
                runner: None,
                arguments: None,
                key: None,
                value: None,
                ..Default::default()
            },
        )
        .unwrap();

        // Toggle overlay: disables OnlineFix64.dll -> OnlineFix64.dll.arachnel-off
        let res = handle_run_action(
            &state,
            ActionRequest {
                action: "overlay-toggle".into(),
                id: Some("fix-game".into()),
                title: None,
                install_dir: None,
                executable: None,
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                option_id: None,
                working_directory: None,
                runner: None,
                arguments: None,
                key: None,
                value: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(res["enabled"], false);
        assert!(game_dir.join("OnlineFix64.dll.arachnel-off").exists());

        // Toggle back: re-enables
        let res2 = handle_run_action(
            &state,
            ActionRequest {
                action: "overlay-toggle".into(),
                id: Some("fix-game".into()),
                title: None,
                install_dir: None,
                executable: None,
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                option_id: None,
                working_directory: None,
                runner: None,
                arguments: None,
                key: None,
                value: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(res2["enabled"], true);
        assert!(game_dir.join("OnlineFix64.dll").exists());
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
        fs::write(&exe_path, b"@echo off\ntimeout /t 30\n").unwrap();

        handle_run_action(
            &state,
            ActionRequest {
                action: "game-import".into(),
                id: Some("proc-game".into()),
                title: Some("Proc Game".into()),
                install_dir: Some(game_dir.to_string_lossy().into()),
                executable: Some(exe_name.into()),
                destination: None,
                sha256: None,
                install_kind: None,
                patch_kind: None,
                patch_root: None,
                location: None,
                addon_id: None,
                option_id: None,
                working_directory: None,
                runner: None,
                arguments: None,
                key: None,
                value: None,
                ..Default::default()
            },
        )
        .unwrap();

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
    fn test_handle_storage_and_settings_actions() {
        let root = scratch();
        let state = test_state(&root);
        let custom_drive = root.join("custom-drive");

        // 1. Add storage drive
        let add_res = handle_run_action(
            &state,
            ActionRequest {
                action: "storage-add-drive".into(),
                drive_label: Some("External SSD".into()),
                drive_path: Some(custom_drive.to_string_lossy().into()),
                ..Default::default()
            },
        )
        .unwrap();
        let drive_id = add_res["drive"]["id"].as_str().unwrap().to_string();

        // 2. Set default drive
        handle_run_action(
            &state,
            ActionRequest {
                action: "storage-set-default".into(),
                drive_id: Some(drive_id.clone()),
                ..Default::default()
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
            ActionRequest {
                action: "settings-update".into(),
                settings: Some(new_settings),
                ..Default::default()
            },
        )
        .unwrap();

        let s2 = handle_get_state(&state).unwrap();
        assert_eq!(s2["settings"]["seed_color"], "#FF00FF");
        assert_eq!(s2["settings"]["theme_mode"], "amoled");

        // 4. Remove storage drive
        handle_run_action(
            &state,
            ActionRequest {
                action: "storage-remove-drive".into(),
                drive_id: Some(drive_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        let s3 = handle_get_state(&state).unwrap();
        let drives3 = s3["settings"]["storage_drives"].as_array().unwrap();
        assert!(!drives3.iter().any(|d| d["id"] == drive_id));
    }

    #[test]
    fn test_legacy_source_actions_are_not_exposed() {
        let root = scratch();
        let state = test_state(&root);
        let result = handle_run_action(
            &state,
            ActionRequest {
                action: "source-add".into(),
                id: Some("old-feed".into()),
                location: Some("/tmp/old-feed.json".into()),
                ..Default::default()
            },
        );
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
            ActionRequest {
                action: "open-folder".into(),
                location: Some(game_dir.display().to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(res["success"], true);

        // 2. Missing path returns error
        let err = handle_run_action(
            &state,
            ActionRequest {
                action: "open-folder".into(),
                location: Some(temp.join("NonExistent").display().to_string()),
                ..Default::default()
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

        // Clear completed/failed transfers
        let clear_res = handle_run_action(
            &state,
            ActionRequest {
                action: "job-clear-completed".into(),
                ..Default::default()
            },
        )
        .unwrap();
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
            ActionRequest {
                action: "steam-install".into(),
                id: Some("steam:620".into()),
                title: Some("Portal 2".into()),
                plugin_id: Some("steamidra".into()),
                ..Default::default()
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
            ActionRequest {
                action: "steam-install".into(),
                id: Some("steam:480".into()),
                title: Some("Spacewar".into()),
                plugin_id: Some("steamidra".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(duplicate["already_working"], true);
    }
}
