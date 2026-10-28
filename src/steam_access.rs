//! Content-source access for externally authorized Steam downloads.
//!
//! This relay is not a Valve authentication API. The caller must already have
//! permission to access the game and DLC. Store discovery, a returned manifest,
//! and an installed receipt are never evidence of ownership. No credentials or
//! depot keys are persisted in settings, library state or IPC snapshots.
use crate::archive::extract_checked;
use crate::{Result, clean_command};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Address the existing manifest relay directly, without catalog URL derivation.
pub fn manifest_url(app_id: u32) -> Result<String> {
    if app_id == 0 {
        return Err("Steam App ID must be positive".into());
    }
    Ok(format!("https://ryuu.badkiko.ru/v1/download/{app_id}"))
}

/// Explain content-source failures separately from packaging or Store failures.
pub fn response_error(app_id: u32, status: &str) -> String {
    match status.trim() {
        "401" | "403" => format!(
            "Content authorization denied for Steam App ID {app_id}. Confirm authorized access with the content source; azdl does not provide Steam account authentication."
        ),
        "404" | "410" => format!(
            "No download manifest is available for Steam App ID {app_id} from the content source."
        ),
        _ => format!(
            "Manifest source could not be reached for Steam App ID {app_id} (HTTP {}). Retry after checking your connection.",
            status.trim()
        ),
    }
}

pub(crate) fn validate_manifest_archive(path: &Path, app_id: u32) -> Result<()> {
    let mut file = fs::File::open(path)?;
    let mut signature = [0u8; 4];
    if file.read_exact(&mut signature).is_err()
        || !matches!(
            signature,
            [b'P', b'K', 3, 4] | [b'P', b'K', 5, 6] | [b'P', b'K', 7, 8]
        )
    {
        return Err(format!(
            "No usable download manifest is available for Steam App ID {app_id}: the content source did not return a ZIP manifest"
        )
        .into());
    }
    Ok(())
}

pub fn fetch_manifest_bundle(root: &Path, app_id: u32) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    let archive = root.join(format!("{app_id}.zip"));
    if fs::symlink_metadata(&archive).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("Manifest archive path is a symbolic link".into());
    }
    let url = crate::steam_access::manifest_url(app_id)?;
    if !archive.is_file() || archive.metadata()?.len() == 0 {
        let output = clean_command("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--retry",
                "2",
                "--max-time",
                "30",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--write-out",
                "%{http_code}",
                "--output",
            ])
            .arg(&archive)
            .arg(&url)
            .output()?;
        if !output.status.success() {
            let _ = fs::remove_file(&archive);
            let code = String::from_utf8_lossy(&output.stdout);
            return Err(crate::steam_access::response_error(app_id, &code).into());
        }
    }
    validate_manifest_archive(&archive, app_id)?;
    let unpacked = root.join("manifest");
    if let Ok(meta) = fs::symlink_metadata(&unpacked) {
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("Manifest staging path is not a directory".into());
        }
        fs::remove_dir_all(&unpacked)?;
    }
    fs::create_dir(&unpacked)?;
    extract_checked(&archive, &unpacked)?;
    Ok(unpacked)
}
