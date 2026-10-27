use crate::Result;
use crate::multiplayer::pe_image_bits;
use std::fs;
use std::path::{Path, PathBuf};

/// Scans for and repairs EA Frostbite activation license verification checks
/// by flipping conditional jump (0x75 / jnz) to unconditional jump (0xEB / jmp)
/// across the 3 standard bytecode patterns.
pub fn heal_ea_activation(game_dir: &Path) -> Result<bool> {
    if !game_dir.is_dir() {
        return Ok(false);
    }

    let candidates = [
        game_dir.join("Core").join("Activation64.dll"),
        game_dir.join("Activation64.dll"),
        game_dir.join("Core").join("Activation.dll"),
        game_dir.join("Activation.dll"),
    ];

    let mut dll_paths = Vec::new();
    for candidate in &candidates {
        if fs::symlink_metadata(candidate).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
        {
            dll_paths.push(candidate.clone());
        }
    }

    // Fallback: search up to 200 entries within 2 directory levels
    if dll_paths.is_empty() {
        find_activation_dlls(game_dir, 0, 2, &mut dll_paths)?;
    }

    let mut patched_any = false;
    for dll_path in dll_paths {
        let Ok(mut data) = fs::read(&dll_path) else {
            continue;
        };
        let mut modified = false;

        // Pattern 1: test al, al; jnz +0x26; mov dword ptr [r12], 0x186aa
        // Hex: 84 c0 75 26 41 c7 04 24 aa 86 01 00
        const P1: &[u8] = &[
            0x84, 0xc0, 0x75, 0x26, 0x41, 0xc7, 0x04, 0x24, 0xaa, 0x86, 0x01, 0x00,
        ];
        if let Some(idx) = find_subsequence(&data, P1) {
            data[idx + 2] = 0xeb;
            modified = true;
        }

        // Pattern 2: test eax, eax; jnz +0x0a; mov ebx, 0x186aa
        // Hex: 85 c0 75 0a bb aa 86 01 00
        const P2: &[u8] = &[0x85, 0xc0, 0x75, 0x0a, 0xbb, 0xaa, 0x86, 0x01, 0x00];
        if let Some(idx) = find_subsequence(&data, P2) {
            data[idx + 2] = 0xeb;
            modified = true;
        }

        // Pattern 3: test edi, edi; jnz +0x07; mov ebx, 0x186ab; jmp +0x05; mov ebx, 0x186aa
        // Hex: 85 ff 75 07 bb ab 86 01 00 eb 05 bb aa 86 01 00
        const P3: &[u8] = &[
            0x85, 0xff, 0x75, 0x07, 0xbb, 0xab, 0x86, 0x01, 0x00, 0xeb, 0x05, 0xbb, 0xaa, 0x86,
            0x01, 0x00,
        ];
        if let Some(idx) = find_subsequence(&data, P3) {
            data[idx + 2] = 0xeb;
            modified = true;
        }

        if modified {
            let backup_path = dll_path.with_extension("dll.ea.bak");
            if !backup_path.exists() {
                let _ = fs::copy(&dll_path, &backup_path);
            }
            fs::write(&dll_path, &data)?;
            patched_any = true;
        }
    }

    Ok(patched_any)
}

/// Rolls back EA activation patch by restoring any .ea.bak files.
pub fn rollback_ea_activation(game_dir: &Path) -> Result<bool> {
    if !game_dir.is_dir() {
        return Ok(false);
    }
    let mut restored = false;
    let candidates = [
        game_dir.join("Core").join("Activation64.dll"),
        game_dir.join("Activation64.dll"),
        game_dir.join("Core").join("Activation.dll"),
        game_dir.join("Activation.dll"),
    ];
    for candidate in &candidates {
        let backup = candidate.with_extension("dll.ea.bak");
        if backup.is_file() {
            fs::copy(&backup, candidate)?;
            let _ = fs::remove_file(&backup);
            restored = true;
        }
    }
    Ok(restored)
}

/// Check if an EA activation backup file exists.
pub fn has_ea_backup(game_dir: &Path) -> bool {
    if !game_dir.is_dir() {
        return false;
    }
    let candidates = [
        game_dir.join("Core").join("Activation64.dll.ea.bak"),
        game_dir.join("Activation64.dll.ea.bak"),
        game_dir.join("Core").join("Activation.dll.ea.bak"),
        game_dir.join("Activation.dll.ea.bak"),
    ];
    candidates.iter().any(|c| c.is_file())
}

/// Check if an EA activation DLL target exists in the game directory.
pub fn has_ea_target(game_dir: &Path) -> bool {
    if !game_dir.is_dir() {
        return false;
    }
    let candidates = [
        game_dir.join("Core").join("Activation64.dll"),
        game_dir.join("Activation64.dll"),
        game_dir.join("Core").join("Activation.dll"),
        game_dir.join("Activation.dll"),
    ];
    candidates.iter().any(|c| c.is_file())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn find_activation_dlls(
    current: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    if depth > max_depth || out.len() >= 10 {
        return Ok(());
    }
    let Ok(entries) = fs::read_dir(current) else {
        return Ok(());
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            find_activation_dlls(&path, depth + 1, max_depth, out)?;
        } else if meta.is_file() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name == "activation64.dll" || name == "activation.dll" {
                out.push(path);
            }
        }
    }
    Ok(())
}

