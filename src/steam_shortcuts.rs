//! Steam's binary shortcuts.vdf. Keep fields we do not understand as bytes so
//! editing an azdl entry does not discard another launcher's settings.
use crate::steam::steam_root;
use crate::{Game, Library, Result, Runner, clean_command, resolve_launch, validate_id};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Object(Vec<Field>),
    Text(Vec<u8>),
    Int(u32),
    Raw(u8, Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Field {
    key: Vec<u8>,
    value: Value,
}

fn read_cstr(data: &[u8], pos: &mut usize) -> Result<Vec<u8>> {
    let end = data[*pos..]
        .iter()
        .position(|byte| *byte == 0)
        .ok_or("unterminated Steam shortcut string")?
        + *pos;
    let text = data[*pos..end].to_vec();
    *pos = end + 1;
    Ok(text)
}

fn take(data: &[u8], pos: &mut usize, length: usize) -> Result<Vec<u8>> {
    let end = pos
        .checked_add(length)
        .ok_or("Steam shortcut is too large")?;
    let bytes = data
        .get(*pos..end)
        .ok_or("truncated Steam shortcut value")?
        .to_vec();
    *pos = end;
    Ok(bytes)
}

fn parse_fields(
    data: &[u8],
    pos: &mut usize,
    depth: usize,
    count: &mut usize,
) -> Result<Vec<Field>> {
    if depth > 32 {
        return Err("Steam shortcut nesting is too deep".into());
    }
    let mut fields = Vec::new();
    loop {
        let kind = *data.get(*pos).ok_or("unterminated Steam shortcut object")?;
        *pos += 1;
        if kind == 8 {
            return Ok(fields);
        }
        *count += 1;
        if *count > 100_000 {
            return Err("Steam shortcut contains too many fields".into());
        }
        let key = read_cstr(data, pos)?;
        let value = match kind {
            0 => Value::Object(parse_fields(data, pos, depth + 1, count)?),
            1 => Value::Text(read_cstr(data, pos)?),
            2 => {
                let bytes = take(data, pos, 4)?;
                Value::Int(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
            3 | 4 | 6 => Value::Raw(kind, take(data, pos, 4)?),
            7 | 9 => Value::Raw(kind, take(data, pos, 8)?),
            5 => {
                let start = *pos;
                loop {
                    let pair = take(data, pos, 2)?;
                    if pair == [0, 0] {
                        break;
                    }
                }
                Value::Raw(kind, data[start..*pos].to_vec())
            }
            _ => return Err(format!("unsupported Steam shortcut field type {kind}").into()),
        };
        fields.push(Field { key, value });
    }
}

fn parse(data: &[u8]) -> Result<Vec<Field>> {
    if data.len() > 16 * 1024 * 1024 {
        return Err("Steam shortcuts.vdf is too large".into());
    }
    let mut pos = 0;
    let fields = parse_fields(data, &mut pos, 0, &mut 0)?;
    if pos != data.len()
        || fields
            .iter()
            .filter(|field| field.key.eq_ignore_ascii_case(b"shortcuts"))
            .count()
            != 1
        || object(&fields, b"shortcuts").is_none()
    {
        return Err("invalid Steam shortcuts.vdf root".into());
    }
    Ok(fields)
}

fn write_fields(fields: &[Field], output: &mut Vec<u8>) {
    for field in fields {
        let kind = match field.value {
            Value::Object(_) => 0,
            Value::Text(_) => 1,
            Value::Int(_) => 2,
            Value::Raw(kind, _) => kind,
        };
        output.push(kind);
        output.extend_from_slice(&field.key);
        output.push(0);
        match &field.value {
            Value::Object(children) => write_fields(children, output),
            Value::Text(value) => {
                output.extend_from_slice(value);
                output.push(0);
            }
            Value::Int(value) => output.extend_from_slice(&value.to_le_bytes()),
            Value::Raw(_, value) => output.extend_from_slice(value),
        }
    }
    output.push(8);
}

fn object<'a>(fields: &'a [Field], key: &[u8]) -> Option<&'a Vec<Field>> {
    fields.iter().find_map(|field| {
        if field.key.eq_ignore_ascii_case(key)
            && let Value::Object(value) = &field.value
        {
            return Some(value);
        }
        None
    })
}

fn object_mut<'a>(fields: &'a mut [Field], key: &[u8]) -> Option<&'a mut Vec<Field>> {
    fields.iter_mut().find_map(|field| {
        if field.key.eq_ignore_ascii_case(key)
            && let Value::Object(value) = &mut field.value
        {
            return Some(value);
        }
        None
    })
}

