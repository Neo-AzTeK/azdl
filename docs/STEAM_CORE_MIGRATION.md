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

## Authentication requirements

The old provider relay supplies manifests and depot keys without establishing Steam account authorization. It cannot become the core access mechanism. Steam OpenID establishes identity, not a content download session. Publisher Web API ownership methods require publisher credentials and are not a universal consumer downloader API. depotkit's C request accepts app/depot IDs, manifests and keys; it does not log in, acquire licenses, obtain request codes or fetch authenticated manifests. An authenticated Steam transport must supply those capabilities; credentials and depot keys must not appear in IPC snapshots, logs or library receipts.

Official reference: https://partner.steamgames.com/doc/features/auth

## Acceptance ledger

Implementation and verification are recorded here as work proceeds. Offline tests do not establish live authorized installation or native Windows execution.
