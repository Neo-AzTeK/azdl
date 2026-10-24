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

    // Fallback: search up to 200 entries within 2 directory levels for steam_api64.dll
    if dll_paths.is_empty() {
        find_dlc_dlls(game_dir, 0, 2, &mut dll_paths)?;
    }

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

    let mut patched_any = false;
    for dll_path in dll_paths {
        let Ok(mut data) = fs::read(&dll_path) else {
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
                let _ = fs::copy(&dll_path, &backup_path);
            }
            fs::write(&dll_path, &data)?;
            patched_any = true;
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
}
