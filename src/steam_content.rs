//! Exact Steam depot selection and base content receipts.

use crate::steam_depot::DepotInfo;
use crate::steam_metadata::{SteamAppMetadata, SteamDepotMetadata};
use crate::{Result, validate_id};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// One depot revision selected from a provider bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepotRevision {
    pub depot_id: u32,
    pub manifest_id: String,
}

/// Depot revisions used to create a managed base installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseContentReceipt {
    pub game_id: String,
    pub app_id: u32,
    pub depots: Vec<DepotRevision>,
}

fn matches_target(depot: &SteamDepotMetadata, os: &str, language: &str) -> bool {
    let matches = |value: Option<&str>, expected: &str| {
        value.is_none_or(|value| {
            value
                .split([',', ';', ' '])
                .any(|part| part.eq_ignore_ascii_case(expected))
        })
    };
    matches(depot.os.as_deref(), os) && matches(depot.language.as_deref(), language)
}

/// Select only known base depots for the game target and installed language.
pub fn select_base_depots(
    available: &[DepotInfo],
    metadata: &SteamAppMetadata,
    target_os: &str,
    language: &str,
) -> Result<Vec<DepotInfo>> {
    let by_id = metadata
        .depots
        .iter()
        .map(|depot| (depot.depot_id, depot))
        .collect::<BTreeMap<_, _>>();
    let mut selected = Vec::new();
    for depot in available {
        let Some(info) = by_id.get(&depot.depot_id) else {
            return Err(format!(
                "Provider depot {} is absent from Steam metadata for App ID {}; its base/DLC role is unknown",
                depot.depot_id, metadata.app_id
            )
            .into());
        };
        if info.dlc_app_id.is_none() && matches_target(info, target_os, language) {
            if let Some(expected) = info.public_manifest_id.as_deref()
                && depot.manifest_id.to_string() != expected
            {
                return Err(format!(
                    "Base depot {} provider revision differs from public manifest {expected}",
                    depot.depot_id
                )
                .into());
            }
            selected.push(depot.clone());
        }
    }
    if selected.is_empty() {
        return Err(format!(
            "No matching base depots for Steam App ID {}",
            metadata.app_id
        )
        .into());
    }
    Ok(selected)
}

/// Check that an installed base still matches the public revision used for a DLC plan.
pub fn validate_base_revision(
    receipt: &BaseContentReceipt,
    metadata: &SteamAppMetadata,
) -> Result<()> {
    if receipt.app_id != metadata.app_id || receipt.depots.is_empty() {
        return Err("Base game content receipt does not match Steam appinfo".into());
    }
    let public = metadata
        .depots
        .iter()
        .filter_map(|depot| {
            depot
                .public_manifest_id
                .as_deref()
                .map(|id| (depot.depot_id, id))
        })
        .collect::<BTreeMap<_, _>>();
    let shared = metadata
        .depots
        .iter()
        .filter(|depot| depot.source_app_id.is_some())
        .map(|depot| depot.depot_id)
        .collect::<BTreeSet<_>>();
    for depot in &receipt.depots {
        if shared.contains(&depot.depot_id) {
            continue;
        }
        if public.get(&depot.depot_id).copied() != Some(depot.manifest_id.as_str()) {
            return Err(format!(
                "Base depot {} has a different revision from current Steam metadata; update the base game before this DLC",
                depot.depot_id
            )
            .into());
        }
    }
    Ok(())
}

/// Resolve the exact DLC depot subset from a provider bundle and public appinfo.
pub fn select_dlc_depots(
    available: &[DepotInfo],
    metadata: &SteamAppMetadata,
    dlc_id: u32,
    target_os: &str,
    language: &str,
) -> Result<Vec<DepotInfo>> {
    let desired = metadata
        .depots
        .iter()
        .filter(|depot| depot.dlc_app_id == Some(dlc_id))
        .filter(|depot| matches_target(depot, target_os, language))
        .collect::<Vec<_>>();
    if desired.is_empty() {
        return Err(format!("No separate depot is identified for DLC {dlc_id}").into());
    }
    let bundle = available
        .iter()
        .map(|depot| (depot.depot_id, depot))
        .collect::<BTreeMap<_, _>>();
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for depot in desired {
        let Some(expected) = depot.public_manifest_id.as_deref() else {
            return Err(format!("DLC depot {} has no public manifest", depot.depot_id).into());
        };
        let Some(artifact) = bundle.get(&depot.depot_id) else {
            return Err(format!(
                "The selected source has no provider manifest for DLC {dlc_id}, depot {}",
                depot.depot_id
            )
            .into());
        };
        if artifact.manifest_id.to_string() != expected {
            return Err(format!(
                "DLC depot {} provider revision differs from public manifest {expected}",
                depot.depot_id
            )
            .into());
        }
        if seen.insert(depot.depot_id) {
            selected.push((*artifact).clone());
        }
    }
    Ok(selected)
}