fn text_value<'a>(fields: &'a [Field], key: &[u8]) -> Option<&'a [u8]> {
    fields.iter().find_map(|field| {
        if field.key.eq_ignore_ascii_case(key)
            && let Value::Text(value) = &field.value
        {
            return Some(value.as_slice());
        }
        None
    })
}

fn put(fields: &mut Vec<Field>, key: &[u8], value: Value) {
    if let Some(field) = fields
        .iter_mut()
        .find(|field| field.key.eq_ignore_ascii_case(key))
    {
        field.value = value;
    } else {
        fields.push(Field {
            key: key.to_vec(),
            value,
        });
    }
}

fn checked_text(value: &str) -> Result<Value> {
    if value.contains('\0') {
        return Err("Steam shortcut value contains NUL".into());
    }
    Ok(Value::Text(value.as_bytes().to_vec()))
}

fn quote_path(path: &Path) -> Result<String> {
    let value = path.to_str().ok_or("Steam shortcut path is not UTF-8")?;
    if value.contains(['"', '\n', '\r', '\0']) {
        return Err("Steam shortcut path contains an unsupported character".into());
    }
    #[cfg(windows)]
    let value = value.replace('/', "\\");
    Ok(format!("\"{value}\""))
}

fn launch_arguments(arguments: &[String]) -> Result<String> {
    let mut joined = Vec::new();
    for argument in arguments {
        if argument.contains(['\0', '\n', '\r']) {
            return Err("Steam launch argument contains a control character".into());
        }
        if argument.is_empty() || argument.contains([' ', '\t', '"']) {
            joined.push(format!("\"{}\"", argument.replace('"', "\\\"")));
        } else {
            joined.push(argument.clone());
        }
    }
    Ok(joined.join(" "))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn app_id(executable: &Path, title: &str) -> Result<u32> {
    let key = format!("{}{title}", quote_path(executable)?);
    Ok(crc32(key.as_bytes()) | 0x8000_0000)
}

fn owner_tag(id: &str) -> Vec<u8> {
    format!("azdl:{id}").into_bytes()
}

fn next_index(fields: &[Field]) -> Result<u32> {
    match fields
        .iter()
        .filter_map(|field| std::str::from_utf8(&field.key).ok()?.parse::<u32>().ok())
        .max()
    {
        Some(number) => number
            .checked_add(1)
            .ok_or_else(|| "Steam shortcut index overflow".into()),
        None => Ok(0),
    }
}

fn owned(fields: &[Field], id: &str) -> bool {
    object(fields, b"tags").is_some_and(|tags| {
        tags.iter()
            .any(|tag| matches!(&tag.value, Value::Text(value) if value == &owner_tag(id)))
    })
}

fn shortcut_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    let root = steam_root()?;
    let userdata = root.join("userdata");
    let mut candidates = Vec::new();
    for item in fs::read_dir(userdata)? {
        let item = item?;
        let name = item.file_name();
        let name = name.to_string_lossy();
        if name == "0" || !name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let config = item.path().join("config");
        if !config.is_dir() {
            continue;
        }
        let shortcut = config.join("shortcuts.vdf");
        let evidence = if shortcut.exists() {
            shortcut.clone()
        } else if config.join("localconfig.vdf").exists() {
            config.join("localconfig.vdf")
        } else {
            config
        };
        let modified = fs::metadata(evidence)?.modified()?;
        candidates.push((modified, shortcut));
    }
    candidates.sort_by(|left, right| right.cmp(left));
    candidates
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .ok_or_else(|| "Steam user config not found".into())
}