/// Ensures required Steam API DLLs exist in the executable's directory.
/// Resolves issues where 64-bit Unity/Mono games call steam_api.dll while shipping steam_api64.dll.
pub fn ensure_steam_api_dll(install_dir: &Path, executable_path: &Path) -> Result<Option<String>> {
    if !install_dir.is_dir() {
        return Ok(None);
    }

    let mut check_paths = Vec::new();
    if executable_path.is_file() {
        check_paths.push(executable_path.to_path_buf());
    }
    for c in crate::steamless::collect_exe_candidates(install_dir) {
        if !check_paths.contains(&c) {
            check_paths.push(c);
        }
    }
    if check_paths.is_empty() {
        return Ok(None);
    }

    let mut provided = Vec::new();
    let mut source_used = None;

    for exe in check_paths {
        let Some(bits) = pe_image_bits(&exe) else {
            continue;
        };
        let Some(exe_dir) = exe.parent() else {
            continue;
        };
        let targets = if bits == 64 {
            vec!["steam_api64.dll", "steam_api.dll"]
        } else {
            vec!["steam_api.dll"]
        };
        for target_name in targets {
            let target_path = exe_dir.join(target_name);
            if target_path.exists() {
                continue;
            }
            if let Some(source) = find_steam_api_dll(install_dir, bits, 4000)?
                && source != target_path
            {
                fs::copy(&source, &target_path)?;
                provided.push(target_name);
                source_used = Some(source);
            }
        }
    }

    if provided.is_empty() {
        Ok(None)
    } else {
        let src_str = source_used
            .map(|s| s.display().to_string())
            .unwrap_or_else(|| "game install".into());
        Ok(Some(format!(
            "steam_api repair: provided {} next to executable (source: {})",
            provided.join(", "),
            src_str
        )))
    }
}

fn find_steam_api_dll(root: &Path, bits: u8, max_entries: usize) -> Result<Option<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0;
    let mut best = None;
    let preferred_name = if bits == 64 {
        "steam_api64.dll"
    } else {
        "steam_api.dll"
    };

    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            seen += 1;
            if seen > max_entries {
                return Ok(best);
            }
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if (name == "steam_api.dll" || name == "steam_api64.dll")
                    && let Some(file_bits) = pe_image_bits(&path)
                    && file_bits == bits
                {
                    if name == preferred_name {
                        return Ok(Some(path));
                    }
                    if best.is_none() {
                        best = Some(path);
                    }
                }
            }
        }
    }
    Ok(best)
}

/// Helper to scan byte stream and replace BIsDlcInstalled license-check conditional jumps with unconditional jumps.
fn patch_dlc_stream(data: &mut [u8], allowed_ids: Option<&[u32]>) -> bool {
    let mut modified = false;
    let len = data.len();
    if len < 14 {
        return false;
    }
    let mut i = 0;
    while i + 13 < len {
        let op = data[i];
        // 0xba (mov edx, imm32), 0xb8 (mov eax, imm32), 0xbe (mov esi, imm32), 0x68 (push imm32)
        if (op == 0xba || op == 0xb8 || op == 0xbe || op == 0x68)
            && data[i + 5] == 0xe8
            && data[i + 10] == 0x84
            && data[i + 11] == 0xc0
        {
            let id = u32::from_le_bytes([data[i + 1], data[i + 2], data[i + 3], data[i + 4]]);
            let id_matches = match allowed_ids {
                Some(ids) if !ids.is_empty() => ids.contains(&id),
                _ => (10..=100_000_000).contains(&id),
            };

            if id_matches {
                let jump_byte = data[i + 12];
                if jump_byte == 0x75 || jump_byte == 0x74 {
                    data[i + 12] = 0xeb;
                    modified = true;
                    i += 13;
                    continue;
                }
            }
        }
        i += 1;
    }
    modified
}

/// Collects candidate DLL paths from the game directory by enumerating PE files
/// matching steam/godotsteam naming patterns up to 2 levels deep.
fn collect_dll_candidates(game_dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut stack = vec![game_dir.to_path_buf()];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        if seen >= 200 {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            seen += 1;
            if seen > 200 {
                return Ok(());
            }
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if (name.contains("steam") && name.ends_with(".dll")) || name.contains("godotsteam")
                {
                    out.push(path);
                }
            }
        }
    }
    Ok(())
}

