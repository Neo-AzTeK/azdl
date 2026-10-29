use crate::{Library, Result, save_library};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageDrive {
    pub id: String,
    pub label: String,
    pub path: PathBuf,
    pub is_default: bool,
}

/// Persisted launcher preferences. Unknown fields from older versions are ignored on load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherSettings {
    #[serde(default)]
    pub storage_drives: Vec<StorageDrive>,
    #[serde(default = "default_seed_color")]
    pub seed_color: String,
    #[serde(default = "default_theme_mode")]
    pub theme_mode: String,
}

fn default_seed_color() -> String {
    "#10B981".into()
}

fn default_theme_mode() -> String {
    "dark".into()
}

impl LauncherSettings {
    pub fn default_for(data_dir: &Path) -> Self {
        let default_games = data_dir.join("games");
        Self {
            storage_drives: vec![StorageDrive {
                id: "default".into(),
                label: "Default Library".into(),
                path: default_games,
                is_default: true,
            }],
            seed_color: "#10B981".into(),
            theme_mode: "dark".into(),
        }
    }

    pub fn default_drive(&self) -> Option<&StorageDrive> {
        self.storage_drives
            .iter()
            .find(|d| d.is_default)
            .or_else(|| self.storage_drives.first())
    }
}

pub fn load_settings(data_dir: &Path) -> Result<LauncherSettings> {
    crate::migration::migrate(data_dir)?;
    let path = data_dir.join("settings.json");
    match fs::read(path) {
        Ok(bytes) => {
            let mut settings: LauncherSettings = serde_json::from_slice(&bytes)?;
            if settings.storage_drives.is_empty() {
                settings.storage_drives.push(StorageDrive {
                    id: "default".into(),
                    label: "Default Library".into(),
                    path: data_dir.join("games"),
                    is_default: true,
                });
            }
            Ok(settings)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LauncherSettings::default_for(data_dir))
        }
        Err(error) => Err(error.into()),
    }
}

pub fn save_settings(data_dir: &Path, settings: &LauncherSettings) -> Result<()> {
    fs::create_dir_all(data_dir)?;
    let target = data_dir.join("settings.json");
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = data_dir.join(format!("settings.json.{}.{nonce}.tmp", std::process::id()));
    let mut file = File::create(&temp)?;
    serde_json::to_writer_pretty(&mut file, settings)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(&target)?;
    }
    fs::rename(temp, target)?;
    Ok(())
}

pub fn add_storage_drive(data_dir: &Path, label: &str, drive_path: &Path) -> Result<StorageDrive> {
    if !drive_path.is_dir() {
        fs::create_dir_all(drive_path)?;
    }
    let canonical = fs::canonicalize(drive_path)?;
    let mut settings = load_settings(data_dir)?;

    let id = format!(
        "drive-{}",
        canonical
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "custom".into())
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>()
    );

    let drive_id = if settings.storage_drives.iter().any(|d| d.id == id) {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        format!("{id}-{nonce}")
    } else {
        id
    };

    let drive = StorageDrive {
        id: drive_id,
        label: if label.trim().is_empty() {
            canonical.display().to_string()
        } else {
            label.trim().to_string()
        },
        path: canonical,
        is_default: settings.storage_drives.is_empty(),
    };

    settings.storage_drives.push(drive.clone());
    save_settings(data_dir, &settings)?;
    Ok(drive)
}

pub fn remove_storage_drive(data_dir: &Path, drive_id: &str) -> Result<()> {
    let mut settings = load_settings(data_dir)?;
    let pos = settings
        .storage_drives
        .iter()
        .position(|d| d.id == drive_id)
        .ok_or_else(|| format!("Drive '{drive_id}' not found"))?;

    let removed = settings.storage_drives.remove(pos);
    if removed.is_default && !settings.storage_drives.is_empty() {
        settings.storage_drives[0].is_default = true;
    }
    save_settings(data_dir, &settings)?;
    Ok(())
}

pub fn set_default_drive(data_dir: &Path, drive_id: &str) -> Result<()> {
    let mut settings = load_settings(data_dir)?;
    let found = settings.storage_drives.iter().any(|d| d.id == drive_id);
    if !found {
        return Err(format!("Drive '{drive_id}' not found").into());
    }
    for d in &mut settings.storage_drives {
        d.is_default = d.id == drive_id;
    }
    save_settings(data_dir, &settings)?;
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let entry_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry_type.is_dir() {
            copy_dir_all(&from, &to)?;
        } else if entry_type.is_file() {
            fs::copy(&from, &to)?;
        } else {
            return Err(format!("Cannot copy unsupported game entry: {}", from.display()).into());
        }
    }
    Ok(())
}

