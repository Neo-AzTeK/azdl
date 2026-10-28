# Steam core migration

## Capability map and audit

| Existing dependency | Destination / disposition |
| --- | --- |
| `plugins.rs`, `plugin_store.rs`, API 4 loading and Qt initialization | Remove; no replacement extension host |
| Plugin catalogs, feeds, derived manifest URLs, store/favorite settings and caches | Remove; legacy identities handled only by versioned migration |
| Steam Store discovery, artwork and metadata | Rust `steam_discovery`, `steam_metadata`; public information only |
| Steam authentication and authorized content | Separate Rust access boundary; depotkit is not an authentication client |
| Depot selection and exact revision validation | `steam_content`, `steam_manifest`; preserve source-pinned fresh base selection and strict DLC compatibility |
| Downloads and native-engine loading | `steam_depot` plus one internal Qt-free depotkit adapter; preserve verification, cancellation and recovery |
| Receipts and DLC transactions | `steam_content`, `dlc`; preserve receipts, shared-depot ownership, rollback and installed IDs |
| Wine/Proton, native execution, storage, archive validation | Remain core capabilities |
| depotkit, libloading, compression libraries and licenses | Normal pinned internal runtime dependencies on both platforms |
| Plugin-supplied DRM, entitlement and ownership patches | Unnecessary for authorized content; do not move into Steam core |
| CLI, IPC, serialized state and Svelte plugin actions | Remove plugin surfaces; direct Steam install entry point |
| Existing persisted data and runtime resources | Versioned, idempotent, backed-up migration with scoped resource archival; never traverse game `Plugins` directories |

## Implementation sequence

1. Remove plugin API/UI/persistence and introduce direct Steam domain entry points.
2. Isolate and package the pinned Qt-free native engine on Linux and Windows.
3. Migrate persisted state without changing game IDs, files, launch configuration, prefixes or rollback records.
4. Wire authorized access, preserving exact manifests and file verification; no public Store entitlement inference.
5. Extend regression coverage, update user documentation, audit references, and run frontend/Rust checks.

## Authorization and service requirements

This deployment keeps the existing manifest relay and assumes authorization has already been supplied externally. `steam_access` obtains manifest bundles from `https://ryuu.badkiko.ru/v1/download/<appid>`; depotkit downloads the selected content from Steam CDNs. The relay is a third-party content source, not a Valve login or ownership API. HTTP 401/403, absent manifests and network errors are distinct failures.

No consumer Steam-account login or entitlement verification is implemented. Public Store metadata, manifest availability, keys and local receipts do not establish ownership. OpenID identifies an account but does not establish a depot download session; publisher ownership methods require suitable publisher credentials. A deployment requiring account login, license acquisition, depot keys, authenticated manifest request codes or CDN authorization must provide a real Steam transport for those operations. depotkit's public C request accepts manifests/keys and exposes no consumer login API. Credentials and keys are excluded from library receipts and IPC snapshots.