/// Scans for and patches `BIsDlcInstalled` license-check conditional jumps
/// in `steam_api64.dll` (or the main game DLL) by flipping `jnz`/`je` to `jmp`.
///
/// If `dlc_ids` is provided and non-empty, only calls targeting those App IDs are patched.
/// If `dlc_ids` is empty, any BIsDlcInstalled call targeting a valid Steam App ID is patched.
///
/// This bypasses the Steamworks API license gate so Unity/Godot loads DLC content
/// even when running under Wine/Proton without a Steam session.
pub fn patch_dlc_installed_ids(game_dir: &Path, dlc_ids: &[u32]) -> Result<bool> {
    if !game_dir.is_dir() {
        return Ok(false);
    }

    let mut dll_paths = Vec::new();

    // Prefer steam_api64.dll next to the game's main executable
    for exe in crate::steamless::collect_exe_candidates(game_dir) {
        if let Some(exe_dir) = exe.parent() {
            let api_path = exe_dir.join("steam_api64.dll");
            if api_path.is_file() {
                dll_paths.push(api_path);
            }
        }
    }

    // Dynamic enumeration: always gather all *steam*.dll / *godotsteam* DLLs
    // from the game directory (up to 200 entries, 2 levels deep).
    collect_dll_candidates(game_dir, &mut dll_paths)?;

    // Also check the main game DLL (e.g. acm2_windows.dll)
    let main_dll_candidates = [
        game_dir.join("acm2_windows.dll"),
        game_dir.join("acm2.dll"),
        game_dir.join("Core").join("acm2_windows.dll"),
        game_dir.join("Core").join("acm2.dll"),
    ];
    for candidate in &main_dll_candidates {
        if fs::symlink_metadata(candidate).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
        {
            dll_paths.push(candidate.clone());
        }
    }

    // Deduplicate paths (preserve order)
    let mut seen = std::collections::HashSet::new();
    dll_paths.retain(|p| seen.insert(p.clone()));

    let mut patched_any = false;
    for dll_path in &dll_paths {
        let Ok(mut data) = fs::read(dll_path) else {
            continue;
        };

        if patch_dlc_stream(
            &mut data,
            if dlc_ids.is_empty() {
                None
            } else {
                Some(dlc_ids)
            },
        ) {
            let backup_path = dll_path.with_extension("dll.dlc.bak");
            if !backup_path.exists() {
                let _ = fs::copy(dll_path, &backup_path);
            }
            if fs::write(dll_path, &data).is_ok() {
                patched_any = true;
            }
        }

        // Also attempt GodotSteam signature patch for godotsteam DLLs
        let dll_name_lower = dll_path
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if dll_name_lower.contains("godotsteam") {
            // Re-read since data may have been consumed by patch_dlc_stream above
            let Ok(mut godot_data) = fs::read(dll_path) else {
                continue;
            };
            if patch_godotsteam_dll(&mut godot_data) {
                let backup_path = dll_path.with_extension("dll.dlc.bak");
                if !backup_path.exists() {
                    let _ = fs::copy(dll_path, &backup_path);
                }
                if fs::write(dll_path, &godot_data).is_ok() {
                    patched_any = true;
                }
            }
        }
    }

    Ok(patched_any)
}

/// Convenience wrapper for `patch_dlc_installed_ids(game_dir, &[])`.
pub fn patch_dlc_installed(game_dir: &Path) -> Result<bool> {
    patch_dlc_installed_ids(game_dir, &[])
}

/// Rolls back DLC installed patch by restoring any .dll.dlc.bak files.
pub fn rollback_dlc_installed(game_dir: &Path) -> Result<bool> {
    if !game_dir.is_dir() {
        return Ok(false);
    }
    let mut restored = false;

    // Restore from steam_api64.dll backups
    for exe in crate::steamless::collect_exe_candidates(game_dir) {
        if let Some(exe_dir) = exe.parent() {
            let api_path = exe_dir.join("steam_api64.dll");
            let backup = api_path.with_extension("dll.dlc.bak");
            if backup.is_file() {
                fs::copy(&backup, &api_path)?;
                let _ = fs::remove_file(&backup);
                restored = true;
            }
        }
    }

    // Fallback: search for steam_api64.dll backups if no executable was found
    if !restored {
        let mut dll_paths = Vec::new();
        find_dlc_dlls(game_dir, 0, 2, &mut dll_paths)?;
        for dll_path in dll_paths {
            if dll_path.file_name().is_some_and(|n| n == "steam_api64.dll") {
                let backup = dll_path.with_extension("dll.dlc.bak");
                if backup.is_file() {
                    fs::copy(&backup, &dll_path)?;
                    let _ = fs::remove_file(&backup);
                    restored = true;
                }
            }
        }
    }

    // Restore from main game DLL backups
    let dll_candidates = [
        game_dir.join("acm2_windows.dll"),
        game_dir.join("acm2.dll"),
        game_dir.join("Core").join("acm2_windows.dll"),
        game_dir.join("Core").join("acm2.dll"),
    ];
    for candidate in &dll_candidates {
        let backup = candidate.with_extension("dll.dlc.bak");
        if backup.is_file() {
            fs::copy(&backup, candidate)?;
            let _ = fs::remove_file(&backup);
            restored = true;
        }
    }

    // Restore from GodotSteam DLL backups (libgodotsteam*.dll)
    let mut godotsteam_paths = Vec::new();
    collect_dll_candidates(game_dir, &mut godotsteam_paths)?;
    for dll_path in godotsteam_paths {
        let name_lower = dll_path
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if name_lower.contains("godotsteam") {
            let backup = dll_path.with_extension("dll.dlc.bak");
            if backup.is_file() {
                fs::copy(&backup, &dll_path)?;
                let _ = fs::remove_file(&backup);
                restored = true;
            }
        }
    }

    Ok(restored)
}

