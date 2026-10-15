# DLC delivery contract

## Supported today

The launcher installs a DLC only when the installed game's source plugin supplies an explicit HTTPS archive or multipart archive with `kind: "dlc"`. The host downloads and checks any supplied SHA-256 digest, extracts with archive path and symlink checks, and adds files without overwriting the base game. A per-DLC manifest records owned files and hashes for safe removal. A failed or interrupted copy can be recovered from the game page; recovery refuses to remove files changed after installation began. DLC records follow the game when it moves to another drive.

Steam Store DLC metadata is descriptive. It is not a download source. A catalog entry with a DLC ID but no artifact is shown as unavailable.

## Provider-owned delivery

Arachnel API 4 exposes a JSON catalog boundary, but its C ABI does not expose the C++ `startOwnedDownload` method. Rust must not call C++ virtual methods or infer a depot URL from a Store DLC ID. A future provider-owned DLC adapter should use a versioned C ABI with an opaque session handle:

1. The host passes the source game ID, DLC ID, platform, and an isolated staging directory. The plugin may download into that directory and report progress and cancellation through the adapter. It receives no writable base game path.
2. The plugin returns a manifest of staged files with relative paths and hashes. The host checks IDs, file types, symlinks, traversal, collisions with base files, and hashes before committing any file.
3. The host commits through the same DLC file manifest and lock used for explicit archives. Cancellation, failure, and process death leave a recoverable state. Removal deletes only unchanged files owned by that DLC.
4. The adapter advertises its ABI version and `owned_dlc` capability. Plugins without it remain compatible and their provider-owned DLC stays unavailable in the UI.

The implementation gate is a real API 4 provider package on both Linux and Windows, with a live install, interruption, removal, and game move test. Until that gate passes, the UI must never present provider-owned DLC as downloadable.
