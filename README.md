# azdl

azdl is an early Rust game launcher. It currently provides a command line library for installed games, Steam Store discovery, executable discovery, direct HTTPS downloads, portable archive and Wine setup installation, reversible local patches, Steam library import, and native, Wine, Proton, or Steam launch profiles. The broader Arachnel feature set is still being ported.

## Build

```sh
cargo build --release
```

The current build was verified with Rust 1.96. HTTPS downloads call `curl`, magnet downloads call `aria2c`, archive installation and patch archives call `bsdtar`, RAR game and patch archives also call `unrar`, and RAR archives with encrypted file names require `bwrap` on Linux. Wine or Proton must be installed separately if used.

## Native Desktop Application (Tauri v2 + Svelte 5 + Material You)

azdl includes a native desktop application in `src-tauri` built with Tauri v2, Svelte 5, and pure Google Material Design 3 (`m3-svelte`). It imports the `azdl` crate directly for zero-HTTP-overhead, in-process `#[tauri::command]` execution.

### Features
- **Frameless MD3 Titlebar:** Custom drag region, integrated minimize/maximize/close controls, global search input, and active game indicator pill with a quick-stop action.
- **Compact Navigation Rail:** Seamless switching between Library, Catalog, and Settings tabs.
- **Persistent Transfer Drawer:** Dockable bottom sheet displaying active downloads, transfer speed, progress bars, and job controls.
- **Visual Shimmer Effects:** Dynamic CSS shimmer animation on library cards during background tasks (downloading, unpacking, patching, launching).
- **Material You Dynamic Theming:** Custom seed color picker and Dark/Light/AMOLED modes. The selected seed stays consistent when opening and leaving game pages.
- **Direct Engine Integration:** Game launching with process-tree monitoring, prefix scanning, SteamStub unpacker and rollback, Online-Fix / SteamFix overlay toggle, patch manager, Steam shortcut injection, and Proton/Wine runner discovery.
- **Cross-Platform:** Opaque Material You windows on Linux and Windows, without desktop glass effects.

### Run & Build

`cargo build --release` in the workspace automatically handles everything: it invokes the Svelte frontend build via `build.rs`, embeds the assets, and compiles the optimized native binary.

Frontend build failures stop the Cargo build instead of reusing older assets. Close
and relaunch the desktop application after rebuilding; an already running window
continues using the frontend embedded in the executable it started from.

```sh
# Build everything (CLI + Desktop + Frontend)
cargo build --release

# Launch the desktop app
./run-desktop.sh
# or launch the binary directly:
./target/release/azdl-desktop
```

For frontend UI development with live hot reload (HMR):
```sh
cd frontend && npm run dev
# In another terminal:
cargo run -p azdl-desktop
```

## Use

Import a game already installed on disk:

```sh
azdl scan "My Game" /games/my-game
azdl import my-game "My Game" /games/my-game
azdl launch my-game
```

`import` picks a likely game executable. If several candidates are similarly plausible, inspect the `scan` output and specify the executable yourself:

```sh
azdl add my-game "My Game" /games/my-game bin/game
azdl launch my-game
```

To keep a local runnable file in azdl's managed games directory, copy and register it with `install-file`:

```sh
azdl install-file my-game "My Game" /downloads/game.sh
azdl launch my-game
```

Download a directly runnable file from a source you have access to, register it, and launch it:

```sh
azdl fetch sample "Sample Game" https://example.org/game.bin game.bin
azdl launch sample
```

For a portable ZIP, 7z, tar, or RAR archive, extract and register the detected game executable in one step:

```sh
azdl install-archive sample "Sample Game" /downloads/sample.zip
azdl fetch-archive sample "Sample Game" https://example.org/sample.zip sample.zip
azdl launch sample
```

