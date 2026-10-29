# azdl

azdl is a Rust launcher for Steam games. It finds games through the Steam Store, downloads them from Steam's CDN, installs them into configured storage drives, adds and removes Steam DLC, and launches them natively or through Wine/Proton with per-game prefixes. It never starts the official Steam client.

azdl only installs Steam games from Steam. It has no archive, torrent, setup-installer or arbitrary-folder installs, and it does not modify game files with DRM, entitlement, emulator or multiplayer patches.

## Build

```sh
cargo build --release
```

Verified with Rust 1.96. Runtime tools: `curl` (runtime and redistributable downloads) and `bsdtar` (manifest bundles and Proton-GE archives). Wine or Proton must be installed separately to run Windows games on Linux.

## Desktop application

`src-tauri` is a Tauri v2 + Svelte 5 desktop app using Material Design 3 (`m3-svelte`) and Google's Material Symbols. It calls the `azdl` crate in-process.

- **Library:** installed games, their state and play controls. Installs whose executable could not be detected appear as pending, with an executable chooser.
- **Discover:** Steam Store search and featured games; install to any configured drive.
- **Game page:** Overview (media, description), DLC (install, remove, recover) and Launch & Runner (runner, executable, launch options, arguments, environment, runtime dependencies, diagnostics). Secondary actions live in the play bar's More menu.
- **Transfers:** a bottom drawer with progress, pause, resume and cancel.
- **Settings:** storage drives and game moves, Proton-GE download (Linux), Material You theming, diagnostics.

```sh
cargo build --release      # builds the frontend, CLI and desktop app
./run-desktop.sh           # or ./target/release/azdl-desktop
```

Frontend failures stop the Cargo build. Restart the desktop app after rebuilding. For live UI development run `cd frontend && npm run dev`, then `cargo run -p azdl-desktop` in another terminal.

## Command line

```sh
azdl steam featured
azdl steam search "Portal"
azdl steam install 620
azdl steam scan                 # games installed by the official Steam client
azdl steam import 620           # register one of them
azdl list
azdl launch steam-620 --windowed
```

Downloads create resumable records in `AZDL_DATA_DIR/jobs`:

```sh
azdl jobs list
azdl jobs show JOB_ID
azdl jobs pause JOB_ID
azdl jobs resume JOB_ID         # background worker
azdl jobs retry JOB_ID          # foreground
azdl jobs cancel JOB_ID
```

If the launch executable cannot be chosen automatically after an install, finish it with `azdl pending add <id> <title> <install-dir> <executable>` or the desktop executable chooser. The game keeps its Steam origin.

Per-game configuration:

```sh
azdl runner steam-620 proton:auto          # auto | native | wine | proton:auto | proton:/path/to/proton
azdl executable steam-620 bin/game.exe
azdl args steam-620 --fullscreen
azdl env steam-620 DXVK_HUD 1
azdl env-remove steam-620 DXVK_HUD
azdl option add steam-620 vr "VR" bin/game_vr.exe - --vr
azdl option select steam-620 vr            # or: default
azdl option list steam-620
azdl option remove steam-620 vr
azdl dependencies steam-620 status         # install-vcredist | install-directx
azdl installscript scan steam-620          # apply
azdl doctor steam-620
azdl runners
azdl shortcut create steam-620             # desktop shortcut (Linux)
azdl steam shortcut create steam-620       # non-Steam shortcut in the Steam client
azdl remove steam-620
```

On Linux, `auto` runs an `.exe` with an installed Proton when a Steam client directory is available, then falls back to Wine; other files run natively. `azdl runners` lists Proton builds from Steam libraries, `compatibilitytools.d` and `AZDL_DATA_DIR/runners`. Set `AZDL_STEAM_ROOT` or `STEAM_COMPAT_CLIENT_INSTALL_PATH` for a nonstandard Steam location. Each game gets its own Wine prefix or Proton compatibility data. Desktop **Uninstall** deletes game files and local state; CLI `azdl remove` only removes the library entry.

Launch options add alternative executables inside the install folder. Game arguments come first, then option arguments, then one-time launch arguments. Desktop shortcuts run `azdl launch` so runner and environment settings apply; Steam shortcuts launch the executable directly under Steam's own compatibility settings.

The library lives in `~/.local/share/azdl/library.json` on Linux or `%APPDATA%/azdl/library.json` on Windows; `XDG_DATA_HOME` and `AZDL_DATA_DIR` change the location.

## Steam content

Rust modules own discovery (`steam_discovery`, `steam_metadata`), content-source access (`steam_access`), install requests and resumption (`steam_install`), depot planning (`steam_depot`), manifest and file verification (`steam_manifest`), receipts and revision compatibility (`steam_content`), CDN transfers (`steam_engine`) and DLC transactions (`dlc`).

Manifest bundles come from the relay at `https://ryuu.badkiko.ru/v1/download/<appid>`; depot content comes from Steam's CDN through the pure-Rust `steamroom` crates. An anonymous Steam session lists CDN servers and supplies CDN auth tokens on demand; it carries no account and grants no entitlement. Only access content you are authorized to use: the relay is not a Valve login API, azdl does not log into Steam, and Store metadata, manifests, depot keys and local receipts are never treated as proof of ownership. Source 401/403 responses, missing manifests, network failures and CDN failures produce distinct errors.

Fresh installs use the source's pinned manifest revisions and record them in content receipts. DLC must match the installed base revision and exact DLC/shared-depot manifests; azdl never silently picks another manifest. Path, symlink, hash and file-set checks run before atomic installation. Interrupted or paused base downloads keep their manifest bundle, revision journal and verified files, and resume into the same target. DLC transfers can be cancelled and restarted with chunk reuse. File commit cannot be paused or cancelled. See [the DLC delivery contract](docs/DLC.md).

## Migration

On first access, schema migration version 1 removes obsolete launcher settings and normalizes Steam source identities while keeping installed IDs, directories, artwork, runners, arguments, environment, prefixes, receipts and rollback journals. Obsolete launcher resources and catalog data are archived under `AZDL_DATA_DIR/migration-backup-v1`; the version marker is published last. The migration is locked across processes and can be repeated after interruption.

## Releases

**Build and Release** runs only manually or on `v*` tags, without restored build caches. Linux and Windows run the test suites before packaging installers and portable archives; the release job generates `SHA256SUMS.txt`. `cargo test -p azdl --lib live_anonymous_session_lists_cdn_servers -- --ignored` checks the anonymous Steam session and CDN directory without downloading content.

Offline Rust and frontend tests do not prove that a game installs, runs, or reaches online services; verify those with real content and a real runtime session. The read-only C++/Qt implementation in `../arachnel` remains an architectural reference.
