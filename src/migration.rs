//! Versioned compatibility boundary for the retired launcher extension format.
//!
//! Only launcher-owned metadata and resource roots are touched. Game trees,
//! prefixes, manifests, content receipts and rollback journals are never traversed.
use crate::{Result, validate_id};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub(crate) mod legacy_catalog;

const VERSION: u64 = 1;

fn regular(path: &Path, directory: bool) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta)
            if !meta.file_type().is_symlink()
                && if directory {
                    meta.is_dir()
                } else {
                    meta.is_file()
                } =>
        {
            Ok(true)
        }
        Ok(_) => Err(format!("Migration refuses unsafe path {}", path.display()).into()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn directory(path: &Path) -> Result<()> {
    if !regular(path, true)? {
        fs::create_dir(path)?;
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Option<Value>> {
    if !regular(path, false)? {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

fn publish(path: &Path, value: &Value) -> Result<()> {
    let temporary = path.with_extension("migration-tmp");
    if temporary.exists() {
        regular(&temporary, false)?;
        fs::remove_file(&temporary)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)?;
    Ok(())
}

fn backup(path: &Path, destination: &Path) -> Result<()> {
    if regular(destination, false)? {
        return Ok(());
    }
    let bytes = fs::read(path)?;
    let temporary = destination.with_extension("backup-tmp");
    if temporary.exists() {
        regular(&temporary, false)?;
        fs::remove_file(&temporary)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, destination)?;
    Ok(())
}

/// Migrate once, with an OS file lock released automatically after process failure.
/// Original JSON and obsolete resources remain under `migration-backup-v1`.
/// Publication is idempotent and the version marker is written last.
pub fn migrate(data_dir: &Path) -> Result<()> {
    if !data_dir.exists() {
        return Ok(());
    }
    regular(data_dir, true)?;
    let marker = data_dir.join("state-version.json");
    if let Some(state) = read_json(&marker)? {
        let version = state["version"]
            .as_u64()
            .ok_or("Invalid state schema version")?;
        if version > VERSION {
            return Err("Launcher data requires a newer azdl version".into());
        }
        if version == VERSION {
            return Ok(());
        }
    }
    let lock_path = data_dir.join("migration.lock");
    if lock_path.exists() {
        regular(&lock_path, false)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock()?;
    if read_json(&marker)?.is_some_and(|state| state["version"] == VERSION) {
        return Ok(());
    }
    let backup_dir = data_dir.join("migration-backup-v1");
    directory(&backup_dir)?;
    let settings_path = data_dir.join("settings.json");
    if let Some(mut settings) = read_json(&settings_path)? {
        let object = settings
            .as_object_mut()
            .ok_or("Invalid launcher settings")?;
        object.remove("plugin_store_urls");
        object.remove("favorite_plugin_id");
        backup(&settings_path, &backup_dir.join("settings.json"))?;
        publish(&settings_path, &settings)?;
    }
    let library_path = data_dir.join("library.json");
    let mut installed_paths = Vec::<PathBuf>::new();
    let mut warnings = Vec::new();
    if let Some(mut library) = read_json(&library_path)? {
        let games = library["games"]
            .as_object_mut()
            .ok_or("Invalid library game map")?;
        // Preserve archive DLC resumability before archiving old source catalogs.
        let sources = data_dir.join("content-sources");
        directory(&sources)?;
        for (id, game) in games.iter_mut() {
            validate_id(id)?;
            if let Some(path) = game["install_dir"].as_str() {
                installed_paths.push(PathBuf::from(path));
            }
            if let Some(qualified) = game["origin"]["qualified_id"].as_str()
                && let Some((source, _)) = qualified.split_once(':')
            {
                validate_id(source)?;
                let cache = data_dir.join("catalog-cache");
                if regular(&cache, true)? {
                    let feed = cache.join(format!("{source}.json"));
                    if regular(&feed, false)? {
                        let entries = match legacy_catalog::parse_feed(&fs::read(&feed)?, source) {
                            Ok(entries) => entries,
                            Err(error) => {
                                warnings.push(
                                    serde_json::json!({"file":feed, "error":error.to_string()}),
                                );
                                Vec::new()
                            }
                        };
                        if let Some(entry) = entries
                            .into_iter()
                            .find(|entry| entry.qualified_id() == qualified)
                        {
                            let addons = entry
                                .addons
                                .into_iter()
                                .filter(|addon| addon.kind == crate::catalog::AddonKind::Dlc)
                                .collect::<Vec<_>>();
                            let target = sources.join(format!("{id}.json"));
                            if !regular(&target, false)? {
                                publish(&target, &serde_json::to_value(addons)?)?;
                            }
                        }
                    }
                }
            }
            let app_id = game["steam_app_id"]
                .as_u64()
                .and_then(|id| u32::try_from(id).ok())
                .filter(|id| *id > 0)
                .or_else(|| {
                    game["origin"]["qualified_id"]
                        .as_str()
                        .and_then(|qualified| qualified.split_once(':'))
                        .and_then(|(_, id)| id.strip_prefix("steam-"))
                        .and_then(|id| id.parse::<u32>().ok())
                        .filter(|id| *id > 0)
                });
            if let Some(app_id) = app_id {
                game["steam_app_id"] = app_id.into();
                if game["origin"].is_object() {
                    game["origin"]["qualified_id"] = format!("steam:steam-{app_id}").into();
                }
            }
            if let Some(object) = game.as_object_mut() {
                object.remove("plugin_id");
                object.remove("plugin_install_kind");
                object.remove("plugin_basic_install");
            }
        }
        backup(&library_path, &backup_dir.join("library.json"))?;
        publish(&library_path, &library)?;
    }
    let pending = data_dir.join("setup-pending");
    if regular(&pending, true)? {
        let saved_pending = backup_dir.join("setup-pending");
        directory(&saved_pending)?;
        for entry in fs::read_dir(&pending)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(mut origin) = read_json(&path)? else {
                continue;
            };
            let app_id = origin["qualified_id"]
                .as_str()
                .and_then(|id| id.split_once(':'))
                .and_then(|(_, id)| id.strip_prefix("steam-"))
                .and_then(|id| id.parse::<u32>().ok())
                .filter(|id| *id > 0);
            if let Some(app_id) = app_id {
                backup(
                    &path,
                    &saved_pending.join(path.file_name().ok_or("Invalid pending origin path")?),
                )?;
                origin["qualified_id"] = format!("steam:steam-{app_id}").into();
                publish(&path, &origin)?;
            }
        }
    }
    // Removed package jobs are archived; content jobs retain IDs, targets and partials.
    let jobs = data_dir.join("jobs");
    if regular(&jobs, true)? {
        let saved_jobs = backup_dir.join("jobs");
        directory(&saved_jobs)?;
        for entry in fs::read_dir(&jobs)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(job) = read_json(&path)? else {
                continue;
            };
            if job["kind"] == "plugin-package" {
                let destination = saved_jobs.join(path.file_name().ok_or("Invalid job path")?);
                if destination.exists() {
                    return Err("Conflicting legacy package job backup".into());
                }
                fs::rename(path, destination)?;
            }
        }
    }
    if !warnings.is_empty() {
        publish(
            &backup_dir.join("catalog-warnings.json"),
            &serde_json::to_value(warnings)?,
        )?;
    }
    for name in [
        "plugins",
        "plugin-downloads",
        "plugin-store.json",
        "catalog-cache",
    ] {
        let source = data_dir.join(name);
        let meta = match fs::symlink_metadata(&source) {
            Ok(meta) => meta,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if meta.file_type().is_symlink() {
            return Err(format!("Migration refuses linked resource root {name}").into());
        }
        // Some old installations may point into a resource root. Preserve those roots.
        let canonical = fs::canonicalize(&source)?;
        if installed_paths
            .iter()
            .any(|path| fs::canonicalize(path).is_ok_and(|path| path.starts_with(&canonical)))
        {
            continue;
        }
        let destination = backup_dir.join(name);
        if destination.exists() {
            return Err(format!("Conflicting migration backup for {name}").into());
        }
        fs::rename(source, destination)?;
    }
    publish(&marker, &serde_json::json!({"version": VERSION}))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "azdl-core-migration-{}-{}",
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
    fn existing_library_migration_preserves_files_receipts_jobs_and_repeats() {
        let root = scratch();
        let game_dir = root.join("games/old-id");
        let preserved = [
            "games/old-id/Game_Data/Plugins/x86_64/game.dll",
            "games/old-id/game.exe",
            "prefixes/old-id/user.reg",
            "compatdata/old-id/pfx/user.reg",
            "content-receipts/old-id.json",
            "dlc/old-id/200/manifest.json",
            "patches/old-id/backup/game.exe",
            "downloads/old-id/game.part",
            "dlc-downloads/old-id/200/payload/content.bin",
        ];
        for name in preserved {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, name.as_bytes()).unwrap();
        }
        fs::create_dir_all(root.join("plugins/provider/runtime")).unwrap();
        fs::write(
            root.join("plugins/provider/runtime/required.dll"),
            b"runtime backup",
        )
        .unwrap();
        fs::create_dir_all(root.join("catalog-cache")).unwrap();
        fs::write(root.join("catalog-cache/provider.json"), br#"{"schema":"arachnel.plugin.catalog.v1","entries":[{"id":"steam-100","title":"Base","steamAppId":100,"addons":[{"id":"bonus","title":"Bonus","kind":"dlc","downloadUrl":"https://example.invalid/bonus.zip","filename":"bonus.zip"}]}]}"#).unwrap();
        let original = json!({"games":{"old-id":{
            "id":"old-id", "title":"Base", "steam_app_id":100,
            "install_dir":game_dir, "executable":game_dir.join("game.exe"),
            "runner":{"kind":"wine"}, "arguments":["-windowed"],
            "environment":{"LANG":"fr_FR.UTF-8"}, "banner_url":"https://example.invalid/header.jpg",
            "origin":{"qualified_id":"provider:steam-100", "version":"pinned", "upload_date":null},
            "launch_options":[{"id":"safe","title":"Safe","executable":"game.exe","arguments":["-safe"]}],
            "selected_launch_option_id":"safe", "unknown_preserved_field":"value"
        }}});
        fs::write(
            root.join("library.json"),
            serde_json::to_vec(&original).unwrap(),
        )
        .unwrap();
        fs::write(root.join("settings.json"), br##"{"plugin_store_urls":["https://example.invalid/store"],"favorite_plugin_id":"provider","default_runner":"wine","seed_color":"#123456"}"##).unwrap();
        fs::create_dir_all(root.join("jobs")).unwrap();
        let content_job = br#"{"id":"content","kind":"game","target":"same","status":"interrupted","progress_bytes":32,"install":{"game_id":"old-id"}}"#;
        fs::write(root.join("jobs/content.json"), content_job).unwrap();
        fs::write(
            root.join("jobs/package.json"),
            br#"{"id":"package","kind":"plugin-package","status":"running"}"#,
        )
        .unwrap();
        migrate(&root).unwrap();
        let migrated = read_json(&root.join("library.json")).unwrap().unwrap();
        let mut expected = original.clone();
        expected["games"]["old-id"]["origin"]["qualified_id"] = "steam:steam-100".into();
        assert_eq!(migrated, expected);
        let loaded = crate::load_library(&root).unwrap();
        assert_eq!(loaded.games["old-id"].arguments, ["-windowed"]);
        assert_eq!(
            crate::dlc::available_for_game(&loaded.games["old-id"], &root).unwrap()[0].id,
            "bonus"
        );
        assert!(!root.join("plugins").exists());
        assert!(
            root.join("migration-backup-v1/plugins/provider/runtime/required.dll")
                .is_file()
        );
        assert_eq!(
            read_json(&root.join("migration-backup-v1/library.json"))
                .unwrap()
                .unwrap(),
            original
        );
        let settings = read_json(&root.join("settings.json")).unwrap().unwrap();
        assert!(settings.get("plugin_store_urls").is_none());
        assert!(settings.get("favorite_plugin_id").is_none());
        assert_eq!(settings["default_runner"], "wine");
        assert!(!root.join("jobs/package.json").exists());
        assert!(root.join("migration-backup-v1/jobs/package.json").is_file());
        assert_eq!(
            fs::read(root.join("jobs/content.json")).unwrap(),
            content_job
        );
        let first = fs::read(root.join("library.json")).unwrap();
        migrate(&root).unwrap();
        assert_eq!(fs::read(root.join("library.json")).unwrap(), first);
        // Simulate process death immediately before publishing the schema marker.
        fs::remove_file(root.join("state-version.json")).unwrap();
        migrate(&root).unwrap();
        assert_eq!(fs::read(root.join("library.json")).unwrap(), first);
        for name in preserved {
            assert_eq!(fs::read(root.join(name)).unwrap(), name.as_bytes());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_optional_catalog_is_archived_without_blocking_library_migration() {
        let root = scratch();
        fs::create_dir(root.join("catalog-cache")).unwrap();
        fs::write(root.join("catalog-cache/old.json"), b"broken cache").unwrap();
        fs::write(root.join("library.json"), br#"{"games":{"kept":{"id":"kept","steam_app_id":480,"origin":{"qualified_id":"old:steam-480"}}}}"#).unwrap();
        migrate(&root).unwrap();
        assert_eq!(
            fs::read(root.join("migration-backup-v1/catalog-cache/old.json")).unwrap(),
            b"broken cache"
        );
        assert!(
            root.join("migration-backup-v1/catalog-warnings.json")
                .is_file()
        );
        assert_eq!(
            read_json(&root.join("library.json")).unwrap().unwrap()["games"]["kept"]["id"],
            "kept"
        );
        migrate(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_source_ids_and_pending_origins_migrate_without_changing_game_ids() {
        let root = scratch();
        fs::write(root.join("library.json"), br#"{"games":{"legacy-id":{"id":"legacy-id","origin":{"qualified_id":"steamidra:steam-480","version":"old"}}}}"#).unwrap();
        fs::create_dir(root.join("setup-pending")).unwrap();
        fs::write(
            root.join("setup-pending/other-id.json"),
            br#"{"qualified_id":"steamidra:steam-620","version":"old"}"#,
        )
        .unwrap();
        migrate(&root).unwrap();
        let library = read_json(&root.join("library.json")).unwrap().unwrap();
        assert_eq!(library["games"]["legacy-id"]["id"], "legacy-id");
        assert_eq!(library["games"]["legacy-id"]["steam_app_id"], 480);
        assert_eq!(
            library["games"]["legacy-id"]["origin"]["qualified_id"],
            "steam:steam-480"
        );
        let pending = read_json(&root.join("setup-pending/other-id.json"))
            .unwrap()
            .unwrap();
        assert_eq!(pending["qualified_id"], "steam:steam-620");
        assert_eq!(pending["version"], "old");
        migrate(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_retains_resource_root_containing_installed_game() {
        let root = scratch();
        let game_dir = root.join("plugins/old/game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("game.exe"), b"game").unwrap();
        fs::write(
            root.join("library.json"),
            serde_json::to_vec(&json!({"games":{"kept":{"install_dir":game_dir}}})).unwrap(),
        )
        .unwrap();
        migrate(&root).unwrap();
        assert_eq!(fs::read(game_dir.join("game.exe")).unwrap(), b"game");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn migration_rejects_linked_metadata_without_touching_target() {
        let root = scratch();
        let target = root.join("outside.json");
        fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(&target, root.join("settings.json")).unwrap();
        assert!(
            migrate(&root)
                .unwrap_err()
                .to_string()
                .contains("unsafe path")
        );
        assert_eq!(fs::read(target).unwrap(), b"{}");
        assert!(!root.join("state-version.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_migration_serializes_and_unknown_schema_fails_closed() {
        let root = scratch();
        fs::write(root.join("settings.json"), b"{}").unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let root = &root;
                scope.spawn(move || migrate(root).unwrap());
            }
        });
        publish(&root.join("state-version.json"), &json!({"version":2})).unwrap();
        assert!(
            migrate(&root)
                .unwrap_err()
                .to_string()
                .contains("newer azdl")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