Game and setup archives use the published `online-fix.me` password when needed. Set `AZDL_ARCHIVE_PASSWORD` before running azdl for an archive with a different password. Archives normally pass the member safety check before extraction. On Linux, an encrypted-header RAR that cannot be listed is extracted inside a Bubblewrap read-only filesystem view, then its resulting files are checked before installation. This fallback requires Bubblewrap support on the host.

For a RAR archive split into parts, place all parts in one folder and pass the first part to `install-archive`, or download and install the set in one command:

```sh
azdl fetch-archive-parts sample "Sample Game" game.part01.rar \
  https://example.org/game.part01.rar https://example.org/game.part02.rar
```

The browser **Download and install** form has a **Multi-part RAR archive** option. Enter one HTTPS URL per line in part order. Each part is kept under `AZDL_DATA_DIR/downloads/<game-id>` and has a resumable download record. If a transfer fails, repeat the command or form submission with the same URLs and filename after fixing the cause.

With aria2 installed, a magnet link can download a portable game or a single archive and then register its detected executable:

```sh
azdl fetch-magnet sample "Sample Game" 'magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567'
azdl launch sample
```

Replace the example hash with the source's real 40-character hexadecimal or 32-character base32 BitTorrent info hash. azdl retains partial files in its downloads directory for aria2 resume. A torrent containing a setup installer or several plausible archives needs a manual install choice after download.

The `fetch` command is for a single executable, including a Windows `.exe`. Archive installation requires a portable game with a runnable file. If the executable choice is ambiguous, files remain in azdl's managed games directory so you can use `scan` and `add`. The `download` command saves an additional HTTPS file inside an existing game's directory and resumes an interrupted transfer from a `.part` file.

HTTPS game, archive, setup, patch, and additional-file downloads create persistent records in `AZDL_DATA_DIR/jobs`:

```sh
azdl jobs list
azdl jobs show JOB_ID
azdl jobs retry JOB_ID
azdl jobs fetch https://example.org/game.zip game.zip [sha256]
azdl jobs fetch-install my-game "My Game" archive https://example.org/game.zip game.zip [sha256]
azdl jobs pause JOB_ID
azdl jobs resume JOB_ID
azdl jobs cancel JOB_ID
```

`jobs list` shows status and the downloaded byte count while a transfer runs and after it stops. `jobs fetch` starts an additional-file download into `AZDL_DATA_DIR/background-downloads`. `jobs fetch-install` saves an install plan and automatically installs a downloaded file, portable archive, Windows installer, or Windows installer archive. The browser **Download and install** form uses these install jobs for HTTPS packages. For a source that publishes a SHA-256 digest, add its 64-character hexadecimal value as the optional final argument or fill the browser SHA-256 field. azdl verifies it after transfer, when reusing a completed job, and before a deferred install. A mismatched file is kept with a `.bad-sha256-*` suffix, and automatic installation refuses it. `jobs pause` keeps the `.part` file. `jobs resume` restarts an interrupted transfer or retries installation from a completed download. `jobs cancel` discards a queued or running transfer; cancelled jobs cannot resume. Worker output is saved in `AZDL_DATA_DIR/jobs/JOB_ID.log`. `jobs retry` resumes a transfer or retries a failed install from its completed download in the foreground. If executable discovery is ambiguous, the game files remain installed and the browser offers **Choose executable**. For an additional-file job, choose **Use file** in the browser, then select its install type. The matching CLI commands are `install-archive`, `install-file`, `setup run`, and `setup archive`.

On Linux, run a Windows setup executable in a game-specific Wine prefix, then import its result:

```sh
azdl setup run my-game "My Game" /downloads/setup.exe
azdl launch my-game
```

Or download a setup executable from an HTTPS source you can access and install it in one step:

```sh
azdl setup fetch my-game "My Game" https://example.org/setup.exe setup.exe
azdl launch my-game
```

