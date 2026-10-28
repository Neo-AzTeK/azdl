# Steam DLC implementation and remaining acceptance

The current contract is [DLC.md](DLC.md). The launcher has one built-in Rust Steam content pipeline and no extension-session ABI.

| Concern | Implemented responsibility | Remaining acceptance |
| --- | --- | --- |
| Identity and display | Store/appinfo plus saved receipts/observations; metadata never establishes entitlement | Live search and game-page metadata outages |
| Content access | Existing manifest source under externally supplied authorization; separate HTTP authorization/unavailable/network failures | Real authorized account/source combination; a separate Steam login transport if deployment requires it |
| Revision selection | Source-pinned fresh base receipts; strict compatible base/DLC revisions and exact manifest artifacts | Adding DLC to real historical base revisions |
| Shared depots | Resolve ownership chains, content App grouping, cycle/missing-manifest rejection and shared files | Live multi-App download and removal |
| Transactions | Verified staging, atomic publication, file hashes, shared ownership and reversible overlays | Process termination at commit boundaries on both hosts |
| Existing data | Locked, versioned migration; IDs, files, receipts, journals, jobs and inert archive-source records retained | Representative copied libraries and multi-drive layouts |
| Native transport | Pinned Qt-free depotkit on both platforms, one internal Rust adapter, progress/cancellation and chunk resume | Native Windows installer/runtime and throttled live downloads |

Preserve exact IDs, manifest hashes and base revision comparisons when extending this model. No public metadata result or legacy content record permits an entitlement bypass. Keep transport calls behind Rust services and Tauri worker boundaries; Svelte renders state and dispatches actions.

Use the opt-in live tests described in DLC.md only with authorized content and isolated base copies. Offline tests and ABI loading do not establish playable DLC or multiplayer compatibility.
