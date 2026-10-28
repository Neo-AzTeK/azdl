# UI inventory and Slint Material feasibility

Audited 2026-10-28 against all 14 Svelte components under `frontend/src`, their state/API/theme/artwork helpers, and the Tauri window, event and resource integration. This is a source and capability assessment, not a completed Slint prototype or performance benchmark.

## Verdict

A complete native desktop rewrite is feasible. The controls and layouts used by azdl have either a direct Material component counterpart or a straightforward Slint composition. It would be a new presentation layer and application adapter, not a CSS/theme replacement. The Rust content, installation, library and launch engines can remain.

The official **Slint Material Components** library is the appropriate target, distinct from merely choosing the `material` style for Slint's standard widgets. The current documented release is Material Components 1.1.0, requiring Slint 1.18+. Its old standalone repository is archived; development lives in the Slint monorepo. These are verified current requirements, not a proposal to install dependencies in this change. [Setup and component index](https://material.slint.dev/getting-started/), [standard-widget styles](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/style/).

## Every current component

Complexity below is an engineering assessment based on the current behavior, not measured implementation time.

| Current component | UI and behavior present | Slint approach | Relative effort |
| --- | --- | --- | --- |
| `App.svelte` | Lazy view routing, global activity bar, error/success snackbars with actions, launch notices, refresh/progress subscriptions, theme state | Root window, view/model selection, progress and notification overlays; replace IPC subscriptions with a UI-thread event bridge | Medium |
| `Titlebar.svelte` | Frameless drag area, minimize/maximize/close, per-view search, clear search, running-game/stop control | Native window movement/actions plus search field and icon controls; retain tab-specific queries | Medium; test Wayland, X11 and Windows |
| `NavRail.svelte` | Library/Discover/Settings navigation and selected state | Official navigation rail with bound selection | Low |
| `LibraryView.svelte` | Responsive installed-game grid, pending installations, import/remove confirmation and empty states | Scrollable model-backed grid, cards and shared dialogs | Medium; validate large-library scrolling |
| `CatalogView.svelte` | Featured/search results, loading/error/empty states, DLC parent routing and direct install | Search/result models and responsive card grid; existing Rust discovery remains | Medium |
| `GameCard.svelte` | Landscape artwork, simultaneous fallback/HQ images, lazy loading, shimmer, size label and play/stop/install/info FABs | Reusable landscape card with asynchronous image state, visibility-aware loading and overlaid actions | Medium |
| `GamePageView.svelte` | Scroll-dependent header, play bar/more menu, screenshots/trailers, description, information tags, Overview/DLC/Patches/Launch tabs, DLC filters/progress/recovery, runner/executable/argument/environment forms and diagnostics | Compose native panels from shared models; use specialized media and artwork services rather than copying DOM code | High; largest view and media integration |
| `SettingsView.svelte` | Searchable categories, storage-drive/game rows, move/remove/default actions, runner and global argument selectors, automation switches, seed presets/custom color, four theme modes and diagnostics | Form panels, selectors, switches, dialogs and palette controls; preserve settings values and validation | Medium |
| `TransferDrawer.svelte` | Expandable floating transfer list, thumbnails, status/error text, byte totals, determinate/estimated progress and pause/resume/retry/cancel/clear actions | Sheet/overlay composition, repeated rows and progress models; preserve backend job semantics | Medium |
| `SectionCard.svelte` | Shared title/icon/badge/action header and body container | Shared card wrapper around official card/layout components | Low |
| `Modal.svelte` | Scrim, focus management, Escape dismissal, close action and variable content/actions | Shared native modal/dialog wrapper; explicitly retain focus restoration and keyboard behavior | Low–medium |
| `ConfirmModal.svelte` | Destructive styling, confirm/cancel/alternative action, async busy lock | Shared confirmation dialog bound to a pending-operation model | Low |
| `ImportGameModal.svelte` | Executable scan, selector/custom relative path, form validation, pending-directory removal and confirmation | Native form dialog and executable model; no browser file-upload dependency | Low–medium |
| `ActionSwitch.svelte` | Official m3 switch, one command per mouse/keyboard/drag change, confirmed-state display and disabled/pending handling | A controlled wrapper around the native Material switch using the same command/read-back contract | Low |

## Atomic elements and visual behavior

