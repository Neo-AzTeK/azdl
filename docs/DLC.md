# DLC delivery contract

## Supported today

The launcher installs source supplied HTTPS DLC archives and has an experimental Steam depot DLC path for managed `steamidra` base installs. Archive downloads check any supplied SHA-256 digest. Both paths stage files, reject archive path traversal and symlinks, and add files without overwriting the base game. A per-DLC manifest records owned files and hashes for safe removal. A failed or interrupted copy can be recovered from the game page; recovery refuses to remove files changed after installation began. DLC records follow the game when it moves to another drive. A depot retry discards the previous partial payload and cancel marker while retaining the transaction journal; unfinished downloads are restarted rather than resumed.

Steam Store DLC metadata is descriptive and is not a download source. Catalog DLC is enabled only when its source supplies a valid HTTPS archive artifact and does not explicitly mark the entry unavailable. For Steam depot DLC, the installer resolves exact matching provider manifests for the selected DLC. An old install can check whether every DLC file already matches the manifest; azdl records that observation without claiming ownership or offering removal. If DLC files are missing, azdl verifies every required base depot file before recording a legacy base revision and proceeding with the download. A changed or incomplete base requires reinstalling the base game. Identical files shared by the base game or another DLC are reused with ownership tracking; different-content conflicts are reported rather than overwritten.

The researched evolution plan is in [DLC_PLAN.md](DLC_PLAN.md). The Steam depot path reuses depotkit and the archive DLC transaction. A Linux check copied a real base game without its DLC, then verified the old base, downloaded and committed the DLC, checked the installed hash, and removed the DLC without changing base files. In-game access, Windows behavior, and different-content overlays still need acceptance and implementation work. The generic provider session ABI below is a separate extension.

The desktop serializes game launch with DLC task registration and file mutations. It refuses to launch while a DLC task is active and refuses install, removal, or recovery while that game is running. This covers games launched and tracked by the desktop; processes started outside azdl are not yet identified.

## Provider-owned delivery

Arachnel API 4 exposes a JSON catalog boundary, but its C ABI does not expose the C++ `startOwnedDownload` method. Rust must not call C++ virtual methods or infer a depot URL from a Store DLC ID. A future provider-owned DLC adapter should use a versioned C ABI with an opaque session handle:

1. The host passes the source game ID, DLC ID, platform, and an isolated staging directory. The plugin may download into that directory and report progress and cancellation through the adapter. It receives no writable base game path.
2. The plugin returns a manifest of staged files with relative paths and hashes. The host checks IDs, file types, symlinks, traversal, collisions with base files, and hashes before committing any file.
3. The host commits through the same DLC file manifest and lock used for explicit archives. Cancellation, failure, and process death leave a recoverable state. Removal deletes only unchanged files owned by that DLC.
4. The adapter advertises its ABI version and `owned_dlc` capability. Plugins without it remain compatible and their provider-owned DLC stays unavailable in the UI.

The implementation gate is a real API 4 provider package on both Linux and Windows, with a live install, interruption, removal, and game move test. Until that gate passes, the UI must never present provider-owned DLC as downloadable.