For a ZIP, 7z, or tar package containing a Windows setup executable, use `azdl setup archive my-game "My Game" /downloads/package.zip`. Use `azdl setup fetch-archive my-game "My Game" https://example.org/package.zip package.zip` to download that package first. The archive is checked before extraction, and a clear `setup.exe`, `install.exe`, or MSI match is run in the Wine prefix. If more than one installer is plausible, the extracted files remain under `AZDL_DATA_DIR/setup-unpack` for manual selection with `azdl setup run`.

`setup run` also accepts `.msi` files and passes extra arguments to the installer. `setup fetch` downloads an HTTPS installer to `AZDL_DATA_DIR/setup-downloads/<game-id>` and runs it. Both search common locations under `AZDL_DATA_DIR/prefixes/<game-id>/drive_c` and register a clear executable match with the Wine runner, which reuses that prefix at launch. If the installer needs interactive choices, make them in its window. If discovery finds several candidates or none, use `azdl setup scan my-game "My Game"`, then `azdl setup add my-game "My Game" <install-dir> <executable>`. `azdl setup import my-game "My Game"` retries automatic discovery after an installer finishes. Installations outside the game prefix need a manual `setup add`.

The browser also lists unfinished portable and Wine installs. It scans their executables, lets you choose a file, and registers the game without repeating the download. The matching CLI command is `azdl pending add <id> <title> <portable|setup> <install-dir> <executable>`. Installations retain their source when finished this way.

Import an installed Steam game and launch it through Steam:

```sh
azdl steam scan
azdl steam import 123456
azdl launch steam-123456
```

The **Discover** view uses Steam Store search and featured metadata. Install directly from a game card or game page, choosing a configured storage drive. Discovery is public metadata: it does not establish ownership or guarantee a downloadable manifest.

```sh
azdl steam featured
azdl steam search "Portal"
azdl steam install 620
```

Rust owns discovery (`steam_discovery`, `steam_metadata`), externally authorized content-source access (`steam_access`), installation requests and resumption (`steam_install`), depot planning/execution (`steam_depot`), exact manifest/file verification (`steam_manifest`), receipts/revision compatibility (`steam_content`), and DLC transactions (`dlc`). CDN transfers run in-process through the internal `steam_engine` adapter over the pure-Rust `steamroom` crates; no native engine library is bundled. There is no launcher extension host, source selector, store or package prerequisite.

Downloads retain the existing manifest relay at `https://ryuu.badkiko.ru/v1/download/<appid>` and download Steam CDN content with `steamroom-client`. An anonymous Steam session lists CDN servers and supplies CDN auth tokens on demand; it carries no account and grants no entitlement. Only access content for which you have authorization. The relay is not a Valve login API; this path assumes authorization has already been supplied externally, as required by this deployment. azdl does not log into Steam or verify account entitlements. OpenID identifies a user but cannot authorize depot downloads, and publisher ownership APIs are not a universal consumer download API. The application never treats Store metadata, a depot key, a returned manifest, or a local receipt as proof of ownership. Source 401/403 responses, missing manifests, connection failures and CDN failures produce distinct actionable errors.

Fresh base installations use the source's explicitly pinned manifest revisions and record them in content receipts. Adding DLC requires compatibility with the installed base revision and exact selected DLC/shared-depot manifests; azdl never silently selects another manifest. Hash, path, symlink and file-set checks run before atomic installation. Base downloads retain their manifest bundle, exact revision journal and verified depot groups on interruption or pause; Resume/Retry uses the original target and ID. Cancellation purges the scoped transfer cache. DLC depot transfers support cancellation/restart with validated chunk reuse; explicit archive DLC also supports pause/resume. File commit cannot be paused or cancelled.

The game page lists Steam DLC metadata independently of content availability. Managed DLC, shared files, rollback and existing content observations survive metadata outages. Legacy explicit archive sources migrate into inert content-source records so paused archive transfers and removal remain usable. See [the DLC delivery contract](docs/DLC.md). Selected drives support archive, executable and Steam depot packages; setup and magnet packages currently require the Default Library directory.

