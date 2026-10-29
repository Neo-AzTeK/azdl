# Steam DLC lifecycle

Steam DLC discovery and content availability are separate. Public Store/appinfo data identifies parent games, DLC and depot relationships; it does not grant ownership or download permission. Content-source authorization is supplied externally. azdl retains the existing manifest relay and transfers Steam CDN content in-process with the pure-Rust `steamroom` crates, without a launcher extension or source selector.

## Exact revisions

Fresh base installations may use the manifest revision explicitly pinned by the content source, even when it differs from today's public revision. The resulting receipt records that revision. This does not relax DLC compatibility: adding DLC validates the installed base receipt against required metadata, resolves exact DLC manifests, and checks the relevant base files immediately before commit. A changed receipt, manifest, depot ID or file fails closed. No fallback to another revision occurs.

`depotfromapp` resolution follows shared-depot ownership, rejects cycles/missing references, and groups downloads by the resolved content App ID. All selected manifests and encrypted filenames are checked before transfer. CRC, filename hashes, file hashes, expected depot/revision IDs, path containment and symlink rejection remain enforced. Staged files must match the selected manifest set exactly.

## File ownership and recovery

DLC installs stage content outside the base directory. Identical existing files can be shared; conflicting files fail before commit. Each transaction records hashes and ownership. Removal deletes only unchanged managed files after their last owner disappears. Steam DLC never replaces base files; journals written by older versions for archive overlays keep their verified backups, and removing them restores the original base bytes. Interrupted transaction recovery refuses to overwrite later user edits.

Game locks coordinate DLC install/remove/recovery, base-file checks, library moves and uninstallation. The desktop blocks these mutations while the game or a conflicting operation is running. External programs modifying files are outside those locks.

Depot transfers support cancellation and restart with revision-validated chunk reuse; they cannot be paused. Cancellation is disabled after file commit begins. Failed transfers remain visible; managed receipts and recovery actions remain usable when metadata services fail.

## Existing installations

Schema migration version 1 preserves game IDs, files, artwork, launch settings, prefixes, base receipts, DLC journals, shared ownership and rollback data. Obsolete catalog caches and archive DLC source lists (`content-sources`) are archived under `migration-backup-v1`; archive DLC can no longer be installed or resumed. Game-owned `Game_Data/Plugins` directories are never interpreted as launcher resources.

## Evidence and acceptance

Offline coverage includes source-pinned base versus current-public DLC selection, shared content-App grouping, absent manifests, corrupt files/manifests, encrypted names, unsafe paths, symlinks, file collisions, shared ownership, rollback, interrupted transfers, cancellation and migration. The `steamroom` engine has passed an anonymous Steam session and CDN directory check plus offline pause/cancel tests. This is not live CDN installation acceptance.

Optional live tests remain opt-in:

- `tests/live_dlc_manifest.rs`: verify an external staged snapshot using `AZDL_LIVE_DLC_MANIFEST`, `AZDL_LIVE_DLC_STAGING`, `AZDL_LIVE_DLC_DEPOT_ID`, `AZDL_LIVE_DLC_MANIFEST_ID` and optional `AZDL_LIVE_DLC_DEPOT_KEY`.
- `tests/live_dlc_install.rs`: an isolated compatible base copy, matching receipt and authorized source/CDN access are required. Set `AZDL_LIVE_DLC_LIBRARY_DIR`, `AZDL_LIVE_DLC_DATA_DIR`, `AZDL_LIVE_DLC_GAME_ID`, `AZDL_LIVE_DLC_APP_ID` and place the copied game under `games/live-dlc-acceptance`.

Run the relevant test with `-- --ignored`. Native Windows execution, real shared-depot transfers, and in-game/service access require separate acceptance. Earlier provider-based acceptance does not prove the new packaged path.
