use crate::{Game, Library, Result, Runner, clean_command};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

#[derive(Debug, Clone)]
enum Value {
    Text(String),
    Object(BTreeMap<String, Value>),
}

impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        let Value::Object(values) = self else {
            return None;
        };
        values
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value)
    }

    fn text(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            Value::Object(_) => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Text(String),
    Open,
    Close,
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    let mut chars = input.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(ch) = chars.next() {
        match ch {
            ch if ch.is_whitespace() => {}
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
            }
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
            '"' => {
                let mut value = String::new();
                let mut closed = false;
                while let Some(next) = chars.next() {
                    match next {
                        '"' => {
                            closed = true;
                            break;
                        }
                        '\\' => match chars.next() {
                            Some('"') => value.push('"'),
                            Some('\\') => value.push('\\'),
                            Some(other) => {
                                value.push('\\');
                                value.push(other);
                            }
                            None => return Err("unterminated VDF escape".into()),
                        },
                        other => value.push(other),
                    }
                }
                if !closed {
                    return Err("unterminated VDF string".into());
                }
                tokens.push(Token::Text(value));
            }
            _ => return Err("unexpected VDF syntax".into()),
        }
    }
    Ok(tokens)
}

fn parse_pairs(
    tokens: &[Token],
    index: &mut usize,
    nested: bool,
) -> Result<BTreeMap<String, Value>> {
    let mut values = BTreeMap::new();
    loop {
        let Some(token) = tokens.get(*index) else {
            return if nested {
                Err("unterminated VDF object".into())
            } else {
                Ok(values)
            };
        };
        if token == &Token::Close {
            if !nested {
                return Err("unexpected VDF closing brace".into());
            }
            *index += 1;
            return Ok(values);
        }
        let Token::Text(key) = token else {
            return Err("expected VDF key".into());
        };
        *index += 1;
        let value = match tokens.get(*index) {
            Some(Token::Text(text)) => {
                *index += 1;
                Value::Text(text.clone())
            }
            Some(Token::Open) => {
                *index += 1;
                Value::Object(parse_pairs(tokens, index, true)?)
            }
            _ => return Err("expected VDF value".into()),
        };
        values.insert(key.clone(), value);
    }
}

fn parse_vdf(input: &str) -> Result<Value> {
    let tokens = tokenize(input.trim_start_matches('\u{feff}'))?;
    let mut index = 0;
    let values = parse_pairs(&tokens, &mut index, false)?;
    if index != tokens.len() {
        return Err("trailing VDF tokens".into());
    }
    Ok(Value::Object(values))
}

#[derive(Debug, Clone)]
pub struct SteamGame {
    pub app_id: u32,
    pub title: String,
    pub install_dir: PathBuf,
    pub installed: bool,
}

pub fn steam_root() -> Result<PathBuf> {
    if let Some(root) = env::var_os("AZDL_STEAM_ROOT") {
        return Ok(PathBuf::from(root));
    }
    #[cfg(windows)]
    {
        for base in [
            env::var_os("PROGRAMFILES(X86)"),
            env::var_os("PROGRAMFILES"),
        ] {
            if let Some(base) = base {
                let path = PathBuf::from(base).join("Steam");
                if path.join("steamapps").is_dir() {
                    return Ok(path);
                }
            }
        }
    }
    #[cfg(not(windows))]
    if let Some(home) = env::var_os("HOME") {
        for path in [
            PathBuf::from(&home).join(".steam/root"),
            PathBuf::from(&home).join(".steam/steam"),
            PathBuf::from(&home).join(".steam/debian-installation"),
            PathBuf::from(&home).join(".local/share/Steam"),
            PathBuf::from(home).join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        ] {
            if path.join("steamapps").is_dir() {
                return Ok(path);
            }
        }
    }
    Err("Steam installation not found; set AZDL_STEAM_ROOT".into())
}

pub(crate) fn library_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let root = fs::canonicalize(root)?;
    let mut paths = BTreeSet::from([root.clone()]);
    let folders = root.join("steamapps/libraryfolders.vdf");
    if folders.is_file() {
        let data = fs::read_to_string(folders)?;
        let parsed = parse_vdf(&data)?;
        if let Some(Value::Object(folders)) = parsed.get("libraryfolders") {
            for value in folders.values() {
                let path = value
                    .get("path")
                    .and_then(Value::text)
                    .or_else(|| value.text());
                if let Some(path) = path {
                    let path = PathBuf::from(path);
                    if path.join("steamapps").is_dir() {
                        paths.insert(fs::canonicalize(path)?);
                    }
                }
            }
        }
    }
    Ok(paths.into_iter().collect())
}