/// Select depots managed beneath a separate DLC App ID.
pub fn select_separate_dlc_depots(
    available: &[DepotInfo],
    metadata: &SteamAppMetadata,
    parent_app_id: u32,
    target_os: &str,
    language: &str,
) -> Result<Vec<DepotInfo>> {
    if metadata.parent_app_id != Some(parent_app_id) {
        return Err("DLC metadata does not identify the installed parent game".into());
    }
    let by_id = metadata
        .depots
        .iter()
        .map(|depot| (depot.depot_id, depot))
        .collect::<BTreeMap<_, _>>();
    let mut selected = Vec::new();
    for artifact in available {
        let Some(info) = by_id.get(&artifact.depot_id) else {
            return Err(
                format!("DLC depot {} is absent from DLC appinfo", artifact.depot_id).into(),
            );
        };
        if info.dlc_app_id.is_none_or(|id| id == metadata.app_id)
            && matches_target(info, target_os, language)
        {
            selected.push(artifact.clone());
        }
    }
    if selected.is_empty() {
        return Err(format!("No matching depots for DLC App ID {}", metadata.app_id).into());
    }
    for artifact in &selected {
        let expected = by_id[&artifact.depot_id]
            .public_manifest_id
            .as_deref()
            .ok_or_else(|| format!("DLC depot {} has no public manifest", artifact.depot_id))?;
        if artifact.manifest_id.to_string() != expected {
            return Err(format!(
                "DLC depot {} provider revision differs from public manifest {expected}",
                artifact.depot_id
            )
            .into());
        }
    }
    Ok(selected)
}

fn receipt_path(data_dir: &Path, game_id: &str) -> Result<std::path::PathBuf> {
    validate_id(game_id)?;
    Ok(data_dir
        .join("content-receipts")
        .join(format!("{game_id}.json")))
}

/// Persist the base depot revisions used to create a managed game install.
pub fn save_base_receipt(data_dir: &Path, receipt: &BaseContentReceipt) -> Result<()> {
    let path = receipt_path(data_dir, &receipt.game_id)?;
    let parent = path.parent().ok_or("invalid receipt path")?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = parent.join(format!(
        ".{}.{}.{nonce}.tmp",
        receipt.game_id,
        std::process::id()
    ));
    let mut file = File::create(&temp)?;
    serde_json::to_writer_pretty(&mut file, receipt)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temp, path)?;
    Ok(())
}

/// Read a managed base receipt; old or imported games have no receipt.
pub fn load_base_receipt(data_dir: &Path, game_id: &str) -> Result<Option<BaseContentReceipt>> {
    let path = receipt_path(data_dir, game_id)?;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let receipt: BaseContentReceipt = serde_json::from_slice(&bytes)?;
    if receipt.game_id != game_id {
        return Err("Base content receipt belongs to another game".into());
    }
    Ok(Some(receipt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn artifact(id: u32, revision: u64) -> DepotInfo {
        DepotInfo {
            depot_id: id,
            manifest_id: revision,
            manifest_path: PathBuf::from(format!("{id}_{revision}.manifest")),
            depot_key: "key".into(),
        }
    }

    #[test]
    fn base_selection_excludes_dlc_and_dlc_requires_exact_revision() {
        let metadata = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"100":{"depots":{"101":{"manifests":{"public":{"gid":"1"}}},"200":{"dlcappid":"300","manifests":{"public":{"gid":"2"}},"config":{"oslist":"windows"}},"201":{"dlcappid":"300","manifests":{"public":{"gid":"3"}},"config":{"oslist":"linux"}}}}}}"#,
            100,
        )
        .unwrap();
        let bundle = vec![artifact(101, 1), artifact(200, 2), artifact(201, 3)];
        let base = select_base_depots(&bundle, &metadata, "windows", "english").unwrap();
        assert_eq!(
            base.iter().map(|d| d.depot_id).collect::<Vec<_>>(),
            vec![101]
        );
        let dlc = select_dlc_depots(&bundle, &metadata, 300, "windows", "english").unwrap();
        assert_eq!(
            dlc.iter().map(|d| d.depot_id).collect::<Vec<_>>(),
            vec![200]
        );
        let wrong = vec![artifact(200, 4)];
        assert!(select_dlc_depots(&wrong, &metadata, 300, "windows", "english").is_err());
    }
}
