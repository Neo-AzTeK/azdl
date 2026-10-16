# DLC delivery contract

## Supported today

The launcher installs source supplied HTTPS DLC archives and has an experimental Steam depot DLC path for managed `steamidra` base installs. Archive downloads check any supplied SHA-256 digest. Both paths stage files, reject archive path traversal and symlinks, and add files without overwriting the base game. A per-DLC manifest records owned files and hashes for safe removal. A failed or interrupted copy can be recovered from the game page; recovery refuses to remove files changed after installation began. DLC records follow the game when it moves to another drive.

Steam Store DLC metadata is descriptive and is not a download source. Catalog DLC is enabled only when its source supplies a valid HTTPS archive artifact and does not explicitly mark the entry unavailable. For Steam depot DLC, the installer resolves exact matching provider manifests for the selected DLC. An old install can check whether every DLC file already matches the manifest; azdl records that observation without claiming ownership or offering removal. If DLC files are missing, azdl verifies every required base depot file before recording a legacy base revision and proceeding with the download. A changed or incomplete base requires reinstalling the base game. Filesystem conflicts are reported rather than overwritten.

The researched evolution plan is in [DLC_PLAN.md](DLC_PLAN.md). The Steam depot path reuses depotkit and the archive DLC transaction. An isolated Linux download, additive commit, and removal succeeded for one provider DLC. Installation into a real base game, game access, Windows behavior, overlap handling, and old-install migration still need acceptance tests. The generic provider session ABI below is a separate extension.

## Provider-owned delivery

Arachnel API 4 exposes a JSON catalog boundary, but its C ABI does not expose the C++ `startOwnedDownload` method. Rust must not call C++ virtual methods or infer a depot URL from a Store DLC ID. A future provider-owned DLC adapter should use a versioned C ABI with an opaque session handle:

1. The host passes the source game ID, DLC ID, platform, and an isolated staging directory. The plugin may download into that directory and report progress and cancellation through the adapter. It receives no writable base game path.
2. The plugin returns a manifest of staged files with relative paths and hashes. The host checks IDs, file types, symlinks, traversal, collisions with base files, and hashes before committing any file.
3. The host commits through the same DLC file manifest and lock used for explicit archives. Cancellation, failure, and process death leave a recoverable state. Removal deletes only unchanged files owned by that DLC.
4. The adapter advertises its ABI version and `owned_dlc` capability. Plugins without it remain compatible and their provider-owned DLC stays unavailable in the UI.

The implementation gate is a real API 4 provider package on both Linux and Windows, with a live install, interruption, removal, and game move test. Until that gate passes, the UI must never present provider-owned DLC as downloadable.
