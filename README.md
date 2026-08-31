# azdl — Open-Source Game Launcher & Runtime Orchestrator Blueprint

A complete, production-ready architectural guide for building a modular, cross-platform game launcher and runtime orchestrator (similar to **Arachnel** or **Hydra**) by integrating battle-tested **Open Source Software (OSS)** tools and community patch engines.

---

## 1. System Architecture

Instead of reinventing custom download protocols or low-level DRM unpackers from scratch, **azdl** acts as a unified **controller and orchestrator** that binds specialized OSS components into an automated pipeline:

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                               Frontend & User Interface                                │
│       (Tauri / Rust + Svelte/React  OR  Qt 6 / QML  OR  Electron / Next.js)             │
└───────────────────────────────────────────┬─────────────────────────────────────────────┘
                                            │ IPC / CLI Controller
┌───────────────────────────────────────────▼─────────────────────────────────────────────┐
│                                 azdl Core Orchestrator                                  │
├──────────────────┬──────────────────┬──────────────────┬─────────────────┬──────────────┤
│ Catalog & Feeds  │ Download Manager │ Patching Engine  │ Runtime Manager │ Steam Export │
└────────┬─────────┴────────┬─────────┴────────┬─────────┴────────┬────────┴──────┬───────┘
         │                  │                  │                  │               │
         ▼                  ▼                  ▼                  ▼               ▼
┌─────────────────┐ ┌────────────────┐ ┌────────────────┐ ┌────────────────┐ ┌─────────────┐
│ Steam Store API │ │DepotDownloader │ │ Steamless CLI  │ │  Proton-GE /   │ │Shortcuts.vdf│
│  SteamGridDB    │ │ (SteamRE / .NET│ │(atom0s / Mono) │ │   Wine Prefix  │ │  & Artwork  │
│  JSON Feeds     │ │   Depot Engine)│ │  Goldberg Emu  │ │ VC++ / DirectX │ │ Steam Deck  │
│                 │ │                │ │Online-Fix Shim │ │  Dependencies  │ │ Integration │
└─────────────────┘ └────────────────┘ └────────────────┘ └────────────────┘ └─────────────┘
```

---

## 2. Core OSS Building Blocks & Upstream Repositories

| Subsystem | Primary OSS Project | Upstream Repository | Role in Pipeline |
| :--- | :--- | :--- | :--- |
| **Depot Downloader** | **DepotDownloader** | [`SteamRE/DepotDownloader`](https://github.com/SteamRE/DepotDownloader) | CLI/library to download Steam manifests, depots, and files directly from Valve CDNs using account credentials or manifest/depot keys. |
| **Steam Client Lib** | **SteamKit2** | [`SteamRE/SteamKit`](https://github.com/SteamRE/SteamKit) | .NET library to interact with Steam network, PICS (Product Info), and manifest servers. |
| **DRM Unpacker** | **Steamless** | [`atom0s/Steamless`](https://github.com/atom0s/Steamless) | Unpacks and strips SteamStub DRM (32-bit and 64-bit) from Portable Executable (`.exe`) files. |
| **Steam Emulator** | **Goldberg Emulator** | [`Mr_Goldberg/goldberg_emulator`](https://gitlab.com/Mr_Goldberg/goldberg_emulator) | Drop-in `steam_api.dll` / `steam_api64.dll` replacement providing offline licensing, DLC unlocking (`DLC.txt`), and LAN multiplayer. |
| **Multiplayer Fixes** | **Online-Fix / SteamFix** | `OnlineFix64.dll` / `SteamFix64.dll` | Shims redirecting Steamworks P2P/lobby traffic over Steam's `SpaceWar` (AppID `480`). |
| **Compatibility Layer**| **Proton-GE** | [`GloriousEggroll/proton-ge-custom`](https://github.com/GloriousEggroll/proton-ge-custom) | High-performance Wine/DXVK/VKD3D distribution optimized for Linux gaming and Steam Deck. |
| **Steam Deck Sync** | **Steam Shortcuts VDF** | Binary VDF Parser | Injects non-Steam game entries directly into `shortcuts.vdf` with custom grid capsules/heroes. |
| **Artwork & Covers** | **SteamGridDB API** | [`SteamGridDB`](https://www.steamgriddb.com/api/v2) | Fetches vertical grid covers, hero banners, logos, and icons. |

---

## 3. End-to-End Orchestration Pipeline

```
  [1. Catalog Select] ──> [2. DepotDownloader] ──> [3. Steamless] ──> [4. Goldberg / Fix]
                                                                             │
  [6. Launch / Steam Deck] <── [5. Proton-GE / Prefix Setup] <───────────────┘