pub fn scan_steam(root: &Path) -> Result<Vec<SteamGame>> {
    let mut games = BTreeMap::new();
    for library in library_paths(root)? {
        let steamapps = library.join("steamapps");
        for entry in fs::read_dir(&steamapps)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            let parsed = parse_vdf(&fs::read_to_string(entry.path())?)?;
            let Some(state) = parsed.get("AppState") else {
                continue;
            };
            let Some(app_id) = state
                .get("appid")
                .and_then(Value::text)
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if app_id == 0 {
                continue;
            }
            let Some(title) = state.get("name").and_then(Value::text) else {
                continue;
            };
            let Some(dir_name) = state.get("installdir").and_then(Value::text) else {
                continue;
            };
            if !matches!(
                Path::new(dir_name)
                    .components()
                    .collect::<Vec<_>>()
                    .as_slice(),
                [Component::Normal(_)]
            ) {
                continue;
            }
            let install_dir = steamapps.join("common").join(dir_name);
            let state_flags = state
                .get("StateFlags")
                .and_then(Value::text)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let installed = state_flags & 4 != 0 && install_dir.is_dir();
            games.insert(
                app_id,
                SteamGame {
                    app_id,
                    title: title.to_string(),
                    install_dir,
                    installed,
                },
            );
        }
    }
    Ok(games.into_values().collect())
}

pub fn import_steam(library: &mut Library, root: &Path, app_id: u32) -> Result<String> {
    let game = scan_steam(root)?
        .into_iter()
        .find(|game| game.app_id == app_id)
        .ok_or("Steam app manifest not found")?;
    if !game.installed {
        return Err("Steam app is not fully installed yet".into());
    }
    let id = format!("steam-{app_id}");
    if library.games.contains_key(&id) {
        return Err("game ID already exists".into());
    }
    library.games.insert(
        id.clone(),
        Game {
            id: id.clone(),
            title: game.title,
            install_dir: fs::canonicalize(game.install_dir)?,
            executable: PathBuf::new(),
            runner: Runner::Steam(app_id),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            last_played_unix: None,
            origin: None,
            steam_app_id: Some(app_id),
            launch_options: Vec::new(),
            selected_launch_option_id: None,
            cover_url: Some(format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{app_id}/library_600x900.jpg")),
            banner_url: Some(format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{app_id}/header.jpg")),
        },
    );
    Ok(id)
}

pub fn steam_command() -> PathBuf {
    if let Some(program) = env::var_os("AZDL_STEAM_COMMAND") {
        return PathBuf::from(program);
    }
    #[cfg(windows)]
    if let Ok(root) = steam_root() {
        return root.join("steam.exe");
    }
    PathBuf::from("steam")
}

pub fn request_install(app_id: u32) -> Result<()> {
    if app_id == 0 {
        return Err("Steam app ID must be positive".into());
    }
    clean_command(steam_command())
        .arg(format!("steam://install/{app_id}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// Launch the official Steam desktop client in the background.
pub fn launch_steam() -> Result<()> {
    clean_command(steam_command())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_manifest_and_multiple_libraries() {
        let root = env::temp_dir().join(format!(
            "azdl-steam-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let other = root.join("other");
        let main = root.join("main");
        fs::create_dir_all(main.join("steamapps/common/Game One")).unwrap();
        fs::create_dir_all(other.join("steamapps/common/Game Two")).unwrap();
        fs::write(
            main.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} \"1\" {{ \"path\" \"{}\" }} }}",
                main.display(),
                other.display()
            ),
        )
        .unwrap();
        fs::write(main.join("steamapps/appmanifest_100.acf"), "\"AppState\" { \"appid\" \"100\" \"name\" \"Game One\" \"StateFlags\" \"4\" \"installdir\" \"Game One\" }").unwrap();
        fs::write(other.join("steamapps/appmanifest_200.acf"), "\"AppState\" { \"appid\" \"200\" \"name\" \"Game Two\" \"StateFlags\" \"4\" \"installdir\" \"Game Two\" }").unwrap();
        let games = scan_steam(&main).unwrap();
        assert_eq!(games.len(), 2);
        assert!(games.iter().all(|game| game.installed));
        let mut library = Library::default();
        assert_eq!(import_steam(&mut library, &main, 200).unwrap(), "steam-200");
        assert_eq!(library.games["steam-200"].runner, Runner::Steam(200));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_bad_vdf_syntax() {
        assert!(parse_vdf("\"AppState\" { \"name\" \"oops\"").is_err());
        assert!(parse_vdf("\"AppState\" { \"name\" \"bad").is_err());
    }
}