/// Check if a DLC installed patch backup file exists.
pub fn has_dlc_installed_patch(game_dir: &Path) -> bool {
    if !game_dir.is_dir() {
        return false;
    }

    // Check steam_api64.dll backups
    for exe in crate::steamless::collect_exe_candidates(game_dir) {
        if let Some(exe_dir) = exe.parent() {
            let api_path = exe_dir.join("steam_api64.dll");
            if api_path.with_extension("dll.dlc.bak").is_file() {
                return true;
            }
        }
    }

    // Check main game DLL backups
    let candidates = [
        game_dir.join("acm2_windows.dll"),
        game_dir.join("acm2.dll"),
        game_dir.join("Core").join("acm2_windows.dll"),
        game_dir.join("Core").join("acm2.dll"),
    ];
    if candidates
        .iter()
        .any(|c| c.with_extension("dll.dlc.bak").is_file())
    {
        return true;
    }

    let mut dll_paths = Vec::new();
    if find_dlc_dlls(game_dir, 0, 2, &mut dll_paths).is_ok() {
        return dll_paths
            .iter()
            .any(|p| p.with_extension("dll.dlc.bak").is_file());
    }

    // Check GodotSteam DLL backups
    let mut godotsteam_paths = Vec::new();
    if collect_dll_candidates(game_dir, &mut godotsteam_paths).is_ok() {
        return godotsteam_paths.iter().any(|p| {
            let name_lower = p
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            name_lower.contains("godotsteam") && p.with_extension("dll.dlc.bak").is_file()
        });
    }

    false
}

/// Check if a DLC installed patch target DLL exists in the game directory.
pub fn has_dlc_installed_target(game_dir: &Path) -> bool {
    if !game_dir.is_dir() {
        return false;
    }

    for exe in crate::steamless::collect_exe_candidates(game_dir) {
        if let Some(exe_dir) = exe.parent() {
            let api_path = exe_dir.join("steam_api64.dll");
            if api_path.is_file() {
                return true;
            }
        }
    }

    let dll_candidates = [
        game_dir.join("acm2_windows.dll"),
        game_dir.join("acm2.dll"),
        game_dir.join("Core").join("acm2_windows.dll"),
        game_dir.join("Core").join("acm2.dll"),
    ];
    if dll_candidates.iter().any(|c| c.is_file()) {
        return true;
    }

    let mut dll_paths = Vec::new();
    if find_dlc_dlls(game_dir, 0, 2, &mut dll_paths).is_ok() {
        return !dll_paths.is_empty();
    }

    // Check GodotSteam DLL targets
    let mut godotsteam_paths = Vec::new();
    if collect_dll_candidates(game_dir, &mut godotsteam_paths).is_ok() {
        return godotsteam_paths.iter().any(|p| {
            let name_lower = p
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            name_lower.contains("godotsteam")
        });
    }

    false
}

fn find_dlc_dlls(
    current: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    if depth > max_depth || out.len() >= 10 {
        return Ok(());
    }
    let Ok(entries) = fs::read_dir(current) else {
        return Ok(());
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            find_dlc_dlls(&path, depth + 1, max_depth, out)?;
        } else if meta.is_file() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name == "steam_api64.dll" || name == "acm2_windows.dll" || name == "acm2.dll" {
                out.push(path);
            }
        }
    }
    Ok(())
}

/// Lightweight representation of a PE section header entry.
#[derive(Clone)]
struct PeSection {
    name: String,
    virtual_address: u32,
    raw_offset: u32,
    #[allow(dead_code)]
    virtual_size: u32,
    raw_size: u32,
}

impl PeSection {
    /// Convert a file offset to a Virtual Address (RVA) within this section.
    fn offset_to_rva(&self, offset: u32) -> Option<u32> {
        if offset >= self.raw_offset && offset < self.raw_offset + self.raw_size {
            Some(offset - self.raw_offset + self.virtual_address)
        } else {
            None
        }
    }

    /// Convert a Virtual Address (RVA) back to a file offset within this section.
    #[allow(dead_code)]
    fn rva_to_offset(&self, rva: u32) -> Option<u32> {
        if rva >= self.virtual_address && rva < self.virtual_address + self.virtual_size {
            Some(rva - self.virtual_address + self.raw_offset)
        } else {
            None
        }
    }
}