pub fn move_game_to_drive(
    data_dir: &Path,
    library: &mut Library,
    game_id: &str,
    target_drive_id: &str,
) -> Result<PathBuf> {
    let settings = load_settings(data_dir)?;
    let target_drive = settings
        .storage_drives
        .iter()
        .find(|d| d.id == target_drive_id)
        .ok_or_else(|| format!("Target drive '{target_drive_id}' not found"))?;

    let game = library
        .games
        .get_mut(game_id)
        .ok_or_else(|| format!("Game '{game_id}' not found in library"))?;

    let _dlc_lock = crate::dlc::lock_game(data_dir, game_id)?;
    if crate::jobs::has_active_dlc_download(data_dir, game_id)? {
        return Err("Wait for the DLC download to finish before moving this game".into());
    }
    crate::dlc::prepare_move(game, data_dir)?;

    let old_dir = &game.install_dir;
    if !old_dir.exists() {
        return Err(format!("Game directory '{}' does not exist", old_dir.display()).into());
    }

    let folder_name = old_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| game_id.to_string());

    let new_dir = target_drive.path.join(&folder_name);
    if new_dir.exists() {
        return Err(format!("Destination '{}' already exists", new_dir.display()).into());
    }

    fs::create_dir_all(&target_drive.path)?;

    // Attempt fs::rename, or fallback to copy and delete if across filesystems
    if fs::rename(old_dir, &new_dir).is_err() {
        if let Err(error) = copy_dir_all(old_dir, &new_dir) {
            let _ = fs::remove_dir_all(&new_dir);
            return Err(error);
        }
        let _ = fs::remove_dir_all(old_dir);
    }

    // Remap executable path relative to the new directory
    if let Ok(rel) = game.executable.strip_prefix(old_dir) {
        game.executable = new_dir.join(rel);
    }
    game.install_dir = new_dir.clone();

    save_library(data_dir, library)?;
    Ok(new_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_translucency_is_ignored_and_not_saved() {
        let mut value =
            serde_json::to_value(LauncherSettings::default_for(Path::new("data"))).unwrap();
        value["translucency"] = serde_json::json!("mica");
        let settings: LauncherSettings = serde_json::from_value(value).unwrap();
        let saved = serde_json::to_value(settings).unwrap();
        assert!(saved.get("translucency").is_none());
    }

    #[test]
    fn moving_game_waits_for_active_dlc_download() {
        let root = std::env::temp_dir().join(format!(
            "azdl-move-dlc-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let game_dir = root.join("games/game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("game.exe"), b"base").unwrap();
        let drive = add_storage_drive(&root, "Other", &root.join("other")).unwrap();
        let mut library = crate::Library::default();
        crate::add_game(
            &mut library,
            "game",
            "Game",
            &game_dir,
            Path::new("game.exe"),
        )
        .unwrap();
        let id = "00000000000000d1";
        let worker = crate::jobs::lock_dlc_worker(&root, id).unwrap();
        crate::jobs::save_job(
            &root,
            &crate::jobs::DownloadJob {
                id: id.into(),
                kind: crate::jobs::JobKind::Dlc,
                url: "steam://100/dlc/200".into(),
                target: root.join("dlc-downloads/game/200/payload"),
                status: crate::jobs::JobStatus::Running,
                pid: Some(std::process::id()),
                error: None,
                updated_unix: 0,
                install: Some(crate::jobs::InstallPlan {
                    game_id: "game".into(),
                    title: "Game".into(),
                }),
                install_state: Some(crate::jobs::InstallState::Pending),
                install_error: None,
                progress_bytes: Some(0),
                total_bytes: None,
            },
        )
        .unwrap();
        let error = move_game_to_drive(&root, &mut library, "game", &drive.id)
            .unwrap_err()
            .to_string();
        assert!(error.contains("DLC download"), "{error}");
        assert!(game_dir.join("game.exe").exists());
        assert_eq!(
            library.games["game"].install_dir,
            fs::canonicalize(&game_dir).unwrap()
        );
        drop(worker);
        let moved = move_game_to_drive(&root, &mut library, "game", &drive.id).unwrap();
        assert_eq!(fs::read(moved.join("game.exe")).unwrap(), b"base");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_settings_load_save_defaults() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("azdl-settings-test-{nonce}"));
        fs::create_dir_all(&temp).unwrap();

        let s1 = load_settings(&temp).unwrap();
        assert_eq!(s1.storage_drives.len(), 1);
        assert_eq!(s1.storage_drives[0].id, "default");
        assert_eq!(s1.seed_color, "#10B981");

        fs::write(
            temp.join("settings.json"),
            br##"{"storage_drives":[],"auto_ea_heal":true,"default_runner":"wine","seed_color":"#123456"}"##,
        )
        .unwrap();
        let legacy = load_settings(&temp).unwrap();
        assert_eq!(legacy.seed_color, "#123456");

        let mut s2 = s1.clone();
        s2.seed_color = "#3B82F6".into();
        s2.theme_mode = "amoled".into();
        save_settings(&temp, &s2).unwrap();

        let s3 = load_settings(&temp).unwrap();
        assert_eq!(s3.seed_color, "#3B82F6");
        assert_eq!(s3.theme_mode, "amoled");

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_add_remove_drive() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("azdl-drives-test-{nonce}"));
        let drive_dir = temp.join("extra_drive");
        fs::create_dir_all(&drive_dir).unwrap();

        let drive = add_storage_drive(&temp, "Extra SSD", &drive_dir).unwrap();
        assert_eq!(drive.label, "Extra SSD");

        let s = load_settings(&temp).unwrap();
        assert_eq!(s.storage_drives.len(), 2);

        set_default_drive(&temp, &drive.id).unwrap();
        let s2 = load_settings(&temp).unwrap();
        assert!(
            s2.storage_drives
                .iter()
                .find(|d| d.id == drive.id)
                .unwrap()
                .is_default
        );

        remove_storage_drive(&temp, &drive.id).unwrap();
        let s3 = load_settings(&temp).unwrap();
        assert_eq!(s3.storage_drives.len(), 1);

        let _ = fs::remove_dir_all(&temp);
    }
}
