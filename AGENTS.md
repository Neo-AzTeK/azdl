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
| [`marker.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/marker.rs) | Arachnel compatibility markers (`.arachnel-steamidra`) and launch option import. | `read_install_marker`, `import_marker_launch_options` |
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
| [`ui.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/ui.rs) | Loopback HTTP server (`127.0.0.1`), token security, REST actions, process tracking. | `start_ui_server`, UI endpoints |
| [`updates.rs`](file:///home/mixcraftio/Code/Repos/Crack/azdl/src/updates.rs) | Staged game updates, previous version retention, rollbacks. | `update_game`, `rollback_update` |

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
3. **Update `PORTING.md`:**
   - If your work implements, expands, or verifies a domain listed in [`PORTING.md`](file:///home/mixcraftio/Code/Repos/Crack/azdl/PORTING.md), update the corresponding row's **Current azdl state** and **Next completion evidence**.
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
