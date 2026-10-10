//! Steam Store discovery data used by the desktop catalog.

use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

type SearchCache = Mutex<HashMap<String, (Instant, Vec<SteamStoreGame>)>>;
static SEARCH_CACHE: OnceLock<SearchCache> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SteamStoreGame {
    pub app_id: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub large_capsule_image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_capsule_image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiny_image: Option<String>,
}

impl SteamStoreGame {
    pub fn new(app_id: u32, title: impl Into<String>) -> Self {
        Self {
            app_id,
            title: title.into(),
            storage_size: None,
            header_image: None,
            large_capsule_image: None,
            small_capsule_image: None,
            tiny_image: None,
        }
    }
}

/// Read Steam's advertised minimum PC free-space requirement, when supplied.
pub fn parse_storage_requirement(requirements: &Value) -> Option<String> {
    let entries = [
        requirements.get("minimum").and_then(Value::as_str),
        requirements.get("recommended").and_then(Value::as_str),
        requirements.as_str(),
    ];
    for html in entries.into_iter().flatten() {
        let mut plain = String::with_capacity(html.len());
        let mut inside_tag = false;
        for character in html.chars() {
            match character {
                '<' => {
                    inside_tag = true;
                    plain.push(' ');
                }
                '>' => {
                    inside_tag = false;
                    plain.push(' ');
                }
                _ if !inside_tag => plain.push(character),
                _ => {}
            }
        }
        let plain = plain.replace("&nbsp;", " ").replace("&#160;", " ");
        let lowercase = plain.to_ascii_lowercase();
        for label in ["storage", "hard drive", "hard disk space", "disk space"] {
            let Some(position) = lowercase.find(label) else {
                continue;
            };
            let Some(value) = plain[position + label.len()..]
                .trim_start()
                .strip_prefix(':')
            else {
                continue;
            };
            let value = value.trim_start();
            let number_end = value
                .find(|character: char| {
                    !character.is_ascii_digit() && character != '.' && character != ','
                })
                .unwrap_or(value.len());
            let number = &value[..number_end];
            if number.is_empty()
                || number
                    .replace(',', ".")
                    .parse::<f64>()
                    .ok()
                    .is_none_or(|n| n <= 0.0)
            {
                continue;
            }
            let unit = value[number_end..]
                .trim_start()
                .split(|character: char| !character.is_ascii_alphabetic())
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            if ["KB", "MB", "GB", "TB"].contains(&unit.as_str()) {
                return Some(format!("{} {unit}", number.replace(',', ".")));
            }
        }
    }
    None
}

fn encode_query(query: &str) -> String {
    let mut encoded = String::new();
    for byte in query.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn read_store_json(url: &str) -> Result<Value> {
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-time",
            "12",
            "--max-filesize",
            "4194304",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            url,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("Steam Store request failed ({})", output.status).into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn parse_items<'a>(items: impl Iterator<Item = &'a Value>) -> Vec<SteamStoreGame> {
    let initial_capacity = items.size_hint().1.unwrap_or(256).min(256);
    let mut games = Vec::with_capacity(initial_capacity);
    let mut seen_app_ids = HashSet::with_capacity(initial_capacity);
    for item in items {
        if item
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "app")
        {
            continue;
        }
        let Some(app_id) = item
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id > 0)
        else {
            continue;
        };
        let Some(title) = item
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };

        if !seen_app_ids.insert(app_id) {
            continue;
        }

        let header_image = item
            .get("header_image")
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        let large_capsule_image = item
            .get("large_capsule_image")
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        let small_capsule_image = item
            .get("small_capsule_image")
            .or_else(|| item.get("capsule_image"))
            .or_else(|| item.get("capsule_imagev5"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        let tiny_image = item
            .get("tiny_image")
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        games.push(SteamStoreGame {
            app_id,
            title: title.to_string(),
            storage_size: item
                .get("pc_requirements")
                .and_then(parse_storage_requirement),
            header_image,
            large_capsule_image,
            small_capsule_image,
            tiny_image,
        });
    }
    games
}

pub fn parse_search_results(payload: &Value) -> Vec<SteamStoreGame> {
    payload
        .get("items")
        .and_then(Value::as_array)
        .map(|items| parse_items(items.iter()))
        .unwrap_or_default()
}

pub fn parse_featured_results(payload: &Value) -> Vec<SteamStoreGame> {
    let items = ["top_sellers", "new_releases", "specials"]
        .into_iter()
        .flat_map(|category| {
            payload
                .get(category)
                .and_then(|value| value.get("items"))
                .and_then(Value::as_array)
                .into_iter()
                .flat_map(|items| items.iter())
        });
    parse_items(items)
}

