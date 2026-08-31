# azdl — Steam Depot & Manifest Downloader Engine

A technical guide and specification for building a standalone, high-performance Steam depot and manifest downloader.

---

## 1. Overview & Architecture

Steam games are distributed as encrypted, compressed ~1 MB chunks over Valve's Content Delivery Network (CDN). To recreate the download engine, you need to implement a 5-stage pipeline:

```
┌───────────────────────────┐
│ 1. Manifest & Key Source  │ ──> Fetch Depot Manifest (.manifest) & 256-bit AES Depot Key
└─────────────┬─────────────┘
              ▼
┌───────────────────────────┐
│ 2. Manifest Parser        │ ──> Extract file trees, chunk SHA-1 hashes, byte offsets & sizes
└─────────────┬─────────────┘
              ▼
┌───────────────────────────┐
│ 3. CDN Chunk Downloader   │ ──> Async HTTP pool pulling /depot/{depotId}/chunk/{chunkId}
└─────────────┬─────────────┘
              ▼
┌───────────────────────────┐
│ 4. Decrypt, Decompress &  │ ──> AES-256-ECB/CBC -> LZMA/Zstd decompress -> SHA-1 verify
│    Assemble Files         │ ──> Write directly to disk at target file offsets
└─────────────┬─────────────┘
              ▼
┌───────────────────────────┐
│ 5. DRM Stripping & Config │ ──> Steamless (SteamStub unpack) + Goldberg Steam Emulator
└───────────────────────────┘
```

---

## 2. Core Concepts & Data Formats

### 2.1. AppIDs, DepotIDs & Manifests
* **AppID:** Unique identifier for a game on Steam (e.g. `480` for SpaceWar, `1091500` for Cyberpunk 2077).
* **DepotID:** Each game consists of multiple depots (executable binaries, OS-specific assets, language audio packs, DLCs).
* **Manifest GID (ID):** A 64-bit integer representing a specific build version of a depot.
* **Depot Key:** A 256-bit (32 bytes / 64 hex characters) symmetric AES key specific to each DepotID.

### 2.2. The Depot Manifest (Protobuf Binary)
The manifest file (`<depot_id>_<manifest_gid>.manifest`) is a serialized Protocol Buffer structure (`DepotManifestPayload`).

```protobuf
message DepotManifestPayload {
  repeated DepotFileMapping files = 1;
  uint32 depot_id = 2;
  uint64 total_uncompressed_size = 3;
  uint64 total_compressed_size = 4;
  uint64 manifest_gid = 5;
}

message DepotFileMapping {
  string filename = 1;          // Relative path (e.g., "bin/game.exe")
  uint64 size = 2;              // Full uncompressed file size
  uint32 flags = 3;             // Attributes (e.g. executable, read-only)
  repeated DepotChunkInfo chunks = 4;
}

message DepotChunkInfo {
  bytes sha = 1;                // 20-byte SHA-1 hash of the uncompressed chunk
  uint32 crc = 2;               // Checksum of compressed chunk
  uint64 offset = 3;            // Byte offset where this chunk starts in the file
  uint32 cb_original = 4;       // Uncompressed chunk size (usually ~1 MB)
  uint32 cb_compressed = 5;     // Compressed chunk size on the CDN
}
```

---

## 3. The Download & Assembly Pipeline

### Step 1: Manifest & Depot Key Acquisition
Steam's official store verifies user licenses before providing manifests and depot keys. To download without an active Steam client session:
1. **Manifest Relay / Public Indexers:** Fetch decrypted manifest files and depot keys from an indexer (e.g. Ryuu, ManifestHub, or local cached manifests).
2. **Local Depot Key Configuration:** Store depot keys mapped by `DepotID` (e.g. `{"1091501": "a1b2c3d4e5f6..."}`).

---

### Step 2: Content Server Resolution
Steam distributes chunks across Valve edge servers and third-party CDNs (Akamai, Fastly, Cloudflare).

* **Direct CDN URL Format:**
  ```text
  http://content<N>.steampowered.com/depot/<depot_id>/chunk/<chunk_sha1_hex>
  ```
  *(where `<chunk_sha1_hex>` is the 40-character lowercase hexadecimal string of `DepotChunkInfo.sha`)*

* **Content Server List:**
  You can query Valve's Content Server Directory service or use any known Steam CDN edge host:
  - `content1.steampowered.com`
  - `content2.steampowered.com`
  - `edge.steam-dns.top.comcast.net`

---

### Step 3: Chunk Processing (Decrypt -> Decompress -> Verify)

For each downloaded chunk payload:

1. **AES-256 Decryption:**
   - **Algorithm:** AES-256 in ECB mode (or CBC mode with IV derived from the first 16 bytes of the depot key / ECB block).
   - **Key:** The 32-byte depot key.
   - **Input:** Raw downloaded chunk bytes.
2. **Decompression:**
   - Decompress the decrypted payload.
   - Steam chunks use either **LZMA** or **Zstandard (Zstd)** depending on the depot version.
3. **Integrity Check:**
   - Compute the **SHA-1** hash of the uncompressed data.
   - Assert: `sha1(uncompressed_bytes) == chunk.sha`.

---

### Step 4: Sparse File Assembly

1. Create target directories and pre-allocate target files according to `DepotFileMapping.filename` and `DepotFileMapping.size`.
2. Write each verified chunk directly into the file at its specified byte offset:
   ```
   file.seek(chunk.offset)
   file.write(uncompressed_chunk_bytes)
   ```
3. Set file permission flags (e.g. `+x` for executables on Linux).

---

### Step 5: Post-Download DRM Stripping & Emulation

Because the downloaded files are pristine, original Steam depot files, they require standard DRM emulation to run standalone:

1. **SteamStub DRM Stripping:**
   - Check executable headers for the `.bind` PE section.
   - Run `Steamless.CLI.exe` (via Wine on Linux or natively on Windows) to unpack the binary.
2. **Steamworks API Emulation (Goldberg Emulator):**
   - Replace or place `steam_api64.dll` / `steam_api.dll` with Goldberg Emulator.
   - Create `steam_settings/steam_appid.txt` containing the target `AppID`.
   - Add `steam_settings/DLC.txt` with `<dlc_id>=<dlc_name>` to unlock add-on content.
3. **Multiplayer (Online-Fix / SteamFix):**
   - Inject SpaceWar (`AppID 480`) overlay DLLs (`OnlineFix64.dll`, `SteamFix64.dll`) and configure Wine DLL overrides (`WINEDLLOVERRIDES="onlinefix64=n,b;winmm=n,b"`) on Linux.

---

## 4. Recommended Technology Stack

| Language | Networking | Crypto & Decompress | Protobuf | Recommended Concurrency |
| :--- | :--- | :--- | :--- | :--- |
| **Rust** | `reqwest` / `hyper` | `aes`, `lzma-rs`, `zstd`, `sha1` | `prost` | `tokio` (50–100 parallel chunk workers) |
| **Python** | `httpx` / `aiohttp` | `cryptography`, `zstandard`, `lzma`, `hashlib` | `protobuf` | `asyncio` task pool |
| **C++20** | `libcurl` / `boost::asio` | `OpenSSL` (EVP), `liblzma`, `libzstd` | `google::protobuf` | Thread pool + async I/O (`io_uring`) |
| **C# / .NET** | `HttpClient` | `System.Security.Cryptography`, `SharpCompress` | `SteamKit2` | `Task.WhenAll` / `Channel<T>` |

---

## 5. Minimal Python Prototype Outline

```python
import asyncio
import hashlib
import httpx
import lzma
import zstandard as zstd
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

async def download_and_process_chunk(client, depot_id: int, chunk_sha: str, depot_key: bytes):
    url = f"http://content1.steampowered.com/depot/{depot_id}/chunk/{chunk_sha}"
    response = await client.get(url)
    encrypted_data = response.content

    # 1. AES-256 Decryption
    cipher = Cipher(algorithms.AES(depot_key), modes.ECB())
    decryptor = cipher.decryptor()
    decrypted = decryptor.update(encrypted_data) + decryptor.finalize()

    # 2. Decompression (Zstd or LZMA)
    try:
        decompressed = zstd.ZstdDecompressor().decompress(decrypted)
    except Exception:
        decompressed = lzma.decompress(decrypted)

    # 3. SHA-1 Integrity Verification
    assert hashlib.sha1(decompressed).hexdigest() == chunk_sha.lower()

    return decompressed
```

---

## 6. Implementation Roadmap

- [ ] **Phase 1:** Protobuf manifest schema compiler & parser (`.manifest` -> file/chunk mapping).
- [ ] **Phase 2:** Multi-threaded HTTP chunk download client with connection pooling and retry logic.
- [ ] **Phase 3:** AES-256 decryption, LZMA/Zstd decompression, and SHA-1 verification pipeline.
- [ ] **Phase 4:** Chunk assembler with random-access file writing and progress reporting.
- [ ] **Phase 5:** Integration with Goldberg Steam Emulator and automated Steamless execution.
