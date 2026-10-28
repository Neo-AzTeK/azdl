//! Read-only parser for archived source catalogs during schema migration.
//! No network access, executable loading or current catalog registration occurs here.
use crate::Result;
use crate::archive::multipart_names;
use crate::catalog::{AddonKind, CatalogAddon, CatalogGame, PackageType, classify, slug};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Component, Path};

fn text_field<'a>(entry: &'a Value, key: &str) -> Option<&'a str> {
    entry
        .get(key)?
        .as_str()
        .filter(|value| !value.trim().is_empty())
}

fn filename_from_url(url: &str) -> Option<String> {
    let path = url.split('?').next()?.split('#').next()?;
    let name = path.rsplit('/').next()?;
    if name.is_empty()
        || !matches!(
            Path::new(name).components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        )
    {
        None
    } else {
        Some(name.to_string())
    }
}

fn parse_sha256(entry: &Value) -> Result<Option<String>> {
    let Some(raw) = entry.get("sha256") else {
        return Ok(None);
    };
    let value = raw
        .as_str()
        .ok_or("sha256 must be 64 hexadecimal characters")?;
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("sha256 must be 64 hexadecimal characters".into());
    }
    Ok(Some(value.to_ascii_lowercase()))
}

struct ArchiveParts {
    urls: Vec<String>,
    first_name: String,
    hashes: Vec<Option<String>>,
}

fn parse_archive_parts(entry: &Value) -> Result<Option<ArchiveParts>> {
    let Some(raw) = entry.get("archiveParts") else {
        return Ok(None);
    };
    let urls = raw
        .as_array()
        .ok_or("archiveParts must be an array of HTTPS URLs")?;
    let parts: Vec<(String, Option<String>)> = urls
        .iter()
        .map(|value| {
            let url = value
                .as_str()
                .or_else(|| value.get("url").and_then(Value::as_str))
                .filter(|url| url.starts_with("https://"))
                .map(str::to_owned)
                .ok_or("every archiveParts item must be an HTTPS URL")?;
            let hash = if value.is_object() {
                parse_sha256(value)?
            } else {
                None
            };
            Ok((url, hash))
        })
        .collect::<Result<_>>()?;
    let urls: Vec<String> = parts.iter().map(|(url, _)| url.clone()).collect();
    let first_name = text_field(entry, "filename")
        .map(str::to_owned)
        .or_else(|| urls.first().and_then(|url| filename_from_url(url)))
        .ok_or("archiveParts needs a first part filename")?;
    if !matches!(
        Path::new(&first_name)
            .components()
            .collect::<Vec<_>>()
            .as_slice(),
        [Component::Normal(_)]
    ) {
        return Err("archiveParts filename must be a single filename".into());
    }
    multipart_names(&first_name, urls.len())?;
    if text_field(entry, "packageType").is_some_and(|kind| {
        !matches!(
            kind.to_ascii_lowercase().as_str(),
            "archive" | "setup-archive" | "installer-archive"
        )
    }) {
        return Err("archiveParts requires an archive package type".into());
    }
    Ok(Some(ArchiveParts {
        urls,
        first_name,
        hashes: parts.into_iter().map(|(_, hash)| hash).collect(),
    }))
}

fn parse_addons(entry: &Value) -> Result<Vec<CatalogAddon>> {
    let mut addons = Vec::new();
    for addon in entry
        .get("addons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = text_field(addon, "id") else {
            continue;
        };
        let kind = match text_field(addon, "kind") {
            Some(kind)
                if kind.eq_ignore_ascii_case("addon") || kind.eq_ignore_ascii_case("add-on") =>
            {
                AddonKind::Addon
            }
            Some(kind) if kind.eq_ignore_ascii_case("dlc") => AddonKind::Dlc,
            _ => AddonKind::Other,
        };
        let parts = parse_archive_parts(addon)?;
        let sha256 = parse_sha256(addon)?;
        if parts.is_some() && sha256.is_some() {
            return Err("use per-volume sha256 values for archiveParts".into());
        }
        let uri = parts
            .as_ref()
            .map(|parts| parts.urls[0].clone())
            .or_else(|| {
                text_field(addon, "downloadUrl")
                    .filter(|uri| uri.starts_with("https://") || uri.starts_with("http://"))
                    .or_else(|| {
                        addon
                            .get("uris")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .find(|uri| uri.starts_with("https://") || uri.starts_with("http://"))
                    })
                    .map(str::to_owned)
            });
        let filename = text_field(addon, "filename")
            .map(str::to_owned)
            .or_else(|| parts.as_ref().map(|parts| parts.first_name.clone()))
            .or_else(|| uri.as_deref().and_then(filename_from_url))
            .or_else(|| {
                let title = text_field(addon, "title")?;
                let lower = title.to_ascii_lowercase();
                for ext in [".rar", ".zip", ".7z"] {
                    if let Some(pos) = lower.find(ext) {
                        return Some(title[..pos + ext.len()].trim().to_string());
                    }
                }
                None
            });
        let referer = text_field(addon, "referer").map(str::to_owned);
        let (archive_parts, archive_part_hashes) = parts
            .map(|parts| (parts.urls, parts.hashes))
            .unwrap_or_default();
        let downloadable_dlc = filename.as_deref().is_some_and(|name| {
            matches!(
                Path::new(name).components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(_)]
            )
        }) && (!archive_parts.is_empty()
            || uri
                .as_deref()
                .is_some_and(|url| url.starts_with("https://")));
        addons.push(CatalogAddon {
            id: id.to_string(),
            title: text_field(addon, "title").unwrap_or(id).to_string(),
            kind,
            uri,
            filename,
            archive_parts,
            sha256,
            archive_part_hashes,
            content_available: addon
                .get("contentAvailable")
                .or_else(|| addon.get("hasManifest"))
                .and_then(Value::as_bool)
                .unwrap_or(true)
                && (kind != AddonKind::Dlc || downloadable_dlc),
            referer,
        });
    }
    Ok(addons)
}

