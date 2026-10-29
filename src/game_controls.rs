//! Observed game-management state, independent of the desktop frontend.
use crate::{Game, Result};
use serde::Serialize;
use std::path::Path;

/// Fresh on-disk state for runtime provisioning controls.
#[derive(Debug, Serialize)]
pub struct GameControls {
    pub runtime: crate::dependencies::GameDependencies,
}

/// Read actual state after a command; do not infer success from its requested action.
pub fn inspect(game: &Game, data_dir: &Path) -> Result<GameControls> {
    Ok(GameControls {
        runtime: crate::dependencies::game_dependencies(game, data_dir),
    })
}