On first access, schema migration version 1 removes obsolete launcher settings and normalizes Steam source identities while preserving installed IDs, directories, artwork, runners, arguments, environment, prefixes, content receipts and rollback journals. Non-package jobs and their partials retain their targets and IDs. Original JSON and obsolete launcher resources are archived under `AZDL_DATA_DIR/migration-backup-v1`; the version marker is published last. The migration is locked across processes and can be repeated after interruption. Invalid optional catalog caches stay archived with a `catalog-warnings.json` record instead of blocking the library. It never traverses game `Game_Data/Plugins` trees. A resource root containing a registered installation remains in place. Existing users do not reinstall their games. Keep the backup until you have verified your library.


```sh
azdl list
azdl download my-game https://example.org/update.dat assets/update.dat
azdl args my-game --fullscreen
azdl env my-game GAME_MODE multiplayer
azdl env-remove my-game GAME_MODE
azdl runners
azdl doctor my-game
azdl executable my-game bin/game.exe
azdl runner my-game wine
azdl runner my-game proton:auto
azdl runner my-game proton:/absolute/path/to/proton
azdl option add my-game multiplayer "Multiplayer" bin/GameMP.exe bin --online
azdl option list my-game
azdl option select my-game multiplayer
azdl option select my-game default
azdl option remove my-game multiplayer
azdl launch my-game --windowed
azdl patch apply my-game /path/to/extracted-patch
azdl patch archive my-game /path/to/patch.zip
azdl patch archive my-game /path/to/wrapped-patch.zip package/fix-files
azdl patch fetch my-game https://example.org/patch.zip patch.zip
azdl patch fetch-parts my-game fix.part01.rar https://example.org/fix.part01.rar https://example.org/fix.part02.rar
azdl patch status my-game
azdl patch disable my-game
azdl patch enable my-game
azdl patch rollback my-game
azdl overlay status my-game
azdl overlay disable my-game
azdl overlay enable my-game
azdl fix-status my-game
azdl remove my-game
```

On Linux, `auto` runs an `.exe` with an installed Proton when a Steam client directory is available, then falls back to Wine. Other files launch natively. `azdl runners` lists Proton installations found in Steam libraries, Steam's `compatibilitytools.d`, and `AZDL_DATA_DIR/runners`. Use `proton:auto` to require an installed Proton, or `proton:/path/to/proton` to choose one. Set `AZDL_STEAM_ROOT` or `STEAM_COMPAT_CLIENT_INSTALL_PATH` when Steam is in a nonstandard location. A Proton profile uses its own compatibility data directory per game. The launcher does not start the official Steam client. Desktop Uninstall deletes managed game files and local state; CLI `azdl remove` only removes the library entry.

`doctor` reports local launch prerequisites for one game, including its resolved runner, missing executable or Wine/Proton/Steam command, patch state, and detected fix markers. It exits with an error when a required local file or runner is unavailable. A clean result does not verify game startup or multiplayer.

On Linux, create a desktop shortcut for an installed game with `azdl shortcut create my-game`. It runs `azdl launch my-game` with the correct library directory, so the selected runner, launch option, arguments, and fix preparation still apply. Use `azdl shortcut status my-game` or `azdl shortcut remove my-game` to inspect or remove it. Pass an optional directory as the last argument to any of these commands, or use the shortcut buttons in browser game settings. The default is the XDG desktop directory, falling back to `~/Desktop`. Regenerate shortcuts after moving the azdl executable.

To add an installed game as a non-Steam game, use `azdl steam shortcut create my-game` or **Add to Steam** in browser game settings. `azdl steam shortcut status my-game` and `azdl steam shortcut remove my-game` inspect or remove azdl's entry. These commands choose the most recently used Steam user config; pass a `shortcuts.vdf` path after the game ID to select another account. azdl preserves other shortcuts and backs up the previous file before writing. Restart Steam after a change. The shortcut launches the selected game executable directly, with its saved arguments. Steam controls compatibility for that shortcut; azdl's runner, environment, and fix preparation are not invoked. Use the desktop shortcut or azdl's Play button when those settings are needed. Steam grid artwork is not exported yet.