```

---

### Step 1: Catalog & Metadata Aggregation
1. **Metadata Resolution:** Query the official Steam Store Web API for rich metadata:
   ```text
   GET https://store.steampowered.com/api/appdetails?appids=<APP_ID>&cc=us&l=en
   ```
2. **Cover Art:** Use Steam CDN image endpoints or query SteamGridDB:
   ```text
   https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/<APP_ID>/library_600x900.jpg
   https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/<APP_ID>/header.jpg
   ```

---

### Step 2: Content Downloading (`SteamRE/DepotDownloader`)
Execute `DepotDownloader` as a background process or embed its core logic:

```bash
# Standard depot download using manifest and depot key:
dotnet DepotDownloader.dll \
  -app <APP_ID> \
  -depot <DEPOT_ID> \
  -manifest <MANIFEST_ID> \
  -depotkey <256_BIT_HEX_DEPOT_KEY> \
  -dir "/path/to/game/install" \
  -max-servers 16 \
  -max-downloads 8 \
  -validate
```

* **Features to leverage:**
  - Fast multi-server parallel chunk downloading over HTTP/HTTPS.
  - Automatic AES-256 decryption, LZMA/Zstd decompression, and SHA-1 chunk validation.
  - `-filelist` / regex filtering to download only essential depots (e.g., skip unnecessary languages).
  - Resuming interrupted downloads with `-validate`.

---

### Step 3: SteamStub DRM Stripping (`atom0s/Steamless`)
1. **Detection:** Scan all `.exe` files in the downloaded directory for PE section headers. If any PE section is named `.bind`, SteamStub DRM is active.
2. **Execution:**
   - **On Windows:**
     ```bash
     Steamless.CLI.exe --quiet "/path/to/game.exe"
     ```
   - **On Linux (via Wine Mono):**
     ```bash
     WINEDEBUG=-all wine Steamless.CLI.exe --quiet "Z:/path/to/game.exe"
     ```
3. **Atomic Swap:** Back up `game.exe` to `game.exe.steamstub.bak` and rename `game.exe.unpacked.exe` to `game.exe`.

---

### Step 4: Steamworks Emulation & DLC Unlocking (`Goldberg Emulator`)
1. **Locate Steam API Binaries:** Search the game folder for `steam_api.dll` (32-bit) or `steam_api64.dll` (64-bit).
2. **Backup & Replace:**
   - Back up original `steam_api64.dll` to `steam_api64.dll.orig`.
   - Copy Goldberg's `steam_api64.dll` into the executable directory.
3. **Configuration Structure:** Create `steam_settings/` adjacent to the executable:
   ```text
   <game_root>/
   ├── game.exe
   ├── steam_api64.dll          # Goldberg emulator DLL
   └── steam_settings/
       ├── steam_appid.txt      # Contains target AppID (e.g., 1091500)
       ├── DLC.txt              # DLC list: <dlc_id>=<dlc_name>
       ├── force_account_name.txt # Custom player username
       └── configs.user.ini     # Optional network / overlay settings
   ```

---

### Step 5: Multiplayer Overlays & Online-Fix (Optional)
If multiplayer via Steam's SpaceWar (`AppID 480`) is desired:
1. Copy `OnlineFix64.dll` (or `SteamFix64.dll`) and `winmm.dll` into the game directory.
2. Configure `OnlineFix.ini` with `FakeAppId=480`.
3. **Wine DLL Overrides (Linux):** When launching under Wine/Proton, set:
   ```bash
   WINEDLLOVERRIDES="onlinefix64=n,b;steamfix64=n,b;winmm=n,b;steam_api64=n,b"
   ```

---

### Step 6: Proton-GE & Wine Runtime Orchestration
1. **Autonomous Proton-GE Downloader:**
   - Query GitHub Releases API for `GloriousEggroll/proton-ge-custom/releases/latest`.
   - Download and extract `GE-Proton<VERSION>.tar.gz` to `~/.local/share/azdl/runners/`.
2. **Prefix Initialization & Launch Command:**
   ```bash
   export STEAM_COMPAT_CLIENT_INSTALL_PATH="$HOME/.steam/root"
   export STEAM_COMPAT_DATA_PATH="$HOME/.local/share/azdl/prefixes/<APP_ID>"
   export WINEDLLOVERRIDES="onlinefix64=n,b;winmm=n,b"

   # Launch through Proton runner:
   ~/.local/share/azdl/runners/GE-Proton9-25/proton run "/path/to/game/game.exe"
   ```
3. **Runtime Dependencies (VC++ & DirectX):**
   - Automatically download `vc_redist.x64.exe` (`https://aka.ms/vs/17/release/vc_redist.x64.exe`).
   - Run silent installation in the game's prefix:
     ```bash
     WINEPREFIX="$STEAM_COMPAT_DATA_PATH/pfx" wine vc_redist.x64.exe /install /quiet /norestart
     ```

---

### Step 7: Native Steam Client & Steam Deck Export (`shortcuts.vdf`)
To allow the game to appear seamlessly in Steam Deck's **Game Mode** or Big Picture:
1. Locate active user configuration: `~/.steam/root/userdata/<USER_ID>/config/shortcuts.vdf`.
2. Parse the binary KeyValues VDF format and append a new shortcut entry:
   ```json
   {
     "AppName": "Cyberpunk 2077",
     "Exe": "\"/usr/bin/azdl\"",
     "StartDir": "\"/path/to/game\"",
     "LaunchOptions": "launch --app 1091500",
     "tags": ["azdl", "Installed"]
   }
   ```
