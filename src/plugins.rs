use crate::archive::extract_checked;
use crate::catalog::{CatalogGame, PackageType, parse_feed};
use crate::validate_id;
use crate::{Result, clean_command};
use libloading::Library as SharedLibrary;
use serde::Deserialize;
use std::ffi::{CString, c_char, c_int, c_void};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub library: String,
    #[serde(default)]
    pub catalog_url: Option<String>,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl PluginManifest {
    /// Whether this plugin advertises a specific capability token.
    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c.eq_ignore_ascii_case(cap))
            || (self.id == "steamidra"
                && matches!(
                    cap,
                    "steam_depot_download" | "manifest_provider" | "catalog_fetch"
                ))
    }
}

/// Derive the manifest bundle download URL for a Steam app from a plugin's catalog URL.
pub fn derive_manifest_download_url(catalog_url: &str, app_id: u32) -> String {
    let base = catalog_url.trim_end_matches('/');
    if let Some(prefix) = base.strip_suffix("/catalog") {
        format!("{prefix}/download/{app_id}")
    } else {
        format!("{base}/download/{app_id}")
    }
}

/// Resolve the manifest bundle download URL from the plugin's configuration, falling back to default.
pub fn manifest_download_url_for_plugin(data_dir: &Path, plugin_id: &str, app_id: u32) -> String {
    let root = plugin_dir(data_dir).join(plugin_id);
    if let Ok(manifest) = read_manifest(&root) {
        if let Some(template) = manifest.download_url.as_deref() {
            if template.contains("{app_id}") {
                return template.replace("{app_id}", &app_id.to_string());
            }
            if template.contains("{id}") {
                return template.replace("{id}", &app_id.to_string());
            }
            return format!("{template}/{app_id}");
        }
        if let Some(catalog) = manifest.catalog_url.as_deref() {
            return derive_manifest_download_url(catalog, app_id);
        }
    }
    format!("https://ryuu.badkiko.ru/v1/download/{app_id}")
}

/// Check if an installed plugin advertises a specific capability.
pub fn plugin_has_capability(data_dir: &Path, plugin_id: &str, capability: &str) -> bool {
    let root = plugin_dir(data_dir).join(plugin_id);
    match read_manifest(&root) {
        Ok(manifest) => manifest.has_capability(capability),
        Err(_) => false,
    }
}

fn plugin_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("plugins")
}

fn read_manifest(root: &Path) -> Result<PluginManifest> {
    let manifest: PluginManifest = serde_json::from_slice(&fs::read(root.join("plugin.json"))?)?;
    validate_id(&manifest.id)?;
    if manifest.name.trim().is_empty() || manifest.version.trim().is_empty() {
        return Err("plugin name or version is empty".into());
    }
    if manifest.api_version != 4 {
        return Err("only Arachnel plugin API 4 is supported".into());
    }
    if !matches!(
        Path::new(&manifest.library)
            .components()
            .collect::<Vec<_>>()
            .as_slice(),
        [Component::Normal(_)]
    ) {
        return Err("plugin library must be a filename".into());
    }
    Ok(manifest)
}