Reference: [Steamworks authentication and ownership](https://partner.steamgames.com/doc/features/auth).

## Migration behavior

`migration.rs` and its private `migration/legacy_catalog.rs` reader are the sole compatibility layer. Schema version 1 uses an OS file lock, synced temporary writes, original JSON backups, scoped resource renames, and a version marker published last. Interrupted migration can be repeated. It preserves installed game IDs and directories, artwork, arguments, runners, explicit environment, prefixes, content receipts, DLC ownership/rollback journals and non-package jobs/partials.

Steam source identities (including older entries lacking an App ID field) and pending installation origins normalize without changing installed IDs. Cached explicit archive DLC becomes inert per-game `content-sources` JSON before obsolete resources are archived. Invalid optional caches are retained with `migration-backup-v1/catalog-warnings.json`; they cannot prevent an otherwise usable library from migrating. Resource roots containing registered installations stay in place. Game `Game_Data/Plugins` trees are never traversed by launcher-resource cleanup. Plugin-supplied fix kits are archived; they are not required or applied by the authorized Steam path. Game-local binaries already installed remain untouched.

## Completed verification (2026-10-28)

| Check | Result |
| --- | --- |
| `cargo fmt --all --check` | Pass |
| `cargo test` | 216 passed; four opt-in tests ignored by default |
| `cargo clippy --all-targets -- -D warnings` | Pass, no warnings |
| Frontend Vitest | 141 passed across 13 files |
| Frontend production build | Pass; existing HLS chunk-size advisory remains |
| Clean pinned Linux engine build | Exact revision plus both tracked patches, two CTest cases passed, `ldd` has no missing or Qt libraries |
| Real Linux native-engine smoke | Opt-in test executed separately: bundled-only loading, exports, ABI version and invalid request behavior pass without extension resources |
| Compiled C mock | Request layout, UTF-8 paths, checksum flag, progress, cancellation, typed pause and rejection of an unsupported ABI pass |
| Windows MSVC source cross-check | Core/all tests and desktop type-check; Linux-host desktop build tooling warns about the unsupported GNU resource compiler, so this is not native Windows validation |
| Linux release packages | `.deb` and `.AppImage` built locally; engine and four license notices present; installed engine lookup/loading checked in extracted package layouts |
| Release workflow | Only manual and `v*` tag triggers; no restored Rust/npm caches or automatic Verify workflow; both hosts build the pinned engine and run frontend/workspace tests, formatting and strict Clippy before packaging |

Local Rust installer builds reused the current workspace toolchain/build outputs. The native-engine build started with empty generated source/build/runtime directories. The release workflow runs on fresh hosted runners without restoring build caches.

Regression coverage includes repeat/concurrent/linked-path migration, corrupt optional caches, preservation of game `Plugins`, pending origins, direct installation requests without extension directories, source-pinned base selection, DLC/base compatibility, shared depots, missing/exact manifests, encrypted and corrupt manifests/files, unsafe archive paths, interrupted transfer caches, pause/cancel/retry, transaction recovery, moves and removal. Paused base downloads retain their manifest SHA-256 revision journal and verified groups; incompatible revisions fail explicitly. Cancellation cleans only that transfer cache. File commit cannot be stopped halfway.

## Remaining acceptance

No live source/CDN game installation, real-account login, in-game DLC access or multiplayer session was run for this migration. Native Windows depotkit execution, NSIS/MSI installation and a complete Windows clean release remain unverified on this Linux host. Engine loading and filesystem tests do not establish game/service compatibility. The updated release workflow has not been dispatched or published from this task.

## Final reference audit

Production Rust/frontend/IPC contains no launcher extension host, registration interface, capability negotiation, API 4 loading, store/favorite state or source selector. `plugins.rs`, `plugin_store.rs` and the bundled-fix module are deleted. The unused direct `rand` dependency and its now-unused lockfile packages are removed; `libloading` remains only in the private depotkit adapter.

Every retained reference has a specific purpose:

- `migration.rs` includes old field/path/schema names and preservation fixtures; the private legacy catalog reader is used only for migration and test fixtures, with no public catalog-feed API or executable loading.
- Game `Plugins` mentions document and test preservation of game assets.
- `steamless.rs` handles the standalone utility's own `Plugins` dependency folder. It is pre-existing utility support, not launcher extension discovery, and is excluded from automatic Steam launch preparation.
- `cmd.rs` removes inherited Qt/GStreamer plugin environment variables from child processes to isolate AppImage/host runtimes; it loads no Qt or source extensions.
- Vite/Svelte `plugins` entries are normal build dependencies required by Svelte and Material 3.
- This audit, README and AGENTS describe the migration or the read-only C++/Qt reference project. The external `../arachnel` tree and workspace-wide historical directives were not rewritten.

Existing generic patch/overlay/DRM utility modules and journals remain independent legacy tools for explicit operations and rollback. No DRM removal, ownership-check patch, activation repair, source-supplied fix kit or emulator configuration is introduced into the built-in Steam installation/launch path.
