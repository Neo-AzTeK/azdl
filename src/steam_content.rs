//! Exact Steam depot selection and base content receipts.

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
    #[serde(default)]
    pub origin: BaseReceiptOrigin,
}

/// How the base revision recorded in a receipt was established.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BaseReceiptOrigin {
    #[default]
    ManagedInstall,
    VerifiedLegacy,
}

/// Read-only evidence that legacy DLC files matched an exact provider revision.
/// This record does not grant ownership or permission to remove those files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedDlcReceipt {
    pub game_id: String,
    pub app_id: u32,
    pub dlc_id: u32,
    pub depots: Vec<DepotRevision>,
    pub verified_unix: u64,
}

/// What public appinfo establishes about separately downloadable DLC depots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlcDepotAvailability {
    /// A target depot has a public manifest or a shared-app reference to resolve.
    Candidate,
    /// DLC metadata is incomplete; an install attempt must resolve it first.
    Unknown,
    /// Public metadata rules out a download through the current depot path.
    Unavailable(&'static str),
}

/// Classify DLC delivery without treating Store membership as downloadable content.
pub fn dlc_depot_availability(
    parent: Option<&SteamAppMetadata>,
    child: Option<&SteamAppMetadata>,
    parent_id: u32,
    dlc_id: u32,
) -> DlcDepotAvailability {
    let parent_depots = parent
        .into_iter()
        .flat_map(|metadata| &metadata.depots)
        .filter(|depot| depot.dlc_app_id == Some(dlc_id))
        .collect::<Vec<_>>();
    let depots = if parent_depots.is_empty() {
        let Some(child) = child else {
            return DlcDepotAvailability::Unknown;
        };
        match child.parent_app_id {
            Some(id) if id != parent_id => {
                return DlcDepotAvailability::Unavailable(
                    "Steam DLC metadata identifies a different parent game",
                );
            }
            None => return DlcDepotAvailability::Unknown,
            _ => {}
        }
        child
            .depots
            .iter()
            .filter(|depot| depot.dlc_app_id.is_none_or(|id| id == dlc_id))
            .collect::<Vec<_>>()
    } else {
        parent_depots
    };
    if depots.is_empty() {
        return DlcDepotAvailability::Unknown;
    }
    let selected = depots
        .into_iter()
        .filter(|depot| matches_target(depot, "windows", "english"))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return DlcDepotAvailability::Unavailable(
            "No DLC depot matches this game's Windows/English target",
        );
    }
    if selected
        .iter()
        .any(|depot| depot.public_manifest_id.is_none() && depot.source_app_id.is_none())
    {
        return DlcDepotAvailability::Unavailable("A required DLC depot has no public manifest");
    }
    DlcDepotAvailability::Candidate
}

pub(crate) fn matches_target(depot: &SteamDepotMetadata, os: &str, language: &str) -> bool {
    let matches = |value: Option<&str>, expected: &str| {
        value.is_none_or(|value| {
            value
                .split([',', ';', ' '])
                .any(|part| part.eq_ignore_ascii_case(expected))
        })
    };
    matches(depot.os.as_deref(), os) && matches(depot.language.as_deref(), language)
}

/// Check that an installed base still matches the public revision used for a DLC plan.
pub fn validate_base_revision(
    receipt: &BaseContentReceipt,
    metadata: &SteamAppMetadata,
) -> Result<()> {
    validate_base_revision_with(receipt, metadata, crate::steam_metadata::fetch_appinfo)
}

fn validate_base_revision_with(
    receipt: &BaseContentReceipt,
    metadata: &SteamAppMetadata,
    mut load_app: impl FnMut(u32) -> Result<SteamAppMetadata>,
) -> Result<()> {
    if receipt.app_id != metadata.app_id || receipt.depots.is_empty() {
        return Err("Base game content receipt does not match Steam appinfo".into());
    }
    let required = metadata
        .depots
        .iter()
        .filter(|depot| depot.dlc_app_id.is_none())
        .filter(|depot| matches_target(depot, "windows", "english"))
        .map(|depot| depot.depot_id)
        .collect::<BTreeSet<_>>();
    if required.is_empty() {
        return Err("Steam metadata identifies no required base depots for this game".into());
    }
    let mut recorded = BTreeMap::new();
    for depot in &receipt.depots {
        if recorded
            .insert(depot.depot_id, depot.manifest_id.as_str())
            .is_some()
        {
            return Err(format!("Base receipt repeats depot {}", depot.depot_id).into());
        }
    }
    if let Some(missing) = required.iter().find(|id| !recorded.contains_key(id)) {
        return Err(format!(
            "Base receipt lacks required depot {missing}; update the base game before adding DLC"
        )
        .into());
    }
    if let Some(extra) = recorded.keys().find(|id| !required.contains(id)) {
        return Err(format!(
            "Base receipt contains depot {extra} that is no longer required; update the base game before adding DLC"
        )
        .into());
    }
    for depot_id in required {
        let source =
            crate::steam_metadata::resolve_depot_source(metadata, depot_id, &mut load_app)?;
        if recorded
            .get(&depot_id)
            .and_then(|id| id.parse::<u64>().ok())
            != Some(source.manifest_id)
        {
            return Err(format!(
                "Base depot {depot_id} has a different revision from current Steam metadata; update the base game before this DLC"
            )
            .into());
        }
    }
    Ok(())
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

fn observation_path(data_dir: &Path, game_id: &str, dlc_id: u32) -> Result<std::path::PathBuf> {
    validate_id(game_id)?;
    if dlc_id == 0 {
        return Err("DLC App ID is missing".into());
    }
    let root = data_dir.join("dlc-observations");
    let parent = root.join(game_id);
    if [root.as_path(), parent.as_path()]
        .iter()
        .any(|path| fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()))
    {
        return Err("DLC observation state is a symbolic link".into());
    }
    Ok(parent.join(format!("{dlc_id}.json")))
}

