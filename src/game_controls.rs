//! Observed game-management state, independent of the desktop frontend.
use crate::{Game, Result};
use serde::Serialize;
use std::path::Path;

/// Fresh on-disk state for reversible controls and runtime provisioning.
#[derive(Debug, Serialize)]
pub struct GameControls {
    pub overlay: crate::overlay::OverlayStatus,
    pub steamstub: crate::steamless::SteamStubStatus,
    pub patch: Option<&'static str>,
    pub ea_heal_applied: bool,
    pub ea_heal_available: bool,
    pub dlc_patch_applied: bool,
    pub dlc_patch_available: bool,
    pub goldberg_has_settings: bool,
    pub goldberg_compatible: bool,
    pub runtime: crate::dependencies::GameDependencies,
}

/// Read actual state after a command; do not infer success from its requested action.
pub fn inspect(game: &Game, data_dir: &Path) -> Result<GameControls> {
    Ok(GameControls {
        overlay: crate::overlay::overlay_status(game)?,
        steamstub: crate::steamless::scan_game_steamstub(game),
        patch: crate::patch::active_patch(data_dir, &game.id)?
            .map(|patch| if patch.enabled { "enabled" } else { "disabled" }),
        ea_heal_applied: crate::fixes::has_ea_backup(&game.install_dir),
        ea_heal_available: crate::fixes::has_ea_target(&game.install_dir),
        dlc_patch_applied: crate::fixes::has_dlc_installed_patch(&game.install_dir),
        dlc_patch_available: crate::fixes::has_dlc_installed_target(&game.install_dir),
        goldberg_has_settings: crate::goldberg::has_goldberg_settings(&game.install_dir),
        goldberg_compatible: crate::goldberg::is_goldberg_compatible(&game.install_dir),
        runtime: crate::dependencies::game_dependencies(game, data_dir),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn snapshots_distinguish_absent_disabled_and_enabled_overlays() {
        let root = std::env::temp_dir().join(format!(
            "azdl-control-state-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = root.join("game");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("game.exe"), b"fixture").unwrap();
        let mut library = crate::Library::default();
        crate::add_game(
            &mut library,
            "fixture",
            "Fixture",
            &install,
            Path::new("game.exe"),
        )
        .unwrap();
        let game = library.games.get_mut("fixture").unwrap();
        game.runner = crate::Runner::Native;
        assert!(!inspect(game, &root).unwrap().overlay.can_toggle);
        fs::write(install.join("OnlineFix64.dll.arachnel-off"), b"fixture DLL").unwrap();
        let disabled = inspect(game, &root).unwrap();
        assert!(disabled.overlay.can_toggle);
        assert!(!disabled.overlay.enabled);
        crate::overlay::set_overlay_enabled(game, true).unwrap();
        assert!(inspect(game, &root).unwrap().overlay.enabled);
        crate::overlay::set_overlay_enabled(game, false).unwrap();
        assert!(!inspect(game, &root).unwrap().overlay.enabled);
        fs::remove_dir_all(root).unwrap();
    }
}
