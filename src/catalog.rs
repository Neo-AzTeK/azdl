use crate::archive::{fetch_archive_in, fetch_archive_parts_in, multipart_names};
use crate::jobs::{JobKind, recorded_download_with_sha256};
use crate::patch::{fetch_patch_archive, fetch_patch_archive_parts};
use crate::setup::{SetupOutcome, fetch_setup, fetch_setup_archive, fetch_setup_archive_parts};
use crate::torrent::fetch_magnet;
use crate::{CatalogOrigin, Game, Library, Result, Runner, fetch_game_in, validate_id};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageType {
    Archive,
    Executable,
    Setup,
    SetupArchive,
    Steam(u32),
    Magnet,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct CatalogGame {
    pub source_id: String,
    pub id: String,
    pub title: String,
    pub description: String,
    pub uri: Option<String>,
    pub filename: Option<String>,
    pub archive_parts: Vec<String>,
    pub sha256: Option<String>,
    pub archive_part_hashes: Vec<Option<String>>,
    pub package_type: PackageType,
    pub upload_date: Option<String>,
    pub version: Option<String>,
    pub addons: Vec<CatalogAddon>,
    pub cover_url: Option<String>,
    pub banner_url: Option<String>,
    pub file_size: Option<String>,
    pub genres: Vec<String>,
    pub steam_app_id: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AddonKind {
    Addon,
    Dlc,
    Other,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CatalogAddon {
    pub id: String,
    pub title: String,
    pub kind: AddonKind,
    pub uri: Option<String>,
    pub filename: Option<String>,
    pub archive_parts: Vec<String>,
    pub sha256: Option<String>,
    pub archive_part_hashes: Vec<Option<String>>,
    pub content_available: bool,
    pub referer: Option<String>,
}

impl CatalogGame {
    pub fn qualified_id(&self) -> String {
        format!("{}:{}", self.source_id, self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed(String),
    SteamRequested(u32),
    NeedsSelection(String),
}

pub(crate) fn classify(filename: Option<&str>, declared: Option<&str>) -> PackageType {
    match declared.map(str::to_ascii_lowercase).as_deref() {
        Some("archive") => return PackageType::Archive,
        Some("executable") | Some("file") => return PackageType::Executable,
        Some("setup") | Some("installer") => return PackageType::Setup,
        Some("setup-archive") | Some("installer-archive") => return PackageType::SetupArchive,
        _ => {}
    }
    let Some(name) = filename else {
        return PackageType::Unknown;
    };
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".msi")
        || (lower.ends_with(".exe") && (lower.starts_with("setup") || lower.starts_with("install")))
    {
        PackageType::Setup
    } else if [".zip", ".7z", ".rar", ".tar", ".tar.gz", ".tgz", ".tar.xz"]
        .iter()
        .any(|ending| lower.ends_with(ending))
    {
        PackageType::Archive
    } else if [".exe", ".appimage", ".bin", ".sh", ".x86_64"]
        .iter()
        .any(|ending| lower.ends_with(ending))
    {
        PackageType::Executable
    } else {
        PackageType::Unknown
    }
}

pub(crate) fn slug(title: &str, index: usize) -> String {
    let mut result = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
        } else if !result.is_empty() && !result.ends_with('-') {
            result.push('-');
        }
    }
    let result = result.trim_matches('-');
    if result.is_empty() {
        format!("game-{index}")
    } else {
        result
            .chars()
            .take(64)
            .collect::<String>()
            .trim_end_matches('-')
            .to_string()
    }
}

fn install_id(game: &CatalogGame) -> String {
    let seed = game.qualified_id();
    let mut hash = 0xcbf29ce484222325u64;
    for byte in seed.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    format!("{}-{}-{hash:08x}", game.source_id, slug(&game.id, 0))
}

pub fn installed_game_id(library: &Library, game: &CatalogGame) -> Option<String> {
    library.games.iter().find_map(|(id, installed)| {
        installed
            .origin
            .as_ref()
            .filter(|origin| origin.qualified_id == game.qualified_id())
            .map(|_| id.clone())
    })
}

struct Prefetch<'a> {
    data_dir: &'a Path,
    folder: &'a str,
    id: &'a str,
    title: &'a str,
    kind: JobKind,
}

impl Prefetch<'_> {
    fn file(&self, filename: &str, url: &str, sha256: Option<&str>) -> Result<()> {
        let Some(sha256) = sha256 else {
            return Ok(());
        };
        let downloads = self.data_dir.join(self.folder).join(self.id);
        fs::create_dir_all(&downloads)?;
        let placeholder = Game {
            id: self.id.to_string(),
            title: self.title.to_string(),
            install_dir: fs::canonicalize(downloads)?,
            executable: PathBuf::new(),
            runner: Runner::Auto,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            last_played_unix: None,
            origin: None,
            steam_app_id: None,
            launch_options: Vec::new(),
            selected_launch_option_id: None,
            cover_url: None,
            banner_url: None,
        };
        recorded_download_with_sha256(
            self.data_dir,
            &placeholder,
            url,
            Path::new(filename),
            self.kind,
            Some(sha256),
        )?;
        Ok(())
    }

    fn parts(&self, first_name: &str, urls: &[String], hashes: &[Option<String>]) -> Result<()> {
        let names = multipart_names(first_name, urls.len())?;
        if urls.len() != hashes.len() {
            return Err("archive part hash count does not match URL count".into());
        }
        for ((name, url), hash) in names.iter().zip(urls).zip(hashes) {
            self.file(name, url, hash.as_deref())?;
        }
        Ok(())
    }
}

