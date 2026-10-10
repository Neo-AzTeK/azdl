use crate::LaunchOption;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) struct InstallMarker {
    pub options: Vec<LaunchOption>,
    pub preferred_executable: Option<PathBuf>,
    pub selected_option_id: Option<String>,
}

fn inside(root: &Path, value: &str, directory: bool) -> Option<PathBuf> {
    if value.is_empty() || value.contains('\0') {
        return None;
    }
    let normalized = value.replace('\\', "/");
    let path = Path::new(&normalized);
    let path = fs::canonicalize(if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    })
    .ok()?;
    if !path.starts_with(root) || (directory && !path.is_dir()) || (!directory && !path.is_file()) {
        return None;
    }
    Some(path)
}

fn text<'a>(object: &'a Value, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Read Arachnel's installed-game marker without trusting its paths or requiring its plugin.
pub(crate) fn read_install_marker(install_dir: &Path) -> Option<InstallMarker> {
    let root = fs::canonicalize(install_dir).ok()?;
    let marker_path = root.join(".arachnel-steamidra");
    let metadata = fs::symlink_metadata(&marker_path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return None;
    }
    let marker: Value = serde_json::from_slice(&fs::read(marker_path).ok()?).ok()?;
    let mut options = Vec::<LaunchOption>::new();
    for (index, raw) in marker
        .get("launchOptions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(64)
        .enumerate()
    {
        let Some(path) = text(raw, "path").or_else(|| text(raw, "executable")) else {
            continue;
        };
        let Some(executable) = inside(&root, path, false) else {
            continue;
        };
        let working_directory = match text(raw, "workingDirectory") {
            Some(directory) => match inside(&root, directory, true) {
                Some(path) => Some(path),
                None => continue,
            },
            None => None,
        };
        let mut id = text(raw, "id").unwrap_or_default().to_string();
        if id.len() > 128
            || id.contains('\0')
            || id == "default"
            || options.iter().any(|opt| opt.id == id)
        {
            id = format!("marker-{}", index + 1);
        }
        if id.is_empty() {
            id = format!("marker-{}", index + 1);
        }
        if options.iter().any(|opt| opt.id == id) {
            continue;
        }
        let arguments: Vec<String> = raw
            .get("arguments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .take(65)
            .map(str::to_owned)
            .collect();
        if arguments.len() > 64
            || arguments
                .iter()
                .any(|arg| arg.len() > 4096 || arg.contains('\0'))
        {
            continue;
        }
        options.push(LaunchOption {
            title: text(raw, "title").unwrap_or(&id).to_string(),
            id,
            executable,
            working_directory,
            arguments,
            kind: text(raw, "type").unwrap_or("game").to_string(),
            is_default: raw
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }

    let requested = text(&marker, "selectedLaunchOptionId")
        .and_then(|id| options.iter().find(|option| option.id == id));
    let launch = marker.get("launch");
    let launch_path = launch
        .filter(|value| text(value, "type").is_none_or(|kind| kind == "exe"))
        .and_then(|value| text(value, "path"))
        .and_then(|path| inside(&root, path, false));
    let default = options.iter().find(|option| option.is_default);
    let selected = requested.or_else(|| {
        if launch_path.is_some() {
            options
                .iter()
                .find(|option| Some(&option.executable) == launch_path.as_ref())
        } else {
            default.or_else(|| (options.len() == 1).then(|| &options[0]))
        }
    });
    let preferred_executable = requested
        .map(|option| option.executable.clone())
        .or(launch_path)
        .or_else(|| default.map(|option| option.executable.clone()))
        .or_else(|| (options.len() == 1).then(|| options[0].executable.clone()));
    let selected_option_id = selected.map(|option| option.id.clone());
    Some(InstallMarker {
        options,
        preferred_executable,
        selected_option_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::import_game;
    use crate::{Library, add_game, resolve_launch};
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn marker_resolves_ambiguous_install_and_imports_launch_options() {
        let root = std::env::temp_dir().join(format!(
            "azdl-marker-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("installed");
        fs::create_dir_all(install.join("bin")).unwrap();
        fs::write(install.join("launcher.exe"), b"launcher").unwrap();
        fs::write(install.join("bin/game.exe"), b"game").unwrap();
        fs::write(root.join("outside.exe"), b"outside").unwrap();
        fs::write(install.join(".arachnel-steamidra"), serde_json::to_vec(&json!({
            "launch": {"type": "exe", "path": "bin\\game.exe"},
            "launchOptions": [
                {"id": "multiplayer", "title": "Multiplayer", "path": "bin/game.exe", "workingDirectory": "bin", "arguments": ["--online mode"], "isDefault": true, "type": "game"},
                {"id": "escape", "path": "../outside.exe"}
            ]
        })).unwrap()).unwrap();
        let mut library = Library::default();
        let chosen = import_game(&mut library, "demo", "Unknown", &install).unwrap();
        assert_eq!(chosen, install.join("bin/game.exe"));
        let game = &library.games["demo"];
        assert_eq!(game.launch_options.len(), 1);
        assert_eq!(
            game.selected_launch_option_id.as_deref(),
            Some("multiplayer")
        );
        let launch = resolve_launch(game).unwrap();
        assert_eq!(launch.executable, chosen);
        assert_eq!(launch.working_directory, install.join("bin"));
        assert_eq!(launch.arguments, ["--online mode"]);
        add_game(
            &mut library,
            "manual",
            "Manual",
            &install,
            Path::new("launcher.exe"),
        )
        .unwrap();
        let manual = &library.games["manual"];
        assert_eq!(manual.executable, install.join("launcher.exe"));
        assert!(manual.selected_launch_option_id.is_none());
        assert_eq!(manual.launch_options.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