/// Decode a previously cached source feed solely to preserve inert content records.
pub(crate) fn parse_feed(payload: &[u8], source_id: &str) -> Result<Vec<CatalogGame>> {
    let root: Value = serde_json::from_slice(payload)?;
    let array = ["downloads", "entries", "games"]
        .into_iter()
        .filter_map(|key| root.get(key).and_then(Value::as_array))
        .find(|entries| !entries.is_empty())
        .ok_or("catalog needs a nonempty downloads, entries, or games array")?;
    let mut seen = BTreeSet::new();
    let mut games = Vec::new();
    for (index, entry) in array.iter().enumerate() {
        let Some(title) = text_field(entry, "title") else {
            continue;
        };
        if text_field(entry, "kind").is_some_and(|kind| !kind.eq_ignore_ascii_case("game")) {
            continue;
        }
        let base_id = text_field(entry, "id")
            .map(str::to_owned)
            .unwrap_or_else(|| slug(title, index));
        let mut id = base_id.clone();
        if !seen.insert(id.clone()) {
            id = format!("{base_id}-{index}");
            while !seen.insert(id.clone()) {
                id.push('x');
            }
        }
        let uris: Vec<&str> = entry
            .get("uris")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let parts = parse_archive_parts(entry)?;
        let sha256 = parse_sha256(entry)?;
        if parts.is_some() && sha256.is_some() {
            return Err("use per-volume sha256 values for archiveParts".into());
        }
        let uri = parts
            .as_ref()
            .map(|parts| parts.urls[0].clone())
            .or_else(|| {
                text_field(entry, "downloadUrl")
                    .or_else(|| uris.iter().copied().find(|uri| uri.starts_with("https://")))
                    .or_else(|| uris.iter().copied().find(|uri| uri.starts_with("magnet:")))
                    .map(str::to_owned)
            });
        let app_id = entry
            .get("steamAppId")
            .and_then(|v| {
                v.as_str()
                    .and_then(|s| s.parse::<u32>().ok())
                    .or_else(|| v.as_u64().and_then(|n| u32::try_from(n).ok()))
            })
            .or_else(|| {
                id.strip_prefix("steam-")
                    .and_then(|s| s.parse::<u32>().ok())
            });
        let filename = text_field(entry, "filename")
            .map(str::to_owned)
            .or_else(|| parts.as_ref().map(|parts| parts.first_name.clone()))
            .or_else(|| {
                uri.as_deref()
                    .filter(|uri| uri.starts_with("https://"))
                    .and_then(filename_from_url)
            });
        let package_type = if parts.is_some() {
            match text_field(entry, "packageType")
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("setup-archive" | "installer-archive") => PackageType::SetupArchive,
                _ => PackageType::Archive,
            }
        } else if uri
            .as_deref()
            .is_some_and(|uri| uri.starts_with("https://"))
        {
            classify(filename.as_deref(), text_field(entry, "packageType"))
        } else if let Some(app_id) = app_id.filter(|id| *id > 0) {
            PackageType::Steam(app_id)
        } else if uri.as_deref().is_some_and(|uri| uri.starts_with("magnet:")) {
            PackageType::Magnet
        } else {
            PackageType::Unknown
        };
        let (archive_parts, archive_part_hashes) = parts
            .map(|parts| (parts.urls, parts.hashes))
            .unwrap_or_default();
        let cover_url = text_field(entry, "coverUrl")
            .or_else(|| text_field(entry, "cover_url"))
            .or_else(|| text_field(entry, "image"))
            .or_else(|| text_field(entry, "poster"))
            .map(str::to_owned)
            .or_else(|| {
                app_id.filter(|id| *id > 0).map(|id| {
                    format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{id}/library_600x900.jpg")
                })
            });
        let banner_url = text_field(entry, "bannerUrl")
            .or_else(|| text_field(entry, "banner_url"))
            .or_else(|| text_field(entry, "headerImage"))
            .or_else(|| text_field(entry, "header_image"))
            .map(str::to_owned)
            .or_else(|| {
                app_id.filter(|id| *id > 0).map(|id| {
                    format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{id}/header.jpg")
                })
            });
        let file_size = text_field(entry, "fileSize")
            .or_else(|| text_field(entry, "file_size"))
            .or_else(|| text_field(entry, "size"))
            .map(str::to_owned);
        let mut genres = Vec::new();
        if let Some(arr) = entry
            .get("genres")
            .or_else(|| entry.get("tags"))
            .and_then(Value::as_array)
        {
            for v in arr {
                if let Some(s) = v.as_str() {
                    genres.push(s.to_string());
                }
            }
        }
        games.push(CatalogGame {
            source_id: source_id.to_string(),
            id,
            title: title.to_string(),
            description: text_field(entry, "description").unwrap_or("").to_string(),
            uri,
            filename,
            archive_parts,
            sha256,
            archive_part_hashes,
            package_type,
            upload_date: text_field(entry, "uploadDate").map(str::to_owned),
            version: text_field(entry, "version").map(str::to_owned),
            addons: parse_addons(entry)?,
            cover_url,
            banner_url,
            file_size,
            genres,
            steam_app_id: app_id,
        });
    }
    Ok(games)
}