Use `option add` to save another executable inside a game's install folder. The working folder can be a relative folder inside the install or `-` to use the executable's folder. The selected option applies to `launch`; shared game arguments come first, then option arguments, then one-time launch arguments. The browser settings page can add, select, and remove these options. `option select <id> default` restores the base executable. Existing libraries load with no options.

`patch apply` overlays files from an extracted patch directory onto a registered game. `patch archive` first inspects and extracts a local archive. It preserves game subdirectories such as `bin` and strips recognizable packaging wrappers. For an ambiguous archive, pass a relative folder inside it as the optional final argument, or fill **Archive root** in game settings. Password-protected ZIP and RAR patches use the published `online-fix.me` password by default; set `AZDL_PATCH_ARCHIVE_PASSWORD` when a package uses another password. Encrypted-header RAR patches need Bubblewrap on Linux. `patch fetch` downloads one HTTPS archive and applies it. `patch fetch-parts` downloads every RAR volume into the same folder before applying the first part. The browser game settings page offers both choices under **Download patch**. Downloaded archives remain under `AZDL_DATA_DIR/patch-downloads/<game-id>`. Patch application keeps copies of replaced files and staged patch files under `AZDL_DATA_DIR/patches/<game-id>`. `patch disable` restores the original files and removes patch-added files while retaining the staged patch; `patch enable` reapplies it. `patch rollback` restores the original files when the patch is enabled, then removes its staged files. If already disabled, rollback leaves the game's current files alone. Close the game before switching or rolling back a patch. Only one patch can be active per game. Disabling an enabled patch replaces files it changed and removes files it added, so keep later edits to those files elsewhere. Roll back a patch before updating or removing the game. Package specific setup steps, including online-fix.me integration, are not automated.

Existing local overlay and patch journals remain available for rollback and removal. Steam installations do not automatically receive DRM removal, entitlement patches, emulator configuration, activation patches or downloaded fix kits. Their selected native executable, Wine/Proton isolation, arguments and explicit environment remain in control. The launcher never starts the official Steam client. A game that requires an unsupported authentication client remains subject to that requirement.

The library is stored in `~/.local/share/azdl/library.json` on Linux or `%APPDATA%/azdl/library.json` on Windows. `XDG_DATA_HOME` and `AZDL_DATA_DIR` can change the location.

The game page keeps runtime dependency installation under **Launch & Runner**. Patch cards have one sliding switch each. They retain the last observed state and stay disabled until the command and state refresh finish; failed commands do not leave an optimistic switch enabled. The backend checks the resolved Wine or Proton prefix when reporting runtime status.

## Depot downloads and releases

Depot downloads are compiled into both executables through `steamroom` and `steamroom-client` (MIT OR Apache-2.0); there is no separate engine to build, bundle or locate. Provider manifests are validated by `steam_manifest` (paths, symlinks, revision IDs) before any chunk is fetched, and files are written in place so interrupted transfers reuse verified chunks. `cargo test -p azdl --lib live_anonymous_session_lists_cdn_servers -- --ignored` checks the anonymous Steam session and CDN directory without downloading content.

**Build and Release** runs only manually or on `v*` tags. It restores no Rust or npm build caches. Linux and Windows run the test suites before packaging installers and portable archives with licenses; the release job generates `SHA256SUMS.txt`. For artifacts without publishing, run it manually and uncheck **Publish as GitHub Release**. No automatic Verify workflow is installed.

Offline Rust/frontend regressions and native Linux engine loading are verified. Windows source cross-checks are distinct from native Windows execution. Live authorized installation, DLC compatibility in an actual game, service login and multiplayer acceptance must be verified with the relevant account/game/service. See [the migration and acceptance ledger](docs/STEAM_CORE_MIGRATION.md).

The read-only implementation in `../arachnel` remains an architectural reference.
