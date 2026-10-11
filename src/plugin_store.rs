use crate::jobs::{JobKind, recorded_download_with_sha256};
use crate::plugins::{PluginManifest, install_bundle_expected, list_plugins};
use crate::{Game, Result, Runner, clean_command, validate_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

const DEFAULT_STORE_URL: &str =
    "https://gitlab.com/BadKiko/arachnel-plugins-sourcelist/-/raw/main/plugins.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorePlugin {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub filename: String,
    pub recommended: bool,
    pub repository: Option<String>,
    pub size: Option<u64>,
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn compatible_platform(build: &Value) -> bool {
    let Some(platforms) = build.get("platforms").and_then(Value::as_array) else {
        return true;
    };
    if platforms.is_empty() {
        return true;
    }
    platforms.iter().filter_map(Value::as_str).any(|platform| {
        platform.eq_ignore_ascii_case("all") || platform.eq_ignore_ascii_case(std::env::consts::OS)
    })
}

fn version_parts(version: &str) -> (Vec<u64>, String) {
    let value = version.trim().trim_start_matches(['v', 'V']);
    let numeric = value
        .bytes()
        .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
        .count();
    let numbers = value[..numeric]
        .split('.')
        .filter_map(|part| part.parse::<u64>().ok())
        .collect();
    (numbers, value[numeric..].to_ascii_lowercase())
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let (left_numbers, left_suffix) = version_parts(left);
    let (right_numbers, right_suffix) = version_parts(right);
    for index in 0..left_numbers.len().max(right_numbers.len()) {
        let compared = left_numbers
            .get(index)
            .unwrap_or(&0)
            .cmp(right_numbers.get(index).unwrap_or(&0));
        if compared != Ordering::Equal {
            return compared;
        }
    }
    left_suffix.cmp(&right_suffix)
}

fn candidate(plugin: &Value, build: &Value) -> Option<StorePlugin> {
    if build.get("apiVersion")?.as_u64()? != 4 || !compatible_platform(build) {
        return None;
    }
    let id = text(plugin, "id")?;
    validate_id(id).ok()?;
    let url = text(build, "url")?;
    if !url.starts_with("https://") {
        return None;
    }
    let sha256 = text(build, "sha256")?.to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let filename = text(build, "file")
        .or_else(|| text(plugin, "file"))
        .map_or_else(|| format!("{id}.arach"), str::to_string);
    let path = Path::new(&filename);
    if path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || path
            .extension()
            .is_none_or(|extension| extension != "arach")
    {
        return None;
    }
    Some(StorePlugin {
        id: id.into(),
        name: text(plugin, "name").unwrap_or(id).into(),
        description: text(plugin, "description").unwrap_or_default().into(),
        version: text(build, "version")?.into(),
        url: url.into(),
        sha256,
        filename,
        recommended: plugin
            .get("recommended")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        repository: text(plugin, "repository").map(str::to_string),
        size: build.get("size").and_then(Value::as_u64),
    })
}

pub fn parse_store(bytes: &[u8]) -> Result<Vec<StorePlugin>> {
    if let Ok(entries) = serde_json::from_slice::<Vec<StorePlugin>>(bytes) {
        return Ok(entries);
    }
    let root: Value = serde_json::from_slice(bytes)?;
    let plugins = root
        .get("plugins")
        .and_then(Value::as_array)
        .ok_or("plugin store document has no plugins array")?;
    let mut selected = BTreeMap::new();
    for plugin in plugins {
        let Some(id) = text(plugin, "id") else {
            continue;
        };
        let builds = plugin.get("builds").and_then(Value::as_array);
        let choices: Vec<&Value> = match builds {
            Some(builds) if !builds.is_empty() => builds.iter().collect(),
            _ => vec![plugin],
        };
        for build in choices {
            let Some(entry) = candidate(plugin, build) else {
                continue;
            };
            let replace = selected.get(id).is_none_or(|existing: &StorePlugin| {
                compare_versions(&entry.version, &existing.version) == Ordering::Greater
            });
            if replace {
                selected.insert(id.to_string(), entry);
            }
        }
    }
    let mut entries: Vec<StorePlugin> = selected.into_values().collect();
    entries.sort_by(|a, b| {
        b.recommended
            .cmp(&a.recommended)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("plugin-store.json")
}

pub fn cached_store(data_dir: &Path) -> Result<Vec<StorePlugin>> {
    match fs::read(cache_path(data_dir)) {
        Ok(bytes) => {
            if let Ok(entries) = serde_json::from_slice::<Vec<StorePlugin>>(&bytes) {
                return Ok(entries);
            }
            parse_store(&bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn fetch_store_url(url: &str) -> Result<Vec<u8>> {
    if !url.starts_with("https://") {
        return Err("plugin store URL must use HTTPS".into());
    }
    let output = clean_command("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "30",
            "--max-filesize",
            "5242880",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
        ])
        .arg(url)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "plugin store fetch failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    if output.stdout.len() > 5 * 1024 * 1024 {
        return Err("plugin store document is too large".into());
    }
    Ok(output.stdout)
}

pub fn add_store(data_dir: &Path, url: &str) -> Result<usize> {
    if !url.starts_with("https://") {
        return Err("plugin store URL must use HTTPS".into());
    }
    let mut settings = crate::settings::load_settings(data_dir)
        .unwrap_or_else(|_| crate::settings::LauncherSettings::default_for(data_dir));
    if !settings.plugin_store_urls.iter().any(|u| u == url) {
        settings.plugin_store_urls.push(url.to_string());
        crate::settings::save_settings(data_dir, &settings)?;
    }
    let entries = refresh_store(data_dir)?;
    Ok(entries.len())
}

pub fn refresh_store(data_dir: &Path) -> Result<Vec<StorePlugin>> {
    let settings = crate::settings::load_settings(data_dir).ok();
    let mut urls = settings.map(|s| s.plugin_store_urls).unwrap_or_default();
    if urls.is_empty() {
        let default_url =
            std::env::var("AZDL_PLUGIN_STORE_URL").unwrap_or_else(|_| DEFAULT_STORE_URL.into());
        urls.push(default_url);
    }
    let mut all_entries = Vec::new();
    let mut any_fetched = false;
    for url in &urls {
        if let Ok(bytes) = fetch_store_url(url)
            && let Ok(entries) = parse_store(&bytes)
        {
            any_fetched = true;
            for entry in entries {
                if !all_entries.iter().any(|e: &StorePlugin| e.id == entry.id) {
                    all_entries.push(entry);
                }
            }
        }
    }
    if !any_fetched && !urls.is_empty() {
        let bytes = fetch_store_url(&urls[0])?;
        all_entries = parse_store(&bytes)?;
    }
    fs::create_dir_all(data_dir)?;
    let target = cache_path(data_dir);
    let temp = target.with_extension("json.tmp");
    let json_bytes = serde_json::to_vec_pretty(&all_entries)?;
    fs::write(&temp, json_bytes)?;
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(&target)?;
    }
    fs::rename(temp, target)?;
    Ok(all_entries)
}

pub fn install_store_plugin(data_dir: &Path, id: &str) -> Result<PluginManifest> {
    validate_id(id)?;
    if list_plugins(data_dir)?.iter().any(|plugin| plugin.id == id) {
        return Err("plugin is already installed".into());
    }
    let entry = cached_store(data_dir)?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or("plugin is not in the cached API 4 store; refresh the store first")?;
    let downloads = data_dir.join("plugin-downloads").join(id);
    fs::create_dir_all(&downloads)?;
    let game = Game {
        id: id.into(),
        title: entry.name.clone(),
        install_dir: fs::canonicalize(&downloads)?,
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
    let archive = recorded_download_with_sha256(
        data_dir,
        &game,
        &entry.url,
        Path::new(&entry.filename),
        JobKind::PluginPackage,
        Some(&entry.sha256),
    )?;
    let result = install_bundle_expected(data_dir, &archive, id);
    let job_id = crate::jobs::job_id_for_target(&archive);
    let _ = crate::jobs::clear_job(data_dir, &job_id);
    let _ = fs::remove_dir_all(&downloads);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_latest_api4_build_for_current_platform() {
        let platform = std::env::consts::OS;
        let json = format!(
            r#"{{"plugins":[{{"id":"source","name":"Source","recommended":true,"builds":[
              {{"version":"1.2.0","apiVersion":2,"url":"https://example.org/old.arach","sha256":"{hash}"}},
              {{"version":"1.3.0","apiVersion":4,"platforms":["{platform}"],"url":"https://example.org/new.arach","sha256":"{hash}","file":"source.arach"}},
              {{"version":"1.4.0","apiVersion":4,"platforms":["unknown"],"url":"https://example.org/other.arach","sha256":"{hash}"}}
            ]}}]}}"#,
            hash = "a".repeat(64)
        );
        let entries = parse_store(json.as_bytes()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].version, "1.3.0");
        assert_eq!(entries[0].filename, "source.arach");
        assert!(entries[0].recommended);
    }

    #[test]
    fn cached_store_round_trips_saved_cache() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data_dir = std::env::temp_dir().join(format!("azdl-store-test-{nonce}"));
        fs::create_dir_all(&data_dir).unwrap();
        let sample = vec![StorePlugin {
            id: "sample".into(),
            name: "Sample Plugin".into(),
            description: "Test description".into(),
            version: "1.0.0".into(),
            url: "https://example.org/sample.arach".into(),
            sha256: "b".repeat(64),
            filename: "sample.arach".into(),
            recommended: true,
            repository: None,
            size: Some(1024),
        }];
        let cache_file = cache_path(&data_dir);
        fs::write(&cache_file, serde_json::to_vec_pretty(&sample).unwrap()).unwrap();

        let loaded = cached_store(&data_dir).expect("should load cached store");
        let _ = fs::remove_dir_all(&data_dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "sample");
        assert_eq!(loaded[0].name, "Sample Plugin");
    }
}