/// Apply a catalog's explicit HTTPS patch addon to the game installed from that catalog entry.
pub fn apply_catalog_addon(
    library: &Library,
    data_dir: &Path,
    game: &CatalogGame,
    addon_id: &str,
) -> Result<(PathBuf, usize)> {
    let addon = game
        .addons
        .iter()
        .find(|addon| addon.id == addon_id)
        .ok_or("unknown catalog addon")?;
    if addon.kind != AddonKind::Addon || !addon.content_available {
        return Err("catalog addon is not an available patch".into());
    }
    let url = addon
        .uri
        .as_deref()
        .filter(|uri| uri.starts_with("https://") || uri.starts_with("http://"))
        .ok_or("catalog addon needs a patch URL")?;
    let filename = addon
        .filename
        .as_deref()
        .ok_or("catalog addon needs an archive filename")?;
    if !matches!(classify(Some(filename), None), PackageType::Archive) {
        return Err("catalog addon must be a patch archive".into());
    }
    let installed_id = installed_game_id(library, game).ok_or("catalog game is not installed")?;
    let installed = library
        .games
        .get(&installed_id)
        .ok_or("installed game missing")?;
    let prefetch = Prefetch {
        data_dir,
        folder: "patch-downloads",
        id: &installed_id,
        title: &installed.title,
        kind: JobKind::Patch,
    };
    if addon.archive_parts.is_empty() {
        prefetch.file(filename, url, addon.sha256.as_deref())?;
        fetch_patch_archive(installed, data_dir, url, Path::new(filename))
    } else {
        prefetch.parts(filename, &addon.archive_parts, &addon.archive_part_hashes)?;
        fetch_patch_archive_parts(
            installed,
            data_dir,
            Path::new(filename),
            &addon.archive_parts,
        )
    }
}

fn pending_setup_path(data_dir: &Path, id: &str) -> Result<PathBuf> {
    validate_id(id)?;
    Ok(data_dir.join("setup-pending").join(format!("{id}.json")))
}

pub(crate) fn origin_for(game: &CatalogGame) -> CatalogOrigin {
    CatalogOrigin {
        qualified_id: game.qualified_id(),
        upload_date: game.upload_date.clone(),
        version: game.version.clone(),
    }
}

pub(crate) fn save_pending_setup_origin(
    data_dir: &Path,
    id: &str,
    game: &CatalogGame,
) -> Result<()> {
    let target = pending_setup_path(data_dir, id)?;
    fs::create_dir_all(target.parent().ok_or("invalid pending setup path")?)?;
    let temp = target.with_extension("json.tmp");
    let mut file = File::create(&temp)?;
    serde_json::to_writer_pretty(&mut file, &origin_for(game))?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(&target)?;
    }
    fs::rename(temp, target)?;
    Ok(())
}