/// Parse PE section headers from a binary blob.
/// Returns a list of sections with their virtual addresses and raw offsets.
fn parse_pe_sections(data: &[u8]) -> Vec<PeSection> {
    let len = data.len();
    if len < 0x80 {
        return Vec::new();
    }

    // Read PE header offset from DOS header (e-0x3c)
    let pe_offset = u32::from_le_bytes([data[0x3c], data[0x3d], data[0x3e], data[0x3f]]) as usize;

    if pe_offset + 24 > len {
        return Vec::new();
    }

    // Verify PE signature
    if &data[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return Vec::new();
    }

    // COFF header starts at pe_offset + 4
    let coff_offset = pe_offset + 4;
    if coff_offset + 20 > len {
        return Vec::new();
    }

    let number_of_sections =
        u16::from_le_bytes([data[coff_offset + 2], data[coff_offset + 3]]) as usize;
    let optional_header_size = u16::from_le_bytes([data[coff_offset + 16], data[coff_offset + 17]]);

    // Optional header starts after COFF header (20 bytes)
    let opt_header_start = coff_offset + 20;
    if optional_header_size == 0 || opt_header_start + optional_header_size as usize > len {
        return Vec::new();
    }

    // Determine magic to know Optional Header size and structure
    let magic = u16::from_le_bytes([data[opt_header_start], data[opt_header_start + 1]]);
    if magic != 0x20b && magic != 0x10b {
        return Vec::new(); // Not PE32+ or PE32
    }

    // Section headers start after COFF header + Optional Header
    let section_start = if optional_header_size == 0 {
        opt_header_start + 96 // use a minimal PE32+ optional header size as fallback
    } else {
        opt_header_start + optional_header_size as usize
    };

    let mut sections = Vec::with_capacity(number_of_sections);
    for i in 0..number_of_sections {
        let sec_offset = section_start + i * 40;
        if sec_offset + 40 > len {
            break;
        }

        // Section name is first 8 bytes (null-padded ASCII)
        let raw_name = &data[sec_offset..sec_offset + 8];
        let end = raw_name.iter().position(|&b| b == 0).unwrap_or(8);
        let name = std::str::from_utf8(&raw_name[..end])
            .unwrap_or("")
            .to_string();

        // Skip unnamed/empty sections
        if name.is_empty() {
            continue;
        }

        let virtual_address = u32::from_le_bytes([
            data[sec_offset + 12],
            data[sec_offset + 13],
            data[sec_offset + 14],
            data[sec_offset + 15],
        ]);
        let raw_offset = u32::from_le_bytes([
            data[sec_offset + 20],
            data[sec_offset + 21],
            data[sec_offset + 22],
            data[sec_offset + 23],
        ]);
        let virtual_size = u32::from_le_bytes([
            data[sec_offset + 8],
            data[sec_offset + 9],
            data[sec_offset + 10],
            data[sec_offset + 11],
        ]);
        let raw_size = u32::from_le_bytes([
            data[sec_offset + 16],
            data[sec_offset + 17],
            data[sec_offset + 18],
            data[sec_offset + 19],
        ]);

        sections.push(PeSection {
            name,
            virtual_address,
            raw_offset,
            virtual_size,
            raw_size,
        });
    }

    sections
}

