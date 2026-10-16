# AGENTS.md — azdl Developer & Agent Operational Guide

This document provides architectural context, security invariants, module mapping, and development workflows for AI coding agents working on **`azdl`**.

For workspace-wide context and the relationship between `azdl` and `arachnel`, see the root [`../AGENTS.md`](file:///home/mixcraftio/Code/Repos/Crack/AGENTS.md).

---

## 1. Project Overview

`azdl` is a modern, lightweight game launcher and runtime orchestrator written in **Rust (edition 2024)**. It ports and expands the capabilities of the C++/Qt reference implementation `arachnel`, featuring:
- Headless CLI and local loopback Web UI (`127.0.0.1`).
- Multi-source game downloads (HTTPS resumable jobs, BitTorrent magnets via aria2c).
- Safe archive extraction (bsdtar, unrar, and Linux Bubblewrap sandboxing for encrypted-header RARs).
- Reversible patch management and multiplayer fix overlays (Online-Fix, SteamFix, `.arachnel-off` toggles).
- Wine and Proton prefix isolation with automated DLL overrides and FakeAppId generation.
- Steam ecosystem integration (manifest imports, binary `shortcuts.vdf` modification).
- Dynamic plugin system loading `.arach` shared libraries via C ABI v4.

---

## 2. Source Code Architecture & Module Guide

All core source code resides in [`src/`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/):

| Module | Core Responsibilities | Key Functions / Types |
| :--- | :--- | :--- |
| [`lib.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/lib.rs) | Domain types, library persistence (`library.json`), runner resolution, process launch. | `Game`, `Library`, `Runner`, `LaunchOption`, `launch_game` |
| [`main.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/main.rs) | CLI entry point, argument parsing, command dispatching. | `run_command`, CLI subcommands |
| [`archive.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/archive.rs) | Archive extraction (ZIP, TAR, 7z, RAR), multi-part RARs, password support, Bubblewrap sandbox. | `extract_archive`, `extract_rar_in_bwrap`, `validate_archive_members` |
| [`catalog.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/catalog.rs) | JSON source catalogs, Arachnel feed parsing, search, multi-part download caching. | `Catalog`, `SourceEntry`, `search_catalog`, `fetch_catalog_package` |
| [`discovery.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/discovery.rs) | Executable detection heuristics; filtering uninstaller, crash handler, redistributable noise. | `discover_candidates`, `find_likely_executable` |
| [`doctor.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/doctor.rs) | Runtime environment diagnosis (Wine, Proton, bwrap, unrar, bsdtar, permissions). | `diagnose_game`, `check_environment` |
| [`jobs.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/jobs.rs) | Persistent background HTTPS downloads, `.part` chunking, SHA-256 validation, install plans. | `DownloadJob`, `JobStatus`, `fetch_install_job`, `resume_job` |
| [`multiplayer.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/multiplayer.rs) | Fix detection (`OnlineFix64.dll`, `SteamFix`, `winmm.dll`), `FakeAppId`, Wine DLL overrides. | `detect_fix_markers`, `prepare_fix_environment`, `winmm_setup` |
| [`overlay.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/overlay.rs) | Reversible `.arachnel-off` file renaming for fix DLLs and AppID files. | `toggle_overlay`, `get_overlay_status` |
| [`patch.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/patch.rs) | File patches, staged patch rollbacks, multi-part patch archives, manifest backups. | `apply_patch`, `rollback_patch`, `toggle_patch` |
| [`plugins.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/plugins.rs) | Dynamic `.arach` library loading via `libloading`, C ABI v4 catalog reading. | `PluginHost`, `load_arach_plugin`, `read_plugin_catalog` |
| [`plugin_store.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/plugin_store.rs) | Remote plugin store fetching, SHA-256 package verification, local installation. | `fetch_store_plugins`, `install_store_plugin` |
| [`runtime.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/runtime.rs) | Host runner discovery (GE-Proton, Valve Proton, Wine), prefix directories. | `discover_proton_runners`, `resolve_runner` |
| [`setup.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/setup.rs) | Running Windows setup installers (`setup.exe`, `.msi`) in isolated Wine prefixes. | `run_wine_setup`, `import_from_prefix` |
| [`sha256.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/sha256.rs) | Pure streaming SHA-256 implementation (zero external crypto dependencies). | `compute_sha256`, `verify_file_digest` |
| [`shortcuts.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/shortcuts.rs) | FreeDesktop `.desktop` shortcut creation, validation, and deletion. | `create_desktop_shortcut`, `remove_desktop_shortcut` |
| [`steam.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/steam.rs) | Steam library scanning, VDF parsing (`libraryfolders.vdf`, `appmanifest_*.acf`). | `scan_steam_libraries`, `parse_vdf` |
| [`steam_shortcuts.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/steam_shortcuts.rs) | Steam binary `shortcuts.vdf` reader/writer, atomic backup preservation. | `add_steam_shortcut`, `remove_steam_shortcut` |
| [`torrent.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/torrent.rs) | Magnet URI validation, aria2c background download coordination. | `download_magnet`, `validate_magnet_uri` |

---

## 3. Critical Security & Sandboxing Invariants

When extending or modifying `azdl`, you must strictly maintain the following security invariants:

1. **No Arbitrary Shell Execution (`sh -c` / `cmd.exe /C`):**
   - Always spawn child processes using `std::process::Command::new(binary).arg(...)`.
   - Never construct shell string commands. Arguments must be passed as distinct vector elements.
2. **Path Traversal & Zip-Slip Protection:**
   - Any archive extraction or patch application must validate that target paths resolve within the designated destination directory.
   - Any relative paths containing `..` or leading `/` must be rejected (`rejects_archive_path_escape`).
3. **Strict Symlink Rejection:**
   - Symlinks in untrusted archives (game archives or patch packages) are forbidden (`rejects_symlinked_patch_paths`, `sandboxed_rar_tree_rejects_links`). Reject archives containing symlinks immediately to prevent file hijack attacks.
4. **Bubblewrap (`bwrap`) Sandboxing on Linux:**
   - Encrypted-header RAR files must be inspected/extracted inside an unprivileged Bubblewrap sandbox with a read-only view of the root filesystem and a confined temporary write directory.
5. **Reversibility of Patches and Overlays:**
   - Staged patches must record all modified and newly created files in their manifest.
   - When disabling or rolling back, newly created files must be removed, and modified files must be restored from backup.
   - Only **one** patch may be active per game at any time.

---

## 4. Cross-Platform Runtime Protocols

### Linux Launch Pipeline
1. Check game runner preference (`Native`, `Wine`, `Proton`).
2. If `Wine` or `Proton`:
   - Initialize isolated Wine prefix: `AZDL_DATA_DIR/prefixes/<game-id>`.
   - Inspect executable and directories for multiplayer fix files (`multiplayer.rs`).
   - If a fix is found, inject `WINEDLLOVERRIDES` (e.g. `onlinefix64=n,b;winmm=n,b;steam_api64=n,b;...`).
   - If `OnlineFix.ini` specifies `FakeAppId`, populate `steam_appid.txt` and export the AppID to the runner environment.
   - Select 32-bit vs. 64-bit `SteamFix` in `winmm.txt` according to binary PE headers.
   - Do not require or auto-spawn Steam: run games cleanly under Wine/Proton prefix isolation without background Steam dependencies.

### Windows Launch Pipeline
- Run game executable directly or through specified launcher arguments.
- Handle paths with native Windows separators and preserve UTF-8 compatibility.

---

## 5. Coding Standards & Rust Idioms

**Commit often.** Make small, coherent commits after verification, staging only changes made for the current task.

1. **Rust Edition:** Rust 2024.
2. **Minimal Dependencies:** Keep `Cargo.toml` lean. Do not introduce heavy dependencies (e.g., Tokio, Reqwest, or full OpenSSL/Ring) unless explicitly justified and approved by the user. Use existing lightweight primitives (`libloading`, `serde`, `serde_json`, standard library).
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
- **Icon Theming:** All inline SVG icons MUST specify `fill="currentColor"`. This guarantees that icon buttons (`.icon-btn`, `.icon-btn.tonal`, `.icon-btn.danger` for trashbins/delete actions) automatically inherit their parent semantic colors and hover states.
- **Theme Modes:** Supports 4 runtime modes: `system`, `dark`, `light`, and `amoled` (true black `#000000` surface).
- **Translucency Support:** Supports Linux compositor blur and Windows Mica/Acrylic backdrops (`--app-backdrop-blur`, `--app-window-bg`).
- **Controls & Typography:**
  - Standard buttons must use `.md3-btn` (`.primary`, `.tonal`, `.outlined`, `.text`, `.danger`) with full pill border radius (`--md-sys-shape-full`).
  - Text inputs must use `.md3-input` with container-high background and focus outlines.
  - Dropdown `<select>` inputs must use custom SVG chevron indicators and container-high fills (`app.css`, `.md3-select`).
  - Icon buttons must use `.icon-btn` with full circle radius and standard hover/tonal/danger tokens.

### 7.3. Shared Section Card Container Component (`SectionCard.svelte`)
- **Single Component for Regular Sections:** All regular section containers across Settings, Panels, and views must use [`SectionCard.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/components/SectionCard.svelte).
- **Title Adjacent to Optional Icon:** Section titles must always be placed directly next to the optional icon (`.title-with-icon`).
- **Flexible Slots:** Supports title, optional SVG path icon, badge pill, description, header action buttons snippet, and custom children body content.
- Do not invent custom, ad-hoc section cards or place section icons disconnected from the section title.

### 7.4. Clean Global Search Bar (Minimalist, No Badge Bloat)
- The search bar is global, located centrally in [`Titlebar.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/components/Titlebar.svelte), and accessible across all tabs.
- **Minimalist Appearance:** Only displays the search icon and contextual placeholder. Never place redundant tab badges next to the search icon.
- **Per-Tab Search State Isolation:** Each view preserves its own temporary search state in `tabSearches = { library: '', catalog: '', settings: '', game: '' }`.
- **Restoration on Tab Switching:** When switching tabs, the search input transitions to that tab's query; returning to a previous tab immediately restores its query.

### 7.5. Component Reusability & Form Modals
- **Always Reuse Components:** Avoid writing ad-hoc modal overlays, duplicated backdrops, or manual escape listeners.
- **Standard Modal Component:** Use [`Modal.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/components/Modal.svelte) for all dialogs (e.g. `AddStoreModal`, `InstallBundleModal`, `ConfigureExecutableModal`, `MoveGameModal`).
- Modals provide standard MD3 scrim blur, elevation shadows, title/subtitle headers, focus trap, Escape key handling, and separated body and action snippets.

### 7.6. Elimination of UI Bloat & Duplicate Actions
- **Clean Layouts:** Remove unneeded, redundant explanatory banners. Host architecture auto-detection is displayed as a single platform icon chip (`Linux` or `Windows`) in the header; Wine prefix isolation notes are integrated directly under the runner selector.
- **No Duplicate Quick Actions:** On game pages, secondary actions ("Open Installation Folder", "Move to Another Drive", "Sync with Steam", "Launch Options", "Diagnostics", "Uninstall Game") belong in a consolidated Material You burger/more dropdown menu (`⋮`) in the play bar. Never duplicate these actions across sidebar cards.
- **Open Game Folder:** Every installed game card/row must provide a direct action to open its installation directory using safe native system file managers (`xdg-open` on Linux, `explorer` on Windows).

### 7.7. Launcher Autonomy & Steam Invariant
- `azdl` is a self-contained game launcher and prefix orchestrator.
- **Never spawn external Steam:** Catalog downloads, manifests, and local game launches must NEVER invoke `steam steam://install/...` or `steam steam://rungameid/...`. All game depots, manifests, and fix overlays are staged locally within the `azdl` workspace and executed under isolated Wine/Proton prefixes.

### 7.8. Never Remake Library Components (Mandatory `m3-svelte` Usage)
- Never create homemade HTML/CSS remakes of Material Design 3 components when official components exist in `m3-svelte`.
- **Tabs:** Use official `Tabs` (`import { Tabs } from 'm3-svelte'`) with `{ name, value }` item objects rather than custom `<button class="section-tab">` bars.
- **Switches:** Use official `Switch` (`import { Switch } from 'm3-svelte'`) with `bind:checked` inside a `<label>` rather than manual track/thumb `.md3-switch` elements.
- **Dialogs & Other Forms:** Use official or shared components (`Modal.svelte`, `m3-svelte`) to ensure strict adherence to Material Design 3 tokens and behaviors.

### 7.9. Mandatory Horizontal Game Cards & Artwork Capsules
- Both game cards (in library and catalog grids) and artwork capsule cards (on the dedicated game page) MUST be **horizontal** (wide landscape orientation, `aspect-ratio: 16 / 9` or Steam header 460x215 ratio), NEVER vertical 2:3 portrait posters.
- Grid containers (`LibraryView`, `CatalogView`) must use responsive wide columns (`repeat(auto-fill, minmax(280px, 1fr))`).
- Prioritize landscape headers and banners (`banner_url`, `header_image`) over portrait covers to avoid distortion and letterboxing.

### 7.10. Game Page Cleanliness & Plugins Configuration Simplicity
- **Game Page:** Game title is rendered in the topbar header (no Steam ID pill, no redundant status badges). The topbar is transparent when scrolled to the top (`scrollTop === 0`) and transitions to a surface container backdrop on scroll. The return button uses a discrete `m3-svelte` text button (`variant="text" size="s"`). Do NOT show "Ready to play" placeholder strings or blinking status indicator dots in the action bar. Tags (genres, sources, versions) belong in the sidebar "Game Information" card formatted with left-aligned label and right-wrapped tags, not directly beneath the title. Quick actions in the more menu (`⋮`) must smoothly scroll to their target sections.
- **Plugins Settings:** The Plugins settings page is strictly a store & installed plugin manager. Do NOT add unnecessary configurability (no bundle upload forms, no custom plugin generator, no manual catalog feed editors). Automatically refresh the plugin store upon navigating to the Plugins page and immediately after adding a store.

### 7.11. Mandatory Error Palette & Universal Confirmation Modal (`ConfirmModal.svelte`)
- **Zero Native Browser Dialogs:** Never use `window.confirm()`, `window.alert()`, or `prompt()` anywhere in the application.
- **Universal Reusable Component:** All confirmation flows (game uninstallation, drive removal, plugin uninstallation, catalog source removal) must reuse [`ConfirmModal.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/components/ConfirmModal.svelte).
- **Error Palette Styling:** All destructive actions and error events must use the Material Design 3 error palette tokens (`--md-sys-color-error`, `--md-sys-color-on-error`, `--md-sys-color-error-container`, `--md-sys-color-on-error-container`, `.md3-btn.danger`, `.icon-btn.danger`).

### 7.12. Unified Notification Architecture
- **Single Source of Truth:** All notifications across the app must route through [`notifications.js`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/notifications.js) (`notify`, `notify.error`, `notify.success`, `notify.warning`, `notify.info`).
- **Unified Snackbar Stack:** Notifications are rendered centrally in [`App.svelte`](file:///home/mixcraftio/Code/Repos/Crack/azdl/frontend/src/App.svelte) with Material 3 container tokens and dismiss actions.
- Never implement ad-hoc local banners, toasts, or alert boxes inside individual views.

### 7.13. Complete Uninstallation & Leftover State Cleanup
- **Complete Disk Removal:** Uninstalling a game (`azdl::remove_game`) must recursively delete the game files on disk, Wine/Proton prefixes (`prefixes/<id>`), desktop and Steam shortcuts, and active transfer jobs.
- **Library Synchronization & Ghost Pruning:** The library state must never keep ghost records pointing to non-existent directories.
- **Post-Operation Leftovers Cleanup:** After every game installation, archive extraction, setup installer execution, or plugin installation, all temporary download archives (`.zip`, `.rar`, `.arach`), installer binaries, and staging folders must be purged (`cleanup_leftovers`).

---

## 8. Domain Capabilities, Status Matrix & Next Milestones

Reference: `../arachnel/src/core`, `../arachnel/qml`, and `../arachnel/docs/ARCHITECTURE.md`. The objective is the full user facing launcher, including download, install, launch, and supported multiplayer flows. A passing Rust build proves only the implemented rows below.

| Arachnel domain | Current azdl state | Next completion evidence |
| --- | --- | --- |
| DLC | Additive explicit archive DLC from a game's installed source plugin can be downloaded, verified, installed without overwriting base files, listed, recovered after interruption, moved with the game, and removed with a file hash manifest; desktop Game Page exposes install and removal. The desktop merges Steam Store DLC IDs with public appinfo depot relationships for installed Steam games, fetches bounded DLC titles, keeps records visible after metadata lookup failures, and explains unavailable content. Catalog searches identify DLC results and route them to the parent game's DLC section, with Store detail fallback for unenriched results. Archive DLC is available only when a valid HTTPS artifact is present. Managed steamidra base installs select non-DLC depots and write exact revision receipts; an experimental background DLC task resolves exact matching provider depots, downloads into staging, and commits files through the recovery manifest. Identical existing files are reused with ownership tracking, so base files remain untouched and files shared by multiple DLCs remain until the last managed reference is removed. Provider bundles now resolve only the selected base or DLC depot manifests, so an absent unrelated DLC manifest does not block a base install; unknown provider depot IDs, missing required base depots, and missing selected revisions fail with specific errors. DLC shared-depot resolution follows bounded depotfromapp manifest chains, rejects cycles, selects exact provider revisions by manifest owner, and downloads groups by content App ID into separate verified staging directories; this has offline fixture coverage but no live shared-depot acceptance. Selected provider binary manifests require matching depot/revision metadata, safe file paths, regular-file hashes, and no symlinks before download. Before DLC file commit, azdl verifies every staged file against the exact selected manifest set, rejecting missing, changed, unlisted, symlinked, and conflicting shared paths. A read-only verifier compares installed bytes against the exact manifest, without granting ownership. The game page offers Check Files for old installs and records exact existing DLC as unmanaged content. For missing content, azdl verifies every required base depot against the current provider manifest before creating a distinct VerifiedLegacy base receipt and proceeding with download; incomplete or changed bases fail without a receipt. Offline archive, metadata, depot-selection, staged-install, catalog-routing, interruption-recovery, drive-move, modified-file, shared-file ownership/removal-order, and uninstall-serialization tests pass; frontend tests/build pass. DLC mutations, game launch, and game uninstall share a per-game file lock; the desktop blocks launch during a DLC task, blocks install/removal/recovery while the game is running, and core and desktop block uninstall during an active DLC download. DLC download workers hold a lifetime file lock; stale running jobs after restart appear interrupted and can be cleared, including on Windows. A new depot attempt clears stale payload bytes and cancellation markers under the worker lock while preserving DLC recovery receipts; partial downloads currently restart from zero. Depot DLC cancellation is coordinated with the transition to file commit; Pause and Resume are unavailable for this transport, and a cancellation/commit test passes. Linux live checks downloaded sample DLC 3875100 (one 539,727,136 byte file) and removed it, verified preexisting DLC without modifying game files, and verified the base depot as VerifiedLegacy. A separate copy of that real base game with the DLC omitted then passed strict legacy verification, downloaded and committed the DLC with a matching SHA-256 receipt, and removed it while retaining all base files and the base receipt. The new staged verifier also matched an isolated copy of the real 539,727,136 byte DLC file against its exact provider manifest; the copy was removed afterward. The Rust core also passes Windows GNU target check and Clippy without warnings; Windows runtime and in-game access remain unverified. | In-game DLC access, Windows runtime acceptance, manifest signature verification, broader old-base migration, different-content overlays, updates, resumable restart recovery, live cancellation acceptance, and provider-owned install ABI |
| Library and storage | JSON library, manual and executable assisted import, Steam manifest import, catalog provenance, backward-compatible persisted Steam App IDs and Discover artwork URLs on installed records, safe recursive game uninstallation (`azdl::remove_game`) completely removing game install directories, Wine/Proton prefixes, shortcuts, and leftover files on disk, automatic ghost entry pruning when folders are deleted, unfinished or portable directory management with immutable entry names (locked to directory ID), candidate executable auto-discovery (`discover_exes` fallback for pending directories) with manual fallback, and safe directory removal (`pending-remove`) with Material 3 confirmation modals (`ConfirmModal`), post-install download and staging leftovers cleanup (`cleanup_leftovers`), configurable data directory, multi-storage drive configuration (`settings.json`), game migration between drives, and automatic drive/Steam scanning | Game file verification/repair, cloud save syncing, migration stress tests |
| Catalog and covers | Steam Store discovery in desktop and CLI through Store search, featured lists, and App ID detail requests; universal AppImage dynamic linker and environment sanitization (`azdl::clean_command`) resolving host dynamic linker collision (exit code 127) by stripping AppImage's internal `$APPDIR/usr/lib` from `LD_LIBRARY_PATH` for spawned tools (`curl`, etc.), ensuring reliable Steam Store discovery across native tarballs, debs, and AppImages. Offline on-disk artwork caching system saving actual `.jpg`/`.png` covers and banners to disk (`<data_dir>/artwork-cache/<id>/`) and serving them via secure custom protocol (`azdl-art://localhost/<id>/banner.jpg`) with traversal guards and background download persistence, enabling 100% offline instant library card rendering. Discover artwork URLs and Steam App IDs are carried through the selected plugin install and persisted to the installed game record. Existing Steam installs without saved artwork resolve through Steam app-details `header_image`; guessed legacy CDN URLs are invalidated from backend and frontend artwork caches. Game cards show only storage size: Discover reads the minimum PC free-space requirement from Steam Store app details for visible cards, while Library reports measured installed files; missing Store requirements remain absent. Search results are cached in memory for five minutes and do not read JSON source feeds or plugin catalogs. The initial page shows featured games, and title/App ID search runs through the Steam Store. `GameCard.svelte` concurrently loads both fallback and high-quality thumbnails, displaying whichever loads first and seamlessly transitioning to the HQ artwork once available. Offline resolver, disk cache, cache invalidation, persistence, dual-load candidate derivation, and card metadata tests pass. | Live desktop acceptance, stable documented search source or keyed app-list synchronization, pagination, richer metadata, and network failure UX |
| Plugins | Installed plugins are selectable on a Steam game page, with a persisted favorite plugin used for automatic choice; multiple plugins without a favorite require explicit selection. At install time, azdl resolves the selected provider by App ID; steamidra uses the Ryuu manifest relay and installed depotkit engine, while other API 4 plugins can provide explicit standard packages. When no download plugins are installed, a dedicated button on the game page links directly to the Plugins store in Settings. Silent background store refresh without intrusive toast notifications. Manifest ZIP extraction reports missing manifests and provider error pages clearly; missing manifest, provider outage, missing depotkit, and missing depot data produce distinct errors. Provider depot parsing requires the requested manifest revision, rejects ambiguous revisions, retains the selected manifest ID, and resolves only depots selected by Steam metadata; wrong-revision and unrelated-missing-DLC fixtures pass. Full Windows compatibility for plugin bundle extraction (supporting Windows built-in `tar.exe`) and best-effort native library verification in `install_bundle_impl` allowing catalog-URL and toolkit plugins (such as steamidra with its bundled Online-Fix kits and Steamless) to install without requiring host Qt6 dynamic runtime dependencies. Offline provider selection and manifest staging tests pass. | Complete source-aware depot selection, interoperable plugin-owned install ABI, real installed-plugin compatibility, online provider acceptance, and authenticated depot download acceptance |
| Source patches | Reversible overlay from a local directory or archive with optional archive root selection, HTTPS patch fetch and apply for one archive or a multi-part RAR set, explicit catalog HTTPS addon patch including split RAR entries, enable/disable toggle, one active patch per game, encrypted ZIP and RAR extraction including encrypted headers on Linux with Bubblewrap | Protected download, live remote multi-part transfer test, cross-platform encrypted-header RAR handling, source-specific setup steps, per game install and multiplayer tests |
| Download jobs | Persistent HTTPS and depot jobs remain available. Desktop Steam installs run in isolated background workers without noisy start notifications; failed workers retain a failed task with an error message, and the app emits a central notification once per failure. Transfer drawer features a real-time Material 3 progress bar with percentage, downloaded/total fractions, and drawer header progress line powered by bundle size estimation from Steam store PC requirements and plugin metadata, while decoupling download jobs from the top app-wide indeterminate indicator bar. Worker deduplication and fast IPC return have offline tests. | Live failed-manifest and successful-download UI acceptance, robust worker crash handling, and concurrent library mutation tests |
| Torrent | Magnet transfer through aria2c and portable payload import; no live transfer verified here | Native Rust transport, persistent job state, progress model, pause/resume, rate limits, live torrent tests |
| Install | Single executable fetch or local managed file copy, checked portable archive extraction through cross-platform archive tool resolution (`tar` on Windows falling back to `bsdtar`, `bsdtar` on Linux) with the online-fix.me password and a Linux Bubblewrap fallback for encrypted headers, multi-part RAR archive extraction and sequential HTTPS part download from CLI/browser/catalog, Linux Wine and native Windows setup.exe/MSI and setup archive run with HTTPS fetch and prefix import, browser and CLI executable selection for unfinished installs, explicit catalog setup entries including split RAR packages, fully decoupled background installer threads preventing long-running IPC stalls or window manager event loop freezes (`_NET_WM_PING`) on multi-gigabyte downloads and unrar/unzip extractions; universal Windows backslash path normalization (`archive::normalize_windows_paths`, clamped to 16 passes) automatically transforming Windows-formatted paths into genuine recursive POSIX directory trees on Linux across all archive extractions (ZIP, RAR, 7z, TAR, setup archives), Steam depot downloads, and game imports with recursive directory merging and zip-slip path traversal guards while keeping executable discovery (`discovery::discover_executables`) fast and read-only; Ryuu Steam manifest unpack and depot cache staging with repair addon download; native Steam depot downloading via `depotkit` C ABI (`steam_depot.rs`) with clamped worker concurrency (16 max workers) to prevent CPU and disk I/O event loop starvation, automated Online-Fix / fix addon extraction and overlay into game tree, architecture-aware steamidra bundled-fix installation with reversible patch backups and launch-time repair of existing managed installs, and automated root executable discovery and registration into library (`discovery::import_game`); selected storage drives are passed through desktop install workers for archive, executable, and Steam depot installs; setup and magnet installs explicitly require the built-in Default Library directory until selected drive support is added; active download target isolation in `pending_installs` to prevent incomplete folders from racing with library state; standalone isolated staging with strict avoidance of external Steam desktop client spawning (verified end-to-end with *Gamble with your friends*, 2.2 GB direct Steam CDN download) | Cross-platform encrypted-header RAR packages, remote multi-part network acceptance test, Rust native archive handling, broader setup detection, plugin install sessions, DLC and source specific updates |
| Launch | Native, Wine, selected Proton, arguments, environment; selectable local launch options with working folders and arguments; local launch diagnostics; browser session running state and Stop for browser-started launch groups; Online Fix, Steam Fix, and related fix DLL marker detection with cross-platform Steam running checks (`steam_running`), pre-launch notice when Steam is not running for Online-Fix games, without starting the Steam client, local fix DLL/FakeAppId environment passed to the Wine or Proton process and file preparation without forcing background Steam client execution, 32-bit SteamFix winmm selection, FreeTP promo popup suppression, and reversible toggle of existing Arachnel style DLL overlays; automated fix payload file synchronization (`ensure_fix_payload_files` propagating root OnlineFix/SteamFix DLLs and configs downward to nested shipping binary folders like `Astro/Binaries/Win64/` to resolve Unreal Engine Steam entitlement checks), relaxed steamidra bundled-fix origin and secondary drive checks (`ensure_steamidra_fix`), enabled for native Windows execution (`Runner::Native`) as well as Wine/Proton; SteamStub PE `.bind` section detection across nested and shipping binaries with Steamless unpacking/rollback (`steamless.rs`), automated Steamless CLI discovery in plugin trees with runtime DLL dependency repair (`Steamless.API.dll`/`SharpDisasm.dll`), automated Steamless GitHub download fallback (`ensure_steamless_cli`) using cross-platform archive extraction, synchronous and non-racing SteamStub DRM stripping across all candidates with atomic backend cache updates and direct status return, EA Frostbite bytecode patcher (`Activation64.dll`/`Activation.dll` jump flip) with rollback (`.ea.bak`), and multi-binary 32/64-bit `steam_api.dll` provisioning from game tree | Persistent game process tracking across UI restarts and detached runners, plugin supplied launch options, source specific overlay install and launch preparation, deeper runtime diagnostics, VR |
| Runtime | Separate Wine prefix and Proton compatibility data; installed Proton discovery and automatic selection on Linux; automatic Proton-GE release lookup and 1-click download/unpack to runners; unequivocal "System Wine" naming; `/api/runners` endpoint exposing native, System Wine, and discovered GE-Proton/Proton runners | Runner provisioning, dependencies, prefix repair |
| Settings | Comprehensive settings model (`LauncherSettings`), JSON persistence (`settings.json`), two-container Storage architecture (Drives for installation paths and Games for installation management with seamless inter-drive migration via pure-Rust recursive directory copying and uninstallation), clean Launch & Runners section (simplified to Default Compatibility Tool selector with unequivocal "System Wine" and global arguments, with the redundant Detected Environments table eliminated), streamlined Plugins & Sources focused strictly on store and local plugins with silent auto-refresh on load and store add, official `m3-svelte` Switch components, dedicated Patches tab focused exclusively on game patches (SteamStub auto-unpack via Steamless, Online-Fix runtime overlay automation) with obsolete Steam library injection UI eliminated, dynamic Material You settings, and system diagnostics ("Doctor" checks) recognizing Windows built-in `tar.exe` alongside `bsdtar` | Cloud settings sync, prefix backup configuration |
| Platform integration | Steam library manifest scan; direct Steam client launch paths have been removed; Linux and Windows desktop shortcut create/status/remove through the azdl launch path (Linux `.desktop` and Windows `.url` shortcuts); automated standalone Linux desktop integration (`ensure_desktop_integration` embedding multi-size icons in `~/.local/share/icons/hicolor`, `pixmaps`, and provisioning desktop launchers in `~/.local/share/applications/` and Desktop (`Bureau`/`Desktop`) with distinct per-identifier `StartupWMClass` (`azdl-desktop` and `io.azdl.launcher`) executed before window mapping, and AppImage persistent binary resolution via `APPIMAGE` environment variable for native Wayland compositor/KWin icon association and taskbar recognition; runtime window icon configuration in Tauri via `window.set_icon`); subprocess environment sanitizer (`clean_command` / `sanitize_command` in `src/cmd.rs`) isolating host processes (`curl`, `wine`, `bsdtar`, `bwrap`, `xdg-open`, desktop validators) from AppImage bundle library pollution and suppressing flashing Windows command prompt windows across all child process calls via `CREATE_NO_WINDOW` (`0x0800_0000`); automated multi-platform GitHub Actions release workflow (`.github/workflows/release.yml`) triggered manually (`workflow_dispatch`) and on tags, compiling and packaging native Linux installers (`.deb`, `.AppImage`), Windows installers (`.exe`, `.msi`), portable archives (`.tar.gz`, `.zip`), and `SHA256SUMS.txt`; removal of obsolete non-Steam shortcuts injection UI in favor of standalone launcher isolation | Live Steam shortcut acceptance test, unified azdl launch preparation from Steam |
| UI | Tauri/Svelte desktop uses Steam Store results for the Discover view with centered empty-state subtext; Discover and Library share one card component with only a storage-size tag, using Steam PC requirements for Discover and measured directory size for Library; download FAB in Discover and Game page using the favorite or sole installed plugin without noisy start toasts; during download the game card in Discover glimmers (`shimmer-active`) with FAB cleanly hidden and pill removed; dedicated Game Page provides direct button navigation to Plugins when none are installed, reactive Steam store metadata loading with resilient video player engine (Hls.js adaptive streaming, native HLS, Akamai CDN MP4 fallback resolution, and teardown lifecycle on media change), determinate download progress bar with bundle size estimation and percentage advancement without blinking dots; the selected Material You seed remains consistent across game-page navigation; Transfer drawer provides active progress bars with percentage and fraction bytes without triggering the titlebar global activity bar; GameCard implements concurrent dual thumbnail loading (concurrent fallback and HQ requests, instant display of whichever arrives first, seamless promotion to HQ when available, and zero-layout-shift layered rendering); pre-launch Steam running verification prompting user via Material 3 ConfirmModal before launching Online-Fix titles; clean Game Page more menu with quick action renamed to "Patches" navigating smoothly to the redesigned Patches tab and section (`#section-patches`) featuring state-aware MD3 cards for Online-Fix overlays, Steamless DRM stripping with responsive optimistic state toggles and action-pending feedback, and EA activation repair; Discover artwork persists into Library and uses CDN fallbacks if a saved URL fails. Central notifications report failed install, plugin, storage, and Proton background tasks. Dead code eliminated (removed unused `GameDetailsModal.svelte` and obsolete `createPlugin`/`installPluginBundle` functions). IPC type safety enforced at compile time with Serde-tagged `Action` enum across backend operations. Battery efficiency optimized for portable hardware (Steam Deck/laptops) via Tauri IPC event push (`state-changed`) combined with adaptive polling (1s active, 15s idle). Frontend build and Vitest checks pass offline. | Live desktop acceptance for discovery, selection, manifest missing/available cases, and cross-platform checks |
| Social and multiplayer | Game's own networking; Steam Fix launch preparation only | Per game supported service integration and multiplayer acceptance tests |

No single launcher can establish that every possible game runs or has multiplayer: game binaries, entitlements, services, and server availability vary. Acceptance must therefore cover each supported game and service combination with a real installation and multiplayer session. Do not claim full completion from unit tests or launcher process startup alone.
