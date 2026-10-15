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
- **Translucency & Cross-Platform:** Mica / Acrylic support on Windows, compositor translucency on Linux.

### Run & Build

`cargo build --release` in the workspace automatically handles everything: it invokes the Svelte frontend build via `build.rs`, embeds the assets, and compiles the optimized native binary.

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

The browser also lists unfinished portable and Wine installs. It scans their executables, lets you choose a file, and registers the game without repeating the download. The matching CLI command is `azdl pending add <id> <title> <portable|setup> <install-dir> <executable>`. Plugin installs retain their source when finished this way.

Import an installed Steam game and launch it through Steam:

```sh
azdl steam scan
azdl steam import 123456
azdl launch steam-123456
```

The **Catalog** view discovers games from the Steam Store API. Search by title or App ID; the initial page shows Steam's featured games. Discovery does not require a Hydra or Ryuu catalog feed and does not assert that a manifest is available. An installed download plugin is chosen on the game page when installation starts.

The CLI exposes the same Steam discovery and plugin choice:

```sh
azdl steam featured
azdl steam search "Portal"
azdl steam install 620 steamidra
```

`steam install` resolves the selected App ID, asks the named installed plugin for that game, and uses azdl's isolated install pipeline. The `steamidra` provider downloads a Ryuu manifest ZIP when installation starts, then uses its installed depotkit engine for game files. For its bundled-fix installs, azdl applies the plugin’s local fix kit when `autoEmbedOnlineFix` is enabled. Existing managed installs receive the kit on their next launch if no fix is present. The files are backed up as a reversible patch; disabling that patch keeps the fix disabled. A missing manifest is reported as unavailable; a network or provider failure is reported separately. Other API 4 plugins are checked for an entry with the selected Steam App ID when installation starts. azdl can install their explicit standard packages, but it cannot call plugin-owned C++ download methods through the current Rust ABI. A plugin that has no compatible entry reports that it cannot download the selected game.

The Plugin Store installs `.arach` bundles and verifies their package digests. The desktop game page lists installed plugins as download choices. A favorite plugin can be set in Settings; it is used automatically for installs, while multiple installed plugins without a favorite require an explicit choice. Search results are Steam games whether or not any installed plugin can download them. Steam Store search and featured routes are used without a bundled Web API key; these Store endpoints are less stable than Valve's documented keyed `IStoreService/GetAppList`. Network acceptance could not be verified in an offline build environment.

For installed games, the game page shows DLC entries explicitly supplied by the source plugin. Archive DLC is installed additively and recorded with file hashes for safe removal. Interrupted DLC installs can be recovered, and installed DLC records follow a game moved to another drive. Steam Store DLC listings alone do not provide downloadable content, and plugin-owned DLC installers are not yet supported; see [the DLC delivery contract](docs/DLC.md). Installing to a selected storage drive is supported for archive, executable, and Steam depot packages; setup and magnet packages currently require the built-in Default Library directory.

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

`overlay status` finds existing local fix files and shows whether they are enabled. `overlay disable` and `overlay enable` rename known DLLs and nearby `steam_appid.txt` files with Arachnel's `.arachnel-off` suffix. They check for name conflicts before moving files and refuse games with a staged azdl patch; use `patch enable` or `patch disable` for those. Close the game before switching the overlay. The browser game settings offer the same controls. `fix-status` checks for `OnlineFix.ini` and Steam Fix markers beside the game executable, in its parent directories, and in recognized nested fix directories. Related EpicFix and SteamOverlay DLL bundles are also recognized for Wine and Proton DLL overrides. For Wine or Proton launches with an enabled Steam Fix marker, azdl asks Steam to start if it is not running. `OnlineFix.ini` alone does not trigger that Steam start. These checks only prepare the client side of a launch; they do not establish that a particular patch, game service, or multiplayer session works.

When importing an existing Arachnel install, azdl reads `.arachnel-steamidra` if present. A valid local launch path can resolve an otherwise ambiguous executable scan, and valid launch options appear in game settings. Paths outside the install are ignored. For Wine or Proton games with a detected Online Fix or Steam Fix, azdl adds overrides for local fix DLLs and reads a numeric `FakeAppId` from the fix INI for the game process. Explicit per-game environment values remain in control. Before Wine or Proton launch, azdl also creates missing `steam_appid.txt` files beside the selected executable, its working folder, and detected fix files. It selects `SteamFix32.dll` or `SteamFix64.dll` in a simple `winmm.txt` according to the executable architecture. Files it creates are included in the local overlay toggle. If an existing App ID file disagrees or `winmm.txt` has custom content, azdl keeps it and prints a launch warning. This prepares launch settings only; actual online play still depends on the game, patch, and service.

The library is stored in `~/.local/share/azdl/library.json` on Linux or `%APPDATA%/azdl/library.json` on Windows. `XDG_DATA_HOME` and `AZDL_DATA_DIR` can change the location.

## Port status

The current implementation covers Steam Store game discovery, library management, HTTP and magnet downloads, persistent HTTPS transfer jobs, API 4 plugin catalog loading at install time, portable archive and Wine setup installation, executable discovery, Steam integration, reversible local file patches, launch domains, and a native Tauri v2 + Svelte 5 Material Design 3 desktop application. It does not yet provide general plugin-owned install and launch behavior, broad installer detection, dependency provisioning, or social features. azdl passes launch arguments and environment to games, but game specific networking and multiplayer have not been validated. There is no general guarantee that every game can launch or play online.

The source implementation in `../arachnel` is the reference for the broader port.
