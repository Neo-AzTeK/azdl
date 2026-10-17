# DLC discovery, planning, and installation

Implementation plan, researched on 2026-10-15. Discovery, exact provider revision matching, managed base receipts, additive staged depot DLC, and background desktop tasks have initial implementations. Live DLC download, commit, and removal into an isolated copy of a real base game succeeded on Linux on 2026-10-16. In-game access remains unverified.

The next milestone is a complete Steam content resolver that joins DLC identities, depot metadata, available artifacts, and installation receipts, including old-install migration and shared file ownership. A general plugin download-session ABI is a later extension, not a prerequisite for Steam depot DLC.

## Evidence and current gaps

Steam supports content included in base game files and content delivered separately. One DLC can require multiple depots for different platforms, architectures, or languages. The older DLC guide describes base-app-managed depots; the depot guide also documents the newer option to manage depots under a DLC App ID. The resolver must support both arrangements. See [Valve's DLC guide](https://partner.steamgames.com/doc/store/application/dlc?l=english) and [depot rules](https://partner.steamgames.com/doc/store/application/depots?l=english).

| Source | Useful information | Limits |
| --- | --- | --- |
| Steam Store `appdetails` | Base game's `dlc` IDs; DLC `type`, name, artwork, and `fullgame.appid` | Presentation and discovery; missing entries or failed requests do not establish that DLC does not exist. No download plan or ownership proof. |
| Steam appinfo | Observed `extended.listofdlc`, depot `dlcappid`, `config`, `manifests.<branch>.gid`, sizes, branch build IDs, and `depotfromapp` | Availability varies; preserve source, retrieval time, and completeness. A public manifest ID does not establish download access. |
| Binary depot manifest | Depot ID, manifest ID, file paths, sizes, hashes, chunks, and file flags | Describes one content snapshot; does not by itself identify the DLC, its parent game, or account ownership. Filenames may be encrypted. |
| Provider manifest bundle | The Lua parser currently extracts depot key references and requested manifest IDs, then pairs them with `.manifest` files | This provider format is not Steam appinfo. The existing parser does not establish DLC membership, target platform, branch, or build compatibility. |
| Local installation receipt | Exact committed files and content revisions | Authoritative only for operations recorded and verified by azdl. Old installations have incomplete provenance. |

SteamKit exposes product information through [PICS](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Steam/Handlers/SteamApps/SteamApps.cs). Its [manifest implementation](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Types/DepotManifest.cs) provides the binary fields above. [DepotDownloader](https://github.com/SteamRE/DepotDownloader/blob/master/DepotDownloader/ContentDownloader.cs) demonstrates branch manifest resolution and following `depotfromapp` references.

A live read on the research date returned these observations:

- [Elden Ring Store metadata](https://store.steampowered.com/api/appdetails?appids=1245620&l=english) listed DLC IDs `3655690`, `2778590`, and `2778580`.
- [Shadow of the Erdtree Store metadata](https://store.steampowered.com/api/appdetails?appids=2778580&l=english) identified itself as `type: dlc` with parent `1245620`.
- [The appinfo relay response](https://api.steamcmd.net/v1/info/1245620) listed nine DLC IDs in `extended.listofdlc`. Depot `2778580` had `dlcappid: 2778580` and a public manifest. Several other DLC depot entries lacked a public manifest. This is an observed third-party relay snapshot, not a direct authenticated PICS query or proof of downloadable content.

The relay is the open-source [steamcmd/api service](https://github.com/steamcmd/api), also used by the Arachnel reference implementation for size metadata. Treat it as a replaceable metadata adapter with bounded timeouts and cached responses. The original research did not download game content. A later isolated transport check downloaded one DLC depot from the sample provider.

Repository findings:

- `src/dlc.rs::available_for_game` explicitly returns no catalog DLC for `steamidra`; the desktop now lists Steam metadata separately through `get_game_dlc`.
- `src-tauri/src/main.rs` drops Store DLC fields from its presentation payload. Keep that payload small and introduce a dedicated core DLC metadata result.
- `src/catalog.rs::parse_addons` now requires a usable HTTPS artifact before enabling archive DLC. Catalog DLC matches route to their parent game's DLC section.
- `src/steam_depot.rs::parse_lua_manifests` preserves exact manifest IDs and rejects missing or ambiguous revisions. Base and DLC installs resolve only their selected depot manifests; absent unrelated DLC revisions no longer block a base install, while unknown depot IDs, missing required base depots, and missing selected revisions fail. Selected binary manifests require matching depot/revision metadata, safe paths, and regular file hashes before download; cryptographic signature validation remains open. The game-page result distinguishes a candidate depot, incomplete metadata, and a known unavailable depot path from parent or child appinfo. Missing parent identity and empty depot lists are unknown rather than proof of included content; an attempted install returns a specific no-matching-depot error.
- `install_steam_manifest_game_in` selects known non-DLC Windows/English depots, resolves shared references by manifest owner, downloads content App ID groups into separate verified staging directories, and publishes the combined base only after exact manifest verification. It writes a base revision receipt and excludes catalog DLC archives from automatic base add-ons. If receipt persistence fails, it removes the newly published base only when the bytes still match the selected manifests; otherwise it retains the directory and reports the rollback failure. Per-game scratch data is cleared after terminal outcomes and before retry. Offline resolver, publish, receipt-failure, and cleanup checks pass; multi-App base download, architecture and branch selection, and old-install migration still need broader acceptance.
- `download_steam_depots` accepts an explicit depot subset and destination. DLC metadata resolution now follows bounded `depotfromapp` manifest references with cycle detection, selects exact provider artifacts for each manifest owner, and groups native downloads by content App ID. Each group uses isolated staging and is verified before a final combined payload check. The shared-depot path has offline fixture coverage; provider and CDN acceptance remain open.
- The staged DLC transaction now reuses identical existing files and tracks whether azdl owns them, retaining managed files until the last DLC reference is removed. Different-content Steam depot overlays still require ordered backups and restoration before general support.

On 2026-10-16, an isolated Linux check used the installed `steamidra` provider and the matching public revision for Anomalous Coffee Machine 2 DLC `3875100`. The depotkit transport downloaded one 539,727,136 byte file into `/tmp`, the staged DLC transaction committed it, the recorded SHA-256 matched the file, and DLC removal deleted both the owned file and state. The base directory was intentionally empty with a synthetic receipt, so this proves the transport and additive transaction only. It does not prove compatibility with an actual base installation or game access.

A preexisting local installation of that game contains the DLC file, and its SHA-1 matches the current provider binary manifest's `sha_content` field. The earlier mismatch was caused by reading field 4 (`sha_filename`) instead of field 5 (`sha_content`); the [manifest schema](https://github.com/ValvePython/steam/blob/master/protobufs/content_manifest.proto) distinguishes them. Exact content verification can establish that the bytes are present, but it does not grant azdl ownership to remove a file created by an older install.

The new read-only legacy check was run against that existing game on 2026-10-16. It added zero game files and saved an observation after verifying the exact DLC depot manifest. The game page can show this as previously verified unmanaged content and offer a recheck.

The same existing game's required base depot passed a separate read-only verification against the current provider manifest. When DLC is missing and no base receipt exists, azdl now requires every matching base depot manifest and verifies every listed file before recording a `VerifiedLegacy` base receipt. Changed or missing files fail with counts and do not create a receipt.

The missing-DLC path was then exercised on Linux against a separate copy of the real base game with DLC `3875100` omitted. The copy passed strict legacy base verification, downloaded the DLC from Steam, committed one 539,727,136 byte file, and recorded its SHA-256 and managed ownership. A checksum comparison found no changed base files. DLC removal deleted that file while preserving the verified base receipt. The original installed game was untouched. This verifies filesystem delivery and removal with real base content; it does not establish in-game access or Windows acceptance.

After staged manifest verification was added, an isolated copy of that real DLC file passed `tests/live_dlc_manifest.rs` against depot `3875100`, revision `5309998554743987160`, on 2026-10-16. The temporary copy was removed afterward. This validates the new exact-content gate with real bytes, while grouped shared-depot transport and in-game access remain unverified.

The current grouped staging path then passed `tests/live_dlc_install.rs` on a fresh isolated copy of the real base game on 2026-10-16. It verified the legacy base, downloaded DLC `3875100` (539,727,136 bytes), verified both the per-group and combined staging payloads, committed one managed file, removed it, and returned the copied base directory to its original file names and sizes. The 2.1 GB temporary copy was deleted. This covers a single content App ID through the new grouping path; multi-App shared depot transport, Windows runtime, and in-game access remain unverified.

## Proposed core model

Keep these concepts independent:

- `DlcDescriptor`: stable identity, optional Steam DLC App ID, parent App ID, source plugin identity, display metadata, and metadata provenance.
- `DlcDelivery`: `Archive`, `SteamDepots`, `IncludedInBase`, `ProviderSession`, or `Unknown`. An empty depot response is `Unknown`, not evidence for `IncludedInBase`.
- `DlcAvailability`: resolved capability and a reason such as missing manifest, incomplete metadata, unsupported target, unavailable provider, or incompatible base revision. Access/entitlement evidence, when supplied by a provider, is recorded separately from file presence.
- `DlcInstallPlan`: immutable plan ID, game/source identity, target OS/architecture/language, branch, metadata snapshot, compatible base receipt revision, exact content-owner App IDs, depot IDs, manifest IDs, sizes, and proposed filesystem operations.
- `InstalledContentReceipt`: base game and DLC content references, exact depot revisions, shared file ownership, hashes, rollback references, and transaction state. Use paths relative to the game root so drive moves remain valid.

Manifest IDs are 64-bit values: keep them as `u64` in Rust and decimal strings in JSON sent to JavaScript. Retain depot credentials inside the backend/provider boundary and exclude them from UI payloads and logs.

## Ordered implementation

### 1. Discover DLC and report availability accurately

Add `src/steam_metadata.rs` for typed Store/appinfo adapters, parsing, caching, and provenance. Merge Store IDs, appinfo relationships, provider catalog entries, and installed receipts by stable identity. A missing Store page must not remove an installed DLC record. Preserve unknown states after network failures.

Start with the existing HTTPS transport and a replaceable appinfo relay adapter; keep room for direct PICS metadata supplied by a native provider. Cache display metadata longer than install metadata, deduplicate concurrent requests, limit response size and concurrency, and refresh plan inputs before installation. Query DLC appinfo where metadata indicates separately managed depots or base metadata leaves the mapping unresolved; bound recursive/shared-app lookups and reject cycles.

Return one typed core result through Tauri's `get_game_dlc`. Display known DLC even when it cannot currently be downloaded, with a short actionable reason. Preserve managed installs and verified legacy observations when Steam metadata fails; both now remain in the game-page result. Derive archive availability from a validated URL, filename, and transport rather than the existing boolean alone. Replace the hardcoded `steamidra` exclusion once its resolver can report explicit states. Index DLC titles and IDs for the existing catalog search and preserve their parent-game link.

Completion evidence: captured minimal metadata fixtures cover missing Store pages, Store/appinfo disagreement, separately managed depots, failed requests, and an archive entry without an artifact. Discovery works without downloading content.

### 2. Resolve exact depot plans

Add a pure planner in `src/steam_content.rs`; keep I/O and native engine calls outside it. Evolve `DepotInfo` into an inventory with exact manifest identity and validation results. Parse provider data without executing Lua. Verify manifest headers against the requested depot/manifest pair, report missing artifacts explicitly, and reject ambiguous fallback selection.

Resolve DLC membership using app metadata and explicit provider mappings. Never infer membership from similar titles, nearby IDs, or a depot filename. Filter by the game's target OS, architecture, and language; a Windows game using Proton still selects Windows depots on Linux. Resolve shared content references and include only the chosen DLC and its required content. Keep runtime redistributables distinct from DLC.

Pin the plan to the installed base revision and the provider's content snapshot. Current public appinfo may describe a newer build than a supplied manifest bundle. If compatibility cannot be established, report it and require a compatible source revision or a coordinated base update. Separately managed apps can have different build IDs; equality of their build IDs is not a compatibility rule.

Preserve actual depot mounting order where supplied. JSON key order and numeric depot order are not substitutes. If the metadata adapter loses ordering, reject conflicting overlays until an authoritative ordering is available. Preview download/disk estimates, using manifest totals when available and marking appinfo estimates as estimates.

Completion evidence: the same inputs produce the same exact plan; no unselected DLC/language depot is included; wrong-manifest fallback, shared-reference cycles, and unknown compatibility produce specific errors. Include a Steam App ID with no provider manifest to verify the existing clear-error requirement.

### 3. Persist content receipts and support staged depot DLC

Write base depot receipts on new Steam installs before offering managed depot DLC. Extract the archive DLC commit/recovery machinery into a common staged-content transaction. Use the existing depotkit engine to fetch the planned subset into an isolated staging directory, grouped by content-owner App ID. Validate staged file types, paths, hashes, Windows case collisions, and the base receipt revision before committing.

The first release supports additive files and identical shared files with reference tracking. Conflicting different-content paths get a clear unsupported/conflict result. This yields useful Steam DLC support without claiming compatibility with every depot overlay arrangement.

Use one game mutation lock for install, DLC change, base update, move, and uninstall; refuse content changes while the game is running. Keep a durable transaction journal with enough per-file state to recover after any individual commit step. Rollback removes only files the transaction created and preserves unrelated modifications. Cleanup removes temporary downloads after success while retaining receipts and rollback data required for recovery.

Completion evidence: interruption at each transaction stage, cancellation, disk-full failure, retry, removal, and drive movement preserve the base game. A real additive DLC completes on Linux and Windows and is checked inside the game separately from filesystem verification.

### 4. Keep DLC in search and the existing game page

Keep the existing DLC section on the installed game's page as the management surface. DLC matches already appear in catalog search; selecting one opens its parent game's page at the DLC section. An uninstalled parent uses the existing base-game installation flow. Do not create a separate DLC page, require a DLC selector during base installation, or add another management surface. Use the same planner for base installation and later DLC changes so the existing base path no longer downloads every depot in a bundle. Retain installed DLC selections across updates.

Keep DLC work in the existing background task and transfer drawer flow. Register its job before lengthy metadata or base-file checks so the user can see that work started, and persist enough state to recover after restart. Tauri remains a command/event adapter; Svelte displays core state and available actions. Add phase labels only if the existing status proves confusing in use.

Keep Install, Remove, and Recover actions in the existing `GamePageView.svelte` section. Managed and verified legacy DLC records remain visible after metadata lookup errors. If the selected depot plugin is removed, the rows remain visible while install and check actions become unavailable with a reason; the provider absence path has an offline test. Fix further concrete defects without a broader state or component rewrite. Reuse the current notifications service and activity indicator.

File installation does not establish that a game grants access to content; included content must not create a fake download or claim activation. Steam's game-facing APIs distinguish ownership and installation; their callbacks do not replace azdl's own job events. See [ISteamApps](https://partner.steamgames.com/doc/api/ISteamApps?l=english).

Completion evidence: searching by DLC title and ID opens the correct parent game; installed and uninstalled parents route correctly. Switching pages during installation, restarting after interruption, duplicate clicks, and provider removal retain consistent status. The existing transfer drawer and game page show the outcome without reloading the app.

### 5. Add ordered overlays, updates, and migration

Extend receipts to track the ordered owners of each path and backups of overridden content. Removing one DLC must restore the next applicable version and retain files still required by the base game or another DLC. Reject user-modified conflicts rather than overwriting them silently. Coordinate base updates with selected DLC and existing launcher patches through the same journal and ownership model.

An old game installed by today's downloader may already contain DLC files because all bundle depots were fetched. Mark it as an installation with unknown content provenance. Offer verification against exact known manifests and migrate only provable records; matching bytes alone must not grant deletion rights over imported/user-owned files. Do not delete unexplained files or claim DLC is missing merely because no DLC receipt exists. Import old additive archive receipts with backward-compatible defaults.

Completion evidence: shared files, different-content overlays, DLC A/B removal order, modified files, base update plus DLC update, and old installations are covered by lifecycle tests. Live testing includes both an additive DLC and a DLC that overlaps base content.

### 6. Extend provider capabilities when required

Move the hardcoded manifest-provider URL and downloader discovery behind the game's selected source adapter. Define structured metadata and artifact-resolution capabilities, then implement them for the existing Steam path. Bind engine selection to an explicit provider capability instead of selecting any installed library with a matching symbol. Keep archive delivery compatible.

A provider that needs its own download process can later implement the versioned session ABI described in [DLC.md](DLC.md), with staging, progress, cancellation, and artifact receipts. Implementing that ABI is independent of the Steam subset downloader. Built-in content and server-managed content remain explicit delivery states; this plan does not equate a metadata entry with an installable artifact.

## First deliverable and validation boundary

Implement steps 1 and 2 first: a populated DLC list plus a deterministic, inspectable plan explaining exactly what could be installed and why anything is unavailable. Then enable staged Steam downloads after receipts and transaction tests pass. Source adapter extraction for the existing Steam provider accompanies that work; the generic session ABI can wait.

Required automated coverage includes 64-bit ID serialization, stale metadata, missing manifests, multiple depots per DLC, target filtering, both depot-management layouts, shared depots, no-separate-file/unknown delivery, interrupted transactions, base/DLC version changes, and migration. Run Rust tests and clippy, frontend tests/build for UI changes, and Linux/Windows CI.

Research verified public metadata and inspected repository code. Linux checks verified provider artifact availability, exact staged content, and DLC download, additive commit, and removal against a copy of the sample base game. In-game access, multi-App shared-depot download, and Windows runtime acceptance remain implementation gates.