/// Discover Steam Store apps by title, App ID, or the current featured lists.
pub fn discover_games(query: &str) -> Result<Vec<SteamStoreGame>> {
    let query = query.trim();
    if query.len() > 128 {
        return Err("Steam search query is too long".into());
    }
    let cache = SEARCH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(entries) = cache.lock()
        && let Some((saved_at, games)) = entries.get(query)
        && saved_at.elapsed() < Duration::from_secs(300)
    {
        return Ok(games.clone());
    }
    let games = fetch_games(query);
    match games {
        Ok(games) => {
            if let Ok(mut entries) = cache.lock() {
                if entries.len() >= 64 {
                    entries.clear();
                }
                entries.insert(query.to_string(), (Instant::now(), games.clone()));
            }
            Ok(games)
        }
        Err(error) => {
            if let Ok(entries) = cache.lock()
                && let Some((_, games)) = entries.get(query)
            {
                return Ok(games.clone());
            }
            Err(error)
        }
    }
}

fn fetch_games(query: &str) -> Result<Vec<SteamStoreGame>> {
    if let Ok(app_id) = query.parse::<u32>()
        && app_id > 0
    {
        let url = format!(
            "https://store.steampowered.com/api/appdetails/?appids={app_id}&l=english&cc=US"
        );
        let payload = read_store_json(&url)?;
        let entry = &payload[app_id.to_string()];
        if entry.get("success").and_then(Value::as_bool) == Some(true)
            && let Some(data) = entry.get("data")
            && let Some(title) = data.get("name").and_then(Value::as_str)
        {
            let header_image = data
                .get("header_image")
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            let capsule_image = data
                .get("capsule_image")
                .or_else(|| data.get("capsule_imagev5"))
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            return Ok(vec![SteamStoreGame {
                app_id,
                title: title.to_string(),
                storage_size: data
                    .get("pc_requirements")
                    .and_then(parse_storage_requirement),
                header_image,
                large_capsule_image: None,
                small_capsule_image: capsule_image,
                tiny_image: None,
            }]);
        }
        return Ok(Vec::new());
    }
    if query.is_empty() {
        let payload = read_store_json(
            "https://store.steampowered.com/api/featuredcategories/?l=english&cc=US",
        )?;
        return Ok(parse_featured_results(&payload));
    }
    let url = format!(
        "https://store.steampowered.com/api/storesearch/?term={}&l=english&cc=US&count=50",
        encode_query(query)
    );
    Ok(parse_search_results(&read_store_json(&url)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn search_filters_non_apps_and_duplicate_ids() {
        let payload = json!({"items": [
            {"type":"app", "id": 400, "name":"Portal", "tiny_image": "https://example.com/tiny.jpg"},
            {"type":"app", "id": 400, "name":"Portal duplicate"},
            {"type":"bundle", "id": 123, "name":"Bundle"},
            {"type":"app", "id": 0, "name":"Invalid"}
        ]});
        assert_eq!(
            parse_search_results(&payload),
            vec![SteamStoreGame {
                app_id: 400,
                title: "Portal".into(),
                storage_size: None,
                header_image: None,
                large_capsule_image: None,
                small_capsule_image: None,
                tiny_image: Some("https://example.com/tiny.jpg".into()),
            }]
        );
    }

    #[test]
    fn featured_merges_categories_once() {
        let payload = json!({
            "top_sellers": {"items":[{"id": 400, "name":"Portal"}]},
            "new_releases": {"items":[{"id": 500, "name":"New"}, {"id": 400, "name":"Portal"}]}
        });
        assert_eq!(parse_featured_results(&payload).len(), 2);
    }

    #[test]
    fn storage_requirement_reads_steam_pc_minimum_without_guessing() {
        let requirements = json!({
            "minimum": "<li><strong>Memory:</strong> 8 GB RAM<br><strong>Storage:</strong> 20 GB available space</li>",
            "recommended": "<strong>Storage:</strong> 30 GB available space"
        });
        assert_eq!(
            parse_storage_requirement(&requirements).as_deref(),
            Some("20 GB")
        );
        assert_eq!(
            parse_storage_requirement(&json!({"minimum": "8 GB RAM"})),
            None
        );
        assert_eq!(
            parse_storage_requirement(
                &json!({"minimum": "<b>Hard Drive:</b> 500 MB available space"})
            )
            .as_deref(),
            Some("500 MB")
        );
    }
}