fn read_shortcuts(path: &Path) -> Result<(Option<Vec<u8>>, Vec<Field>)> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err("Steam shortcuts path is not a regular file".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match fs::read(path) {
        Ok(bytes) => {
            let fields = parse(&bytes)?;
            Ok((Some(bytes), fields))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((
            None,
            vec![Field {
                key: b"shortcuts".to_vec(),
                value: Value::Object(Vec::new()),
            }],
        )),
        Err(error) => Err(error.into()),
    }
}

fn write_shortcuts(path: &Path, previous: Option<&[u8]>, fields: &[Field]) -> Result<()> {
    let mut bytes = Vec::new();
    write_fields(fields, &mut bytes);
    let parent = path.parent().ok_or("Steam shortcuts path has no parent")?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = parent.join(format!(".shortcuts.{}.{nonce}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        let current = match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if current.as_deref() != previous {
            return Err("Steam shortcuts changed during update; retry".into());
        }
        if let Some(previous) = previous {
            let backup = parent.join(format!("shortcuts.vdf.azdl.{nonce}.bak"));
            let mut copy = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(backup)?;
            copy.write_all(previous)?;
            copy.sync_all()?;
        }
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

/// Add a direct game target to Steam. Steam chooses its own compatibility
/// settings for this shortcut, just as Arachnel's Steam export does.
pub fn create_shortcut(game: &Game, explicit_path: Option<&Path>) -> Result<(PathBuf, u32)> {
    validate_id(&game.id)?;
    if matches!(game.runner, Runner::Steam(_)) {
        return Err("Steam library games already have a Steam entry".into());
    }
    let target = resolve_launch(game)?;
    let path = shortcut_path(explicit_path)?;
    let (previous, mut root) = read_shortcuts(&path)?;
    let shortcuts = object_mut(&mut root, b"shortcuts").ok_or("Steam shortcuts root missing")?;
    let matches: Vec<_> = shortcuts
        .iter()
        .enumerate()
        .filter(|(_, field)| matches!(&field.value, Value::Object(value) if owned(value, &game.id)))
        .map(|(index, _)| index)
        .collect();
    if matches.len() > 1 {
        return Err("more than one Steam shortcut belongs to this game".into());
    }
    let exe = quote_path(&target.executable)?;
    if shortcuts.iter().enumerate().any(|(index, field)| {
        Some(index) != matches.first().copied()
            && matches!(&field.value, Value::Object(value) if text_value(value, b"Exe") == Some(exe.as_bytes()))
    }) {
        return Err("another Steam shortcut already uses this executable".into());
    }
    let mut entry = matches
        .first()
        .and_then(|index| match &shortcuts[*index].value {
            Value::Object(value) => Some(value.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let id = app_id(&target.executable, &game.title)?;
    put(&mut entry, b"appid", Value::Int(id));
    put(&mut entry, b"AppName", checked_text(&game.title)?);
    put(&mut entry, b"Exe", checked_text(&exe)?);
    put(
        &mut entry,
        b"StartDir",
        checked_text(&quote_path(&target.working_directory)?)?,
    );
    put(
        &mut entry,
        b"LaunchOptions",
        checked_text(&launch_arguments(&target.arguments)?)?,
    );
    if matches.is_empty() {
        put(
            &mut entry,
            b"icon",
            checked_text(&target.executable.to_string_lossy())?,
        );
        put(&mut entry, b"ShortcutPath", checked_text("")?);
        put(&mut entry, b"IsHidden", Value::Int(0));
        put(&mut entry, b"AllowDesktopConfig", Value::Int(1));
        put(&mut entry, b"AllowOverlay", Value::Int(1));
        put(&mut entry, b"OpenVR", Value::Int(0));
        put(&mut entry, b"Devkit", Value::Int(0));
        put(&mut entry, b"DevkitGameID", checked_text("")?);
        put(&mut entry, b"DevkitOverrideAppID", Value::Int(0));
        put(&mut entry, b"LastPlayTime", Value::Int(0));
        put(&mut entry, b"FlatpakAppID", checked_text("")?);
    }
    let tags = match object(&entry, b"tags") {
        Some(tags) => tags.clone(),
        None => Vec::new(),
    };
    if !owned(&entry, &game.id) {
        let mut tags = tags;
        let next = next_index(&tags)?;
        tags.push(Field {
            key: next.to_string().into_bytes(),
            value: Value::Text(owner_tag(&game.id)),
        });
        put(&mut entry, b"tags", Value::Object(tags));
    }
    if let Some(index) = matches.first() {
        shortcuts[*index].value = Value::Object(entry);
    } else {
        let next = next_index(shortcuts)?;
        shortcuts.push(Field {
            key: next.to_string().into_bytes(),
            value: Value::Object(entry),
        });
    }
    write_shortcuts(&path, previous.as_deref(), &root)?;
    if let Some(parent) = path.parent() {
        let grid_dir = parent.join("grid");
        let _ = install_grid_artwork(&grid_dir, id, game);
    }
    Ok((path, id))
}

pub fn shortcut_status(game: &Game, explicit_path: Option<&Path>) -> Result<bool> {
    let path = shortcut_path(explicit_path)?;
    let (_, root) = read_shortcuts(&path)?;
    Ok(object(&root, b"shortcuts")
        .ok_or("Steam shortcuts root missing")?
        .iter()
        .any(|field| matches!(&field.value, Value::Object(value) if owned(value, &game.id))))
}

pub fn remove_shortcut(game: &Game, explicit_path: Option<&Path>) -> Result<bool> {
    let path = shortcut_path(explicit_path)?;
    let (previous, mut root) = read_shortcuts(&path)?;
    let shortcuts = object_mut(&mut root, b"shortcuts").ok_or("Steam shortcuts root missing")?;
    let removed_ids: Vec<u32> = shortcuts
        .iter()
        .filter_map(|field| {
            if let Value::Object(ref entry) = field.value
                && owned(entry, &game.id)
            {
                entry.iter().find_map(|f| {
                    if f.key.eq_ignore_ascii_case(b"appid")
                        && let Value::Int(id) = f.value
                    {
                        Some(id)
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        })
        .collect();
    let before = shortcuts.len();
    shortcuts
        .retain(|field| !matches!(&field.value, Value::Object(value) if owned(value, &game.id)));
    if shortcuts.len() == before {
        return Ok(false);
    }
    write_shortcuts(&path, previous.as_deref(), &root)?;
    if let Some(parent) = path.parent() {
        let grid_dir = parent.join("grid");
        for id in removed_ids {
            remove_grid_artwork(&grid_dir, id);
        }
    }
    Ok(true)
}

pub fn sync_all_shortcuts(library: &Library) -> Result<usize> {
    let mut count = 0;
    for game in library.games.values() {
        if create_shortcut(game, None).is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

fn copy_or_download_art(source: &str, install_dir: &Path, dest: &Path) -> bool {
    if source.starts_with("https://") {
        let temp = match dest.file_name() {
            Some(name) => {
                let mut tmp_name = name.to_os_string();
                tmp_name.push(".tmp");
                dest.with_file_name(tmp_name)
            }
            None => return false,
        };
        let status = clean_command("curl")
            .args([
                "--fail",
                "--location",
                "--max-time",
                "10",
                "--retry",
                "2",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--output",
            ])
            .arg(&temp)
            .arg(source)
            .status();
        if let Ok(st) = status
            && st.success()
        {
            let _ = fs::rename(&temp, dest);
            return true;
        }
        let _ = fs::remove_file(&temp);
        false
    } else {
        let path_str = source.trim_start_matches("file://");
        let path = Path::new(path_str);
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            install_dir.join(path)
        };
        if let Ok(meta) = fs::symlink_metadata(&candidate)
            && meta.is_file()
            && !meta.file_type().is_symlink()
        {
            fs::copy(&candidate, dest).is_ok()
        } else {
            false
        }
    }
}

/// Copy or download artwork (capsule, landscape, hero) into Steam's grid cache.
pub fn install_grid_artwork(grid_dir: &Path, app_id: u32, game: &Game) -> Result<()> {
    if !grid_dir.exists() {
        fs::create_dir_all(grid_dir)?;
    }
    let portrait = grid_dir.join(format!("{app_id}p.jpg"));
    let landscape = grid_dir.join(format!("{app_id}.jpg"));
    let hero = grid_dir.join(format!("{app_id}_hero.jpg"));

    let mut got_portrait = portrait.is_file();
    let mut got_landscape = landscape.is_file();
    let mut got_hero = hero.is_file();

    if !got_portrait && let Some(ref cover) = game.cover_url {
        got_portrait = copy_or_download_art(cover, &game.install_dir, &portrait);
    }
    if !got_hero && let Some(ref banner) = game.banner_url {
        got_hero = copy_or_download_art(banner, &game.install_dir, &hero);
    }
    if !got_landscape && let Some(ref banner) = game.banner_url {
        got_landscape = copy_or_download_art(banner, &game.install_dir, &landscape);
    }

    let steam_app_id = game
        .origin
        .as_ref()
        .and_then(|o| o.qualified_id.strip_prefix("steam:").map(str::to_string))
        .or_else(|| {
            if !game.id.is_empty() && game.id.chars().all(|c| c.is_ascii_digit()) {
                Some(game.id.clone())
            } else {
                None
            }
        });

    if let Some(ref steam_id) = steam_app_id {
        if !got_portrait {
            got_portrait = copy_or_download_art(
                &format!(
                    "https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{steam_id}/library_600x900.jpg"
                ),
                &game.install_dir,
                &portrait,
            ) || copy_or_download_art(
                &format!(
                    "https://cdn.akamai.steamstatic.com/steam/apps/{steam_id}/library_capsule.jpg"
                ),
                &game.install_dir,
                &portrait,
            );
        }
        if !got_landscape {
            got_landscape = copy_or_download_art(
                &format!(
                    "https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{steam_id}/header.jpg"
                ),
                &game.install_dir,
                &landscape,
            );
        }
        if !got_hero {
            got_hero = copy_or_download_art(
                &format!(
                    "https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{steam_id}/library_hero.jpg"
                ),
                &game.install_dir,
                &hero,
            ) || copy_or_download_art(
                &format!(
                    "https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{steam_id}/header.jpg"
                ),
                &game.install_dir,
                &hero,
            );
        }
    }

    if !got_landscape && got_portrait {
        let _ = fs::copy(&portrait, &landscape);
    }
    if !got_hero && got_landscape {
        let _ = fs::copy(&landscape, &hero);
    } else if !got_hero && got_portrait {
        let _ = fs::copy(&portrait, &hero);
    }

    Ok(())
}

/// Remove grid artwork files associated with an app ID.
pub fn remove_grid_artwork(grid_dir: &Path, app_id: u32) {
    if !grid_dir.is_dir() {
        return;
    }
    for ext in ["jpg", "png"] {
        let _ = fs::remove_file(grid_dir.join(format!("{app_id}p.{ext}")));
        let _ = fs::remove_file(grid_dir.join(format!("{app_id}.{ext}")));
        let _ = fs::remove_file(grid_dir.join(format!("{app_id}_hero.{ext}")));
    }
    let _ = fs::remove_file(grid_dir.join(format!("{app_id}_logo.png")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Library, add_game};

    #[test]
    fn preserves_other_shortcuts_and_rejects_malformed_file() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("azdl-steam-shortcut-{nonce}"));
        fs::create_dir(&root).unwrap();
        let install = root.join("game");
        fs::create_dir(&install).unwrap();
        fs::write(install.join("game.exe"), b"game").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "sample",
            "Sample Game",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = &library.games["sample"];
        let path = root.join("shortcuts.vdf");
        let old_entry = vec![
            Field {
                key: b"Exe".to_vec(),
                value: Value::Text(b"\"/another/game\"".to_vec()),
            },
            Field {
                key: b"custom".to_vec(),
                value: Value::Raw(7, 5u64.to_le_bytes().to_vec()),
            },
        ];
        let initial = vec![Field {
            key: b"shortcuts".to_vec(),
            value: Value::Object(vec![Field {
                key: b"0".to_vec(),
                value: Value::Object(old_entry.clone()),
            }]),
        }];
        let mut bytes = Vec::new();
        write_fields(&initial, &mut bytes);
        fs::write(&path, &bytes).unwrap();
        let (_, id) = create_shortcut(game, Some(&path)).unwrap();
        assert_ne!(id & 0x8000_0000, 0);
        assert!(shortcut_status(game, Some(&path)).unwrap());
        let mut with_custom = parse(&fs::read(&path).unwrap()).unwrap();
        let entries = object_mut(&mut with_custom, b"shortcuts").unwrap();
        let Value::Object(owned_entry) = &mut entries[1].value else {
            panic!("azdl shortcut is not an object");
        };
        owned_entry.push(Field {
            key: b"thirdParty".to_vec(),
            value: Value::Raw(9, 17u64.to_le_bytes().to_vec()),
        });
        let mut modified = Vec::new();
        write_fields(&with_custom, &mut modified);
        fs::write(&path, modified).unwrap();
        create_shortcut(game, Some(&path)).unwrap();
        let added = parse(&fs::read(&path).unwrap()).unwrap();
        let entries = object(&added, b"shortcuts").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].value, Value::Object(old_entry.clone()));
        let Value::Object(owned_entry) = &entries[1].value else {
            panic!("azdl shortcut is not an object");
        };
        assert_eq!(
            owned_entry
                .iter()
                .find(|field| field.key == b"thirdParty")
                .unwrap()
                .value,
            Value::Raw(9, 17u64.to_le_bytes().to_vec())
        );
        assert!(remove_shortcut(game, Some(&path)).unwrap());
        assert!(!shortcut_status(game, Some(&path)).unwrap());
        let remaining = parse(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(object(&remaining, b"shortcuts").unwrap().len(), 1);
        fs::write(&path, b"broken").unwrap();
        assert!(create_shortcut(game, Some(&path)).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_grid_artwork_install_and_removal() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("azdl-steam-grid-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        let install = root.join("game");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("game.exe"), b"game").unwrap();
        let cover_file = install.join("mycover.jpg");
        fs::write(&cover_file, b"JPEG_DATA").unwrap();

        let mut library = Library::default();
        add_game(
            &mut library,
            "sample-grid",
            "Sample Grid Game",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = library.games.get_mut("sample-grid").unwrap();
        game.cover_url = Some("mycover.jpg".to_string());

        let path = root.join("shortcuts.vdf");
        let (_, id) = create_shortcut(game, Some(&path)).unwrap();

        let grid_dir = root.join("grid");
        let portrait = grid_dir.join(format!("{id}p.jpg"));
        let landscape = grid_dir.join(format!("{id}.jpg"));
        let hero = grid_dir.join(format!("{id}_hero.jpg"));

        assert!(portrait.exists(), "portrait grid artwork should exist");
        assert_eq!(fs::read(&portrait).unwrap(), b"JPEG_DATA");
        assert!(
            landscape.exists(),
            "fallback landscape artwork should exist"
        );
        assert!(hero.exists(), "fallback hero artwork should exist");

        assert!(remove_shortcut(game, Some(&path)).unwrap());
        assert!(
            !portrait.exists(),
            "portrait grid artwork should be removed"
        );
        assert!(
            !landscape.exists(),
            "landscape grid artwork should be removed"
        );
        assert!(!hero.exists(), "hero grid artwork should be removed");

        fs::remove_dir_all(root).unwrap();
    }
}
