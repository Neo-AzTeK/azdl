//! Steam installation requests; Store metadata never grants content access.
use crate::Result;
use crate::catalog::{CatalogGame, PackageType};

/// Construct a direct Steam install request without querying content services.
pub fn game_for_app(app_id: u32, title: &str) -> Result<CatalogGame> {
    if app_id == 0 || title.trim().is_empty() {
        return Err("Steam App ID and game title are required".into());
    }
    Ok(CatalogGame {
        source_id: "steam".into(),
        id: format!("steam-{app_id}"),
        title: title.trim().into(),
        description: String::new(),
        uri: None,
        filename: None,
        archive_parts: Vec::new(),
        sha256: None,
        archive_part_hashes: Vec::new(),
        package_type: PackageType::Steam(app_id),
        upload_date: None,
        version: None,
        addons: Vec::new(),
        cover_url: None,
        banner_url: None,
        file_size: None,
        genres: Vec::new(),
        steam_app_id: Some(app_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_request_requires_no_installed_resources() {
        let game = game_for_app(480, "Spacewar").unwrap();
        assert_eq!(game.source_id, "steam");
        assert_eq!(game.id, "steam-480");
        assert!(game.addons.is_empty());
        assert!(game_for_app(0, "Spacewar").is_err());
        assert!(game_for_app(480, " ").is_err());
    }
}