fn library_file(root: &Path, manifest: &PluginManifest) -> Result<PathBuf> {
    let base = &manifest.library;
    let candidates = [
        base.clone(),
        format!(
            "{}{base}{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ),
        format!("{base}{}", std::env::consts::DLL_SUFFIX),
    ];
    for name in candidates {
        let path = root.join(name);
        if fs::symlink_metadata(&path)
            .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
        {
            return Ok(path);
        }
    }
    Err("plugin library file is missing".into())
}

/// Resolve the library declared by one installed plugin, without searching other providers.
pub(crate) fn installed_library_file(data_dir: &Path, id: &str) -> Result<PathBuf> {
    validate_id(id)?;
    let root = plugin_dir(data_dir).join(id);
    let manifest = read_manifest(&root)?;
    if manifest.id != id {
        return Err("plugin folder does not match its ID".into());
    }
    library_file(&root, &manifest)
}

fn bundle_root(staging: &Path) -> Result<PathBuf> {
    if staging.join("plugin.json").is_file() {
        return Ok(staging.to_path_buf());
    }
    let mut found = None;
    for entry in fs::read_dir(staging)? {
        let path = entry?.path();
        if path.is_dir() && path.join("plugin.json").is_file() {
            if found.is_some() {
                return Err("plugin package has multiple manifests".into());
            }
            found = Some(path);
        }
    }
    found.ok_or_else(|| "plugin package has no plugin.json".into())
}

type ApiVersion = unsafe extern "C" fn() -> c_int;
type Create = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type Destroy = unsafe extern "C" fn(*mut c_void);
type CatalogJson = unsafe extern "C" fn(*mut c_void, *mut *mut c_char, *mut usize) -> c_int;
type CatalogFree = unsafe extern "C" fn(*mut c_char);

#[cfg(target_os = "linux")]
fn ensure_qt_application() {
    unsafe {
        if let Ok(lib) = libloading::Library::new("libQt6Core.so.6") {
            type InstanceExistsFn = unsafe extern "C" fn() -> bool;
            type InitFn = unsafe extern "C" fn(
                *mut std::ffi::c_void,
                *mut i32,
                *mut *mut std::ffi::c_char,
                i32,
            );
            let needs_init = lib
                .get::<InstanceExistsFn>(b"_ZN16QCoreApplication14instanceExistsEv\0")
                .is_ok_and(|f| !f());
            if needs_init
                && let Ok(init_fn) = lib.get::<InitFn>(b"_ZN16QCoreApplicationC1ERiPPci\0")
            {
                static mut ARGC: i32 = 1;
                static mut APP_NAME: [u8; 5] = *b"azdl\0";
                static mut ARGV: [*mut std::ffi::c_char; 2] =
                    [std::ptr::null_mut(), std::ptr::null_mut()];
                ARGV[0] = (&raw mut APP_NAME).cast();
                let app_mem = Box::into_raw(Box::new([0u8; 64]));
                init_fn(app_mem.cast(), &raw mut ARGC, (&raw mut ARGV).cast(), 0);
                std::mem::forget(lib);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn ensure_qt_application() {}

fn open_api(path: &Path) -> Result<SharedLibrary> {
    ensure_qt_application();
    // SAFETY: Loading a user-installed native plugin executes its initializer. The API version
    // and required C exports are checked before any plugin instance is created.
    let library = unsafe { SharedLibrary::new(path) }?;
    unsafe {
        let version = library.get::<ApiVersion>(b"arachnel_plugin_api_version\0")?;
        if version() != 4 {
            return Err("plugin library does not implement API 4".into());
        }
        library.get::<Create>(b"arachnel_plugin_create\0")?;
        library.get::<Destroy>(b"arachnel_plugin_destroy\0")?;
        library.get::<CatalogJson>(b"arachnel_plugin_catalog_json\0")?;
        library.get::<CatalogFree>(b"arachnel_plugin_catalog_json_free\0")?;
    }
    Ok(library)
}

fn native_catalog_direct(root: &Path, manifest: &PluginManifest) -> Result<Vec<u8>> {
    let library = open_api(&library_file(root, manifest)?)?;
    let root_utf8 = root.to_str().ok_or("plugin path is not UTF-8")?;
    let root_arg = CString::new(root_utf8)?;
    // SAFETY: open_api verified the C exports and signatures from Arachnel's API 4. The plugin
    // instance and returned allocation are released with the plugin's matching exported calls
    // before the library is unloaded.
    unsafe {
        let create = library.get::<Create>(b"arachnel_plugin_create\0")?;
        let destroy = library.get::<Destroy>(b"arachnel_plugin_destroy\0")?;
        let catalog = library.get::<CatalogJson>(b"arachnel_plugin_catalog_json\0")?;
        let free = library.get::<CatalogFree>(b"arachnel_plugin_catalog_json_free\0")?;
        let instance = create(root_arg.as_ptr());
        if instance.is_null() {
            return Err("plugin could not be created".into());
        }
        let mut ptr: *mut c_char = std::ptr::null_mut();
        let mut len: usize = 0;
        let code = catalog(instance, &mut ptr, &mut len);
        let payload = if code == 0 && !ptr.is_null() && len <= 128 * 1024 * 1024 {
            Some(std::slice::from_raw_parts(ptr.cast::<u8>(), len).to_vec())
        } else {
            None
        };
        if !ptr.is_null() {
            free(ptr);
        }
        destroy(instance);
        payload.ok_or_else(|| format!("plugin catalog failed (code {code})").into())
    }
}

fn native_catalog(root: &Path, manifest: &PluginManifest) -> Result<Vec<u8>> {
    let root_buf = root.to_path_buf();
    let manifest_clone = manifest.clone();
    let (tx, rx) = mpsc::channel::<std::result::Result<Vec<u8>, String>>();
    let handle = std::thread::Builder::new()
        .name(format!("plugin-catalog-{}", manifest.id))
        .spawn(move || {
            let res = native_catalog_direct(&root_buf, &manifest_clone).map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
    if let Err(err) = handle {
        return Err(format!("failed to spawn plugin worker thread: {err}").into());
    }
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(res) => res.map_err(|e| e.into()),
        Err(_) => Err("plugin catalog query timed out after 30 seconds".into()),
    }
}

fn cached_plugin_feed(data_dir: &Path, id: &str) -> PathBuf {
    data_dir.join("catalog-cache").join(format!("{id}.json"))
}

fn fetch_remote_plugin_catalog(url: &str) -> Result<Vec<u8>> {
    let temp_file =
        std::env::temp_dir().join(format!("azdl-plugin-cat-{}.tmp", std::process::id()));
    let status = clean_command("curl")
        .args([
            "--fail",
            "--location",
            "--retry",
            "3",
            "--max-time",
            "30",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
        ])
        .arg("--output")
        .arg(&temp_file)
        .arg(url)
        .status()?;
    if !status.success() {
        if temp_file.exists() {
            let _ = fs::remove_file(&temp_file);
        }
        return Err(format!("catalog download failed ({status})").into());
    }
    let bytes = fs::read(&temp_file)?;
    let _ = fs::remove_file(&temp_file);
    Ok(bytes)
}

fn validate_catalog_payload(payload: &[u8]) -> Result<()> {
    let json: serde_json::Value = serde_json::from_slice(payload)?;
    if let Some(schema) = json.get("schema").and_then(|v| v.as_str()) {
        if schema != "arachnel.plugin.catalog.v1" {
            return Err("plugin returned an unsupported catalog schema".into());
        }
    } else if json.get("entries").is_none()
        && json.get("downloads").is_none()
        && json.get("games").is_none()
        && !json.is_array()
    {
        return Err("plugin returned an unsupported catalog schema".into());
    }
    Ok(())
}

fn write_plugin_catalog_cache(data_dir: &Path, id: &str, payload: &[u8]) -> Result<()> {
    let target = cached_plugin_feed(data_dir, id);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = target.with_extension("json.tmp");
    fs::write(&temp, payload)?;
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(&target)?;
    }
    fs::rename(temp, target)?;
    Ok(())
}

fn refresh_plugin_catalog_payload(
    data_dir: &Path,
    id: &str,
    root: &Path,
    manifest: &PluginManifest,
) -> Result<Vec<u8>> {
    let payload = match native_catalog(root, manifest) {
        Ok(bytes) => bytes,
        Err(native_err) => {
            if let Some(url) = &manifest.catalog_url {
                fetch_remote_plugin_catalog(url)?
            } else {
                return Err(native_err);
            }
        }
    };
    validate_catalog_payload(&payload)?;
    let _ = write_plugin_catalog_cache(data_dir, id, &payload);
    Ok(payload)
}

pub fn list_plugins(data_dir: &Path) -> Result<Vec<PluginManifest>> {
    let root = plugin_dir(data_dir);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut plugins = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || !entry.path().join("plugin.json").is_file() {
            continue;
        }
        let manifest = read_manifest(&entry.path())?;
        if entry.file_name() != manifest.id.as_str() {
            return Err("plugin folder does not match its ID".into());
        }
        plugins.push(manifest);
    }
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(plugins)
}

pub fn remove_plugin(data_dir: &Path, id: &str) -> Result<()> {
    validate_id(id)?;
    let root = plugin_dir(data_dir).join(id);
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    let cache = cached_plugin_feed(data_dir, id);
    if cache.exists() {
        let _ = fs::remove_file(cache);
    }
    Ok(())
}

pub fn refresh_plugin_catalog(data_dir: &Path, id: &str) -> Result<usize> {
    validate_id(id)?;
    let root = plugin_dir(data_dir).join(id);
    if !root.is_dir() {
        return Err("unknown catalog source or plugin".into());
    }
    let manifest = read_manifest(&root)?;
    if manifest.id != id {
        return Err("plugin folder does not match its ID".into());
    }
    let payload = refresh_plugin_catalog_payload(data_dir, id, &root, &manifest)?;
    let games = parse_feed(&payload, id)?;
    Ok(games.len())
}

pub fn catalog_for(data_dir: &Path, id: &str) -> Result<Vec<CatalogGame>> {
    validate_id(id)?;
    let root = plugin_dir(data_dir).join(id);
    if !root.is_dir() {
        return Err("unknown catalog source or plugin".into());
    }
    let manifest = read_manifest(&root)?;
    if manifest.id != id {
        return Err("plugin folder does not match its ID".into());
    }

    let cache_path = cached_plugin_feed(data_dir, id);
    let payload = if cache_path.is_file() {
        match fs::read(&cache_path) {
            Ok(bytes) if validate_catalog_payload(&bytes).is_ok() => bytes,
            _ => refresh_plugin_catalog_payload(data_dir, id, &root, &manifest)?,
        }
    } else {
        refresh_plugin_catalog_payload(data_dir, id, &root, &manifest)?
    };

    let json: serde_json::Value = serde_json::from_slice(&payload)?;
    if json
        .get("entries")
        .and_then(|entries| entries.as_array())
        .is_some_and(Vec::is_empty)
    {
        return Ok(Vec::new());
    }
    let mut games = parse_feed(&payload, id)?;
    for game in &mut games {
        classify_plugin_delivery(game);
    }
    Ok(games)
}

fn classify_plugin_delivery(game: &mut CatalogGame) {
    game.package_type = match (game.plugin_install_kind, &game.package_type) {
        (Some(0), PackageType::Archive | PackageType::Executable | PackageType::Magnet) => {
            game.plugin_basic_install = true;
            game.package_type.clone()
        }
        (Some(1), PackageType::Archive) => {
            game.plugin_basic_install = true;
            PackageType::SetupArchive
        }
        (Some(1), PackageType::SetupArchive) => {
            game.plugin_basic_install = true;
            PackageType::SetupArchive
        }
        (Some(1), PackageType::Setup | PackageType::Executable) => {
            game.plugin_basic_install = true;
            PackageType::Setup
        }
        _ => PackageType::Plugin,
    };
}

pub fn catalog_games(data_dir: &Path) -> Result<Vec<CatalogGame>> {
    let mut games = Vec::new();
    for manifest in list_plugins(data_dir)? {
        match catalog_for(data_dir, &manifest.id) {
            Ok(plugin_games) => games.extend(plugin_games),
            Err(err) => {
                eprintln!(
                    "Warning: failed to load catalog for plugin '{}': {err}",
                    manifest.id
                );
            }
        }
    }
    Ok(games)
}

/// Resolve a Steam app through one installed download provider at install time.
pub fn game_for_steam_app(
    data_dir: &Path,
    plugin_id: &str,
    app_id: u32,
    title: &str,
) -> Result<CatalogGame> {
    if app_id == 0 {
        return Err("Steam App ID must be positive".into());
    }
    let manifest = list_plugins(data_dir)?
        .into_iter()
        .find(|manifest| manifest.id == plugin_id)
        .ok_or("selected download plugin is not installed")?;

    // The steamidra bundle provides depotkit. Its manifest relay is addressed by App ID,
    // so the full plugin catalog is unnecessary for an installation attempt.
    if manifest.id == "steamidra" {
        let payload = serde_json::json!({"entries": [{
            "id": format!("steam-{app_id}"),
            "title": title,
            "steamAppId": app_id,
        }]});
        let mut game = parse_feed(&serde_json::to_vec(&payload)?, plugin_id)?
            .into_iter()
            .next()
            .ok_or("could not build Steam game install request")?;
        game.package_type = PackageType::Plugin;
        game.plugin_install_kind = Some(2);
        return Ok(game);
    }

    let game = catalog_for(data_dir, plugin_id)?
        .into_iter()
        .find(|game| game.steam_app_id == Some(app_id))
        .ok_or_else(|| format!("{plugin_id} has no download for Steam App ID {app_id}"))?;
    if matches!(
        game.package_type,
        PackageType::Steam(_) | PackageType::Unknown
    ) || (game.package_type == PackageType::Plugin && game.uri.is_none())
    {
        return Err(format!(
            "{plugin_id} needs a plugin-owned installer that azdl does not support yet"
        )
        .into());
    }
    Ok(game)
}

pub fn install_bundle(data_dir: &Path, archive: &Path) -> Result<PluginManifest> {
    install_bundle_impl(data_dir, archive, None)
}

pub(crate) fn install_bundle_expected(
    data_dir: &Path,
    archive: &Path,
    expected_id: &str,
) -> Result<PluginManifest> {
    install_bundle_impl(data_dir, archive, Some(expected_id))
}

fn install_bundle_impl(
    data_dir: &Path,
    archive: &Path,
    expected_id: Option<&str>,
) -> Result<PluginManifest> {
    if archive
        .extension()
        .is_none_or(|extension| extension != "arach")
    {
        return Err("plugin package must be a .arach archive".into());
    }
    let archive = fs::canonicalize(archive)?;
    if !archive.is_file() {
        return Err("plugin package is not a file".into());
    }
    let plugins = plugin_dir(data_dir);
    fs::create_dir_all(&plugins)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staging = plugins.join(format!(".extract-{}-{nonce}", std::process::id()));
    fs::create_dir(&staging)?;
    let result = (|| {
        extract_checked(&archive, &staging)?;
        let root = bundle_root(&staging)?;
        let manifest = read_manifest(&root)?;
        if expected_id.is_some_and(|id| id != manifest.id) {
            return Err("plugin package ID does not match the store entry".into());
        }
        let destination = plugins.join(&manifest.id);
        if fs::symlink_metadata(&destination).is_ok() {
            return Err("plugin ID is already installed".into());
        }
        if let Ok(lib_path) = library_file(&root, &manifest) {
            match open_api(&lib_path) {
                Ok(_) => {}
                Err(err) => {
                    if manifest.catalog_url.is_none() && cfg!(target_os = "linux") {
                        return Err(err);
                    }
                    eprintln!(
                        "Notice: plugin native library could not be dynamically loaded ({err}); installed for catalog URL and bundled kits."
                    );
                }
            }
        } else if manifest.catalog_url.is_none() {
            return Err("plugin library file is missing".into());
        }
        fs::rename(&root, destination)?;
        Ok(manifest)
    })();
    if staging.exists() {
        fs::remove_dir_all(staging)?;
    }
    result
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::Library;
    use crate::catalog::install_game;
    use std::process::Command;

    #[test]
    fn store_install_rejects_bundle_with_different_plugin_id() {
        if Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "azdl-plugin-id-check-{}-{nonce}",
            std::process::id()
        ));
        let source = root.join("bundle");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("plugin.json"),
            r#"{"id":"unexpected","name":"Unexpected","version":"1.0","apiVersion":4,"library":"stub"}"#,
        )
        .unwrap();
        let archive = root.join("package.arach");
        assert!(
            Command::new("bsdtar")
                .args(["-a", "-c", "-f"])
                .arg(&archive)
                .arg("-C")
                .arg(&source)
                .arg("plugin.json")
                .status()
                .unwrap()
                .success()
        );
        assert!(install_bundle_expected(&root, &archive, "expected").is_err());
        assert!(!root.join("plugins/unexpected").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn standard_plugin_deliveries_can_use_generic_install() {
        let payload = br#"{"entries":[
            {"id":"portable","title":"Portable","kind":"game","installKind":0,"uris":["magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"]},
            {"id":"direct","title":"Direct","kind":"game","installKind":0,"uris":["https://example.org/game.zip"]},
            {"id":"installer","title":"Installer","kind":"game","installKind":1,"uris":["https://example.org/setup.exe"]},
            {"id":"setup-package","title":"Setup Package","kind":"game","installKind":1,"uris":["https://example.org/setup.zip"]},
            {"id":"fix-owned","title":"Fix Owned","kind":"game","installKind":2,"uris":["https://example.org/fix.zip"]},
            {"id":"owned-download","title":"Owned Download","kind":"game","installKind":0,"steamAppId":"480"}
        ]}"#;
        let mut games = parse_feed(payload, "stub").unwrap();
        for game in &mut games {
            classify_plugin_delivery(game);
        }
        assert_eq!(games[0].package_type, PackageType::Magnet);
        assert_eq!(games[1].package_type, PackageType::Archive);
        assert_eq!(games[2].package_type, PackageType::Setup);
        assert_eq!(games[3].package_type, PackageType::SetupArchive);
        assert!(games[..4].iter().all(|game| game.plugin_basic_install));
        assert_eq!(games[4].package_type, PackageType::Plugin);
        assert_eq!(games[5].package_type, PackageType::Plugin);
        assert!(!games[4].plugin_basic_install && !games[5].plugin_basic_install);
    }

    #[test]
    fn installs_api4_bundle_and_reads_native_catalog() {
        if Command::new("cc").arg("--version").output().is_err()
            || Command::new("bsdtar").arg("--version").output().is_err()
        {
            return;
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("azdl-plugin-{}-{nonce}", std::process::id()));
        let bundle = root.join("bundle");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(
            bundle.join("plugin.json"),
            r#"{"id":"stub","name":"Stub","version":"1.0","apiVersion":4,"library":"stub"}"#,
        )
        .unwrap();
        fs::write(
            root.join("stub.c"),
            r#"
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
int arachnel_plugin_api_version(void) { return 4; }
void *arachnel_plugin_create(const char *root) { return root ? (void *)1 : NULL; }
void arachnel_plugin_destroy(void *plugin) { (void)plugin; }
int arachnel_plugin_catalog_json(void *plugin, char **out, size_t *len) {
  static const char json[] = "{\"schema\":\"arachnel.plugin.catalog.v1\",\"entries\":[{\"id\":\"demo\",\"title\":\"Demo\",\"kind\":\"game\",\"addons\":[{\"id\":\"fix\",\"title\":\"Fix\",\"kind\":\"addon\",\"downloadUrl\":\"https://example.org/fix.zip\"}]}]}";
  if (!plugin) return 1;
  *len = sizeof(json) - 1;
  *out = malloc(*len);
  memcpy(*out, json, *len);
  return 0;
}
void arachnel_plugin_catalog_json_free(char *ptr) { free(ptr); }
"#,
        )
        .unwrap();
        assert!(
            Command::new("cc")
                .args(["-shared", "-fPIC", "-o"])
                .arg(bundle.join("libstub.so"))
                .arg(root.join("stub.c"))
                .status()
                .unwrap()
                .success()
        );
        let archive = root.join("stub.arach");
        assert!(
            Command::new("bsdtar")
                .args(["--format", "zip", "-cf"])
                .arg(&archive)
                .arg("-C")
                .arg(&root)
                .arg("bundle")
                .status()
                .unwrap()
                .success()
        );
        let data = root.join("data");
        let manifest = install_bundle(&data, &archive).unwrap();
        assert_eq!(manifest.id, "stub");
        let games = catalog_for(&data, "stub").unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].qualified_id(), "stub:demo");
        assert_eq!(games[0].package_type, PackageType::Plugin);
        assert_eq!(games[0].addons.len(), 1);
        assert_eq!(games[0].addons[0].kind, crate::catalog::AddonKind::Addon);
        assert!(install_game(&mut Library::default(), &data, &games[0]).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod steam_selection_tests {
    use super::*;

    #[test]
    fn depot_library_resolves_only_the_declared_provider_file() {
        let root = std::env::temp_dir().join(format!(
            "azdl-depot-provider-select-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let other = root.join("plugins/other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("engine"), b"other plugin").unwrap();
        fs::write(
            other.join("plugin.json"),
            br#"{"id":"other","name":"Other","version":"1","apiVersion":4,"library":"engine"}"#,
        )
        .unwrap();
        assert!(installed_library_file(&root, "steamidra").is_err());

        let selected = root.join("plugins/steamidra");
        fs::create_dir_all(&selected).unwrap();
        fs::write(selected.join("engine"), b"selected plugin").unwrap();
        fs::write(selected.join("unlisted"), b"not declared").unwrap();
        fs::write(
            selected.join("plugin.json"),
            br#"{"id":"steamidra","name":"Steamidra","version":"1","apiVersion":4,"library":"engine"}"#,
        )
        .unwrap();
        assert_eq!(
            installed_library_file(&root, "steamidra").unwrap(),
            selected.join("engine")
        );

        fs::write(
            selected.join("plugin.json"),
            br#"{"id":"other","name":"Other","version":"1","apiVersion":4,"library":"engine"}"#,
        )
        .unwrap();
        assert!(installed_library_file(&root, "steamidra").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_plugin_is_resolved_only_for_the_requested_app() {
        let root = std::env::temp_dir().join(format!(
            "azdl-steam-plugin-select-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let provider = root.join("plugins/steamidra");
        fs::create_dir_all(&provider).unwrap();
        fs::write(
            provider.join("plugin.json"),
            br#"{"id":"steamidra","name":"SteaMidra","version":"1","apiVersion":4,"library":"plugin"}"#,
        )
        .unwrap();

        let game = game_for_steam_app(&root, "steamidra", 620, "Portal 2").unwrap();
        assert_eq!(game.source_id, "steamidra");
        assert_eq!(game.steam_app_id, Some(620));
        assert_eq!(game.title, "Portal 2");
        assert_eq!(game.package_type, PackageType::Plugin);
        assert!(game_for_steam_app(&root, "missing", 620, "Portal 2").is_err());
        assert!(game_for_steam_app(&root, "steamidra", 0, "Portal 2").is_err());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plugin_capabilities_and_manifest_url_derivation() {
        let root = std::env::temp_dir().join(format!(
            "azdl-plugin-caps-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let provider = root.join("plugins/custom_provider");
        fs::create_dir_all(&provider).unwrap();
        fs::write(
            provider.join("plugin.json"),
            br#"{"id":"custom_provider","name":"Custom","version":"1","apiVersion":4,"library":"plugin","catalogUrl":"https://mirror.example.org/v1/catalog","capabilities":["steam_depot_download"]}"#,
        )
        .unwrap();

        assert!(plugin_has_capability(&root, "custom_provider", "steam_depot_download"));
        assert!(!plugin_has_capability(&root, "custom_provider", "unknown_cap"));
        // steamidra has built-in defaults
        let steamidra_dir = root.join("plugins/steamidra");
        fs::create_dir_all(&steamidra_dir).unwrap();
        fs::write(
            steamidra_dir.join("plugin.json"),
            br#"{"id":"steamidra","name":"Steam","version":"1","apiVersion":4,"library":"plugin"}"#,
        )
        .unwrap();
        assert!(plugin_has_capability(&root, "steamidra", "steam_depot_download"));
        assert!(plugin_has_capability(&root, "steamidra", "manifest_provider"));

        // URL derivation from catalogUrl
        let url = manifest_download_url_for_plugin(&root, "custom_provider", 12345);
        assert_eq!(url, "https://mirror.example.org/v1/download/12345");

        // Fallback when missing or no catalogUrl
        let fallback = manifest_download_url_for_plugin(&root, "nonexistent", 12345);
        assert_eq!(fallback, "https://ryuu.badkiko.ru/v1/download/12345");

        fs::remove_dir_all(root).unwrap();
    }
}