3. Compute the shortcut AppID:
   $$\text{ShortcutAppID} = (\text{CRC32}(\text{"\"<Exe>\"\"<AppName>\""}) \mid \text{0x80000000}) \ll 32$$
4. Drop cover art directly into `~/.steam/root/userdata/<USER_ID>/config/grid/`:
   - Vertical capsule: `<AppID>p.jpg` (600x900)
   - Hero banner: `<AppID>_hero.jpg` (1920x620)
   - Logo: `<AppID>_logo.png` (transparent PNG)

---

## 4. Automation Wrapper Example (Python CLI Controller)

A minimal orchestrator illustrating how azdl chains the OSS tools:

```python
import subprocess
import os
import shutil
from pathlib import Path

class AzdlOrchestrator:
    def __init__(self, tools_dir: Path, install_dir: Path):
        self.tools_dir = tools_dir
        self.install_dir = install_dir

    def download_depot(self, app_id: int, depot_id: int, manifest_id: int, depot_key: str):
        """Step 1: Download chunks using DepotDownloader."""
        cmd = [
            "dotnet", str(self.tools_dir / "DepotDownloader.dll"),
            "-app", str(app_id),
            "-depot", str(depot_id),
            "-manifest", str(manifest_id),
            "-depotkey", depot_key,
            "-dir", str(self.install_dir),
            "-validate"
        ]
        subprocess.run(cmd, check=True)

    def unpack_steamstub(self, exe_path: Path):
        """Step 2: Strip SteamStub DRM using Steamless CLI."""
        steamless_cli = self.tools_dir / "Steamless.CLI.exe"
        cmd = ["wine", str(steamless_cli), "--quiet", str(exe_path)]
        subprocess.run(cmd, check=True, env={**os.environ, "WINEDEBUG": "-all"})

        unpacked = exe_path.with_name(f"{exe_path.stem}.unpacked.exe")
        if unpacked.exists():
            shutil.move(exe_path, exe_path.with_suffix(".exe.steamstub.bak"))
            shutil.move(unpacked, exe_path)

    def apply_goldberg(self, game_dir: Path, app_id: int, dlc_dict: dict):
        """Step 3: Inject Goldberg Steamworks Emulator and DLC configuration."""
        goldberg_dll = self.tools_dir / "goldberg" / "steam_api64.dll"
        target_dll = game_dir / "steam_api64.dll"
        
        if target_dll.exists():
            shutil.copy(target_dll, game_dir / "steam_api64.dll.orig")
        shutil.copy(goldberg_dll, target_dll)

        # Write settings
        settings_dir = game_dir / "steam_settings"
        settings_dir.mkdir(exist_ok=True)
        (settings_dir / "steam_appid.txt").write_text(str(app_id))
        
        if dlc_dict:
            dlc_content = "\n".join(f"{k}={v}" for k, v in dlc_dict.items())
            (settings_dir / "DLC.txt").write_text(dlc_content)

    def run_game(self, proton_dir: Path, prefix_dir: Path, exe_path: Path):
        """Step 4: Launch via Proton-GE."""
        env = os.environ.copy()
        env["STEAM_COMPAT_CLIENT_INSTALL_PATH"] = str(Path.home() / ".steam/root")
        env["STEAM_COMPAT_DATA_PATH"] = str(prefix_dir)
        env["WINEDLLOVERRIDES"] = "onlinefix64=n,b;winmm=n,b;steam_api64=n,b"
        
        subprocess.run([str(proton_dir / "proton"), "run", str(exe_path)], env=env)
```

---

## 5. Recommended Modern Tech Stack for Building azdl

| Layer | Recommended Choice | Rationale |
| :--- | :--- | :--- |
| **GUI Framework** | **Tauri (Rust + React / Svelte)** | Extremely lightweight (~15 MB RAM vs ~300 MB Electron), native speed, secure Rust backend for process and file manipulation. |
| **Alternative GUI** | **Qt 6 (C++20 / QML)** | Native cross-platform performance (used by Arachnel), hardware accelerated UI. |
| **Backend Core** | **Rust or C# (.NET 8/9)** | Direct interop with `SteamKit2` / `DepotDownloader` libraries without spawning sub-processes. |
| **Database & Cache** | **SQLite + sled / RocksDB** | Fast local library state, cover art caching, and metadata indexing. |
| **CLI Mode** | **Clap (Rust) or Click (Python)** | Enables headless downloading and Steam Deck terminal automation scripts. |

---

## 6. Implementation Roadmap

- [ ] **Phase 1 (Downloader Core):** Integrate `DepotDownloader` CLI or wrap `SteamKit2` in a background worker service with progress reporting.
- [ ] **Phase 2 (DRM & Patch Pipeline):** Implement the PE `.bind` scanner, silent `Steamless.CLI.exe` execution, and Goldberg emulator injector.
- [ ] **Phase 3 (Runtime & Wine/Proton):** Add Proton-GE auto-updater, prefix bootstrap, and VC++ redist automated installer.
- [ ] **Phase 4 (Steam Deck Integration):** Build the `shortcuts.vdf` binary writer and automated artwork downloader.
- [ ] **Phase 5 (User Interface):** Connect the frontend catalog, download queue manager, game settings, and launch controls.
