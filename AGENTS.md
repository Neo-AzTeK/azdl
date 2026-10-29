# AGENTS.md — azdl Developer & Agent Operational Guide

This document provides architectural context, security invariants, module mapping, and development workflows for AI coding agents working on **`azdl`**.

For workspace-wide context and the relationship between `azdl` and `arachnel`, see the root [`../AGENTS.md`](file:///home/mixcraftio/Code/Repos/Crack/AGENTS.md).

---

## 1. Project Overview

`azdl` is a Steam game launcher and runtime orchestrator written in **Rust (edition 2024)**, with a CLI and a Tauri desktop app. It ports the relevant parts of the C++/Qt reference implementation `arachnel`.

**Scope: Steam games from Steam only.** azdl discovers games through the Steam Store, downloads depots from Steam's CDN, installs and removes Steam DLC, imports games installed by the official Steam client, and launches games natively or through isolated Wine/Proton prefixes. It does **not** install from archives, torrents, setup installers, catalogs or arbitrary folders. Do not reintroduce these install sources.

---

## 2. Source Code Architecture & Module Guide

All core source code resides in [`src/`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/):

| Module | Core Responsibilities | Key Functions / Types |
| :--- | :--- | :--- |
| `lib.rs` | Domain types, library persistence (`library.json`), launch options, process launch, game removal. | `Game`, `Library`, `Runner`, `spawn_game`, `remove_game` |
| `main.rs` | CLI entry point and command dispatch. | `run` |
| `archive.rs` | Checked `bsdtar` extraction of manifest bundles; Windows path normalization. | `extract_checked`, `normalize_windows_paths` |
| `discovery.rs` | Executable detection heuristics for installed depots. | `discover_executables`, `import_game` |
| `doctor.rs` | Per-game launch diagnostics and host tool checks. | `diagnose_game`, `system_diagnostics` |
| `jobs.rs` | Steam game and DLC job records, pause/cancel markers, background workers. | `DownloadJob`, `retry_job`, `start_background` |
| `runtime.rs` | Proton/Wine discovery and Proton-GE download. | `discover_proton`, `resolve_runner` |
| `dependencies.rs`, `installscript.rs` | VC++/DirectX provisioning and Steam `installscript.vdf` registry application in prefixes. | `ensure_game_dependencies`, `apply_installscript_for_game` |
| `settings.rs` | Storage drives, game moves, theming. | `LauncherSettings`, `move_game_to_drive` |
| `shortcuts.rs`, `steam_shortcuts.rs` | Desktop shortcuts and non-Steam shortcuts in the Steam client. | `create_shortcut` |
| `steam.rs` | Local Steam library scanning and import. | `scan_steam`, `import_steam`, `steam_root` |
| `steam_discovery.rs`, `steam_metadata.rs` | Store search/featured lists, appinfo and DLC metadata. | `discover_games`, `fetch_appinfo` |
| `steam_access.rs`, `steam_install.rs` | Relay manifest bundles, install requests, pending-executable origins, job resumption. | `SteamGame`, `install_game_in`, `resume_job` |
| `steam_depot.rs`, `steam_content.rs`, `steam_manifest.rs` | Depot planning, exact revisions, receipts, manifest and file verification. | `install_steam_manifest_game_in`, `install_steam_dlc` |
| `steam_engine.rs` (private) | In-process `steamroom` CDN transfers: anonymous session, auth tokens, progress, pause/cancel. | Internal adapter |
| `dlc.rs` | Atomic, reversible DLC file transactions with shared-file ownership. | `install_staged_with_guard`, `remove_with_guard` |
| `migration.rs` | Versioned migration of legacy data. | `migrate` |
| `http.rs` (private) | Shared HTTPS client for small JSON lookups. | `get_bytes` |
| `steamless.rs` | SteamStub DRM detection and unpack via the bundled Steamless CLI, with executable backups. | `scan_game_steamstub`, `unpack_game`, `rollback_game` |
| `goldberg.rs` | Goldberg Steam emulator provisioning and `steam_settings/` synchronization. | `prepare_goldberg_for_game`, `rollback_goldberg_config` |
| `fixes.rs` | EA activation heal, `steam_api(64).dll` provisioning, DLC-entitlement (`BIsDlcInstalled`) and GodotSteam patches. | `heal_ea_activation`, `patch_dlc_installed_ids`, `ensure_steam_api_dll` |
| `multiplayer.rs` | Fix detection and launch-time env/DLL staging (Online-Fix, SteamFix); PE bitness probing. Generic fix payloads auto-adapt to the game: the emulated Steam App ID (`FakeAppId` in `OnlineFix.ini`/`SteamFix.ini`, `steam_appid.txt` and Steam* env) is driven from the game's known `steam_app_id` or a per-game `SteamAppId` override. | `detect_fix`, `prepare_game_launch`, `FixKind` |
| `overlay.rs` | Online-Fix overlay enable/disable via a managed Steam AppID marker. | `overlay_status`, `set_overlay_enabled` |
| `patch.rs` | Local standalone patch framework: apply from an extracted dir or local (optionally encrypted) archive, enable/disable/rollback. No remote fetch. | `apply_patch`, `apply_patch_archive_with_root`, `enable_patch`, `rollback_patch` |
| `game_controls.rs` | Observed per-game control snapshot (overlay, steamstub, patch, EA, DLC, Goldberg, runtime) read back after commands. | `inspect`, `GameControls` |

---

## 3. Critical Security Invariants

When extending or modifying `azdl`, you must strictly maintain the following invariants:

1. **No Arbitrary Shell Execution (`sh -c` / `cmd.exe /C`):**
   - Always spawn child processes using `std::process::Command::new(binary).arg(...)`.
   - Never construct shell string commands. Arguments must be passed as distinct vector elements.
2. **Path Traversal Protection:**
   - Manifest bundles, depot manifests and DLC payloads must only write inside their destination directory; reject `..`, absolute paths and Windows drive prefixes (`rejects_archive_path_escape`, `manifest_entries_reject_links_and_escaping_paths`).
3. **Strict Symlink Rejection:**
   - Depot manifests and staged payloads containing symbolic links are rejected before any file is written.
4. **Exact Content Verification:**
   - Installed files must match the selected manifest revisions (paths, sizes, SHA-1) before atomic publication; never silently substitute another manifest revision.
5. **Reversible DLC:**
   - DLC transactions record backups for exact rollback.

---

## 4. Cross-Platform Runtime Protocols

### Linux Launch Pipeline
- Resolve Native, Wine or Proton and honor the selected executable, arguments and environment.
- Keep Wine prefixes at `AZDL_DATA_DIR/prefixes/<game-id>` and Proton data at `compatdata/<game-id>`.
- Never start the official Steam client; unsupported account/client requirements remain explicit limitations.

### Windows Launch Pipeline
- Run game executable directly or through specified launcher arguments.
- Handle paths with native Windows separators and preserve UTF-8 compatibility.

---

## 5. Coding Standards & Rust Idioms

**Commit often.** Make small, coherent commits after verification, staging only changes made for the current task.

1. **Rust Edition:** Rust 2024.
2. **Minimal Dependencies:** Keep `Cargo.toml` lean. Do not introduce heavy dependencies (e.g., Tokio, Reqwest, or full OpenSSL/Ring) unless explicitly justified and approved by the user. The approved exception is `steamroom`/`steamroom-client` with their Tokio, Reqwest and rustls stack: async code stays confined to `steam_engine`, and other modules reach HTTPS only through the shared blocking client in `http.rs` for small metadata requests (large or resumable file transfers keep their own download paths). Otherwise use existing lightweight primitives (`serde`, `serde_json`, standard library).
3. **Idiomatic Error Handling:** Return `Result<T, Box<dyn std::error::Error>>` or specific module errors with descriptive error messages. Avoid `unwrap()` or `expect()` in production library paths.
4. **Formatting & Clippy:**
   - Code must format cleanly via `cargo fmt`.
   - Must compile with zero warnings under `cargo clippy --all-targets`.
5. **Comments & Documentation:**
   - Document non-obvious algorithms, platform quirks, PE header handling, or byte manipulation clearly using standard doc comments (`///`).

---

## 6. Verification & Porting Maintenance Protocol

Whenever you perform work on `azdl`:

1. **Run Full Test Suite:**
   ```sh
   cargo test
   ```
   Ensure all existing tests (unit and integration) pass. Add unit tests for every new feature or bug fix.
2. **Run Lint Checks:**
   ```sh
   cargo clippy --all-targets
   ```
3. **Update Domain Capabilities & Feature Matrix (Section 8):**
   - If your work implements, expands, or verifies a domain listed in [Section 8](#8-domain-capabilities-status-matrix--next-milestones), update the corresponding row's **Current azdl state** and **Next completion evidence**.
   - **Realistic Reporting:** Never claim that a game has working online multiplayer or full compatibility based only on successful process spawning or unit tests. Clearly record unit/offline verification vs. real network session acceptance.

---

## 7. Frontend UI/UX Architecture & Coding Directives

The `azdl` frontend is built with **Svelte 5** and strict adherence to **Material Design 3 (MD3)** desktop guidelines:

### 7.1. App-Wide Indeterminate Activity Indicator
- A discrete 2.5px Material 3 indeterminate progress bar is mounted directly below the title bar in [`App.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/App.svelte).
- Reactive trigger: `isAppDoingSomething = isApiBusy || hasActiveJobs || hasActiveTasks`.
- **Zero Layout Shift:** The loader is positioned absolutely/relatively with fixed height, ensuring no UI jumping or flickering when network or disk activities start or end.
- All backend-facing actions (`runAction`, `getState`, etc.) must run asynchronously via `spawn_blocking` and notify `onBusyChange` in [`api.js`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/api.js).

### 7.2. Mandatory Material You (Material Design 3) Theming & Unthemed Element Prevention
- **Mandatory Design Tokens:** Always style UI elements with CSS custom properties (`--md-sys-color-*`, `--md-sys-shape-*`, `--md-sys-elevation-*`). Never hardcode raw hex values (`#ffffff`, `#10b981`, `#ef4444`, etc.) or arbitrary RGBA values into templates or scoped styles.
- **Icons Come Only From the Icon Library — NEVER Raw SVG:** Every icon MUST be imported from `@ktibow/iconset-material-symbols` (e.g. `import iconPlay from '@ktibow/iconset-material-symbols/play-arrow'`) and rendered through `m3-svelte` (its `Icon` component or a component's `icon` prop). NEVER write `<svg>`/`<path>` markup, hand-copied path data (`d="M..."`), SVG data URIs, CSS `url(...svg)` backgrounds, icon font glyphs, or Unicode/emoji characters used as icons (`✕`, `✔`, `▶`, `⋮`, …). Pick the symbol that names what the control does, not one that merely resembles an old drawing. Use Google's default **Outlined** style: import the `-outline` variant whenever the iconset has one (`lock-open-outline`, `settings-outline`, …); the plain names are the Filled style and read as solid blobs at 16–20px. Line icons with no `-outline` file (`close`, `add`, `search`, …) are identical in both styles. If a needed symbol seems missing, search the iconset first; never draw one by hand. When touching a file that still contains a raw SVG icon, replace it with the library icon.
- **Icon Theming:** Library icons inherit `currentColor`, so icon buttons (`.icon-btn`, `.icon-btn.tonal`, `.icon-btn.danger` for trashbins/delete actions) take their parent semantic colors and hover states automatically.
- **Theme Modes:** Supports 4 runtime modes: `system`, `dark`, `light`, and `amoled` (true black `#000000` surface).
- **Opaque Windows:** Window backgrounds always use opaque theme surfaces. Do not restore Mica, Acrylic, compositor blur, or translucency preferences.
- **Controls & Typography:**
  - Standard buttons must use `.md3-btn` (`.primary`, `.tonal`, `.outlined`, `.text`, `.danger`) with full pill border radius (`--md-sys-shape-full`).
  - Text inputs must use `.md3-input` with container-high background and focus outlines.
  - Dropdowns must use the library chevron icon (never a custom SVG chevron) with container-high fills (`app.css`, `.md3-select`).
  - Icon buttons must use `.icon-btn` with full circle radius and standard hover/tonal/danger tokens.

### 7.3. Shared Section Card Container Component (`SectionCard.svelte`)
- **Single Component for Regular Sections:** All regular section containers across Settings, Panels, and views must use [`SectionCard.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/components/SectionCard.svelte).
- **Title Adjacent to Optional Icon:** Section titles must always be placed directly next to the optional icon (`.title-with-icon`).
- **Flexible Slots:** Supports title, optional library icon (`@ktibow/iconset-material-symbols`), badge pill, description, header action buttons snippet, and custom children body content.
- Do not invent custom, ad-hoc section cards or place section icons disconnected from the section title.

### 7.4. Clean Global Search Bar (Minimalist, No Badge Bloat)
- The search bar is global, located centrally in [`Titlebar.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/components/Titlebar.svelte), and accessible across all tabs.
- **Minimalist Appearance:** Only displays the search icon and contextual placeholder. Never place redundant tab badges next to the search icon.
- **Per-Tab Search State Isolation:** Each view preserves its own temporary search state in `tabSearches = { library: '', catalog: '', settings: '', game: '' }`.
- **Restoration on Tab Switching:** When switching tabs, the search input transitions to that tab's query; returning to a previous tab immediately restores its query.

### 7.5. Component Reusability & Form Modals
- **Always Reuse Components:** Avoid writing ad-hoc modal overlays, duplicated backdrops, or manual escape listeners.
- **Standard Modal Component:** Use [`Modal.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/components/Modal.svelte) for all dialogs (e.g. `ConfigureExecutableModal`, `MoveGameModal`).
- Modals provide standard MD3 scrim blur, elevation shadows, title/subtitle headers, focus trap, Escape key handling, and separated body and action snippets.

### 7.6. Elimination of UI Bloat & Duplicate Actions
- **Clean Layouts:** Remove unneeded, redundant explanatory banners. Settings show only controls the backend actually reads; never add a setting that nothing consumes.
- **No Duplicate Quick Actions:** On game pages, secondary actions ("Open Installation Folder", "Move to Another Drive", "Sync with Steam", "Launch Options", "Diagnostics", "Uninstall Game") belong in a consolidated Material You burger/more dropdown menu (`⋮`) in the play bar. Never duplicate these actions across sidebar cards.
- **Open Game Folder:** Every installed game card/row must provide a direct action to open its installation directory using safe native system file managers (`xdg-open` on Linux, `explorer` on Windows).

### 7.7. Launcher Autonomy & Steam Invariant
- `azdl` is a self-contained game launcher and prefix orchestrator.
- **Never spawn external Steam:** Downloads, manifests, and local game launches must NEVER invoke `steam steam://install/...` or `steam steam://rungameid/...`. All game depots and manifests are staged locally within the `azdl` workspace and executed under isolated Wine/Proton prefixes.

### 7.8. Never Remake Library Components (Mandatory `m3-svelte` Usage)
- Never create homemade HTML/CSS remakes of Material Design 3 components when official components exist in `m3-svelte`.
- **Tabs:** Use official `Tabs` (`import { Tabs } from 'm3-svelte'`) with `{ name, value }` item objects rather than custom `<button class="section-tab">` bars.
- **Switches:** Use official `Switch` (`import { Switch } from 'm3-svelte'`) with `bind:checked` inside a `<label>` rather than manual track/thumb `.md3-switch` elements.
- **Dialogs & Other Forms:** Use official or shared components (`Modal.svelte`, `m3-svelte`) to ensure strict adherence to Material Design 3 tokens and behaviors.

### 7.9. Mandatory Horizontal Game Cards & Artwork Capsules
- Both game cards (in library and catalog grids) and artwork capsule cards (on the dedicated game page) MUST be **horizontal** (wide landscape orientation, `aspect-ratio: 16 / 9` or Steam header 460x215 ratio), NEVER vertical 2:3 portrait posters.
- Grid containers (`LibraryView`, `CatalogView`) must use responsive wide columns (`repeat(auto-fill, minmax(280px, 1fr))`).
- Prioritize landscape headers and banners (`banner_url`, `header_image`) over portrait covers to avoid distortion and letterboxing.

### 7.10. Game Page Cleanliness
- **Game Page:** Tabs are Overview, DLC, Patches and Launch & Runner (which also holds diagnostics). The Patches tab holds the reversible per-game patch/fix controls (Online-Fix overlay, Steamless, DLC license bypass, Goldberg, EA activation, standalone patch) as confirmed-state `ActionSwitch` cards; diagnostics are not duplicated there. Game title is rendered in the topbar header (no Steam ID pill, no redundant status badges). The topbar is transparent when scrolled to the top (`scrollTop === 0`) and transitions to a surface container backdrop on scroll. The return button uses a discrete `m3-svelte` text button (`variant="text" size="s"`). Do NOT show "Ready to play" placeholder strings or blinking status indicator dots in the action bar. Tags (genres, sources, versions) belong in the sidebar "Game Information" card formatted with left-aligned label and right-wrapped tags, not directly beneath the title. Quick actions in the more menu (`⋮`) must smoothly scroll to their target sections.

### 7.11. Mandatory Error Palette & Universal Confirmation Modal (`ConfirmModal.svelte`)
- **Zero Native Browser Dialogs:** Never use `window.confirm()`, `window.alert()`, or `prompt()` anywhere in the application.
- **Universal Reusable Component:** All confirmation flows (game uninstallation, drive removal, DLC removal) must reuse [`ConfirmModal.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/components/ConfirmModal.svelte).
- **Error Palette Styling:** All destructive actions and error events must use the Material Design 3 error palette tokens (`--md-sys-color-error`, `--md-sys-color-on-error`, `--md-sys-color-error-container`, `--md-sys-color-on-error-container`, `.md3-btn.danger`, `.icon-btn.danger`).

### 7.12. Unified Notification Architecture
- **Single Source of Truth:** All notifications across the app must route through [`notifications.js`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/notifications.js) (`notify`, `notify.error`, `notify.success`, `notify.warning`, `notify.info`).
- **Unified Snackbar Stack:** Notifications are rendered centrally in [`App.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/App.svelte) with Material 3 container tokens and dismiss actions.
- Never implement ad-hoc local banners, toasts, or alert boxes inside individual views.

### 7.13. Complete Uninstallation & Leftover State Cleanup
- **Complete Disk Removal:** Uninstalling a game (`azdl::remove_game`) must recursively delete the game files on disk, Wine/Proton prefixes (`prefixes/<id>`), desktop and Steam shortcuts, and active transfer jobs.
- **Library Synchronization & Ghost Pruning:** The library state must never keep ghost records pointing to non-existent directories.
- **Post-Operation Leftovers Cleanup:** After every game installation, manifest bundles, transfer caches and staging folders must be purged (`cleanup_leftovers`).

---

## 8. Domain Capabilities Status Matrix & Next Milestones

| Domain | Current azdl state and evidence | Next completion evidence |
| --- | --- | --- |
| Steam discovery | Public Store search/artwork and appinfo are Rust modules; never entitlement evidence. Direct Discover and game-page installation needs no launcher extensions. | Live metadata and UI acceptance across supported games |
| Authorized content access | `steam_access` addresses the existing manifest relay directly under externally supplied authorization. Distinct authorization/missing-content/network errors. No fabricated Steam login or entitlement API. | Actual authenticated account transport if required by a deployment; live authorized source/CDN acceptance |
| Depot engine | Private `steam_engine` drives `steamroom-client` in-process; no native library, FFI or bundled runtime. Manifests pass `steam_manifest` validation plus link/path checks before transfer. One anonymous session (local Steam cell) serves every depot group of an install; 32 concurrent chunks. Offline tests cover cancel and pause markers on a stalled transfer, unsafe entries, cell parsing and key parsing. Live on Linux: App 5335510 (1.1 GB) installed and verified in about 40 s; paused at 30% and resumed to a byte-identical install through `azdl jobs resume`. | DLC downloads, auth-token-gated depots and Windows execution |
| Revision planning and receipts | `steam_content`, `steam_metadata`, `steam_manifest` and `steam_depot` retain source-pinned fresh-base revisions, exact depot/manifest checks, DLC/base compatibility and shared-depot content-App grouping. Offline corruption, missing-manifest, revision mismatch, path and symlink regressions pass. | Live base and shared-depot DLC combinations |
| Migration | Version 1 in `migration.rs`, OS file locking, original JSON/resource backups and marker-last publication. Repeated/concurrent migration, resource roots containing installed games, and linked metadata rejection have tests. IDs, launch configuration, game `Plugins`, prefixes, receipts, rollback records and content jobs survive. Obsolete launcher resources are archived, never loaded. | Migration acceptance against representative user library copies |
| Download recovery | Steam game and DLC jobs persist progress; other job kinds no longer exist and unreadable legacy records are skipped. Base transfer revision journals pin IDs and manifest SHA-256 before cache reuse. Engine pause produces a typed stopped result; Resume/Retry returns through the core Steam installer, and the CLI `jobs work` worker resumes Steam jobs. Live Linux base pause/resume passed. | Process interruption and DLC retry on both hosts; Windows pause/resume |
| DLC lifecycle | Steam DLC only: atomic additive installation from staged depots, shared ownership, collision rejection, removal, recovery journals and game moves have offline regressions. Archive DLC sources were removed; migration archives old `content-sources`. | Live DLC addition/removal against an authorized compatible base |
| Installation and storage | Steam installs only, into configured drives, with exact file-set verification before atomic publication, safe moves and complete removal. Installs needing an executable choice stay pending and keep their Steam origin when finished from the CLI or desktop. Archive, setup, torrent, HTTPS and folder-import installs were removed. | Native Windows and Linux game installs and failure recovery |
| Launch and runtime | Native Windows and isolated Linux Wine/Proton launches retain explicit arguments, environment and runner settings. Windows games on `auto` use the `default_runner` setting (validated on save, covered by `runtime::tests::auto_windows_game_uses_configured_default_runner`). Steam `installscript.vdf` registry entries and VC++/DirectX are provisioned into prefixes. Online-Fix/SteamFix payloads are game-agnostic: at launch the emulated App ID is driven from the game's `steam_app_id` (or a `SteamAppId` override), rewriting the INI `FakeAppId`, `steam_appid.txt` and Steam* env so one generic fix applies to any game (`multiplayer::tests::adapts_generic_online_fix_ini_to_game_steam_app_id`, `steam_app_id_override_in_environment_adapts_fix_ini`, `keeps_fix_ini_app_id_without_an_authoritative_source`). | Per-game runtime acceptance; live multiplayer session validation |
| Game management controls | Launch & Runner holds full-width runner dropdown, executable, launch options, arguments, environment, runtime dependencies and diagnostics. The Patches tab holds reversible per-game patch/fix switches read back from the live `game_controls` snapshot. Runtime installs and patch toggles are confirmed by fresh read-back (`controlActionConfirmed`) and stay disabled during commands. | Native Microsoft runtime installation acceptance and broader accessibility checks |
| CLI/API/UI | Settings → Compatibility Runners has a default-runner dropdown listing the same runners as the game page (excluding Native) with confirmed read-back. CLI, desktop actions and UI expose Steam install, DLC, launch, configuration, storage, shortcuts, diagnostics and the local patch/fix framework (Steamless, Goldberg, DLC-entitlement, EA heal, Online-Fix overlay, standalone patch) with its Patches tab and Settings category. Plugin, catalog, archive, setup, torrent, import, drive-scan, remote patch-fetch and Steam-running surfaces stay removed, as do settings nothing read (global launch arguments, download limits). Icons come only from Material Symbols (outlined), enforced by `icons.test.js`. | Live desktop interactions and accessibility acceptance |
| Packaging | Manual/tag-only release workflow; no cache restoration or automatic Verify workflow. Depot downloads are compiled in; no native engine is fetched, built or bundled. Local Linux `.deb`/`.AppImage` builds passed before the engine change. | Native Windows NSIS/MSI execution and distribution-host installation acceptance |
| Supporting tools | Manifest-bundle extraction (`bsdtar`), HTTPS for runtimes and redistributables (`curl`), shortcuts and diagnostics remain. `unrar` and Bubblewrap return for password-protected and encrypted-header RAR patch bundles applied from local archives (`extract_checked_with_passphrase`); `aria2c` stays unused. | Broader platform acceptance |

No offline suite establishes account authorization, successful live installation or multiplayer. Verify each supported game and service with authorized content and a real runtime session.