/// Record a successful content check without claiming ownership of legacy files.
pub fn save_observation(data_dir: &Path, receipt: &ObservedDlcReceipt) -> Result<()> {
    let path = observation_path(data_dir, &receipt.game_id, receipt.dlc_id)?;
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("DLC observation file is a symbolic link".into());
    }
    let parent = path.parent().ok_or("invalid DLC observation path")?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = parent.join(format!(".{}-{nonce}.tmp", receipt.dlc_id));
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

/// Read a prior exact content observation for an older game installation.
pub fn load_observation(
    data_dir: &Path,
    game_id: &str,
    dlc_id: u32,
) -> Result<Option<ObservedDlcReceipt>> {
    let path = observation_path(data_dir, game_id, dlc_id)?;
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("DLC observation file is a symbolic link".into());
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let receipt: ObservedDlcReceipt = serde_json::from_slice(&bytes)?;
    if receipt.game_id != game_id || receipt.dlc_id != dlc_id {
        return Err("DLC observation belongs to another game".into());
    }
    Ok(Some(receipt))
}

/// List prior exact-content observations for one game, including when Steam is offline.
pub fn list_observations(data_dir: &Path, game_id: &str) -> Result<Vec<ObservedDlcReceipt>> {
    let path = observation_path(data_dir, game_id, 1)?;
    let parent = path.parent().ok_or("invalid DLC observation path")?;
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut observations = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(dlc_id) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|id| id.parse::<u32>().ok())
            .filter(|id| *id > 0)
        else {
            continue;
        };
        if let Some(receipt) = load_observation(data_dir, game_id, dlc_id)? {
            observations.push(receipt);
        }
    }
    observations.sort_by_key(|receipt| receipt.dlc_id);
    Ok(observations)
}

