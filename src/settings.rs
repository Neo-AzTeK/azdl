use crate::discovery::discover_executables;
use crate::{Game, Library, Result, Runner, clean_command, save_library, validate_id};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherSettings {
    #[serde(default)]
    pub storage_drives: Vec<StorageDrive>,
    #[serde(default)]
    pub downloads_dir: Option<PathBuf>,

    #[serde(default = "default_runner_str")]
    pub default_runner: String,
    #[serde(default)]
    pub global_launch_args: String,
    #[serde(default)]
    pub default_proton_id: Option<String>,
    #[serde(default)]
    pub proton_priority: Vec<String>,

    #[serde(default = "default_max_downloads")]
    pub max_concurrent_downloads: u32,
    #[serde(default)]
    pub speed_limit_bytes_per_sec: Option<u64>,
    #[serde(default)]
    pub auto_clean_completed: bool,

    #[serde(default)]
    pub auto_steam_shortcut_sync: bool,
    #[serde(default = "default_true")]
    pub auto_steam_grid_artwork: bool,
    #[serde(default = "default_true")]
    pub auto_steamless_unpack: bool,
    #[serde(default = "default_true")]
    pub auto_online_fix_overlay: bool,

    #[serde(default = "default_seed_color")]
    pub seed_color: String,
    #[serde(default = "default_theme_mode")]
    pub theme_mode: String,
    #[serde(default = "default_translucency")]
    pub translucency: String,
    #[serde(default)]
    pub plugin_store_urls: Vec<String>,
}

fn default_runner_str() -> String {
    "auto".into()
}

fn default_max_downloads() -> u32 {
    3
}

fn default_true() -> bool {
    true
}

fn default_seed_color() -> String {
    "#10B981".into()
}

fn default_theme_mode() -> String {
    "dark".into()
}

fn default_translucency() -> String {
    "none".into()
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
            downloads_dir: Some(data_dir.join("downloads")),
            default_runner: "auto".into(),
            global_launch_args: String::new(),
            default_proton_id: None,
            proton_priority: Vec::new(),
            max_concurrent_downloads: 3,
            speed_limit_bytes_per_sec: None,
            auto_clean_completed: false,
            auto_steam_shortcut_sync: false,
            auto_steam_grid_artwork: true,
            auto_steamless_unpack: true,
            auto_online_fix_overlay: true,
            seed_color: "#10B981".into(),
            theme_mode: "dark".into(),
            translucency: "none".into(),
            plugin_store_urls: Vec::new(),
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

pub fn scan_drives_for_games(data_dir: &Path, library: &mut Library) -> Result<usize> {
    let settings = load_settings(data_dir)?;
    let mut newly_added = 0;

    for drive in &settings.storage_drives {
        if !drive.path.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&drive.path)? {
            let entry = entry?;
            let folder_path = entry.path();
            if !folder_path.is_dir() {
                continue;
            }
            let folder_name = entry.file_name().to_string_lossy().to_string();
            if folder_name.starts_with('.') || folder_name == "prefixes" {
                continue;
            }

            let id = folder_name
                .to_lowercase()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>();
            if id.is_empty() || validate_id(&id).is_err() || library.games.contains_key(&id) {
                continue;
            }

            if let Ok(candidates) = discover_executables(&folder_path, &folder_name)
                && let Some(first_exe) = candidates.first()
            {
                let title = folder_name.clone();
                let runner = if first_exe
                    .path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                {
                    Runner::Auto
                } else {
                    Runner::Native
                };

                let game = Game {
                    id: id.clone(),
                    title,
                    install_dir: fs::canonicalize(&folder_path)?,
                    executable: fs::canonicalize(&first_exe.path)?,
                    runner,
                    arguments: Vec::new(),
                    environment: std::collections::BTreeMap::new(),
                    last_played_unix: None,
                    origin: None,
                    steam_app_id: None,
                    launch_options: Vec::new(),
                    selected_launch_option_id: None,
                    cover_url: None,
                    banner_url: None,
                };

                library.games.insert(id, game);
                newly_added += 1;
            }
        }
    }

    if newly_added > 0 {
        save_library(data_dir, library)?;
    }
    Ok(newly_added)
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
        let status = clean_command("cp")
            .arg("-r")
            .arg(old_dir)
            .arg(&new_dir)
            .status()?;
        if !status.success() {
            return Err("Failed to copy game files to destination drive".into());
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
        assert_eq!(s1.default_runner, "auto");
        assert_eq!(s1.seed_color, "#10B981");

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
