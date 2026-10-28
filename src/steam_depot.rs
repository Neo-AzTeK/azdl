use crate::catalog::{CatalogGame, InstallOutcome, save_pending_setup_origin, stamp_origin};
use crate::jobs::{
    DownloadJob, InstallKind, InstallPlan, InstallState, JobKind, JobStatus, job_id_for_target,
    load_job, now, save_job,
};
use crate::steam::{import_steam, scan_steam, steam_root};
use crate::steam_access::fetch_manifest_bundle as fetch_provider_bundle;
use crate::steam_metadata::{ResolvedDepotSource, SteamAppMetadata};
use crate::{Game, Library, Result, discovery, validate_id};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::steam_engine::{DownloadProgressSpan, download_steam_depots_with_progress};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepotInfo {
    pub depot_id: u32,
    pub manifest_id: u64,
    pub manifest_path: PathBuf,
    pub depot_key: String,
}

/// Resolve each provider depot to exactly one manifest revision.
pub fn parse_lua_manifests(manifest_dir: &Path, app_id: u32) -> Result<Vec<DepotInfo>> {
    parse_lua_manifests_filtered(manifest_dir, app_id, None)
}

fn parse_lua_manifests_filtered(
    manifest_dir: &Path,
    app_id: u32,
    filter: Option<(
        &std::collections::BTreeSet<u32>,
        &std::collections::BTreeSet<u32>,
    )>,
) -> Result<Vec<DepotInfo>> {
    if !manifest_dir.is_dir() {
        return Err("manifest directory does not exist".into());
    }

    let mut raw_depots: Vec<(u32, String)> = Vec::new();
    let mut manifest_ids: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();

    // Scan for all .lua scripts in manifest_dir
    for entry in fs::read_dir(manifest_dir)?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "lua")
            && fs::symlink_metadata(&path)
                .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
            && let Ok(content) = fs::read_to_string(&path)
        {
            parse_lua_content(&content, &mut raw_depots, &mut manifest_ids);
        }
    }

    if let Some((selected, known)) = filter {
        if let Some((unknown, _)) = raw_depots.iter().find(|(id, _)| !known.contains(id)) {
            return Err(format!(
                "Provider depot {unknown} is absent from Steam metadata for App ID {app_id}; its base/DLC role is unknown"
            )
            .into());
        }
        raw_depots.retain(|(id, _)| selected.contains(id));
    }

    let mut depots = Vec::new();

    for (depot_id, depot_key) in raw_depots {
        let requested = manifest_ids.get(&depot_id).copied();
        let mut matches = Vec::new();
        let mut roots = vec![manifest_dir.to_path_buf()];
        if let Some(parent) = manifest_dir.parent() {
            roots.push(parent.join("depotcache"));
        }
        for root in roots {
            let entries = match fs::read_dir(&root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let Some(id) = name
                    .strip_prefix(&format!("{depot_id}_"))
                    .and_then(|tail| tail.strip_suffix(".manifest"))
                    .and_then(|id| id.parse::<u64>().ok())
                else {
                    continue;
                };
                if requested.is_none_or(|expected| expected == id) {
                    let path = entry.path();
                    let meta = fs::symlink_metadata(&path)?;
                    if !meta.is_file() || meta.file_type().is_symlink() {
                        return Err(
                            format!("Depot {depot_id} manifest is not a regular file").into()
                        );
                    }
                    matches.push((id, path));
                }
            }
        }
        matches.sort_by(|a, b| a.1.cmp(&b.1));
        matches.dedup_by(|a, b| a.0 == b.0);
        let (manifest_id, manifest_path) = match matches.len() {
            1 => matches.remove(0),
            0 => return Err(format!(
                "No exact manifest for Steam App ID {app_id}, depot {depot_id}{}",
                requested.map_or(String::new(), |id| format!(", revision {id}"))
            ).into()),
            _ => return Err(format!(
                "Multiple manifest revisions for Steam App ID {app_id}, depot {depot_id}; the provider must specify one"
            ).into()),
        };
        depots.push(DepotInfo {
            depot_id,
            manifest_id,
            manifest_path,
            depot_key,
        });
    }

    depots.sort_by_key(|depot| depot.depot_id);
    Ok(depots)
}

fn parse_lua_content(
    content: &str,
    depots: &mut Vec<(u32, String)>,
    manifest_ids: &mut std::collections::HashMap<u32, u64>,
) {
    for line in content.lines() {
        let line = line.trim();
        // Match addappid(depot_id, [flag], "key")
        if let Some(start) = line.find("addappid(") {
            let args_part = &line[start + "addappid(".len()..];
            if let Some(end) = args_part.find(')') {
                let args_str = &args_part[..end];
                let tokens: Vec<&str> = args_str.split(',').map(str::trim).collect();
                if tokens.len() >= 2
                    && let Ok(depot_id) = tokens[0].parse::<u32>()
                {
                    // The key is the last token or 3rd token
                    let key_token =
                        tokens[tokens.len() - 1].trim_matches(|c| c == '"' || c == '\'');
                    if key_token.len() >= 32
                        && key_token.chars().all(|c| c.is_ascii_hexdigit())
                        && !depots.iter().any(|(id, _)| *id == depot_id)
                    {
                        depots.push((depot_id, key_token.to_string()));
                    }
                }
            }
        }

        // Match setManifestid(depot_id, "manifest_id")
        if let Some(start) = line.find("setManifestid(") {
            let args_part = &line[start + "setManifestid(".len()..];
            if let Some(end) = args_part.find(')') {
                let args_str = &args_part[..end];
                let tokens: Vec<&str> = args_str.split(',').map(str::trim).collect();
                if tokens.len() >= 2
                    && let Ok(depot_id) = tokens[0].parse::<u32>()
                {
                    let man_id = tokens[1].trim_matches(|c| c == '"' || c == '\'');
                    if let Ok(man_id) = man_id.parse::<u64>() {
                        manifest_ids.insert(depot_id, man_id);
                    }
                }
            }
        }
    }
}

pub use crate::archive::normalize_windows_paths;

/// Verify every target base depot in an older installation without changing game files.
pub fn verify_legacy_base(
    game: &Game,
    root: &Path,
    metadata: &crate::steam_metadata::SteamAppMetadata,
) -> Result<crate::steam_content::BaseContentReceipt> {
    verify_legacy_base_with(
        game,
        root,
        metadata,
        crate::steam_metadata::fetch_appinfo,
        fetch_provider_bundle,
    )
}

fn verify_legacy_base_with(
    game: &Game,
    root: &Path,
    metadata: &SteamAppMetadata,
    load_app: impl FnMut(u32) -> Result<SteamAppMetadata>,
    fetch_bundle: impl FnMut(&Path, u32) -> Result<PathBuf>,
) -> Result<crate::steam_content::BaseContentReceipt> {
    let app_id = metadata.app_id;
    let depots = resolve_base_manifests_with(
        &root.join("base-verification-provider"),
        metadata,
        load_app,
        fetch_bundle,
    )?;
    verify_base_manifest_files(game, &depots)?;
    Ok(crate::steam_content::BaseContentReceipt {
        game_id: game.id.clone(),
        app_id,
        depots: depots
            .iter()
            .map(|(depot, _)| crate::steam_content::DepotRevision {
                depot_id: depot.depot_id,
                manifest_id: depot.manifest_id.to_string(),
            })
            .collect(),
        origin: crate::steam_content::BaseReceiptOrigin::VerifiedLegacy,
    })
}

