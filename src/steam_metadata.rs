//! Steam DLC identity and depot relationships from public app metadata.

use crate::{Result, clean_command};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

type MetadataCache = Mutex<HashMap<u32, (Instant, SteamAppMetadata)>>;
static CACHE: OnceLock<MetadataCache> = OnceLock::new();

/// Public depot metadata. Manifest IDs remain integers in Rust and strings in UI JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamDepotMetadata {
    pub depot_id: u32,
    pub dlc_app_id: Option<u32>,
    pub source_app_id: Option<u32>,
    pub public_manifest_id: Option<String>,
    pub os: Option<String>,
    pub architecture: Option<String>,
    pub language: Option<String>,
}

/// DLC identities and depot relations visible for one Steam application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamAppMetadata {
    pub app_id: u32,
    pub app_type: Option<String>,
    pub name: Option<String>,
    pub parent_app_id: Option<u32>,
    pub dlc_ids: Vec<u32>,
    pub depots: Vec<SteamDepotMetadata>,
    pub public_build_id: Option<String>,
}

fn numeric_id(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .or_else(|| value.as_str().and_then(|id| id.parse().ok()))
        .filter(|id| *id > 0)
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Parse one steamcmd-style appinfo response without treating absent fields as an empty catalog.
pub fn parse_appinfo(payload: &[u8], app_id: u32) -> Result<SteamAppMetadata> {
    let root: Value = serde_json::from_slice(payload)?;
    let app = root
        .get("data")
        .and_then(|data| data.get(app_id.to_string()))
        .filter(|app| app.is_object())
        .ok_or_else(|| format!("Steam appinfo is unavailable for App ID {app_id}"))?;
    let mut dlc_ids = BTreeSet::new();
    if let Some(list) = app.pointer("/extended/listofdlc").and_then(Value::as_str) {
        for id in list
            .split(',')
            .filter_map(|id| id.trim().parse::<u32>().ok())
        {
            if id > 0 {
                dlc_ids.insert(id);
            }
        }
    }
    let mut depots = Vec::new();
    if let Some(entries) = app.get("depots").and_then(Value::as_object) {
        for (key, entry) in entries {
            let Ok(depot_id) = key.parse::<u32>() else {
                continue;
            };
            let dlc_app_id = entry.get("dlcappid").and_then(numeric_id);
            if let Some(id) = dlc_app_id {
                dlc_ids.insert(id);
            }
            let config = entry.get("config").unwrap_or(&Value::Null);
            depots.push(SteamDepotMetadata {
                depot_id,
                dlc_app_id,
                source_app_id: entry.get("depotfromapp").and_then(numeric_id),
                public_manifest_id: entry
                    .pointer("/manifests/public/gid")
                    .and_then(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .or_else(|| value.as_u64().map(|id| id.to_string()))
                    })
                    .filter(|id| id.parse::<u64>().is_ok()),
                os: config.get("oslist").and_then(text),
                architecture: config.get("osarch").and_then(text),
                language: config.get("language").and_then(text),
            });
        }
    }
    depots.sort_by_key(|depot| depot.depot_id);
    Ok(SteamAppMetadata {
        app_id,
        app_type: app.pointer("/common/type").and_then(text),
        name: app.pointer("/common/name").and_then(text),
        parent_app_id: app.pointer("/common/parent").and_then(numeric_id),
        dlc_ids: dlc_ids.into_iter().collect(),
        depots,
        public_build_id: app
            .pointer("/depots/branches/public/buildid")
            .and_then(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_u64().map(|id| id.to_string()))
            }),
    })
}

/// Fetch public appinfo through a bounded relay request, with a short in-process cache.
pub fn fetch_appinfo(app_id: u32) -> Result<SteamAppMetadata> {
    if app_id == 0 {
        return Err("Steam App ID is missing".into());
    }
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some((at, metadata)) = cache.get(&app_id)
        && at.elapsed() < Duration::from_secs(300)
    {
        return Ok(metadata.clone());
    }
    let url = format!("https://api.steamcmd.net/v1/info/{app_id}");
    let output = clean_command("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            "--max-filesize",
            "4000000",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
        ])
        .arg(url)
        .output()?;
    if !output.status.success() {
        return Err(format!("Steam appinfo lookup failed for App ID {app_id}").into());
    }
    let metadata = parse_appinfo(&output.stdout, app_id)?;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(app_id, (Instant::now(), metadata.clone()));
    }
    Ok(metadata)
}

