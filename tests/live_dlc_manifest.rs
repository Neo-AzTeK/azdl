//! Optional acceptance check against an externally supplied Steam depot snapshot.

use azdl::steam_manifest::{read_manifest, verify_staged_exact};
use std::{env, path::PathBuf};

#[test]
#[ignore = "requires an exact provider manifest and isolated staged depot files"]
fn live_staged_dlc_matches_selected_manifest() -> azdl::Result<()> {
    let manifest_path = PathBuf::from(env::var("AZDL_LIVE_DLC_MANIFEST")?);
    let staged_path = PathBuf::from(env::var("AZDL_LIVE_DLC_STAGING")?);
    let depot_id = env::var("AZDL_LIVE_DLC_DEPOT_ID")?.parse()?;
    let manifest_id = env::var("AZDL_LIVE_DLC_MANIFEST_ID")?.parse()?;
    let manifest = read_manifest(&manifest_path, depot_id, manifest_id)?;
    verify_staged_exact(&[manifest], &staged_path)
}