fn resolve_base_manifests_with(
    root: &Path,
    metadata: &SteamAppMetadata,
    load_app: impl FnMut(u32) -> Result<SteamAppMetadata>,
    fetch_bundle: impl FnMut(&Path, u32) -> Result<PathBuf>,
) -> Result<Vec<(DepotInfo, crate::steam_manifest::DepotManifest)>> {
    let selected_ids = metadata
        .depots
        .iter()
        .filter(|depot| depot.dlc_app_id.is_none())
        .filter(|depot| crate::steam_content::matches_target(depot, "windows", "english"))
        .map(|depot| depot.depot_id)
        .collect::<std::collections::BTreeSet<_>>();
    if selected_ids.is_empty() {
        return Err(
            "Cannot verify the old base game: Steam metadata identifies no required base depots"
                .into(),
        );
    }
    let depots =
        resolve_dlc_download_groups_with(root, metadata, &selected_ids, load_app, fetch_bundle)?
            .depots;
    let manifests = depots
        .iter()
        .map(|depot| {
            crate::steam_manifest::read_manifest_with_key(
                &depot.manifest_path,
                depot.depot_id,
                depot.manifest_id,
                &depot.depot_key,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    crate::steam_manifest::validate_manifest_set(&manifests)?;
    Ok(depots.into_iter().zip(manifests).collect())
}

fn verify_base_manifest_files(
    game: &Game,
    depots: &[(DepotInfo, crate::steam_manifest::DepotManifest)],
) -> Result<()> {
    for (depot, manifest) in depots {
        let result = crate::steam_manifest::verify_installed(manifest, &game.install_dir)?;
        if !result.complete() {
            return Err(format!(
                "Base depot {} differs from the current provider manifest ({} missing, {} changed files); reinstall the base game before adding DLC",
                depot.depot_id, result.missing, result.changed
            )
            .into());
        }
    }
    Ok(())
}

/// Selected manifests grouped by the App ID used to download their content.
#[derive(Debug)]
struct ResolvedDepotDownloads {
    groups: Vec<(u32, Vec<DepotInfo>)>,
    depots: Vec<DepotInfo>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ManifestRevisionPolicy {
    CurrentPublic,
    /// A fresh base installation records and verifies the source's selected revision.
    ProviderPinnedBase,
}

fn resolve_dlc_download_groups(
    root: &Path,
    metadata: &SteamAppMetadata,
    selected_ids: &BTreeSet<u32>,
) -> Result<ResolvedDepotDownloads> {
    resolve_dlc_download_groups_with(
        root,
        metadata,
        selected_ids,
        crate::steam_metadata::fetch_appinfo,
        fetch_provider_bundle,
    )
}

fn resolve_dlc_download_groups_with(
    root: &Path,
    metadata: &SteamAppMetadata,
    selected_ids: &BTreeSet<u32>,
    load_app: impl FnMut(u32) -> Result<SteamAppMetadata>,
    fetch_bundle: impl FnMut(&Path, u32) -> Result<PathBuf>,
) -> Result<ResolvedDepotDownloads> {
    resolve_download_groups_with(
        root,
        metadata,
        selected_ids,
        ManifestRevisionPolicy::CurrentPublic,
        load_app,
        fetch_bundle,
    )
}

fn resolve_download_groups_with(
    root: &Path,
    metadata: &SteamAppMetadata,
    selected_ids: &BTreeSet<u32>,
    revision_policy: ManifestRevisionPolicy,
    mut load_app: impl FnMut(u32) -> Result<SteamAppMetadata>,
    mut fetch_bundle: impl FnMut(&Path, u32) -> Result<PathBuf>,
) -> Result<ResolvedDepotDownloads> {
    if selected_ids.is_empty() {
        return Err(format!("No matching depots for Steam App ID {}", metadata.app_id).into());
    }
    if revision_policy == ManifestRevisionPolicy::ProviderPinnedBase
        && metadata
            .depots
            .iter()
            .any(|depot| selected_ids.contains(&depot.depot_id) && depot.dlc_app_id.is_some())
    {
        return Err("A base installation cannot select DLC depots".into());
    }
    let mut by_manifest_app = BTreeMap::<u32, Vec<ResolvedDepotSource>>::new();
    for depot_id in selected_ids {
        let resolved =
            crate::steam_metadata::resolve_depot_source(metadata, *depot_id, &mut load_app)?;
        by_manifest_app
            .entry(resolved.manifest_app_id)
            .or_default()
            .push(resolved);
    }
    let mut download_groups = BTreeMap::<u32, Vec<DepotInfo>>::new();
    let mut selected = Vec::new();
    for (manifest_app_id, sources) in by_manifest_app {
        let manifest_metadata = if manifest_app_id == metadata.app_id {
            metadata.clone()
        } else {
            load_app(manifest_app_id)?
        };
        let known = manifest_metadata
            .depots
            .iter()
            .map(|depot| depot.depot_id)
            .collect::<BTreeSet<_>>();
        let wanted = sources
            .iter()
            .map(|source| source.depot_id)
            .collect::<BTreeSet<_>>();
        let unpacked = fetch_bundle(
            &root.join(format!("provider-{manifest_app_id}")),
            manifest_app_id,
        )?;
        let available =
            parse_lua_manifests_filtered(&unpacked, manifest_app_id, Some((&wanted, &known)))?;
        let artifacts = available
            .iter()
            .map(|artifact| (artifact.depot_id, artifact))
            .collect::<BTreeMap<_, _>>();
        for source in sources {
            let artifact = artifacts.get(&source.depot_id).ok_or_else(|| {
                format!(
                    "The selected source has no provider manifest for depot {} from App ID {manifest_app_id}",
                    source.depot_id
                )
            })?;
            if revision_policy == ManifestRevisionPolicy::CurrentPublic
                && artifact.manifest_id != source.manifest_id
            {
                return Err(format!(
                    "Depot {} provider revision differs from public manifest {}: source provides {}. The source and public metadata refer to different versions.",
                    source.depot_id, source.manifest_id, artifact.manifest_id
                )
                .into());
            }
            download_groups
                .entry(source.download_app_id)
                .or_default()
                .push((*artifact).clone());
            selected.push((*artifact).clone());
        }
    }
    selected.sort_by_key(|depot| depot.depot_id);
    for depots in download_groups.values_mut() {
        depots.sort_by_key(|depot| depot.depot_id);
    }
    Ok(ResolvedDepotDownloads {
        groups: download_groups.into_iter().collect(),
        depots: selected,
    })
}

pub fn install_steam_dlc(game: &Game, data_dir: &Path, dlc_id: u32) -> Result<usize> {
    validate_id(&game.id)?;
    if dlc_id == 0 {
        return Err("DLC App ID is missing".into());
    }
    let app_id = game.steam_app_id.ok_or("This game has no Steam App ID")?;
    let root = data_dir
        .join("dlc-downloads")
        .join(&game.id)
        .join(dlc_id.to_string());
    let payload = root.join("payload");
    let job_id = job_id_for_target(&payload);
    let _active_worker = crate::jobs::lock_dlc_worker(data_dir, &job_id)?;
    prepare_dlc_retry(data_dir, &payload, &job_id)?;
    let job = DownloadJob {
        id: job_id.clone(),
        kind: JobKind::Dlc,
        url: format!("steam://{app_id}/dlc/{dlc_id}"),
        target: payload.clone(),
        status: JobStatus::Running,
        pid: Some(std::process::id()),
        error: None,
        updated_unix: now()?,
        expected_sha256: None,
        install: Some(InstallPlan {
            game_id: game.id.clone(),
            title: format!("{} DLC {dlc_id}", game.title),
            kind: InstallKind::Archive,
        }),
        install_state: Some(InstallState::Pending),
        install_error: None,
        progress_bytes: Some(0),
        total_bytes: None,
    };
    save_job(data_dir, &job)?;
    let run = (|| -> Result<usize> {
        let receipt = crate::steam_content::load_base_receipt(data_dir, &game.id)?;
        let metadata = crate::steam_metadata::fetch_appinfo(app_id)?;
        crate::jobs::check_dlc_cancelled(data_dir, &job_id)?;
        let (source_metadata, selected_ids) = if metadata
            .depots
            .iter()
            .any(|depot| depot.dlc_app_id == Some(dlc_id))
        {
            let selected_ids = metadata
                .depots
                .iter()
                .filter(|depot| depot.dlc_app_id == Some(dlc_id))
                .filter(|depot| crate::steam_content::matches_target(depot, "windows", "english"))
                .map(|depot| depot.depot_id)
                .collect::<BTreeSet<_>>();
            (metadata.clone(), selected_ids)
        } else {
            let child = crate::steam_metadata::fetch_appinfo(dlc_id)?;
            if child.parent_app_id != Some(app_id) {
                return Err("DLC metadata does not identify the installed parent game".into());
            }
            let selected_ids = child
                .depots
                .iter()
                .filter(|depot| depot.dlc_app_id.is_none_or(|id| id == dlc_id))
                .filter(|depot| crate::steam_content::matches_target(depot, "windows", "english"))
                .map(|depot| depot.depot_id)
                .collect::<BTreeSet<_>>();
            (child, selected_ids)
        };
        if selected_ids.is_empty() {
            return Err(format!(
                "Steam metadata does not identify a Windows/English depot for DLC {dlc_id}"
            )
            .into());
        }
        let downloads = resolve_dlc_download_groups(&root, &source_metadata, &selected_ids)?;
        crate::jobs::check_dlc_cancelled(data_dir, &job_id)?;
        let manifests = downloads
            .depots
            .iter()
            .map(|depot| {
                crate::steam_manifest::read_manifest_with_key(
                    &depot.manifest_path,
                    depot.depot_id,
                    depot.manifest_id,
                    &depot.depot_key,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        crate::steam_manifest::validate_manifest_set(&manifests)?;
        let total_bytes = manifests
            .iter()
            .flat_map(|manifest| &manifest.files)
            .try_fold(0_u64, |total, file| {
                total.checked_add(file.size).ok_or("DLC size exceeds u64")
            })?;
        let check_lock = crate::dlc::lock_game(data_dir, &game.id)?;
        if !crate::dlc::list_interrupted(game, data_dir)?.is_empty() {
            return Err("Recover interrupted DLC before changing game content".into());
        }
        let mut all_present = true;
        for manifest in &manifests {
            if !crate::steam_manifest::verify_installed(manifest, &game.install_dir)?.complete() {
                all_present = false;
            }
            crate::jobs::check_dlc_cancelled(data_dir, &job_id)?;
        }
        if all_present {
            crate::steam_content::save_observation(
                data_dir,
                &crate::steam_content::ObservedDlcReceipt {
                    game_id: game.id.clone(),
                    app_id,
                    dlc_id,
                    depots: downloads
                        .depots
                        .iter()
                        .map(|depot| crate::steam_content::DepotRevision {
                            depot_id: depot.depot_id,
                            manifest_id: depot.manifest_id.to_string(),
                        })
                        .collect(),
                    verified_unix: now()?,
                },
            )?;
            return Ok(0);
        }
        crate::steam_content::clear_observation(data_dir, &game.id, dlc_id)?;
        let receipt = match receipt.as_ref() {
            Some(receipt) => receipt.clone(),
            None => {
                let verified = verify_legacy_base_with(
                    game,
                    &root,
                    &metadata,
                    crate::steam_metadata::fetch_appinfo,
                    fetch_provider_bundle,
                )?;
                crate::steam_content::save_base_receipt(data_dir, &verified)?;
                verified
            }
        };
        crate::steam_content::validate_base_revision(&receipt, &metadata)?;
        let base_manifests = resolve_base_manifests_with(
            &root.join("base-verification-provider"),
            &metadata,
            crate::steam_metadata::fetch_appinfo,
            fetch_provider_bundle,
        )?;
        crate::steam_manifest::check_additive_install(&manifests, &game.install_dir)?;
        crate::jobs::check_dlc_cancelled(data_dir, &job_id)?;
        let mut job = load_job(data_dir, &job_id)?;
        job.total_bytes = (total_bytes > 0).then_some(total_bytes);
        save_job(data_dir, &job)?;
        fs::create_dir_all(&payload)?;
        drop(check_lock);
        (|| -> Result<usize> {
            stage_verified_dlc_groups_with(
                data_dir,
                &job_id,
                &payload,
                &downloads,
                &manifests,
                total_bytes,
                |group_dir, content_app_id, group, span| {
                    download_steam_depots_with_progress(
                        group_dir,
                        content_app_id,
                        group,
                        data_dir,
                        &job_id,
                        span,
                    )
                },
            )?;
            commit_steam_dlc(
                game,
                data_dir,
                dlc_id,
                &payload,
                &receipt,
                &base_manifests,
                &job_id,
            )
        })()
    })();
    let cancelled = load_job(data_dir, &job_id).is_ok_and(|job| job.status == JobStatus::Cancelled)
        || data_dir
            .join("jobs")
            .join(format!("{job_id}.cancel"))
            .exists();
    let cleanup = if run.is_ok() || cancelled {
        remove_staging_dir(&root)
    } else {
        Ok(())
    };
    let cleanup_failed = cleanup.is_err();
    let run = match (run, cleanup) {
        (Ok(count), Ok(())) => Ok(count),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => {
            Err(format!("DLC installed, but staging cleanup failed: {error}").into())
        }
        (Err(error), Err(cleanup_error)) => {
            Err(format!("{error}; staging cleanup failed: {cleanup_error}").into())
        }
    };
    let error = run.as_ref().err().map(ToString::to_string);
    let cancelled = crate::jobs::finish_dlc_job(data_dir, &job_id, error.as_deref())?;
    finish_stopped_dlc(run, cancelled, cleanup_failed)
}

fn finish_stopped_dlc(run: Result<usize>, cancelled: bool, cleanup_failed: bool) -> Result<usize> {
    if cancelled && !cleanup_failed {
        return Err(crate::jobs::TransferStopped::Cancelled(None).into());
    }
    run
}

fn prepare_dlc_retry(data_dir: &Path, payload: &Path, job_id: &str) -> Result<()> {
    crate::jobs::clear_cancel(data_dir, job_id)?;
    let root = payload
        .parent()
        .ok_or("DLC staging payload has no parent")?;
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("DLC staging root is not a regular directory".into());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err("DLC retry scratch contains a symbolic link".into());
        }
        let preserve_group = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("group-"))
            .and_then(|id| id.parse::<u32>().ok())
            .is_some_and(|id| id > 0);
        let preserve_revision = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("group-"))
            .and_then(|name| name.strip_suffix(".resume.json"))
            .and_then(|id| id.parse::<u32>().ok())
            .is_some_and(|id| id > 0);
        if preserve_group && metadata.is_dir() {
            continue;
        }
        if preserve_revision && metadata.is_file() {
            continue;
        }
        if metadata.is_dir() {
            remove_staging_dir(&path)?;
        } else if metadata.is_file() {
            fs::remove_file(path)?;
        } else {
            return Err("DLC retry scratch contains a non-regular entry".into());
        }
    }
    Ok(())
}

fn remove_staging_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)?;
        }
        Ok(_) => return Err("DLC staging payload is not a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Removes any files from staging that are not listed in the expected depot manifests.
pub fn prune_unlisted_staged_files(
    manifests: &[crate::steam_manifest::DepotManifest],
    staged_dir: &Path,
) -> Result<()> {
    match fs::symlink_metadata(staged_dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err("Depot staging root is not a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let allowed = crate::steam_manifest::expected_files(manifests)?;
    let root = fs::canonicalize(staged_dir)?;
    let mut staged = BTreeSet::new();
    crate::steam_manifest::collect_staged_files(&root, Path::new(""), &mut staged)?;
    for extra in staged.difference(&allowed) {
        let path = root.join(extra);
        fs::remove_file(&path)?;
    }
    Ok(())
}

fn stage_verified_dlc_groups_with(
    data_dir: &Path,
    job_id: &str,
    payload: &Path,
    downloads: &ResolvedDepotDownloads,
    manifests: &[crate::steam_manifest::DepotManifest],
    total_bytes: u64,
    mut download: impl FnMut(&Path, u32, &[DepotInfo], DownloadProgressSpan) -> Result<()>,
) -> Result<()> {
    let root = payload
        .parent()
        .ok_or("DLC staging payload has no parent")?;
    let mut completed_bytes = 0_u64;
    for (content_app_id, group) in &downloads.groups {
        crate::jobs::check_dlc_cancelled(data_dir, job_id)?;
        let group_dir = root.join(format!("group-{content_app_id}"));
        let revision_path = root.join(format!("group-{content_app_id}.resume.json"));
        let mut revision = group
            .iter()
            .map(|depot| {
                Ok((
                    depot.depot_id,
                    depot.manifest_id,
                    crate::sha256::file_sha256(&depot.manifest_path)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        revision.sort();
        let previous_revision = fs::read(&revision_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<(u32, u64, String)>>(&bytes).ok());
        if previous_revision.as_ref() != Some(&revision) {
            remove_staging_dir(&group_dir)?;
            fs::create_dir_all(&group_dir)?;
            fs::write(&revision_path, serde_json::to_vec(&revision)?)?;
        }
        let group_manifests = group
            .iter()
            .map(|depot| {
                crate::steam_manifest::read_manifest_with_key(
                    &depot.manifest_path,
                    depot.depot_id,
                    depot.manifest_id,
                    &depot.depot_key,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let reusable = fs::symlink_metadata(&group_dir)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
            && crate::steam_manifest::verify_staged_exact(&group_manifests, &group_dir).is_ok();
        if !reusable {
            fs::create_dir_all(&group_dir)?;
            prune_unlisted_staged_files(&group_manifests, &group_dir)?;
            download(
                &group_dir,
                *content_app_id,
                group,
                DownloadProgressSpan {
                    completed_bytes,
                    overall_bytes: (total_bytes > 0).then_some(total_bytes),
                },
            )?;
            normalize_windows_paths(&group_dir)?;
            prune_unlisted_staged_files(&group_manifests, &group_dir)?;
        }
        crate::steam_manifest::verify_staged_exact(&group_manifests, &group_dir)?;
        let group_bytes = group_manifests
            .iter()
            .flat_map(|manifest| &manifest.files)
            .try_fold(0_u64, |total, file| {
                total
                    .checked_add(file.size)
                    .ok_or("DLC group size exceeds u64")
            })?;
        completed_bytes = completed_bytes
            .checked_add(group_bytes)
            .ok_or("DLC size exceeds u64")?;
        let mut job = load_job(data_dir, job_id)?;
        job.progress_bytes = Some(job.progress_bytes.unwrap_or(0).max(completed_bytes));
        save_job(data_dir, &job)?;
        merge_verified_payload(&group_dir, payload)?;
    }
    crate::steam_manifest::verify_staged_exact(manifests, payload)
}

fn commit_steam_dlc(
    game: &Game,
    data_dir: &Path,
    dlc_id: u32,
    payload: &Path,
    expected_base: &crate::steam_content::BaseContentReceipt,
    base_manifests: &[(DepotInfo, crate::steam_manifest::DepotManifest)],
    job_id: &str,
) -> Result<usize> {
    crate::dlc::install_downloaded_staged_with_guard(
        game,
        data_dir,
        &dlc_id.to_string(),
        payload,
        || {
            let current = crate::steam_content::load_base_receipt(data_dir, &game.id)?;
            if current.as_ref() != Some(expected_base) {
                return Err(
                "Base game revision changed during the DLC download; retry against the current base"
                    .into(),
            );
            }
            verify_base_manifest_files(game, base_manifests)?;
            crate::jobs::begin_dlc_commit(data_dir, job_id)
        },
    )
}

fn merge_verified_payload(source: &Path, target: &Path) -> Result<()> {
    let source_meta = fs::symlink_metadata(source)?;
    if !source_meta.is_dir() || source_meta.file_type().is_symlink() {
        return Err("DLC staging group is not a regular directory".into());
    }
    match fs::symlink_metadata(target) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err("Combined DLC staging path is not a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(target)?,
        Err(error) => return Err(error.into()),
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.file_type().is_symlink() {
            return Err("DLC staging group contains a symbolic link".into());
        }
        if metadata.is_dir() {
            merge_verified_payload(&from, &to)?;
        } else if metadata.is_file() {
            match fs::symlink_metadata(&to) {
                Ok(existing) if !existing.is_file() || existing.file_type().is_symlink() => {
                    return Err("Combined DLC staging path is not a regular file".into());
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            fs::copy(&from, &to)?;
        } else {
            return Err("DLC staging group contains a non-regular file".into());
        }
    }
    Ok(())
}

fn publish_verified_base(
    stage: &Path,
    target: &Path,
    manifests: &[crate::steam_manifest::DepotManifest],
) -> Result<()> {
    crate::steam_manifest::verify_staged_exact(manifests, stage)?;
    if fs::symlink_metadata(target).is_ok() {
        fs::remove_dir(target)?;
    }
    fs::rename(stage, target)?;
    Ok(())
}

fn save_new_base_receipt_with(
    target: &Path,
    manifests: &[crate::steam_manifest::DepotManifest],
    save: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if let Err(error) = save() {
        let rollback = crate::steam_manifest::verify_staged_exact(manifests, target)
            .and_then(|_| remove_staging_dir(target));
        return match rollback {
            Ok(()) => Err(format!("Base receipt could not be saved; verified new files were removed: {error}").into()),
            Err(rollback_error) => Err(format!("Base receipt could not be saved: {error}; published files were retained because safe rollback failed: {rollback_error}").into()),
        };
    }
    Ok(())
}

fn finish_base_install<T>(result: Result<T>, scratch: &Path) -> Result<T> {
    let cancelled = result.as_ref().err().is_some_and(|error| {
        matches!(
            error.downcast_ref::<crate::jobs::TransferStopped>(),
            Some(crate::jobs::TransferStopped::Cancelled(_))
        )
    });
    let cleanup = if result.is_ok() || cancelled {
        remove_staging_dir(scratch)
    } else {
        Ok(())
    };
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => {
            Err(format!("Base game installed, but scratch cleanup failed: {error}").into())
        }
        (Err(error), Err(cleanup_error)) => {
            Err(format!("{error}; scratch cleanup failed: {cleanup_error}").into())
        }
    }
}

fn prepare_base_scratch(data_dir: &Path, scratch: &Path) -> Result<()> {
    let root = data_dir.join("downloads");
    if fs::symlink_metadata(&root).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("Download scratch root is a symbolic link".into());
    }
    fs::create_dir_all(&root)?;
    match fs::symlink_metadata(scratch) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err("Steam download scratch is not a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(scratch)?,
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Pin a complete revision set before allowing any base transfer cache to be reused.
fn pin_base_download(root: &Path, depots: &[DepotInfo]) -> Result<()> {
    let mut revisions = depots
        .iter()
        .map(|depot| {
            Ok((
                depot.depot_id,
                depot.manifest_id,
                crate::sha256::file_sha256(&depot.manifest_path)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    revisions.sort();
    let target = root.join("base.resume.json");
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
            let previous: Vec<(u32, u64, String)> = serde_json::from_slice(&fs::read(&target)?)?;
            if previous != revisions {
                return Err("Steam base revision changed since this download started. Cancel the old transfer before starting a different revision".into());
            }
        }
        Ok(_) => return Err("Steam base revision journal is not a regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if fs::read_dir(root)?.any(|entry| {
                entry.is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("group-"))
            }) {
                return Err("Cannot resume unpinned Steam depot groups. Cancel the old transfer and start again".into());
            }
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            use std::io::Write;
            output.write_all(&serde_json::to_vec(&revisions)?)?;
            output.sync_all()?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Perform end-to-end installation of a Steam manifest-based catalog game:
/// downloads exact manifests, invokes the native engine, verifies files and
/// registers the installation while reporting background progress.
pub fn install_steam_manifest_game(
    library: &mut Library,
    data_dir: &Path,
    game: &CatalogGame,
    app_id: u32,
) -> Result<InstallOutcome> {
    install_steam_manifest_game_in(library, data_dir, &data_dir.join("games"), game, app_id)
}

/// Install a manifest game into a selected library drive.
pub fn install_steam_manifest_game_in(
    library: &mut Library,
    data_dir: &Path,
    games_dir: &Path,
    game: &CatalogGame,
    app_id: u32,
) -> Result<InstallOutcome> {
    validate_id(&game.id)?;

    // 1. If installed in local Steam, import directly
    if let Ok(root) = steam_root()
        && scan_steam(&root)?
            .iter()
            .any(|candidate| candidate.app_id == app_id && candidate.installed)
    {
        let id = import_steam(library, &root, app_id)?;
        stamp_origin(library, &id, game);
        return Ok(InstallOutcome::Installed(id));
    }
    let _game_lock = crate::dlc::lock_game(data_dir, &game.id)?;
    let job_id = job_id_for_target(&games_dir.join(&game.id));
    let _worker_lock = crate::jobs::acquire_lock(data_dir, &job_id)?;

    // 2. Prepare downloads directory
    let download_dir = data_dir
        .join("downloads")
        .join(format!("steam-{app_id}-{}", game.id));
    let result = install_steam_manifest_game_in_inner(
        library,
        data_dir,
        games_dir,
        game,
        app_id,
        &download_dir,
    );
    finish_base_install(result, &download_dir)
}

fn install_steam_manifest_game_in_inner(
    library: &mut Library,
    data_dir: &Path,
    games_dir: &Path,
    game: &CatalogGame,
    app_id: u32,
    download_dir: &Path,
) -> Result<InstallOutcome> {
    prepare_base_scratch(data_dir, download_dir)?;

    let unpack_dir = fetch_provider_bundle(download_dir, app_id)?;

    // 4. Parse depots from Lua manifests
    let appinfo = crate::steam_metadata::fetch_appinfo(app_id)?;
    let selected_ids = appinfo
        .depots
        .iter()
        .filter(|depot| depot.dlc_app_id.is_none())
        .filter(|depot| crate::steam_content::matches_target(depot, "windows", "english"))
        .map(|depot| depot.depot_id)
        .collect::<std::collections::BTreeSet<_>>();
    let downloads = resolve_download_groups_with(
        download_dir,
        &appinfo,
        &selected_ids,
        ManifestRevisionPolicy::ProviderPinnedBase,
        crate::steam_metadata::fetch_appinfo,
        |root, manifest_app_id| {
            if manifest_app_id == app_id {
                Ok(unpack_dir.clone())
            } else {
                fetch_provider_bundle(root, manifest_app_id)
            }
        },
    )?;
    let manifests = downloads
        .depots
        .iter()
        .map(|depot| {
            crate::steam_manifest::read_manifest_with_key(
                &depot.manifest_path,
                depot.depot_id,
                depot.manifest_id,
                &depot.depot_key,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    crate::steam_manifest::validate_manifest_set(&manifests)?;
    let total_bytes = manifests
        .iter()
        .flat_map(|manifest| &manifest.files)
        .try_fold(0_u64, |total, file| {
            total
                .checked_add(file.size)
                .ok_or("Base game size exceeds u64")
        })?;

    pin_base_download(download_dir, &downloads.depots)?;

    // 6. Target installation directory
    fs::create_dir_all(games_dir)?;
    let target_dir = games_dir.join(&game.id);
    match fs::symlink_metadata(&target_dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            if fs::read_dir(&target_dir)?.next().is_some() {
                return Err("Base game target already contains files; choose an empty installation directory".into());
            }
        }
        Ok(_) => return Err("Base game target is not a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let base_stage = games_dir.join(format!(
        ".{}-{}-{nonce}.staging",
        game.id,
        std::process::id()
    ));

    // 7. Register and track DownloadJob in transfers drawer
    let job_id = job_id_for_target(&target_dir);
    let mut job = DownloadJob {
        id: job_id.clone(),
        kind: JobKind::Game,
        url: format!("steam://{app_id}"),
        target: target_dir.clone(),
        status: JobStatus::Running,
        pid: Some(std::process::id()),
        error: None,
        updated_unix: now()?,
        expected_sha256: None,
        install: Some(InstallPlan {
            game_id: game.id.clone(),
            title: game.title.clone(),
            kind: InstallKind::Archive,
        }),
        install_state: Some(InstallState::Pending),
        install_error: None,
        progress_bytes: Some(0),
        total_bytes: (total_bytes > 0).then_some(total_bytes),
    };
    save_job(data_dir, &job)?;

    // 8. Fetch each content App ID separately and merge only verified files.
    let dl_result = (|| -> Result<()> {
        fs::create_dir_all(&base_stage)?;
        let mut completed_bytes = 0_u64;
        for (content_app_id, group) in &downloads.groups {
            let group_dir = download_dir.join(format!("group-{content_app_id}"));
            fs::create_dir_all(&group_dir)?;
            let group_manifests = group
                .iter()
                .map(|depot| {
                    crate::steam_manifest::read_manifest_with_key(
                        &depot.manifest_path,
                        depot.depot_id,
                        depot.manifest_id,
                        &depot.depot_key,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let reusable = fs::symlink_metadata(&group_dir)
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
                && crate::steam_manifest::verify_staged_exact(&group_manifests, &group_dir).is_ok();
            if !reusable {
                prune_unlisted_staged_files(&group_manifests, &group_dir)?;
                download_steam_depots_with_progress(
                    &group_dir,
                    *content_app_id,
                    group,
                    data_dir,
                    &job_id,
                    DownloadProgressSpan {
                        completed_bytes,
                        overall_bytes: (total_bytes > 0).then_some(total_bytes),
                    },
                )?;
                normalize_windows_paths(&group_dir)?;
                prune_unlisted_staged_files(&group_manifests, &group_dir)?;
            }
            crate::steam_manifest::verify_staged_exact(&group_manifests, &group_dir)?;
            merge_verified_payload(&group_dir, &base_stage)?;
            let group_bytes = group_manifests
                .iter()
                .flat_map(|manifest| &manifest.files)
                .try_fold(0_u64, |total, file| {
                    total
                        .checked_add(file.size)
                        .ok_or("Base game size exceeds u64")
                })?;
            completed_bytes = completed_bytes
                .checked_add(group_bytes)
                .ok_or("Base game size exceeds u64")?;
            let mut progress = load_job(data_dir, &job_id)?;
            progress.progress_bytes =
                Some(progress.progress_bytes.unwrap_or(0).max(completed_bytes));
            save_job(data_dir, &progress)?;
        }
        let mut committing = load_job(data_dir, &job_id)?;
        if data_dir
            .join("jobs")
            .join(format!("{job_id}.cancel"))
            .exists()
        {
            committing.status = JobStatus::Cancelled;
            save_job(data_dir, &committing)?;
            crate::jobs::clear_cancel(data_dir, &job_id)?;
            return Err(crate::jobs::TransferStopped::Cancelled(Some(job_id.clone())).into());
        }
        if data_dir
            .join("jobs")
            .join(format!("{job_id}.pause"))
            .exists()
        {
            committing.status = JobStatus::Paused;
            committing.pid = None;
            save_job(data_dir, &committing)?;
            crate::jobs::clear_pause(data_dir, &job_id)?;
            return Err(crate::jobs::TransferStopped::Paused(job_id.clone()).into());
        }
        committing.install_state = Some(InstallState::Installing);
        save_job(data_dir, &committing)?;
        publish_verified_base(&base_stage, &target_dir, &manifests)?;
        Ok(())
    })();
    if dl_result.is_err() {
        let _ = remove_staging_dir(&base_stage);
        let is_cancelled =
            load_job(data_dir, &job_id).is_ok_and(|j| j.status == JobStatus::Cancelled);
        if is_cancelled {
            let _ = remove_staging_dir(&base_stage);
            for (content_app_id, _) in &downloads.groups {
                let _ = remove_staging_dir(&download_dir.join(format!("group-{content_app_id}")));
            }
        }
    }
    if let Err(e) = dl_result {
        if let Ok(mut j) = load_job(data_dir, &job_id)
            && j.status == JobStatus::Running
        {
            j.status = JobStatus::Failed;
            j.error = Some(e.to_string());
            let _ = save_job(data_dir, &j);
        }
        return Err(e);
    }
    let receipt = crate::steam_content::BaseContentReceipt {
        game_id: game.id.clone(),
        app_id,
        depots: downloads
            .depots
            .iter()
            .map(|depot| crate::steam_content::DepotRevision {
                depot_id: depot.depot_id,
                manifest_id: depot.manifest_id.to_string(),
            })
            .collect(),
        origin: crate::steam_content::BaseReceiptOrigin::ManagedInstall,
    };
    if let Err(error) = save_new_base_receipt_with(&target_dir, &manifests, || {
        crate::steam_content::save_base_receipt(data_dir, &receipt)
    }) {
        if let Ok(mut failed) = load_job(data_dir, &job_id) {
            failed.status = JobStatus::Failed;
            failed.error = Some(error.to_string());
            failed.updated_unix = now().unwrap_or_default();
            let _ = save_job(data_dir, &failed);
        }
        return Err(error);
    }

    // 9. Normalize backslash paths on Linux
    let _ = normalize_windows_paths(&target_dir);

    // 11. Import into Library
    let import_result = discovery::import_game(library, &game.id, &game.title, &target_dir);
    match import_result {
        Ok(_) => {
            stamp_origin(library, &game.id, game);
            job.status = JobStatus::Complete;
            job.install_state = Some(InstallState::Complete);
            job.updated_unix = now().unwrap_or(0);
            let _ = save_job(data_dir, &job);
            Ok(InstallOutcome::Installed(game.id.clone()))
        }
        Err(_) => {
            job.status = JobStatus::Complete;
            job.install_state = Some(InstallState::NeedsSelection);
            job.updated_unix = now().unwrap_or(0);
            let _ = save_job(data_dir, &job);
            save_pending_setup_origin(data_dir, &game.id, game)?;
            Ok(InstallOutcome::NeedsSelection(game.id.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam_access::validate_manifest_archive;
    use sha1::{Digest, Sha1};

    #[test]
    fn fresh_base_uses_provider_revision_while_dlc_requires_public_revision() {
        let root = std::env::temp_dir().join(format!(
            "azdl-base-revision-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bundle = root.join("provider");
        fs::create_dir_all(&bundle).unwrap();
        let source_revision = 5181693942702269240_u64;
        let public_revision = 4133263528433366014_u64;
        fs::write(
            bundle.join("5335510.lua"),
            format!(
                "addappid(5335511,0,\"{}\")\nsetManifestid(5335511,\"{source_revision}\")",
                "00".repeat(32)
            ),
        )
        .unwrap();
        fs::write(
            bundle.join(format!("5335511_{source_revision}.manifest")),
            b"selection fixture",
        )
        .unwrap();
        let mut metadata = crate::steam_metadata::parse_appinfo(
            &serde_json::to_vec(&serde_json::json!({"data":{"5335510":{"depots":{
                "5335511":{"manifests":{"public":{"gid":public_revision.to_string()}}}
            }}}}))
            .unwrap(),
            5335510,
        )
        .unwrap();
        let selected = BTreeSet::from([5335511]);
        let base = resolve_download_groups_with(
            &root,
            &metadata,
            &selected,
            ManifestRevisionPolicy::ProviderPinnedBase,
            |_| panic!("no shared app lookup expected"),
            |_, _| Ok(bundle.clone()),
        )
        .unwrap();
        assert_eq!(base.depots[0].manifest_id, source_revision);
        assert_eq!(base.groups[0].1[0].manifest_id, source_revision);

        let error = resolve_dlc_download_groups_with(
            &root,
            &metadata,
            &selected,
            |_| panic!("no shared app lookup expected"),
            |_, _| Ok(bundle.clone()),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(&source_revision.to_string()), "{error}");
        assert!(error.contains(&public_revision.to_string()), "{error}");

        fs::rename(
            bundle.join(format!("5335511_{source_revision}.manifest")),
            bundle.join(format!("5335511_{public_revision}.manifest")),
        )
        .unwrap();
        let error = resolve_download_groups_with(
            &root,
            &metadata,
            &selected,
            ManifestRevisionPolicy::ProviderPinnedBase,
            |_| panic!("no shared app lookup expected"),
            |_, _| Ok(bundle.clone()),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("No exact manifest"), "{error}");

        metadata.depots[0].dlc_app_id = Some(42);
        let error = resolve_download_groups_with(
            &root,
            &metadata,
            &selected,
            ManifestRevisionPolicy::ProviderPinnedBase,
            |_| panic!("no shared app lookup expected"),
            |_, _| panic!("DLC role must be rejected before loading the provider"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("cannot select DLC depots"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires a saved appinfo response and extracted provider bundle"]
    fn live_base_provider_matches_windows_manifest() -> Result<()> {
        let app_id: u32 = std::env::var("AZDL_LIVE_BASE_APP_ID")?.parse()?;
        let appinfo_path = PathBuf::from(std::env::var("AZDL_LIVE_BASE_APPINFO")?);
        let manifest_dir = PathBuf::from(std::env::var("AZDL_LIVE_BASE_MANIFEST_DIR")?);
        let metadata = crate::steam_metadata::parse_appinfo(&fs::read(appinfo_path)?, app_id)?;
        let manifests = resolve_base_manifests_with(
            &manifest_dir,
            &metadata,
            |_| Err("Additional appinfo snapshot required".into()),
            |_, requested| {
                if requested == app_id {
                    Ok(manifest_dir.clone())
                } else {
                    Err("Additional provider bundle required".into())
                }
            },
        )?;
        assert!(!manifests.is_empty());
        let files = manifests
            .iter()
            .flat_map(|(_, manifest)| &manifest.files)
            .collect::<Vec<_>>();
        let bytes = files.iter().try_fold(0_u64, |total, file| {
            total
                .checked_add(file.size)
                .ok_or("Manifest size exceeds u64")
        })?;
        println!(
            "Validated {} depots, {} files, {bytes} bytes",
            manifests.len(),
            files.len()
        );
        Ok(())
    }

    #[test]
    fn cancelled_dlc_reports_cleanup_failures() {
        let stopped = finish_stopped_dlc(Err("cancelled".into()), true, false).unwrap_err();
        assert!(
            stopped
                .downcast_ref::<crate::jobs::TransferStopped>()
                .is_some()
        );
        let cleanup =
            finish_stopped_dlc(Err("staging cleanup failed".into()), true, true).unwrap_err();
        assert_eq!(cleanup.to_string(), "staging cleanup failed");
        assert!(
            cleanup
                .downcast_ref::<crate::jobs::TransferStopped>()
                .is_none()
        );
    }

    fn binary_manifest_fixture(
        depot_id: u32,
        manifest_id: u64,
        path: &str,
        body: &[u8],
    ) -> Vec<u8> {
        fn varint(mut value: u64) -> Vec<u8> {
            let mut bytes = Vec::new();
            while value >= 128 {
                bytes.push((value as u8 & 0x7f) | 0x80);
                value >>= 7;
            }
            bytes.push(value as u8);
            bytes
        }
        fn number(field: u8, value: u64) -> Vec<u8> {
            let mut bytes = vec![field << 3];
            bytes.extend(varint(value));
            bytes
        }
        fn field_bytes(field: u8, value: &[u8]) -> Vec<u8> {
            let mut bytes = vec![(field << 3) | 2];
            bytes.extend(varint(value.len() as u64));
            bytes.extend(value);
            bytes
        }
        fn section(magic: u32, payload: &[u8]) -> Vec<u8> {
            let mut bytes = magic.to_le_bytes().to_vec();
            bytes.extend((payload.len() as u32).to_le_bytes());
            bytes.extend(payload);
            bytes
        }
        let mut file = field_bytes(1, path.as_bytes());
        file.extend(number(2, body.len() as u64));
        file.extend(field_bytes(5, &Sha1::digest(body)));
        let mut metadata = number(1, u64::from(depot_id));
        metadata.extend(number(2, manifest_id));
        let mut manifest = section(0x71F617D0, &field_bytes(1, &file));
        manifest.extend(section(0x1F4812BE, &metadata));
        manifest.extend(section(0x1B81B817, &[]));
        manifest.extend(0x32C415AB_u32.to_le_bytes());
        manifest
    }

    #[test]
    fn base_install_cleans_success_but_preserves_failed_transfer_cache() {
        let root = std::env::temp_dir().join(format!(
            "azdl-base-scratch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for success in [true, false] {
            let scratch = root.join(if success { "success" } else { "failure" });
            fs::create_dir_all(scratch.join("group")).unwrap();
            fs::write(scratch.join("group/partial.bin"), b"partial").unwrap();
            let result = if success {
                finish_base_install(Ok(()), &scratch)
            } else {
                finish_base_install(Err("download failed".into()), &scratch)
            };
            assert_eq!(result.is_ok(), success);
            assert_eq!(scratch.exists(), !success);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn base_install_preserves_interrupted_scratch_before_retry() {
        let root = std::env::temp_dir().join(format!(
            "azdl-base-retry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let scratch = root.join("downloads/steam-100-game");
        fs::create_dir_all(scratch.join("group-100")).unwrap();
        fs::write(scratch.join("group-100/partial.bin"), b"stale").unwrap();
        prepare_base_scratch(&root, &scratch).unwrap();
        assert!(scratch.is_dir());
        assert_eq!(
            fs::read(scratch.join("group-100/partial.bin")).unwrap(),
            b"stale"
        );
        finish_base_install(Ok(()), &scratch).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn base_resume_pins_manifest_bytes_and_rejects_revision_substitution() {
        let root = std::env::temp_dir().join(format!(
            "azdl-base-pin-{}-{}",
            std::process::id(),
            now().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        let manifest = root.join("101_42.manifest");
        fs::write(&manifest, b"manifest").unwrap();
        let mut depots = vec![DepotInfo {
            depot_id: 101,
            manifest_id: 42,
            manifest_path: manifest.clone(),
            depot_key: "key".into(),
        }];
        pin_base_download(&root, &depots).unwrap();
        fs::create_dir(root.join("group-100")).unwrap();
        fs::write(root.join("group-100/chunk.part"), b"partial").unwrap();
        pin_base_download(&root, &depots).unwrap();
        depots[0].manifest_id = 43;
        assert!(
            pin_base_download(&root, &depots)
                .unwrap_err()
                .to_string()
                .contains("revision changed")
        );
        depots[0].manifest_id = 42;
        fs::write(&manifest, b"changed manifest bytes").unwrap();
        assert!(pin_base_download(&root, &depots).is_err());
        assert_eq!(
            fs::read(root.join("group-100/chunk.part")).unwrap(),
            b"partial"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn base_publish_requires_verified_content_and_preserves_existing_target() {
        use crate::steam_manifest::{DepotManifest, ManifestFile};
        use sha1::{Digest, Sha1};

        let root = std::env::temp_dir().join(format!(
            "azdl-base-publish-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let stage = root.join("stage");
        let target = root.join("game");
        fs::create_dir_all(&stage).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(stage.join("base.bin"), b"wrong").unwrap();
        let manifest = DepotManifest {
            depot_id: 101,
            manifest_id: 1,
            files: vec![ManifestFile {
                path: PathBuf::from("base.bin"),
                size: 4,
                sha1: Sha1::digest(b"base").into(),
            }],
        };
        assert!(publish_verified_base(&stage, &target, std::slice::from_ref(&manifest)).is_err());
        assert!(target.is_dir());
        assert!(!target.join("base.bin").exists());

        fs::write(stage.join("base.bin"), b"base").unwrap();
        fs::write(target.join("user-file"), b"preserve").unwrap();
        assert!(publish_verified_base(&stage, &target, std::slice::from_ref(&manifest)).is_err());
        assert_eq!(fs::read(target.join("user-file")).unwrap(), b"preserve");
        fs::remove_file(target.join("user-file")).unwrap();
        publish_verified_base(&stage, &target, std::slice::from_ref(&manifest)).unwrap();
        assert_eq!(fs::read(target.join("base.bin")).unwrap(), b"base");
        assert!(!stage.exists());

        let error = save_new_base_receipt_with(&target, std::slice::from_ref(&manifest), || {
            Err("receipt disk full".into())
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("verified new files were removed"), "{error}");
        assert!(!target.exists());

        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("base.bin"), b"base").unwrap();
        publish_verified_base(&stage, &target, std::slice::from_ref(&manifest)).unwrap();
        fs::write(target.join("base.bin"), b"changed").unwrap();
        let error =
            save_new_base_receipt_with(&target, &[manifest], || Err("receipt disk full".into()))
                .unwrap_err()
                .to_string();
        assert!(error.contains("published files were retained"), "{error}");
        assert_eq!(fs::read(target.join("base.bin")).unwrap(), b"changed");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn separately_downloaded_groups_merge_for_one_verified_commit() {
        use crate::steam_manifest::{DepotManifest, ManifestFile, verify_staged_exact};
        use sha1::{Digest, Sha1};

        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-groups-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = root.join("first");
        let second = root.join("second");
        let combined = root.join("combined");
        fs::create_dir_all(first.join("data")).unwrap();
        fs::create_dir_all(second.join("data")).unwrap();
        fs::write(first.join("data/base.bin"), b"first").unwrap();
        fs::write(second.join("data/addon.bin"), b"second").unwrap();
        let manifest = |depot_id, path: &str, bytes: &[u8]| DepotManifest {
            depot_id,
            manifest_id: u64::from(depot_id),
            files: vec![ManifestFile {
                path: PathBuf::from(path),
                size: bytes.len() as u64,
                sha1: Sha1::digest(bytes).into(),
            }],
        };
        let base = manifest(201, "data/base.bin", b"first");
        let addon = manifest(202, "data/addon.bin", b"second");
        verify_staged_exact(std::slice::from_ref(&base), &first).unwrap();
        verify_staged_exact(std::slice::from_ref(&addon), &second).unwrap();
        merge_verified_payload(&first, &combined).unwrap();
        merge_verified_payload(&second, &combined).unwrap();
        verify_staged_exact(&[base.clone(), addon], &combined).unwrap();

        let game_dir = root.join("game");
        fs::create_dir_all(game_dir.join("data")).unwrap();
        fs::write(game_dir.join("data/game.exe"), b"base game").unwrap();
        let game = Game {
            id: "grouped-dlc-game".into(),
            title: "Grouped DLC game".into(),
            install_dir: game_dir.clone(),
            executable: PathBuf::from("data/game.exe"),
            runner: crate::Runner::Auto,
            arguments: Vec::new(),
            environment: Default::default(),
            last_played_unix: None,
            origin: None,
            steam_app_id: Some(100),
            launch_options: Vec::new(),
            selected_launch_option_id: None,
            cover_url: None,
            banner_url: None,
        };
        assert_eq!(
            crate::dlc::install_staged(&game, &root, "900", &combined).unwrap(),
            2
        );
        assert_eq!(fs::read(game_dir.join("data/base.bin")).unwrap(), b"first");
        assert_eq!(
            fs::read(game_dir.join("data/addon.bin")).unwrap(),
            b"second"
        );
        assert_eq!(
            crate::dlc::list_installed(&game, &root).unwrap(),
            vec!["900"]
        );
        crate::dlc::remove(&game, &root, "900").unwrap();
        assert!(!game_dir.join("data/base.bin").exists());
        assert!(!game_dir.join("data/addon.bin").exists());
        assert_eq!(
            fs::read(game_dir.join("data/game.exe")).unwrap(),
            b"base game"
        );

        let conflict_dir = root.join("conflict");
        fs::create_dir_all(conflict_dir.join("data")).unwrap();
        fs::write(conflict_dir.join("data/base.bin"), b"other").unwrap();
        let conflict = manifest(203, "data/base.bin", b"other");
        verify_staged_exact(std::slice::from_ref(&conflict), &conflict_dir).unwrap();
        merge_verified_payload(&conflict_dir, &combined).unwrap();
        assert!(verify_staged_exact(&[base, conflict], &combined).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grouped_dlc_worker_uses_each_content_app_and_verifies_before_commit() {
        let data_dir = std::env::temp_dir().join(format!(
            "azdl-grouped-dlc-worker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = data_dir.join("dlc-downloads/game/900");
        let payload = root.join("payload");
        let provider = data_dir.join("provider");
        fs::create_dir_all(&payload).unwrap();
        fs::create_dir_all(&provider).unwrap();
        let depot = |id: u32, app_id: u32, name: &str, body: &[u8]| {
            let manifest_path = provider.join(format!("{id}_{app_id}.manifest"));
            fs::write(
                &manifest_path,
                binary_manifest_fixture(id, u64::from(app_id), name, body),
            )
            .unwrap();
            DepotInfo {
                depot_id: id,
                manifest_id: u64::from(app_id),
                manifest_path,
                depot_key: "0123456789abcdef0123456789abcdef".into(),
            }
        };
        let first = depot(201, 100, "data/first.bin", b"first");
        let second = depot(202, 200, "data/second.bin", b"second");
        let downloads = ResolvedDepotDownloads {
            groups: vec![(100, vec![first.clone()]), (200, vec![second.clone()])],
            depots: vec![first, second],
        };
        let manifests = downloads
            .depots
            .iter()
            .map(|depot| {
                crate::steam_manifest::read_manifest_with_key(
                    &depot.manifest_path,
                    depot.depot_id,
                    depot.manifest_id,
                    &depot.depot_key,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        crate::steam_manifest::validate_manifest_set(&manifests).unwrap();
        let job_id = job_id_for_target(&payload);
        save_job(
            &data_dir,
            &DownloadJob {
                id: job_id.clone(),
                kind: JobKind::Dlc,
                url: "steam://100/dlc/900".into(),
                target: payload.clone(),
                status: JobStatus::Running,
                pid: Some(std::process::id()),
                error: None,
                updated_unix: now().unwrap(),
                expected_sha256: None,
                install: None,
                install_state: Some(InstallState::Pending),
                install_error: None,
                progress_bytes: Some(0),
                total_bytes: Some(11),
            },
        )
        .unwrap();
        let mut calls = Vec::new();
        stage_verified_dlc_groups_with(
            &data_dir,
            &job_id,
            &payload,
            &downloads,
            &manifests,
            11,
            |group_dir, app_id, depots, span| {
                calls.push((
                    app_id,
                    depots[0].depot_id,
                    span.completed_bytes,
                    span.overall_bytes,
                ));
                fs::create_dir_all(group_dir.join("data"))?;
                let (name, body): (&str, &[u8]) = match app_id {
                    100 => ("first.bin", b"first"),
                    200 => ("second.bin", b"second"),
                    _ => panic!("unexpected content App ID"),
                };
                fs::write(group_dir.join("data").join(name), body)?;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            calls,
            vec![(100, 201, 0, Some(11)), (200, 202, 5, Some(11))]
        );
        assert_eq!(
            load_job(&data_dir, &job_id).unwrap().progress_bytes,
            Some(11)
        );
        remove_staging_dir(&payload).unwrap();
        fs::create_dir_all(&payload).unwrap();
        fs::write(root.join("group-200/data/second.bin"), b"changed").unwrap();
        let interrupted = stage_verified_dlc_groups_with(
            &data_dir,
            &job_id,
            &payload,
            &downloads,
            &manifests,
            11,
            |group_dir, app_id, _, _| {
                assert_eq!(app_id, 200);
                fs::write(group_dir.join("data/second.bin"), b"sec")?;
                Err("simulated connection failure".into())
            },
        );
        assert!(interrupted.is_err());
        prepare_dlc_retry(&data_dir, &payload, &job_id).unwrap();
        let mut retried = Vec::new();
        stage_verified_dlc_groups_with(
            &data_dir,
            &job_id,
            &payload,
            &downloads,
            &manifests,
            11,
            |group_dir, app_id, depots, span| {
                retried.push((app_id, depots[0].depot_id, span.completed_bytes));
                assert_eq!(fs::read(group_dir.join("data/second.bin"))?, b"sec");
                fs::create_dir_all(group_dir.join("data"))?;
                fs::write(group_dir.join("data/second.bin"), b"second")?;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(retried, vec![(200, 202, 5)]);
        assert_eq!(fs::read(payload.join("data/first.bin")).unwrap(), b"first");
        assert_eq!(
            fs::read(payload.join("data/second.bin")).unwrap(),
            b"second"
        );
        let game_dir = data_dir.join("game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("game.exe"), b"base").unwrap();
        let game = Game {
            id: "game".into(),
            title: "Game".into(),
            install_dir: game_dir.clone(),
            executable: PathBuf::from("game.exe"),
            runner: crate::Runner::Auto,
            arguments: Vec::new(),
            environment: Default::default(),
            last_played_unix: None,
            origin: None,
            steam_app_id: Some(100),
            launch_options: Vec::new(),
            selected_launch_option_id: None,
            cover_url: None,
            banner_url: None,
        };
        let base = crate::steam_content::BaseContentReceipt {
            game_id: game.id.clone(),
            app_id: 100,
            depots: vec![crate::steam_content::DepotRevision {
                depot_id: 101,
                manifest_id: "11".into(),
            }],
            origin: crate::steam_content::BaseReceiptOrigin::ManagedInstall,
        };
        let base_depot = depot(101, 11, "game.exe", b"base");
        let base_manifest = crate::steam_manifest::read_manifest(
            &base_depot.manifest_path,
            base_depot.depot_id,
            base_depot.manifest_id,
        )
        .unwrap();
        let base_manifests = vec![(base_depot, base_manifest)];
        let mut changed = base.clone();
        changed.depots[0].manifest_id = "12".into();
        crate::steam_content::save_base_receipt(&data_dir, &changed).unwrap();
        let error = commit_steam_dlc(
            &game,
            &data_dir,
            900,
            &payload,
            &base,
            &base_manifests,
            &job_id,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("Base game revision changed"), "{error}");
        assert!(!game_dir.join("data/first.bin").exists());
        assert_eq!(
            load_job(&data_dir, &job_id).unwrap().install_state,
            Some(InstallState::Pending)
        );
        crate::steam_content::save_base_receipt(&data_dir, &base).unwrap();
        fs::write(game_dir.join("game.exe"), b"changed").unwrap();
        let error = commit_steam_dlc(
            &game,
            &data_dir,
            900,
            &payload,
            &base,
            &base_manifests,
            &job_id,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("Base depot 101 differs"), "{error}");
        assert!(!game_dir.join("data/first.bin").exists());
        fs::write(game_dir.join("game.exe"), b"base").unwrap();
        assert_eq!(
            commit_steam_dlc(
                &game,
                &data_dir,
                900,
                &payload,
                &base,
                &base_manifests,
                &job_id
            )
            .unwrap(),
            2
        );
        crate::dlc::remove(&game, &data_dir, "900").unwrap();
        assert_eq!(fs::read(game_dir.join("game.exe")).unwrap(), b"base");
        assert!(!game_dir.join("data/first.bin").exists());
        assert!(!game_dir.join("data/second.bin").exists());
        fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn shared_dlc_depots_use_manifest_owner_and_download_app_groups() {
        let root = std::env::temp_dir().join(format!(
            "azdl-shared-dlc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let parent_bundle = root.join("parent");
        let shared_bundle = root.join("shared");
        fs::create_dir_all(&parent_bundle).unwrap();
        fs::create_dir_all(&shared_bundle).unwrap();
        let key = "0123456789abcdef0123456789abcdef";
        fs::write(
            parent_bundle.join("100.lua"),
            format!("addappid(201,0,\"{key}\")\nsetManifestid(201,\"11\")"),
        )
        .unwrap();
        fs::write(parent_bundle.join("201_11.manifest"), b"parent").unwrap();
        fs::write(
            shared_bundle.join("200.lua"),
            format!("addappid(202,0,\"{key}\")\nsetManifestid(202,\"22\")"),
        )
        .unwrap();
        fs::write(shared_bundle.join("202_22.manifest"), b"shared").unwrap();
        let parent = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"100":{"depots":{"201":{"dlcappid":"900","manifests":{"public":{"gid":"11"}}},"202":{"dlcappid":"900","depotfromapp":"200"}}}}}"#,
            100,
        )
        .unwrap();
        let shared = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"200":{"depots":{"202":{"manifests":{"public":{"gid":"22"}}}}}}}"#,
            200,
        )
        .unwrap();
        let ids = [201, 202].into_iter().collect();
        let resolve = || {
            resolve_dlc_download_groups_with(
                &root,
                &parent,
                &ids,
                |id| {
                    assert_eq!(id, 200);
                    Ok(shared.clone())
                },
                |_, id| match id {
                    100 => Ok(parent_bundle.clone()),
                    200 => Ok(shared_bundle.clone()),
                    _ => panic!("unexpected provider App ID"),
                },
            )
        };
        let downloads = resolve().unwrap();
        assert_eq!(downloads.groups.len(), 2);
        assert_eq!(downloads.groups[0].0, 100);
        assert_eq!(downloads.groups[0].1[0].depot_id, 201);
        assert_eq!(downloads.groups[1].0, 200);
        assert_eq!(downloads.groups[1].1[0].depot_id, 202);
        assert_eq!(downloads.depots.len(), 2);

        let mut base = parent.clone();
        for depot in &mut base.depots {
            depot.dlc_app_id = None;
        }
        let base_depots = base
            .depots
            .iter()
            .filter(|depot| depot.dlc_app_id.is_none())
            .map(|depot| depot.depot_id)
            .collect();
        let resolved_base = resolve_dlc_download_groups_with(
            &root,
            &base,
            &base_depots,
            |_| Ok(shared.clone()),
            |_, id| match id {
                100 => Ok(parent_bundle.clone()),
                200 => Ok(shared_bundle.clone()),
                _ => panic!("unexpected provider App ID"),
            },
        )
        .unwrap();
        assert_eq!(resolved_base.depots.len(), 2);
        assert_eq!(resolved_base.depots[1].manifest_id, 22);

        let mut free_parent = parent.clone();
        free_parent.free_to_download = true;
        let free_downloads = resolve_dlc_download_groups_with(
            &root,
            &free_parent,
            &ids,
            |_| Ok(shared.clone()),
            |_, id| match id {
                100 => Ok(parent_bundle.clone()),
                200 => Ok(shared_bundle.clone()),
                _ => panic!("unexpected provider App ID"),
            },
        )
        .unwrap();
        assert_eq!(free_downloads.groups.len(), 1);
        assert_eq!(free_downloads.groups[0].0, 100);
        assert_eq!(free_downloads.groups[0].1.len(), 2);

        fs::write(
            shared_bundle.join("200.lua"),
            format!("addappid(202,0,\"{key}\")\nsetManifestid(202,\"23\")"),
        )
        .unwrap();
        fs::write(shared_bundle.join("202_23.manifest"), b"wrong").unwrap();
        let error = resolve().unwrap_err().to_string();
        assert!(error.contains("provider revision differs"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_base_verifies_parent_and_shared_depot_bytes_before_receipt() {
        let root = std::env::temp_dir().join(format!(
            "azdl-shared-base-verify-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let game_dir = root.join("game");
        let parent_bundle = root.join("parent-provider");
        let shared_bundle = root.join("shared-provider");
        for dir in [&game_dir, &parent_bundle, &shared_bundle] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(game_dir.join("base.bin"), b"base").unwrap();
        fs::write(game_dir.join("shared.bin"), b"shared").unwrap();
        let key = "0123456789abcdef0123456789abcdef";
        fs::write(
            parent_bundle.join("100.lua"),
            format!("addappid(101,0,\"{key}\")\nsetManifestid(101,\"11\")"),
        )
        .unwrap();
        fs::write(
            shared_bundle.join("200.lua"),
            format!("addappid(102,0,\"{key}\")\nsetManifestid(102,\"22\")"),
        )
        .unwrap();
        fs::write(
            parent_bundle.join("101_11.manifest"),
            binary_manifest_fixture(101, 11, "base.bin", b"base"),
        )
        .unwrap();
        fs::write(
            shared_bundle.join("102_22.manifest"),
            binary_manifest_fixture(102, 22, "shared.bin", b"shared"),
        )
        .unwrap();
        let parent = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"100":{"depots":{"101":{"manifests":{"public":{"gid":"11"}}},"102":{"depotfromapp":"200"}}}}}"#,
            100,
        )
        .unwrap();
        let shared = crate::steam_metadata::parse_appinfo(
            br#"{"data":{"200":{"depots":{"102":{"manifests":{"public":{"gid":"22"}}}}}}}"#,
            200,
        )
        .unwrap();
        let game = Game {
            id: "game".into(),
            title: "Game".into(),
            install_dir: game_dir.clone(),
            executable: PathBuf::from("base.bin"),
            runner: crate::Runner::Auto,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            last_played_unix: None,
            origin: None,
            steam_app_id: Some(100),
            launch_options: Vec::new(),
            selected_launch_option_id: None,
            cover_url: None,
            banner_url: None,
        };
        let verify = || {
            verify_legacy_base_with(
                &game,
                &root,
                &parent,
                |id| {
                    assert_eq!(id, 200);
                    Ok(shared.clone())
                },
                |_, id| match id {
                    100 => Ok(parent_bundle.clone()),
                    200 => Ok(shared_bundle.clone()),
                    _ => panic!("unexpected provider App ID"),
                },
            )
        };
        let receipt = verify().unwrap();
        assert_eq!(
            receipt.origin,
            crate::steam_content::BaseReceiptOrigin::VerifiedLegacy
        );
        assert_eq!(
            receipt.depots,
            vec![
                crate::steam_content::DepotRevision {
                    depot_id: 101,
                    manifest_id: "11".into()
                },
                crate::steam_content::DepotRevision {
                    depot_id: 102,
                    manifest_id: "22".into()
                },
            ]
        );
        assert!(!root.join("content-receipts").exists());
        assert_eq!(fs::read(game_dir.join("base.bin")).unwrap(), b"base");
        assert_eq!(fs::read(game_dir.join("shared.bin")).unwrap(), b"shared");

        fs::write(game_dir.join("shared.bin"), b"changed").unwrap();
        let error = verify().unwrap_err().to_string();
        assert!(error.contains("Base depot 102 differs"), "{error}");
        assert!(!root.join("content-receipts").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dlc_retry_discards_provider_payload_and_cancel_marker_but_keeps_groups_and_receipts() {
        let data_dir = std::env::temp_dir().join(format!(
            "azdl-dlc-retry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let payload = data_dir
            .join("dlc-downloads")
            .join("test-game")
            .join("123")
            .join("payload");
        fs::create_dir_all(&payload).unwrap();
        fs::write(payload.join("old-depot.bin"), b"stale").unwrap();
        let scratch = payload.parent().unwrap();
        fs::create_dir_all(scratch.join("provider-456/manifest")).unwrap();
        fs::write(scratch.join("provider-456/456.zip"), b"stale provider").unwrap();
        fs::create_dir_all(scratch.join("group-456")).unwrap();
        fs::write(scratch.join("group-456/partial.bin"), b"partial").unwrap();
        let receipt = data_dir.join("dlc").join("test-game").join("123");
        fs::create_dir_all(&receipt).unwrap();
        fs::write(receipt.join("state.json"), b"journal").unwrap();
        let id = job_id_for_target(&payload);
        let worker = crate::jobs::lock_dlc_worker(&data_dir, &id).unwrap();
        let cancel = data_dir.join("jobs").join(format!("{id}.cancel"));
        fs::write(&cancel, b"").unwrap();

        prepare_dlc_retry(&data_dir, &payload, &id).unwrap();

        assert!(!payload.exists());
        assert!(!scratch.join("provider-456").exists());
        assert_eq!(
            fs::read(scratch.join("group-456/partial.bin")).unwrap(),
            b"partial"
        );
        assert!(!cancel.exists());
        assert_eq!(fs::read(receipt.join("state.json")).unwrap(), b"journal");
        drop(worker);
        fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn provider_error_page_is_reported_as_missing_manifest() {
        let path = std::env::temp_dir().join(format!(
            "azdl-manifest-response-{}-{}.zip",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, br#"{"error":"app not found"}"#).unwrap();
        let error = validate_manifest_archive(&path, 620)
            .unwrap_err()
            .to_string();
        fs::remove_file(&path).unwrap();
        assert!(error.contains("No usable download manifest"));
        assert!(error.contains("620"));
    }

    #[test]
    fn parses_lua_addappid_and_manifest_id() {
        let lua = r#"
            addappid(3892271,0,"90695be6010d5ec2e8890f1d742357316f4d5f74a8a9d22b2ae076b184797167")
            setManifestid(3892271,"4322338465787944513")
            addappid(3892270)
        "#;
        let mut depots = Vec::new();
        let mut manifest_ids = std::collections::HashMap::new();
        parse_lua_content(lua, &mut depots, &mut manifest_ids);

        assert_eq!(depots.len(), 1);
        assert_eq!(depots[0].0, 3892271);
        assert_eq!(
            depots[0].1,
            "90695be6010d5ec2e8890f1d742357316f4d5f74a8a9d22b2ae076b184797167"
        );
        assert_eq!(manifest_ids.get(&3892271), Some(&4322338465787944513));
    }

    #[test]
    fn rejects_another_revision_when_requested_manifest_is_missing() {
        let root = std::env::temp_dir().join(format!(
            "azdl-manifest-selection-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("123.lua"),
            "addappid(456,0,\"0123456789abcdef0123456789abcdef\")\nsetManifestid(456,\"789\")",
        )
        .unwrap();
        fs::write(root.join("456_788.manifest"), b"manifest").unwrap();
        let error = parse_lua_manifests(&root, 123).unwrap_err().to_string();
        assert!(error.contains("depot 456, revision 789"));
        fs::write(root.join("456_789.manifest"), b"manifest").unwrap();
        let selected = parse_lua_manifests(&root, 123).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].manifest_id, 789);
        assert_eq!(selected[0].manifest_path, root.join("456_789.manifest"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unrelated_missing_dlc_manifest_does_not_block_base_selection() {
        let root = std::env::temp_dir().join(format!(
            "azdl-manifest-filter-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let key = "0123456789abcdef0123456789abcdef";
        fs::write(
            root.join("100.lua"),
            format!(
                "addappid(101,0,\"{key}\")\nsetManifestid(101,\"10\")\naddappid(201,0,\"{key}\")\nsetManifestid(201,\"20\")"
            ),
        )
        .unwrap();
        fs::write(root.join("101_10.manifest"), b"base").unwrap();
        let known = [101, 201].into_iter().collect();
        let base = [101].into_iter().collect();
        let selected = parse_lua_manifests_filtered(&root, 100, Some((&base, &known))).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|depot| depot.depot_id)
                .collect::<Vec<_>>(),
            vec![101]
        );
        let dlc = [201].into_iter().collect();
        assert!(
            parse_lua_manifests_filtered(&root, 100, Some((&dlc, &known)))
                .unwrap_err()
                .to_string()
                .contains("depot 201, revision 20")
        );
        fs::write(root.join("100.lua"), format!("addappid(999,0,\"{key}\")")).unwrap();
        assert!(
            parse_lua_manifests_filtered(&root, 100, Some((&base, &known)))
                .unwrap_err()
                .to_string()
                .contains("Provider depot 999 is absent")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalizes_windows_backslashes_into_subdirectories() {
        let temp = std::env::temp_dir().join(format!("azdl-test-norm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let bad_file = temp.join(r"SubFolder\Nested\file.txt");
        let _ = fs::create_dir_all(bad_file.parent().unwrap());
        fs::write(&bad_file, b"content").unwrap();

        normalize_windows_paths(&temp).unwrap();

        let good_file = temp.join("SubFolder").join("Nested").join("file.txt");
        assert!(good_file.is_file());
        assert_eq!(fs::read(&good_file).unwrap(), b"content");

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn prunes_unlisted_files_and_preserves_valid_depot_files() {
        let temp = std::env::temp_dir().join(format!("azdl-test-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(temp.join("data")).unwrap();

        let valid_file = temp.join("data").join("file1.bin");
        let unlisted_file = temp.join("data").join("unlisted.tmp");
        fs::write(&valid_file, b"valid content").unwrap();
        fs::write(&unlisted_file, b"stray download artifact").unwrap();

        let manifests = vec![crate::steam_manifest::DepotManifest {
            depot_id: 100,
            manifest_id: 1,
            files: vec![crate::steam_manifest::ManifestFile {
                path: PathBuf::from("data/file1.bin"),
                size: 13,
                sha1: [0u8; 20],
            }],
        }];

        prune_unlisted_staged_files(&manifests, &temp).unwrap();

        assert!(valid_file.is_file());
        assert!(!unlisted_file.exists());

        let _ = fs::remove_dir_all(&temp);
    }
}