/// Read DLC identities advertised by the Steam Store for a base game.
pub fn fetch_store_dlc_ids(app_id: u32) -> Result<Vec<u32>> {
    if app_id == 0 {
        return Err("Steam App ID is missing".into());
    }
    let url = format!(
        "https://store.steampowered.com/api/appdetails?appids={app_id}&filters=basic&l=english"
    );
    let output = clean_command("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "8",
            "--max-filesize",
            "4000000",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
        ])
        .arg(url)
        .output()?;
    if !output.status.success() {
        return Err(format!("Steam Store DLC lookup failed for App ID {app_id}").into());
    }
    parse_store_dlc_ids(&output.stdout, app_id)
}

/// Parse Store DLC identities while preserving a missing response as an error.
pub fn parse_store_dlc_ids(payload: &[u8], app_id: u32) -> Result<Vec<u32>> {
    let root: Value = serde_json::from_slice(payload)?;
    let data = root
        .get(app_id.to_string())
        .filter(|entry| entry.get("success").and_then(Value::as_bool) == Some(true))
        .and_then(|entry| entry.get("data"))
        .ok_or_else(|| format!("Steam Store metadata is unavailable for App ID {app_id}"))?;
    let mut ids = BTreeSet::new();
    if let Some(values) = data.get("dlc").and_then(Value::as_array) {
        ids.extend(values.iter().filter_map(numeric_id));
    }
    Ok(ids.into_iter().collect())
}

/// Merge DLC IDs from Store metadata without discarding relationships from appinfo.
pub fn merge_store_ids(metadata: &mut SteamAppMetadata, store_ids: &[u32]) {
    let mut ids = metadata.dlc_ids.iter().copied().collect::<BTreeSet<_>>();
    ids.extend(store_ids.iter().copied().filter(|id| *id > 0));
    metadata.dlc_ids = ids.into_iter().collect();
}

/// Group depots associated with each DLC App ID.
pub fn dlc_depots(metadata: &SteamAppMetadata) -> BTreeMap<u32, Vec<SteamDepotMetadata>> {
    let mut groups = BTreeMap::new();
    for depot in &metadata.depots {
        if let Some(id) = depot.dlc_app_id {
            groups
                .entry(id)
                .or_insert_with(Vec::new)
                .push(depot.clone());
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dlc_ids_and_depot_mappings_without_losing_large_manifest_ids() {
        let payload = br#"{"data":{"100":{"extended":{"listofdlc":"200, 201"},"depots":{"300":{"dlcappid":"200","manifests":{"public":{"gid":"1674424364022381183"}},"config":{"oslist":"windows"}},"301":{"dlcappid":"201"},"branches":{"public":{"buildid":"900"}}}}}}"#;
        let mut metadata = parse_appinfo(payload, 100).unwrap();
        merge_store_ids(&mut metadata, &[202, 200]);
        assert_eq!(metadata.dlc_ids, vec![200, 201, 202]);
        assert_eq!(metadata.public_build_id.as_deref(), Some("900"));
        assert_eq!(
            metadata.depots[0].public_manifest_id.as_deref(),
            Some("1674424364022381183")
        );
        assert_eq!(
            dlc_depots(&metadata)[&200][0].os.as_deref(),
            Some("windows")
        );
        assert!(dlc_depots(&metadata)[&201][0].public_manifest_id.is_none());
    }

    #[test]
    fn missing_appinfo_is_an_error() {
        assert!(parse_appinfo(br#"{"data":{}}"#, 100).is_err());
    }

    #[test]
    fn reads_dlc_title_and_parent_without_a_depot_list() {
        let payload =
            br#"{"data":{"200":{"common":{"name":"Sample DLC","parent":"100","type":"DLC"}}}}"#;
        let metadata = parse_appinfo(payload, 200).unwrap();
        assert_eq!(metadata.name.as_deref(), Some("Sample DLC"));
        assert_eq!(metadata.app_type.as_deref(), Some("DLC"));
        assert_eq!(metadata.parent_app_id, Some(100));
        assert!(metadata.depots.is_empty());
    }

    #[test]
    fn store_dlc_ids_are_distinct_from_installable_artifacts() {
        let payload = br#"{"100":{"success":true,"data":{"dlc":[200,201,200]}}}"#;
        assert_eq!(parse_store_dlc_ids(payload, 100).unwrap(), vec![200, 201]);
        assert!(parse_store_dlc_ids(br#"{"100":{"success":false}}"#, 100).is_err());
    }
}