| Element family in the current UI | Migration assessment |
| --- | --- |
| Text, headings, labels, tooltips and SVG icons | Native text/image/path elements; extract the used Material icon assets from the current JS icon packages. Preserve accessible labels. |
| Filled/tonal/text/outline/danger buttons, icon buttons and FABs | Direct component coverage; apply the existing semantic error palette to destructive actions. |
| Switches, text/search inputs, radio tabs, selects and contextual menus | Component equivalents exist. Bind values to application models and preserve validation, command locks and keyboard handling. |
| Cards, badges/chips, separators and selected/running/disabled states | Standard components plus application-specific composition; do not build an entirely new widget library. |
| Responsive grids, sidebars, scrolling, smooth section navigation and animated headers | Native layouts/scroll positions and animations. CSS grid rules and `scrollIntoView` must be reimplemented, not translated verbatim. |
| Determinate/indeterminate/estimated progress, shimmer and notifications | Rebuild the visual animations and overlays; preserve actual job values, busy counts, error retention and cancellation restrictions. |
| Color presets, custom color input, system/dark/light/AMOLED modes | Port the existing palette algorithm and persistence. A default Material theme alone does not reproduce azdl's seed-derived colors or artwork-derived palettes. |
| Screenshots, capsule/header artwork, fallback/HQ promotion and caching | Move fetch/cache/decode coordination into a background Rust service and update native image properties on the UI thread. |
| Steam descriptions | The current page converts HTML to **plain text** with `DOMParser`; it does not render arbitrary HTML. A Rust text conversion preserves current behavior without embedding a browser. Rich formatting would be an optional additional feature. |
| Native window controls, focus, hover, clipping, error colors and keyboard access | Feasible, but validate behavior and scaling on both supported operating systems; do not assume browser CSS and native rendering are pixel-identical. |

The official library's component index covers the relevant control families. Slint also has custom-titlebar, asynchronous-image, and video integration examples. [Component catalogue](https://material.slint.dev/getting-started/), [official examples](https://github.com/slint-ui/slint/blob/master/examples/README.md).

## The main integration cost: trailers

The current player uses the browser `<video>` element, lazy-loaded Hls.js, native HLS support detection, and MP4 fallbacks, with playback cleanup on media changes. That cannot be carried over by renaming a widget. Slint's upstream FFmpeg and GStreamer examples show a native integration route; they do not establish drop-in parity with this player's HLS/MP4 behavior. [Upstream video examples and guidance](https://github.com/slint-ui/slint/discussions/5463).

A full-parity rewrite therefore needs a native media adapter, playback controls, decoder/runtime packaging, buffering/error handling, audio/video synchronization and teardown testing on Linux and Windows. Opening trailers externally is an optional simplification, but it changes the existing experience. Video is the first feature I would prototype before committing to a whole-UI cutover.

## Code that survives and code that changes

- Keep the core Rust modules and persisted game/settings/content formats. The new `game_controls::inspect` snapshot is already independent of Tauri and can serve a Slint model directly.
- Extract application orchestration currently inside `src-tauri/src/main.rs` into a UI-neutral service: running processes, task registration, job/control commands, metadata caches and mutation notifications. Reusing the core library is not the same as reusing the entire Tauri binary.
- Replace `api.js`/Tauri invoke/listen with Rust callbacks and queued model updates. Keep I/O and installs off the Slint event-loop thread; preserve progress and cancellation.
- Preserve the isolated `worker-install` command-line path or move it behind a shared worker executable. Do not lose background-process recovery while replacing the shell.
- Port JS behavior in `state.js`, `covers.js`, `dlc.js`, transfer helpers and `theme.js` into shared Rust/UI models. Browser localStorage, DOM visibility events, canvas palette extraction and intersection observers require native equivalents.
- Replace Svelte/HTML/CSS/Hls.js/Vite and desktop WebView packaging for the native target. Select and verify a non-Qt backend/renderer explicitly to retain the Qt-free requirement; leaving native backend selection implicit is insufficient.
- Continue bundling depotkit and its notices. Add only the native media/runtime dependencies actually selected. Replace the Tauri-specific installer shell and window integration while retaining manual/tag release triggers and clean builds.
- The existing browser preview/fallback is a separate concern: a native in-process Rust adapter does not automatically create a working browser application. A WASM target or a retained separate preview would need an explicit design.

Slint's framework licensing must be selected separately from application code licensing; its documented desktop community option requires attribution, with other licensing options available. This is an adoption detail to settle before packaging a replacement. [Official licensing overview](https://slint.dev/get-started).

## Recommended sequence

1. Build a separate native shell prototype using the official Material component library, a landscape library grid, one dialog, progress and confirmed-state switches.
2. Prove trailer playback, async artwork, keyboard/focus behavior, window controls, theme modes and packaging on Linux and Windows.
3. Extract the existing desktop application service and connect the Rust domain model without changing library/install formats.
4. Port Library, Discover, Settings, Transfers, and finally the complete game page; carry over regression scenarios for failure, cancellation, stale responses and game switching.
5. Replace the shipped frontend only after the same core tests and native UI acceptance pass. No full Slint rewrite was started by this UI cleanup.