/// Forget a stale observation without changing any game files.
pub fn clear_observation(data_dir: &Path, game_id: &str, dlc_id: u32) -> Result<()> {
    match fs::remove_file(observation_path(data_dir, game_id, dlc_id)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dlc_availability_distinguishes_separate_files_from_store_membership() {
        let parent = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"100":{"extended":{"listofdlc":"200,201,202"},"depots":{"301":{"dlcappid":"200","manifests":{"public":{"gid":"99"}}},"302":{"dlcappid":"201"},"303":{"dlcappid":"202","depotfromapp":"300"}}}}}"#,
            100,
        )
        .unwrap();
        assert_eq!(
            dlc_depot_availability(Some(&parent), None, 100, 200),
            DlcDepotAvailability::Candidate
        );
        assert!(matches!(
            dlc_depot_availability(Some(&parent), None, 100, 201),
            DlcDepotAvailability::Unavailable(reason) if reason.contains("no public manifest")
        ));
        assert_eq!(
            dlc_depot_availability(Some(&parent), None, 100, 202),
            DlcDepotAvailability::Candidate
        );
        assert_eq!(
            dlc_depot_availability(Some(&parent), None, 100, 203),
            DlcDepotAvailability::Unknown
        );
        let child = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"203":{"common":{"parent":"100","type":"DLC"}}}}"#,
            203,
        )
        .unwrap();
        assert_eq!(
            dlc_depot_availability(Some(&parent), Some(&child), 100, 203),
            DlcDepotAvailability::Unknown
        );
        assert!(matches!(
            dlc_depot_availability(Some(&parent), Some(&child), 101, 203),
            DlcDepotAvailability::Unavailable(reason) if reason.contains("different parent")
        ));
        let no_parent = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"206":{"common":{"type":"DLC"},"depots":{"406":{"manifests":{"public":{"gid":"126"}}}}}}}"#,
            206,
        )
        .unwrap();
        assert_eq!(
            dlc_depot_availability(Some(&parent), Some(&no_parent), 100, 206),
            DlcDepotAvailability::Unknown
        );
        let empty_depots = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"207":{"common":{"parent":"100","type":"DLC"},"depots":{}}}}"#,
            207,
        )
        .unwrap();
        assert_eq!(
            dlc_depot_availability(Some(&parent), Some(&empty_depots), 100, 207),
            DlcDepotAvailability::Unknown
        );
        let separate = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"204":{"common":{"parent":"100","type":"DLC"},"depots":{"401":{"manifests":{"public":{"gid":"123"}},"config":{"oslist":"windows"}}}}}}"#,
            204,
        )
        .unwrap();
        assert_eq!(
            dlc_depot_availability(Some(&parent), Some(&separate), 100, 204),
            DlcDepotAvailability::Candidate
        );
        let linux_only = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"205":{"common":{"parent":"100","type":"DLC"},"depots":{"402":{"manifests":{"public":{"gid":"124"}},"config":{"oslist":"linux"}}}}}}"#,
            205,
        )
        .unwrap();
        assert!(matches!(
            dlc_depot_availability(Some(&parent), Some(&linux_only), 100, 205),
            DlcDepotAvailability::Unavailable(reason) if reason.contains("Windows/English")
        ));
    }

    #[test]
    fn base_revision_requires_all_target_depots_and_exact_shared_revision() {
        let base = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"100":{"depots":{"101":{"manifests":{"public":{"gid":"1"}}},"102":{"depotfromapp":"200"},"103":{"manifests":{"public":{"gid":"3"}},"config":{"oslist":"linux"}},"201":{"dlcappid":"300","manifests":{"public":{"gid":"4"}}}}}}}"#,
            100,
        )
        .unwrap();
        let shared = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"200":{"depots":{"102":{"manifests":{"public":{"gid":"2"}}}}}}}"#,
            200,
        )
        .unwrap();
        let receipt = BaseContentReceipt {
            game_id: "steam-100".into(),
            app_id: 100,
            depots: vec![
                DepotRevision {
                    depot_id: 101,
                    manifest_id: "1".into(),
                },
                DepotRevision {
                    depot_id: 102,
                    manifest_id: "2".into(),
                },
            ],
            origin: BaseReceiptOrigin::ManagedInstall,
        };
        let check = |receipt: &BaseContentReceipt, shared: &SteamAppMetadata| {
            validate_base_revision_with(receipt, &base, |app_id| {
                assert_eq!(app_id, 200);
                Ok(shared.clone())
            })
        };
        check(&receipt, &shared).unwrap();

        let mut missing = receipt.clone();
        missing.depots.pop();
        assert!(
            check(&missing, &shared)
                .unwrap_err()
                .to_string()
                .contains("lacks required depot 102")
        );

        let mut extra = receipt.clone();
        extra.depots.push(DepotRevision {
            depot_id: 103,
            manifest_id: "3".into(),
        });
        assert!(
            check(&extra, &shared)
                .unwrap_err()
                .to_string()
                .contains("no longer required")
        );

        let mut changed = receipt.clone();
        changed.depots[1].manifest_id = "9".into();
        assert!(
            check(&changed, &shared)
                .unwrap_err()
                .to_string()
                .contains("different revision")
        );

        let mut newer = shared.clone();
        newer.depots[0].public_manifest_id = Some("9".into());
        assert!(
            check(&receipt, &newer)
                .unwrap_err()
                .to_string()
                .contains("different revision")
        );
    }

    #[test]
    fn observed_legacy_dlc_is_distinct_from_managed_install_state() {
        let root = std::env::temp_dir().join(format!(
            "azdl-observed-dlc-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let receipt = ObservedDlcReceipt {
            game_id: "legacy-game".into(),
            app_id: 100,
            dlc_id: 200,
            depots: vec![DepotRevision {
                depot_id: 201,
                manifest_id: "12345678901234567890".into(),
            }],
            verified_unix: 42,
        };
        save_observation(&root, &receipt).unwrap();
        let later = ObservedDlcReceipt {
            dlc_id: 300,
            ..receipt.clone()
        };
        save_observation(&root, &later).unwrap();
        assert_eq!(
            load_observation(&root, "legacy-game", 200).unwrap(),
            Some(receipt.clone())
        );
        assert_eq!(
            list_observations(&root, "legacy-game").unwrap(),
            vec![receipt, later]
        );
        assert!(load_base_receipt(&root, "legacy-game").unwrap().is_none());
        assert!(!root.join("dlc/legacy-game/200/complete").exists());
        clear_observation(&root, "legacy-game", 200).unwrap();
        assert!(
            load_observation(&root, "legacy-game", 200)
                .unwrap()
                .is_none()
        );
        assert_eq!(list_observations(&root, "legacy-game").unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_base_receipts_default_to_managed_origin() {
        let old: BaseContentReceipt = serde_json::from_str(
            r#"{"game_id":"steam-100","app_id":100,"depots":[{"depot_id":101,"manifest_id":"123"}]}"#,
        )
        .unwrap();
        assert_eq!(old.origin, BaseReceiptOrigin::ManagedInstall);
        let migrated = BaseContentReceipt {
            origin: BaseReceiptOrigin::VerifiedLegacy,
            ..old
        };
        let encoded = serde_json::to_string(&migrated).unwrap();
        assert_eq!(
            serde_json::from_str::<BaseContentReceipt>(&encoded)
                .unwrap()
                .origin,
            BaseReceiptOrigin::VerifiedLegacy
        );
    }
}
