//! Optional end-to-end DLC filesystem acceptance against an isolated base copy.

use azdl::{dlc, load_library, steam_depot};
use std::{collections::BTreeMap, env, fs, path::Path};

fn regular_files(root: &Path) -> azdl::Result<BTreeMap<String, u64>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Live DLC base fixture must contain only regular files".into());
        }
        files.insert(
            entry.file_name().to_string_lossy().into_owned(),
            metadata.len(),
        );
    }
    Ok(files)
}

#[test]
#[ignore = "requires a copied compatible base and authorized live content access"]
fn live_dlc_install_and_remove_preserve_base() -> azdl::Result<()> {
    let library_dir = env::var("AZDL_LIVE_DLC_LIBRARY_DIR")?;
    let data_dir = fs::canonicalize(env::var("AZDL_LIVE_DLC_DATA_DIR")?)?;
    let source_game_id = env::var("AZDL_LIVE_DLC_GAME_ID")?;
    let dlc_id: u32 = env::var("AZDL_LIVE_DLC_APP_ID")?.parse()?;
    let library = load_library(Path::new(&library_dir))?;
    let mut game = library
        .games
        .get(&source_game_id)
        .ok_or("Source game is absent from the supplied library")?
        .clone();
    game.id = "live-dlc-acceptance".into();
    game.install_dir = data_dir.join("games").join(&game.id);
    if !game.install_dir.is_dir()
        || fs::symlink_metadata(&game.install_dir)?
            .file_type()
            .is_symlink()
    {
        return Err("Isolated base copy is missing or is a symbolic link".into());
    }
    let before = regular_files(&game.install_dir)?;
    if before.is_empty() {
        return Err("Isolated base copy contains no files".into());
    }
    let count = steam_depot::install_steam_dlc(&game, &data_dir, dlc_id)?;
    if count == 0 || !dlc::list_installed(&game, &data_dir)?.contains(&dlc_id.to_string()) {
        return Err("Live DLC was not installed as managed content".into());
    }
    dlc::remove(&game, &data_dir, &dlc_id.to_string())?;
    if !dlc::list_installed(&game, &data_dir)?.is_empty() {
        return Err("Managed DLC state remains after removal".into());
    }
    if regular_files(&game.install_dir)? != before {
        return Err("Base files changed after DLC install and removal".into());
    }
    Ok(())
}
