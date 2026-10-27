//! Optional acceptance check against an externally supplied Steam depot snapshot.

use azdl::steam_manifest::{read_manifest, read_manifest_with_key, verify_staged_exact};
use std::{env, path::PathBuf};

#[test]
#[ignore = "requires an exact provider manifest and isolated staged depot files"]
fn live_staged_dlc_matches_selected_manifest() -> azdl::Result<()> {
    let manifest_path = PathBuf::from(env::var("AZDL_LIVE_DLC_MANIFEST")?);
    let staged_path = PathBuf::from(env::var("AZDL_LIVE_DLC_STAGING")?);
    let depot_id = env::var("AZDL_LIVE_DLC_DEPOT_ID")?.parse()?;
    let manifest_id = env::var("AZDL_LIVE_DLC_MANIFEST_ID")?.parse()?;
    let manifest = match env::var("AZDL_LIVE_DLC_DEPOT_KEY") {
        Ok(key) => read_manifest_with_key(&manifest_path, depot_id, manifest_id, &key)?,
        Err(env::VarError::NotPresent) => read_manifest(&manifest_path, depot_id, manifest_id)?,
        Err(error) => return Err(error.into()),
    };
    verify_staged_exact(&[manifest], &staged_path)
}
