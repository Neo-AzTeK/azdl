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
}