/// Scans a GodotSteam DLL for the `isDLCInstalled` function and patches it to return true.
///
/// Strategy:
/// 1. Parse PE section headers to identify .text and .rdata sections.
/// 2. Find the ASCII string "Apps class not found when calling: isDLCInstalled" in .rdata.
/// 3. Compute its RVA (str_rva).
/// 4. Scan .text for RIP-relative `lea` instructions where (inst_rva + 7 + disp32) == str_rva.
/// 5. Scan backward from the matching lea to the function prologue (push rbx; sub rsp, imm8 etc.)
/// 6. Overwrite the first 6 bytes with `mov eax, 1; ret` (b8 01 00 00 00 c3).
///
/// Falls back to searching for `xor al, al` (32 c0) → ret pattern if PE-aware search fails.
///
/// Returns true if at least one byte was modified.
pub fn patch_godotsteam_dll(dll_bytes: &mut [u8]) -> bool {
    const TARGET_STRING: &[u8] = b"Apps class not found when calling: isDLCInstalled";

    let len = dll_bytes.len();
    if len < TARGET_STRING.len() + 10 {
        return false;
    }

    // Parse PE section headers for RVA-aware scanning
    let sections = parse_pe_sections(dll_bytes);

    // Find .rdata and .text sections
    let rdata_section = find_section_by_name(&sections, ".rdata");
    let text_section = find_section_by_name(&sections, ".text");

    // Strategy A: PE-aware search using section headers
    if let (Some(rdata), Some(text)) = (rdata_section, text_section) {
        // Find the target string within .rdata's raw range
        let str_file_offset =
            find_subsequence_in_range(dll_bytes, TARGET_STRING, rdata.raw_offset, rdata.raw_size);

        if let Some(str_off) = str_file_offset {
            let str_rva = match rdata.offset_to_rva(str_off) {
                Some(rva) => rva,
                None => return patch_xor_al_fallback(dll_bytes),
            };

            // Scan .text for RIP-relative lea instructions targeting this string's RVA
            let text_start = text.raw_offset as usize;
            let text_end = (text.raw_offset + text.raw_size) as usize;

            for i in (text_start..text_end).rev() {
                if i + 7 > len {
                    continue;
                }

                // Check for lea reg, [rip + disp32]: 48/4c 8d <modrm> <disp32>
                // RIP-relative addressing in x86-64 has (modrm & 0xc7) == 0x05
                if (dll_bytes[i] != 0x48 && dll_bytes[i] != 0x4c) || dll_bytes[i + 1] != 0x8d {
                    continue;
                }
                let modrm = dll_bytes[i + 2];
                if (modrm & 0xc7) != 0x05 {
                    continue;
                }

                let disp32 = i32::from_le_bytes([
                    dll_bytes[i + 3],
                    dll_bytes[i + 4],
                    dll_bytes[i + 5],
                    dll_bytes[i + 6],
                ]);

                // Convert instruction file offset to RVA
                let inst_rva = match text.offset_to_rva(i as u32) {
                    Some(rva) => rva,
                    None => continue,
                };

                // RIP after this 7-byte instruction points to inst_rva + 7
                let rip_after = inst_rva.wrapping_add(7);
                let target_rva = (rip_after as i64 + disp32 as i64) as u32;

                // Match if target_rva points to the string or within 64 bytes before (e.g. prefix)
                let diff = str_rva.wrapping_sub(target_rva);
                if diff <= 64 {
                    // Found the lea that references our target string.
                    // Now scan backward from this instruction's file offset for a prologue.
                    let lea_file_offset = i;
                    let prologue_start = if lea_file_offset > 128 {
                        lea_file_offset - 128
                    } else {
                        text_start
                    };

                    for j in (prologue_start..lea_file_offset).rev() {
                        // Check for push rbx; sub rsp, imm8: 40 53 48 83 ec xx
                        if dll_bytes[j] == 0x40
                            && j + 5 <= lea_file_offset
                            && dll_bytes[j + 1] == 0x53
                            && dll_bytes[j + 2] == 0x48
                            && dll_bytes[j + 3] == 0x83
                            && dll_bytes[j + 4] == 0xec
                        {
                            let patch = [0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3];
                            if dll_bytes[j..j + patch.len()] != patch {
                                dll_bytes[j..j + patch.len()].copy_from_slice(&patch);
                                return true;
                            }
                        }

                        // Check for push rdi; sub rsp, imm8: 40 57 48 83 ec xx
                        if dll_bytes[j] == 0x40
                            && j + 5 <= lea_file_offset
                            && dll_bytes[j + 1] == 0x57
                            && dll_bytes[j + 2] == 0x48
                            && dll_bytes[j + 3] == 0x83
                            && dll_bytes[j + 4] == 0xec
                        {
                            let patch = [0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3];
                            if dll_bytes[j..j + patch.len()] != patch {
                                dll_bytes[j..j + patch.len()].copy_from_slice(&patch);
                                return true;
                            }
                        }

                        // Check for push rbp; mov rbp, rsp: 55 48 89/8b ec/e5
                        if j + 3 <= lea_file_offset
                            && dll_bytes[j] == 0x55
                            && dll_bytes[j + 1] == 0x48
                            && (dll_bytes[j + 2] == 0x8b || dll_bytes[j + 2] == 0x89)
                            && (dll_bytes[j + 3] == 0xec || dll_bytes[j + 3] == 0xe5)
                        {
                            let patch = [0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3];
                            if dll_bytes[j..j + patch.len()] != patch {
                                dll_bytes[j..j + patch.len()].copy_from_slice(&patch);
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }

    // Strategy B: Fallback — search for `xor al, al` (32 c0) → ret pattern anywhere in the binary
    patch_xor_al_fallback(dll_bytes)
}

/// Find a section by name from parsed PE sections.
fn find_section_by_name<'a>(sections: &'a [PeSection], name: &str) -> Option<&'a PeSection> {
    sections.iter().find(|s| s.name == name)
}

/// Search for `needle` within a specific byte range [offset, offset + size).
fn find_subsequence_in_range(
    haystack: &[u8],
    needle: &[u8],
    offset: u32,
    size: u32,
) -> Option<u32> {
    let start = offset as usize;
    let end = (offset + size) as usize;
    if start >= haystack.len() || end > haystack.len() || start > end {
        return None;
    }
    let slice = &haystack[start..end];
    slice
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|pos| (start + pos) as u32)
}

/// Fallback: search for `xor al, al` (32 c0) followed by ret (c3) and patch to `mov al, 1; ret`.
fn patch_xor_al_fallback(dll_bytes: &mut [u8]) -> bool {
    let xor_al_pattern = [0x32, 0xc0]; // xor al, al
    if let Some(xor_offset) = find_subsequence(dll_bytes, &xor_al_pattern) {
        let len = dll_bytes.len();
        // Verify it's followed by a ret (c3) or return sequence
        if xor_offset + 2 < len && dll_bytes[xor_offset + 2] == 0xc3 {
            let patch = [0xb0, 0x01, 0xc3]; // mov al, 1; ret
            // Only check the first 2 bytes (mov al, 1) since the ret is common to both
            if dll_bytes[xor_offset] != patch[0] || dll_bytes[xor_offset + 1] != patch[1] {
                dll_bytes[xor_offset..xor_offset + patch.len()].copy_from_slice(&patch);
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "azdl-fixes-{}-{}-{}",
            prefix,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn ea_activation_patch_replaces_jump_and_rolls_back() {
        let dir = test_dir("ea");
        let core = dir.join("Core");
        fs::create_dir_all(&core).unwrap();
        let dll = core.join("Activation64.dll");

        // Construct fake DLL with Pattern 1:
        // 84 c0 75 26 41 c7 04 24 aa 86 01 00
        let mut content = vec![0x90; 64];
        let p1 = [
            0x84, 0xc0, 0x75, 0x26, 0x41, 0xc7, 0x04, 0x24, 0xaa, 0x86, 0x01, 0x00,
        ];
        content.extend_from_slice(&p1);
        content.extend_from_slice(&[0x90; 32]);
        fs::write(&dll, &content).unwrap();

        assert!(heal_ea_activation(&dir).unwrap());

        let patched = fs::read(&dll).unwrap();
        // Check that 0x75 at offset + 2 was replaced with 0xEB
        assert_eq!(patched[64 + 2], 0xeb);

        // Rollback
        assert!(rollback_ea_activation(&dir).unwrap());
        let restored = fs::read(&dll).unwrap();
        assert_eq!(restored[64 + 2], 0x75);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_steam_api_provisioning() {
        let dir = test_dir("steamapi");
        let bin = dir.join("bin");
        let sub = dir.join("sub");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&sub).unwrap();

        // Create fake 64-bit PE
        let mut pe = vec![0u8; 0x100];
        pe[0] = b'M';
        pe[1] = b'Z';
        pe[0x3c] = 0x80;
        pe[0x80] = b'P';
        pe[0x81] = b'E';
        pe[0x82] = 0;
        pe[0x83] = 0;
        // Machine 0x8664 = x86_64
        pe[0x84] = 0x64;
        pe[0x85] = 0x86;

        let exe = bin.join("Game.exe");
        fs::write(&exe, &pe).unwrap();

        let dll_sub = sub.join("steam_api64.dll");
        fs::write(&dll_sub, &pe).unwrap();

        let result = ensure_steam_api_dll(&dir, &exe).unwrap();
        assert!(result.is_some());
        assert!(bin.join("steam_api64.dll").is_file());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn dlc_installed_patch_replaces_jump_and_rolls_back() {
        let dir = test_dir("dlc");
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();

        // Construct fake steam_api64.dll with BIsDlcInstalled(3875100) pattern:
        // ba 94 2f 3b 00 e8 xx xx xx xx 84 c0 75 <offset>
        let mut content = vec![0x90; 64];
        let p1 = [
            0xba, 0x94, 0x2f, 0x3b, 0x00, 0xe8, 0x05, 0x00, 0x00, 0x00, 0x84, 0xc0, 0x75, 0x12,
        ];
        content.extend_from_slice(&p1);
        content.extend_from_slice(&[0x90; 32]);
        let dll = bin.join("steam_api64.dll");
        fs::write(&dll, &content).unwrap();

        assert!(has_dlc_installed_target(&dir));
        assert!(!has_dlc_installed_patch(&dir));

        assert!(patch_dlc_installed(&dir).unwrap());
        assert!(has_dlc_installed_patch(&dir));

        let patched = fs::read(&dll).unwrap();
        // Check that 0x75 at offset +12 (ba 94 2f 3b 00 e8 05 00 00 00 84 c0) was replaced with 0xEB
        assert_eq!(patched[64 + 12], 0xeb);

        // Rollback
        assert!(rollback_dlc_installed(&dir).unwrap());
        assert!(!has_dlc_installed_patch(&dir));
        let restored = fs::read(&dll).unwrap();
        assert_eq!(restored[64 + 12], 0x75);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn dlc_installed_patch_filters_by_specific_app_ids() {
        let dir = test_dir("dlc_specific");
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();

        let mut content = vec![0x90; 32];
        // App ID 123456 = 0x0001E240 -> [0x40, 0xe2, 0x01, 0x00]
        let p1 = [
            0xba, 0x40, 0xe2, 0x01, 0x00, 0xe8, 0x10, 0x00, 0x00, 0x00, 0x84, 0xc0, 0x75, 0x08,
        ];
        content.extend_from_slice(&p1);
        content.extend_from_slice(&[0x90; 16]);
        let dll = bin.join("steam_api64.dll");
        fs::write(&dll, &content).unwrap();

        // Should NOT patch when filtering for unrelated ID
        assert!(!patch_dlc_installed_ids(&dir, &[999_999]).unwrap());

        // Should patch when ID is in allowed list
        assert!(patch_dlc_installed_ids(&dir, &[123_456, 999_999]).unwrap());
        let patched = fs::read(&dll).unwrap();
        assert_eq!(patched[32 + 12], 0xeb);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn dlc_installed_patch_handles_je_variant() {
        let dir = test_dir("dlc_je");
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();

        // Pattern with je (0x74) instead of jnz
        let mut content = vec![0x90; 64];
        let p1 = [
            0xba, 0x94, 0x2f, 0x3b, 0x00, 0xe8, 0x05, 0x00, 0x00, 0x00, 0x84, 0xc0, 0x74, 0x12,
        ];
        content.extend_from_slice(&p1);
        content.extend_from_slice(&[0x90; 32]);
        let dll = bin.join("steam_api64.dll");
        fs::write(&dll, &content).unwrap();

        assert!(patch_dlc_installed(&dir).unwrap());

        let patched = fs::read(&dll).unwrap();
        assert_eq!(patched[64 + 12], 0xeb);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn godotsteam_patch_replaces_function_with_mov_eax_1_ret() {
        let dir = test_dir("godotsteam");
        // Construct a fake GodotSteam DLL with proper PE headers:
        // - .text section (raw_offset=0x200) containing prologue + lea instruction
        // - .rdata section (raw_offset=0x400) containing the target string
        let mut content = vec![0u8; 2048];

        // --- DOS Header (offsets 0-0x7F) ---
        content[0] = b'M';
        content[1] = b'Z'; // MZ signature
        content[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes()); // e_lfanew -> PE header at 0x80

        // --- PE Signature + COFF Header (offsets 0x80-0x97) ---
        content[0x80..0x84].copy_from_slice(b"PE\0\0"); // PE signature
        // COFF Header (20 bytes at 0x84..0x98)
        let mut coff = [0u8; 20];
        coff[0..2].copy_from_slice(&0x8664u16.to_le_bytes()); // Machine: x86_64
        coff[2..4].copy_from_slice(&2u16.to_le_bytes()); // NumberOfSections: 2
        coff[16..18].copy_from_slice(&0xE0u16.to_le_bytes()); // SizeOfOptionalHeader: 0xE0
        coff[18..20].copy_from_slice(&0x0022u16.to_le_bytes()); // Characteristics: EXECUTABLE | LARGE_ADDRESS_AWARE
        content[0x84..0x98].copy_from_slice(&coff);

        // --- Minimal PE32+ Optional Header at offset 0x98 (0xE0 bytes) ---
        content[0x98..0x9A].copy_from_slice(&0x20bu16.to_le_bytes()); // Magic (PE32+)

        // --- Section Headers start at 0x98 + 0xE0 = 0x178 ---
        // .text section header at offset 0x178
        let text_hdr = 0x178;
        content[text_hdr..text_hdr + 8].copy_from_slice(b".text\0\0\0");
        content[text_hdr + 8..text_hdr + 12].copy_from_slice(&0x200u32.to_le_bytes()); // VirtualSize
        content[text_hdr + 12..text_hdr + 16].copy_from_slice(&0x1000u32.to_le_bytes()); // VirtualAddress
        content[text_hdr + 16..text_hdr + 20].copy_from_slice(&0x200u32.to_le_bytes()); // SizeOfRawData
        content[text_hdr + 20..text_hdr + 24].copy_from_slice(&0x200u32.to_le_bytes()); // PointerToRawData

        // .rdata section header at offset 0x1A0
        let rdata_hdr = 0x1A0;
        content[rdata_hdr..rdata_hdr + 8].copy_from_slice(b".rdata\0\0");
        content[rdata_hdr + 8..rdata_hdr + 12].copy_from_slice(&0x200u32.to_le_bytes()); // VirtualSize
        content[rdata_hdr + 12..rdata_hdr + 16].copy_from_slice(&0x2000u32.to_le_bytes()); // VirtualAddress
        content[rdata_hdr + 16..rdata_hdr + 20].copy_from_slice(&0x200u32.to_le_bytes()); // SizeOfRawData
        content[rdata_hdr + 20..rdata_hdr + 24].copy_from_slice(&0x400u32.to_le_bytes()); // PointerToRawData

        // --- Fill .text section (raw offset 0x200) with code ---
        let text_base = 0x200;

        // Prologue: push rbx; sub rsp, 0x40 (at file offset 0x200 = RVA 0x1000)
        let prologue = [0x40, 0x53, 0x48, 0x83, 0xec, 0x40];
        content[text_base..text_base + 6].copy_from_slice(&prologue);

        // Padding with NOPs up to lea offset (0x210)
        content[text_base + 6..0x210].fill(0x90);

        // Target string at file offset 0x410 (inside .rdata raw range: 0x400..0x600)
        let str_file_offset: u32 = 0x410;
        let target_str = b"Apps class not found when calling: isDLCInstalled";

        // lea rax, [rip + disp32] at file offset 0x210 (in .text section)
        // RVA of lea = 0x1000 + (0x210 - 0x200) = 0x1010
        // RIP after this 7-byte instruction = 0x1010 + 7 = 0x1017
        // RVA of target string = 0x2000 + (0x410 - 0x400) = 0x2010
        let disp32: i32 = (0x2010u32.wrapping_sub(0x1017)) as i32; // 0xFF9
        let lea_bytes = [
            0x48,
            0x8d,
            0x05, // lea rax, [rip + disp32]
            (disp32 & 0xFF) as u8,
            ((disp32 >> 8) & 0xFF) as u8,
            ((disp32 >> 16) & 0xFF) as u8,
            ((disp32 >> 24) & 0xFF) as u8,
        ];
        content[0x210..0x217].copy_from_slice(&lea_bytes);

        // --- Place target string in .rdata section ---
        content[str_file_offset as usize..str_file_offset as usize + target_str.len()]
            .copy_from_slice(target_str);

        let mut patched = content.clone();
        assert!(patch_godotsteam_dll(&mut patched));

        // Verify the function entry was replaced with mov eax, 1; ret
        assert_eq!(patched[text_base], 0xb8); // mov eax, 1
        assert_eq!(patched[text_base + 1], 0x01);
        assert_eq!(patched[text_base + 2], 0x00);
        assert_eq!(patched[text_base + 3], 0x00);
        assert_eq!(patched[text_base + 4], 0x00);
        assert_eq!(patched[text_base + 5], 0xc3); // ret

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn godotsteam_patch_fallback_xor_al_to_mov_al_1() {
        let dir = test_dir("godotsteam_fallback");
        // Test fallback: when no lea prologue is found, fall back to xor al, al -> mov al, 1
        let mut content = vec![0x90; 256];

        // Place xor al, al (32 c0) at offset 0x10 followed by ret (c3)
        content[0x10] = 0x32;
        content[0x11] = 0xc0; // xor al, al
        content[0x12] = 0xc3; // ret

        let mut patched = content.clone();
        assert!(patch_godotsteam_dll(&mut patched));

        // Verify the fallback patch was applied
        assert_eq!(patched[0x10], 0xb0); // mov al, 1
        assert_eq!(patched[0x11], 0x01);
        assert_eq!(patched[0x12], 0xc3); // ret

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn godotsteam_patch_no_match_returns_false() {
        let dir = test_dir("godotsteam_nomatch");
        let mut content = vec![0x90; 64];
        assert!(!patch_godotsteam_dll(&mut content));

        let _ = fs::remove_dir_all(dir);
    }
}