pub fn attach_pending_setup_origin(data_dir: &Path, game: &mut Game) -> Result<()> {
    let target = pending_setup_path(data_dir, &game.id)?;
    match fs::read(target) {
        Ok(bytes) => game.origin = Some(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub fn clear_pending_setup_origin(data_dir: &Path, id: &str) -> Result<()> {
    let target = pending_setup_path(data_dir, id)?;
    match fs::remove_file(target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn install_game(
    library: &mut Library,
    data_dir: &Path,
    game: &CatalogGame,
) -> Result<InstallOutcome> {
    install_game_in(library, data_dir, &data_dir.join("games"), game)
}

/// Install a catalog game into a selected library drive.
pub fn install_game_in(
    library: &mut Library,
    data_dir: &Path,
    games_dir: &Path,
    game: &CatalogGame,
) -> Result<InstallOutcome> {
    match game.package_type {
        PackageType::Steam(app_id) => crate::steam_depot::install_steam_manifest_game_in(
            library, data_dir, games_dir, game, app_id,
        ),
        PackageType::Archive
        | PackageType::Executable
        | PackageType::Setup
        | PackageType::SetupArchive => {
            let id = install_id(game);
            let url = game
                .uri
                .as_deref()
                .ok_or("catalog entry has no HTTPS download URL")?;
            let filename = game
                .filename
                .as_deref()
                .ok_or("catalog entry needs a filename")?;
            let (folder, kind) = match game.package_type {
                PackageType::Archive => ("downloads", JobKind::Archive),
                PackageType::Executable => ("games", JobKind::Game),
                PackageType::Setup => ("setup-downloads", JobKind::Setup),
                PackageType::SetupArchive => ("setup-downloads", JobKind::SetupArchive),
                _ => unreachable!(),
            };
            let prefetch = Prefetch {
                data_dir,
                folder,
                id: &id,
                title: &game.title,
                kind,
            };
            let filename_path = Path::new(filename);
            match game.package_type {
                PackageType::Archive => {
                    if game.archive_parts.is_empty() {
                        prefetch.file(filename, url, game.sha256.as_deref())?;
                    } else {
                        prefetch.parts(filename, &game.archive_parts, &game.archive_part_hashes)?;
                    }
                    let pending = games_dir.join(&id);
                    let existed_before = fs::symlink_metadata(&pending).is_ok();
                    let result = if game.archive_parts.is_empty() {
                        fetch_archive_in(
                            library,
                            data_dir,
                            games_dir,
                            &id,
                            &game.title,
                            url,
                            filename_path,
                        )
                    } else {
                        fetch_archive_parts_in(
                            library,
                            data_dir,
                            games_dir,
                            &id,
                            &game.title,
                            filename_path,
                            &game.archive_parts,
                        )
                    };
                    if let Err(error) = result {
                        if !existed_before && pending.is_dir() && !library.games.contains_key(&id) {
                            save_pending_setup_origin(data_dir, &id, game)?;
                            return Ok(InstallOutcome::NeedsSelection(id));
                        }
                        return Err(error);
                    }
                }
                PackageType::Executable => {
                    prefetch.file(filename, url, game.sha256.as_deref())?;
                    fetch_game_in(
                        library,
                        data_dir,
                        games_dir,
                        &id,
                        &game.title,
                        url,
                        filename_path,
                    )?;
                }
                PackageType::Setup => {
                    if games_dir != data_dir.join("games") {
                        return Err("Setup packages currently require the built-in Default Library directory".into());
                    }
                    prefetch.file(filename, url, game.sha256.as_deref())?;
                    let (_, outcome) =
                        fetch_setup(library, data_dir, &id, &game.title, url, filename_path, &[])?;
                    if matches!(outcome, SetupOutcome::NeedsSelection(_)) {
                        save_pending_setup_origin(data_dir, &id, game)?;
                        return Ok(InstallOutcome::NeedsSelection(id));
                    }
                }
                PackageType::SetupArchive => {
                    if games_dir != data_dir.join("games") {
                        return Err("Setup packages currently require the built-in Default Library directory".into());
                    }
                    if game.archive_parts.is_empty() {
                        prefetch.file(filename, url, game.sha256.as_deref())?;
                    } else {
                        prefetch.parts(filename, &game.archive_parts, &game.archive_part_hashes)?;
                    }
                    let (_, outcome) = if game.archive_parts.is_empty() {
                        fetch_setup_archive(
                            library,
                            data_dir,
                            &id,
                            &game.title,
                            url,
                            filename_path,
                            &[],
                        )?
                    } else {
                        fetch_setup_archive_parts(
                            library,
                            data_dir,
                            &id,
                            &game.title,
                            filename_path,
                            &game.archive_parts,
                            &[],
                        )?
                    };
                    if matches!(outcome, SetupOutcome::NeedsSelection(_)) {
                        save_pending_setup_origin(data_dir, &id, game)?;
                        return Ok(InstallOutcome::NeedsSelection(id));
                    }
                }
                _ => unreachable!(),
            }
            stamp_origin(library, &id, game);
            Ok(InstallOutcome::Installed(id))
        }
        PackageType::Magnet => {
            if games_dir != data_dir.join("games") {
                return Err(
                    "Magnet packages currently require the built-in Default Library directory"
                        .into(),
                );
            }
            let id = install_id(game);
            let uri = game
                .uri
                .as_deref()
                .ok_or("catalog entry has no magnet URI")?;
            let pending = data_dir.join("games").join(&id);
            let existed_before = fs::symlink_metadata(&pending).is_ok();
            if let Err(error) = fetch_magnet(library, data_dir, &id, &game.title, uri) {
                if !existed_before && pending.is_dir() && !library.games.contains_key(&id) {
                    save_pending_setup_origin(data_dir, &id, game)?;
                    return Ok(InstallOutcome::NeedsSelection(id));
                }
                return Err(error);
            }
            stamp_origin(library, &id, game);
            Ok(InstallOutcome::Installed(id))
        }
        PackageType::Unknown => Err("catalog entry has no supported install method".into()),
    }
}

pub(crate) fn stamp_origin(library: &mut Library, id: &str, game: &CatalogGame) {
    if let Some(installed) = library.games.get_mut(id) {
        installed.origin = Some(origin_for(game));
        installed.steam_app_id = game.steam_app_id.or_else(|| {
            game.id
                .strip_prefix("steam-")
                .and_then(|value| value.parse::<u32>().ok())
        });
        if game.banner_url.is_some() {
            installed.banner_url = game.banner_url.clone();
        }
        if game.cover_url.is_some() {
            installed.cover_url = game.cover_url.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::legacy_catalog::parse_feed;

    #[test]
    fn dlc_without_download_artifact_is_unavailable() {
        let payload = br#"{"games":[{"id":"sample","title":"Sample","addons":[{"id":"200","kind":"dlc","title":"Metadata only"},{"id":"201","kind":"dlc","title":"Archive","downloadUrl":"https://example.org/dlc.zip"}]}]}"#;
        let games = parse_feed(payload, "sample-source").unwrap();
        assert!(!games[0].addons[0].content_available);
        assert!(games[0].addons[1].content_available);
    }
    use crate::add_game;
    use crate::jobs::{DownloadJob, JobKind, JobStatus};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_arachnel_feed_shapes_and_delivery() {
        let feed = br#"{"downloads":[{"title":"Sample Game","uris":["https://example.org/sample.zip"],"uploadDate":"2026-01-01"},{"title":"Magnet Game","uris":["magnet:?xt=urn:btih:123"]},{"title":"Extra","kind":"dlc"}]}"#;
        let games = parse_feed(feed, "sample").unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].package_type, PackageType::Archive);
        assert_eq!(games[1].package_type, PackageType::Magnet);
        let steam = br#"{"entries":[{"id":"steam-480","title":"Spacewar","steamAppId":"480"}]}"#;
        assert_eq!(
            parse_feed(steam, "store").unwrap()[0].package_type,
            PackageType::Steam(480)
        );
        let installers = br#"{"entries":[{"title":"Setup Game","downloadUrl":"https://example.org/setup.exe"},{"title":"MSI Game","downloadUrl":"https://example.org/game.msi"},{"title":"Archived Setup","downloadUrl":"https://example.org/game.zip","packageType":"setup-archive"}]}"#;
        let kinds: Vec<_> = parse_feed(installers, "store")
            .unwrap()
            .into_iter()
            .map(|entry| entry.package_type)
            .collect();
        assert_eq!(
            kinds,
            vec![
                PackageType::Setup,
                PackageType::Setup,
                PackageType::SetupArchive
            ]
        );
        let fallback =
            br#"{"downloads":[],"entries":[{"title":"Second","kind":"Game","steamAppId":480}]}"#;
        assert_eq!(parse_feed(fallback, "store").unwrap().len(), 1);
    }

    #[test]
    fn stamp_origin_persists_steam_identity_and_artwork() {
        let root = std::env::temp_dir().join(format!(
            "azdl-catalog-artwork-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install_dir = root.join("game");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(install_dir.join("game.exe"), b"game").unwrap();

        let mut library = Library::default();
        add_game(
            &mut library,
            "steam-620",
            "Portal 2",
            &install_dir,
            Path::new("game.exe"),
        )
        .unwrap();
        let catalog_game = parse_feed(
            br#"{"entries":[{"id":"steam-620","title":"Portal 2","steamAppId":620,"version":"1.0","bannerUrl":"https://cdn.example/header.jpg","coverUrl":"https://cdn.example/cover.jpg"}]}"#,
            "steam",
        )
        .unwrap()
        .remove(0);

        stamp_origin(&mut library, "steam-620", &catalog_game);

        let installed = &library.games["steam-620"];
        assert_eq!(installed.steam_app_id, Some(620));
        assert_eq!(
            installed.banner_url.as_deref(),
            Some("https://cdn.example/header.jpg")
        );
        assert_eq!(
            installed.cover_url.as_deref(),
            Some("https://cdn.example/cover.jpg")
        );
        assert_eq!(
            installed.origin.as_ref().unwrap().version.as_deref(),
            Some("1.0")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalog_split_rar_downloads_all_cached_parts_before_install() {
        if Command::new("bsdtar").arg("--version").output().is_err()
            || Command::new("unrar").arg("-?").output().is_err()
        {
            return;
        }
        let feed = br#"{"entries":[{"id":"split","title":"Split Game","archiveParts":["https://example.org/game.part01.rar","https://example.org/game.part02.rar","https://example.org/game.part03.rar","https://example.org/game.part04.rar"],"addons":[{"id":"fix","title":"Fix","kind":"addon","archiveParts":["https://example.org/fix.part01.rar","https://example.org/fix.part02.rar"]}]}]}"#;
        let mut game = parse_feed(feed, "source").unwrap().remove(0);
        let first_fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/test_read_format_rar5_multiarchive_solid.part01.rar");
        game.archive_part_hashes[0] = Some(crate::sha256::file_sha256(&first_fixture).unwrap());
        assert_eq!(game.package_type, PackageType::Archive);
        assert_eq!(game.filename.as_deref(), Some("game.part01.rar"));
        assert_eq!(game.archive_parts.len(), 4);
        assert_eq!(game.addons[0].archive_parts.len(), 2);
        let malformed = br#"{"entries":[{"id":"split","title":"Split Game","archiveParts":["http://example.org/game.part01.rar","https://example.org/game.part02.rar"]}]}"#;
        assert!(parse_feed(malformed, "source").is_err());
        let unsafe_name = br#"{"entries":[{"id":"split","title":"Split Game","filename":"../game.part01.rar","archiveParts":["https://example.org/1","https://example.org/2"]}]}"#;
        assert!(parse_feed(unsafe_name, "source").is_err());
        let mirrors = br#"{"entries":[{"id":"game","title":"Game","uris":["https://example.org/game.zip","https://mirror.example.org/game.zip"]}]}"#;
        assert!(
            parse_feed(mirrors, "source").unwrap()[0]
                .archive_parts
                .is_empty()
        );
        let digests = serde_json::json!({"entries":[{
            "id":"split","title":"Split Game","archiveParts":[
                {"url":"https://example.org/game.part01.rar","sha256":"a".repeat(64)},
                {"url":"https://example.org/game.part02.rar"}
            ]
        }]});
        let checked = parse_feed(&serde_json::to_vec(&digests).unwrap(), "source")
            .unwrap()
            .remove(0);
        assert_eq!(
            checked.archive_part_hashes[0].as_deref(),
            Some("a".repeat(64).as_str())
        );
        assert!(checked.archive_part_hashes[1].is_none());

        let root = std::env::temp_dir().join(format!(
            "azdl-catalog-parts-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let id = install_id(&game);
        let downloads = root.join("downloads").join(&id);
        fs::create_dir_all(&downloads).unwrap();
        fs::create_dir_all(root.join("jobs")).unwrap();
        for number in 1..=4 {
            let name = format!("game.part{number:02}.rar");
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "tests/fixtures/test_read_format_rar5_multiarchive_solid.part{number:02}.rar"
            ));
            let target = downloads.join(&name);
            fs::copy(fixture, &target).unwrap();
            let mut hash = 0xcbf29ce484222325u64;
            for byte in target.to_string_lossy().as_bytes() {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
            let job_id = format!("{hash:016x}");
            let job = DownloadJob {
                id: job_id.clone(),
                kind: JobKind::Archive,
                url: game.archive_parts[number - 1].clone(),
                target,
                status: JobStatus::Complete,
                pid: None,
                error: None,
                updated_unix: 0,
                expected_sha256: None,
                install: None,
                install_state: None,
                install_error: None,
                progress_bytes: None,
                total_bytes: None,
            };
            fs::write(
                root.join("jobs").join(format!("{job_id}.json")),
                serde_json::to_vec(&job).unwrap(),
            )
            .unwrap();
        }
        let mut library = Library::default();
        assert!(matches!(
            install_game(&mut library, &root, &game).unwrap(),
            InstallOutcome::NeedsSelection(_) | InstallOutcome::Installed(_)
        ));
        assert!(root.join("games").join(&id).join("test6.bin").is_file());
        assert!(
            root.join("games")
                .join(&id)
                .join("elf-Linux-ARMv7-ls")
                .is_file()
        );
        assert!(
            crate::jobs::list_jobs(&root)
                .unwrap()
                .iter()
                .any(|job| job.expected_sha256 == game.archive_part_hashes[0])
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalog_digest_rejects_modified_cached_archive_before_install() {
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let feed = serde_json::json!({"entries":[{
            "id":"archive","title":"Archive","downloadUrl":"https://example.org/archive.zip",
            "sha256":expected
        }]});
        let game = parse_feed(&serde_json::to_vec(&feed).unwrap(), "source")
            .unwrap()
            .remove(0);
        assert_eq!(game.sha256.as_deref(), Some(expected));
        let root = std::env::temp_dir().join(format!(
            "azdl-catalog-hash-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let id = install_id(&game);
        let downloads = root.join("downloads").join(&id);
        fs::create_dir_all(&downloads).unwrap();
        let target = downloads.join("archive.zip");
        fs::write(&target, b"modified").unwrap();
        let mut hash = 0xcbf29ce484222325u64;
        for byte in target.to_string_lossy().as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        let job_id = format!("{hash:016x}");
        fs::create_dir_all(root.join("jobs")).unwrap();
        let job = DownloadJob {
            id: job_id.clone(),
            kind: JobKind::Archive,
            url: game.uri.clone().unwrap(),
            target,
            status: JobStatus::Complete,
            pid: None,
            error: None,
            updated_unix: 0,
            expected_sha256: None,
            install: None,
            install_state: None,
            install_error: None,
            progress_bytes: None,
            total_bytes: None,
        };
        fs::write(
            root.join("jobs").join(format!("{job_id}.json")),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        assert!(install_game(&mut Library::default(), &root, &game).is_err());
        assert!(!root.join("games").join(&id).exists());
        assert_eq!(
            crate::jobs::load_job(&root, &job_id)
                .unwrap()
                .expected_sha256
                .as_deref(),
            Some(expected)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalog_split_rar_addon_applies_from_cached_parts() {
        if Command::new("bsdtar").arg("--version").output().is_err()
            || Command::new("unrar").arg("-?").output().is_err()
        {
            return;
        }
        let feed = br#"{"entries":[{"id":"game","title":"Game","addons":[{"id":"fix","title":"Fix","kind":"addon","archiveParts":["https://example.org/fix.part01.rar","https://example.org/fix.part02.rar","https://example.org/fix.part03.rar","https://example.org/fix.part04.rar"]}]}]}"#;
        let catalog = parse_feed(feed, "source").unwrap().remove(0);
        let root = std::env::temp_dir().join(format!(
            "azdl-catalog-patch-parts-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("game.exe"), b"game").unwrap();
        fs::write(install.join("test.bin"), b"original").unwrap();
        let mut library = Library::default();
        add_game(
            &mut library,
            "game",
            "Game",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        library.games.get_mut("game").unwrap().origin = Some(origin_for(&catalog));
        let downloads = root.join("patch-downloads/game");
        fs::create_dir_all(&downloads).unwrap();
        fs::create_dir_all(root.join("jobs")).unwrap();
        for number in 1..=4 {
            let target = downloads.join(format!("fix.part{number:02}.rar"));
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "tests/fixtures/test_read_format_rar5_multiarchive_solid.part{number:02}.rar"
            ));
            fs::copy(fixture, &target).unwrap();
            let mut hash = 0xcbf29ce484222325u64;
            for byte in target.to_string_lossy().as_bytes() {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
            let job_id = format!("{hash:016x}");
            let job = DownloadJob {
                id: job_id.clone(),
                kind: JobKind::Patch,
                url: catalog.addons[0].archive_parts[number - 1].clone(),
                target,
                status: JobStatus::Complete,
                pid: None,
                error: None,
                updated_unix: 0,
                expected_sha256: None,
                install: None,
                install_state: None,
                install_error: None,
                progress_bytes: None,
                total_bytes: None,
            };
            fs::write(
                root.join("jobs").join(format!("{job_id}.json")),
                serde_json::to_vec(&job).unwrap(),
            )
            .unwrap();
        }
        let (_, count) = apply_catalog_addon(&library, &root, &catalog, "fix").unwrap();
        assert_eq!(count, 9);
        assert_eq!(fs::metadata(install.join("test.bin")).unwrap().len(), 1200);
        crate::patch::rollback_patch(&library.games["game"], &root).unwrap();
        assert_eq!(fs::read(install.join("test.bin")).unwrap(), b"original");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ambiguous_catalog_archive_keeps_files_and_origin_for_selection() {
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "azdl-catalog-pending-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("mode-a.exe"), b"game").unwrap();
        fs::write(source.join("mode-b.exe"), b"game").unwrap();
        let game = parse_feed(br#"{"games":[{"id":"modes","title":"Modes","downloadUrl":"https://example.org/modes.zip","uploadDate":"2026-10-01"}]}"#, "source").unwrap().remove(0);
        let id = install_id(&game);
        let target = root.join("downloads").join(&id).join("modes.zip");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        assert!(
            Command::new("bsdtar")
                .args(["-a", "-c", "-f"])
                .arg(&target)
                .arg("-C")
                .arg(&source)
                .arg(".")
                .status()
                .unwrap()
                .success()
        );
        let mut hash = 0xcbf29ce484222325u64;
        for byte in target.to_string_lossy().as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        let job_id = format!("{hash:016x}");
        fs::create_dir_all(root.join("jobs")).unwrap();
        let job = DownloadJob {
            id: job_id.clone(),
            kind: JobKind::Archive,
            url: game.uri.clone().unwrap(),
            target: target.clone(),
            status: JobStatus::Complete,
            pid: None,
            error: None,
            updated_unix: 0,
            expected_sha256: None,
            install: None,
            install_state: None,
            install_error: None,
            progress_bytes: None,
            total_bytes: None,
        };
        fs::write(
            root.join("jobs").join(format!("{job_id}.json")),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        let mut library = Library::default();
        assert!(
            matches!(install_game(&mut library, &root, &game).unwrap(), InstallOutcome::NeedsSelection(ref found) if found == &id)
        );
        assert!(library.games.is_empty());
        let installed = root.join("games").join(&id);
        assert!(installed.join("mode-a.exe").is_file());
        add_game(
            &mut library,
            &id,
            "Modes",
            &installed,
            Path::new("mode-a.exe"),
        )
        .unwrap();
        let registered = library.games.get_mut(&id).unwrap();
        attach_pending_setup_origin(&root, registered).unwrap();
        assert_eq!(
            registered.origin.as_ref().unwrap().qualified_id,
            "source:modes"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_feed_search_and_lookup() {
        let feed = br#"{"games":[{"id":"one","title":"Game One","downloadUrl":"https://example.org/one.exe"}]}"#;
        let games = parse_feed(feed, "local").unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].id, "one");
        assert_eq!(games[0].package_type, PackageType::Executable);
    }

    #[test]
    fn test_catalog_install_never_spawns_steam() {
        let root = std::env::temp_dir().join(format!("azdl-cat-no-steam-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let game = CatalogGame {
            id: "steam-999999".to_string(),
            title: "Standalone Test Game".to_string(),
            description: "Test description".to_string(),
            package_type: PackageType::Steam(999999),
            source_id: "store".to_string(),
            steam_app_id: Some(999999),
            filename: None,
            uri: None,
            sha256: None,
            archive_parts: Vec::new(),
            archive_part_hashes: Vec::new(),
            upload_date: None,
            version: None,
            addons: Vec::new(),
            cover_url: None,
            banner_url: None,
            file_size: None,
            genres: Vec::new(),
        };
        let mut library = Library::default();
        // A missing bundled engine must fail locally before contacting any content service.
        let res = install_game(&mut library, &root, &game);
        assert!(res.is_err());
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("Steam download engine"));
        fs::remove_dir_all(root).unwrap();
    }
}
