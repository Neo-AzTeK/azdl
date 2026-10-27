//! Bounded reading and verification of Steam protobuf depot manifests.

use crate::Result;
use aes::Aes256;
use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

const PAYLOAD_MAGIC: u32 = 0x71F617D0;
const METADATA_MAGIC: u32 = 0x1F4812BE;
const SIGNATURE_MAGIC: u32 = 0x1B81B817;
const END_MAGIC: u32 = 0x32C415AB;
const MAX_MANIFEST_BYTES: u64 = 128 * 1024 * 1024;
const DIRECTORY_FLAG: u64 = 64;
const SYMLINK_FLAG: u64 = 512;

/// A regular file described by one exact depot revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFile {
    pub path: PathBuf,
    pub size: u64,
    pub sha1: [u8; 20],
}

/// The exact depot and file snapshot in a provider binary manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepotManifest {
    pub depot_id: u32,
    pub manifest_id: u64,
    pub files: Vec<ManifestFile>,
}

/// Read-only file verification counts. This never grants ownership of legacy files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestVerification {
    pub matching: usize,
    pub missing: usize,
    pub changed: usize,
}

impl ManifestVerification {
    /// Whether every regular file in the depot matches its recorded size and SHA-1.
    pub fn complete(self) -> bool {
        self.matching > 0 && self.missing == 0 && self.changed == 0
    }
}

#[derive(Clone, Copy)]
enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}

fn varint(bytes: &[u8], cursor: &mut usize) -> Result<u64> {
    let mut value = 0_u64;
    for index in 0..10 {
        let byte = *bytes
            .get(*cursor)
            .ok_or("Truncated depot manifest varint")?;
        *cursor += 1;
        if index == 9 && byte > 1 {
            return Err("Depot manifest varint overflows u64".into());
        }
        value |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("Depot manifest varint is too long".into())
}

fn fields(bytes: &[u8]) -> Result<Vec<(u32, Field<'_>)>> {
    let mut cursor = 0;
    let mut result = Vec::new();
    while cursor < bytes.len() {
        let tag = varint(bytes, &mut cursor)?;
        let number = u32::try_from(tag >> 3)?;
        if number == 0 {
            return Err("Depot manifest protobuf field number is zero".into());
        }
        let value = match tag & 7 {
            0 => Field::Number(varint(bytes, &mut cursor)?),
            2 => {
                let len = usize::try_from(varint(bytes, &mut cursor)?)?;
                let end = cursor
                    .checked_add(len)
                    .ok_or("Depot manifest field overflow")?;
                let value = bytes
                    .get(cursor..end)
                    .ok_or("Truncated depot manifest field")?;
                cursor = end;
                Field::Bytes(value)
            }
            1 | 5 => {
                let len = if tag & 7 == 1 { 8 } else { 4 };
                cursor = cursor
                    .checked_add(len)
                    .ok_or("Depot manifest field overflow")?;
                bytes
                    .get(..cursor)
                    .ok_or("Truncated depot manifest fixed field")?;
                continue;
            }
            _ => return Err("Unsupported depot manifest protobuf wire type".into()),
        };
        result.push((number, value));
    }
    Ok(result)
}

fn section<'a>(bytes: &'a [u8], cursor: &mut usize, expected: u32) -> Result<&'a [u8]> {
    let header = bytes
        .get(*cursor..cursor.checked_add(8).ok_or("Manifest section overflow")?)
        .ok_or("Truncated depot manifest section")?;
    let magic = u32::from_le_bytes(header[..4].try_into()?);
    if magic != expected {
        return Err("Unexpected depot manifest section".into());
    }
    let size = u32::from_le_bytes(header[4..8].try_into()?) as usize;
    *cursor += 8;
    let end = cursor
        .checked_add(size)
        .ok_or("Manifest section overflow")?;
    let content = bytes
        .get(*cursor..end)
        .ok_or("Truncated depot manifest section body")?;
    *cursor = end;
    Ok(content)
}

fn safe_path(name: &str) -> Result<PathBuf> {
    let normalized = name.replace('\\', "/");
    if normalized.is_empty() || normalized.contains(':') || normalized.contains('\0') {
        return Err("Depot manifest has an invalid file path".into());
    }
    let path = PathBuf::from(&normalized);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
        || normalized.split('/').any(str::is_empty)
    {
        return Err("Depot manifest file path escapes the game directory".into());
    }
    Ok(path)
}

fn decrypt_name(encoded: &[u8], cipher: &Aes256) -> Result<String> {
    let compact = encoded
        .iter()
        .copied()
        .filter(|byte| !matches!(byte, b'\r' | b'\n' | b' '))
        .collect::<Vec<_>>();
    let encrypted = STANDARD
        .decode(&compact)
        .or_else(|_| STANDARD_NO_PAD.decode(&compact))
        .map_err(|_| "Invalid encrypted depot filename encoding")?;
    if encrypted.len() < 32 || encrypted.len() % 16 != 0 {
        return Err("Invalid encrypted depot filename length".into());
    }
    let mut initial = GenericArray::clone_from_slice(&encrypted[..16]);
    cipher.decrypt_block(&mut initial);
    let mut previous: [u8; 16] = initial.into();
    let mut plaintext = Vec::with_capacity(encrypted.len() - 16);
    for chunk in encrypted[16..].as_chunks::<16>().0 {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        plaintext.extend(
            block
                .iter()
                .zip(previous)
                .map(|(byte, prior)| *byte ^ prior),
        );
        previous.copy_from_slice(chunk);
    }
    let padding = usize::from(*plaintext.last().ok_or("Empty encrypted depot filename")?);
    if padding == 0
        || padding > 16
        || padding > plaintext.len()
        || plaintext[plaintext.len() - padding..]
            .iter()
            .any(|byte| usize::from(*byte) != padding)
    {
        return Err("Invalid encrypted depot filename padding".into());
    }
    plaintext.truncate(plaintext.len() - padding);
    while plaintext.last() == Some(&0) {
        plaintext.pop();
    }
    Ok(String::from_utf8(plaintext)?)
}

fn depot_cipher(key: &str) -> Result<Aes256> {
    if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Depot key must contain 64 hexadecimal characters".into());
    }
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&key[index * 2..index * 2 + 2], 16)?;
    }
    Aes256::new_from_slice(&bytes).map_err(|_| "Invalid depot key".into())
}

fn parse_file(bytes: &[u8], cipher: Option<&Aes256>) -> Result<Option<ManifestFile>> {
    let mut name = None;
    let mut size = None;
    let mut flags = 0;
    let mut hash = None;
    let mut name_hash = None;
    let mut link = false;
    for (number, value) in fields(bytes)? {
        match (number, value) {
            (1, Field::Bytes(value)) => name = Some(value),
            (2, Field::Number(value)) => size = Some(value),
            (3, Field::Number(value)) => flags = value,
            (4, Field::Bytes(value)) => name_hash = Some(value),
            (5, Field::Bytes(value)) => hash = Some(<[u8; 20]>::try_from(value)?),
            (7, Field::Bytes(value)) => link = !value.is_empty(),
            _ => {}
        }
    }
    if flags & SYMLINK_FLAG != 0 || link {
        return Err("Depot manifest contains a symbolic link".into());
    }
    let name = name.ok_or("Depot manifest file has no name")?;
    let name = match cipher {
        Some(cipher) => decrypt_name(name, cipher)?,
        None => std::str::from_utf8(name)?.to_owned(),
    };
    if cipher.is_some()
        && let Some(expected) = name_hash
    {
        let actual = Sha1::digest(name.to_lowercase().as_bytes());
        if expected.len() != actual.len() || &actual[..] != expected {
            return Err("Decrypted depot filename does not match its hash".into());
        }
    }
    let path = safe_path(&name)?;
    if flags & DIRECTORY_FLAG != 0 {
        return Ok(None);
    }
    Ok(Some(ManifestFile {
        path,
        size: size.ok_or("Depot manifest file has no size")?,
        sha1: hash.ok_or("Depot manifest file has no content hash")?,
    }))
}

/// Parse an unencrypted protobuf manifest and validate its depot and revision IDs.
pub fn read_manifest(
    path: &Path,
    expected_depot: u32,
    expected_revision: u64,
) -> Result<DepotManifest> {
    read_manifest_inner(path, expected_depot, expected_revision, None)
}

/// Parse a manifest, decrypting encrypted filenames with the provider's depot key.
pub fn read_manifest_with_key(
    path: &Path,
    expected_depot: u32,
    expected_revision: u64,
    depot_key: &str,
) -> Result<DepotManifest> {
    read_manifest_inner(path, expected_depot, expected_revision, Some(depot_key))
}

fn read_manifest_inner(
    path: &Path,
    expected_depot: u32,
    expected_revision: u64,
    depot_key: Option<&str>,
) -> Result<DepotManifest> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_MANIFEST_BYTES {
        return Err("Depot manifest is not a supported regular file".into());
    }
    let bytes = fs::read(path)?;
    let mut cursor = 0;
    let payload = section(&bytes, &mut cursor, PAYLOAD_MAGIC)?;
    let metadata = section(&bytes, &mut cursor, METADATA_MAGIC)?;
    let _signature = section(&bytes, &mut cursor, SIGNATURE_MAGIC)?;
    let end = bytes
        .get(cursor..cursor + 4)
        .ok_or("Depot manifest end marker is missing")?;
    if u32::from_le_bytes(end.try_into()?) != END_MAGIC || cursor + 4 != bytes.len() {
        return Err("Depot manifest has an invalid end marker".into());
    }
    let mut depot_id = None;
    let mut manifest_id = None;
    let mut encrypted = false;
    let mut clear_crc = None;
    let mut encrypted_crc = None;
    for (number, value) in fields(metadata)? {
        match (number, value) {
            (1, Field::Number(value)) => depot_id = Some(u32::try_from(value)?),
            (2, Field::Number(value)) => manifest_id = Some(value),
            (4, Field::Number(value)) => encrypted = value != 0,
            (8, Field::Number(value)) => encrypted_crc = Some(u32::try_from(value)?),
            (9, Field::Number(value)) => clear_crc = Some(u32::try_from(value)?),
            _ => {}
        }
    }
    if depot_id != Some(expected_depot) || manifest_id != Some(expected_revision) {
        return Err("Depot manifest metadata does not match the selected revision".into());
    }
    let cipher = if encrypted {
        Some(depot_cipher(
            depot_key.ok_or("Depot manifest filenames are encrypted")?,
        )?)
    } else {
        None
    };
    let selected_crc = if encrypted { encrypted_crc } else { clear_crc };
    if let Some(expected_crc) = selected_crc {
        let payload_len = u32::try_from(payload.len())?;
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&payload_len.to_le_bytes());
        hasher.update(payload);
        if hasher.finalize() != expected_crc {
            return Err("Depot manifest payload CRC does not match metadata".into());
        }
    }
    let mut files = Vec::new();
    for (number, value) in fields(payload)? {
        if let (1, Field::Bytes(value)) = (number, value)
            && let Some(file) = parse_file(value, cipher.as_ref())?
        {
            files.push(file);
        }
    }
    if files.is_empty() {
        return Err("Depot manifest contains no regular files".into());
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut portable_paths = BTreeSet::new();
    for file in &files {
        if !portable_paths.insert(file.path.to_string_lossy().to_lowercase()) {
            return Err("Depot manifest contains file paths that collide on Windows".into());
        }
    }
    Ok(DepotManifest {
        depot_id: expected_depot,
        manifest_id: expected_revision,
        files,
    })
}

fn existing_file(root: &Path, relative: &Path) -> Result<Option<PathBuf>> {
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("Installed depot path is a symbolic link".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    if !fs::symlink_metadata(&path)?.is_file() {
        return Err("Installed depot path is not a regular file".into());
    }
    Ok(Some(path))
}

/// Compare installed bytes with every regular file in a binary depot manifest.
pub fn verify_installed(
    manifest: &DepotManifest,
    install_dir: &Path,
) -> Result<ManifestVerification> {
    let root = fs::canonicalize(install_dir)?;
    if !root.is_dir() {
        return Err("Game installation directory is missing".into());
    }
    let mut result = ManifestVerification {
        matching: 0,
        missing: 0,
        changed: 0,
    };
    let mut buffer = [0_u8; 64 * 1024];
    for entry in &manifest.files {
        let Some(path) = existing_file(&root, &entry.path)? else {
            result.missing += 1;
            continue;
        };
        if fs::metadata(&path)?.len() != entry.size {
            result.changed += 1;
            continue;
        }
        let mut file = File::open(&path)?;
        let mut hasher = Sha1::new();
        loop {
            let size = file.read(&mut buffer)?;
            if size == 0 {
                break;
            }
            hasher.update(&buffer[..size]);
        }
        if hasher.finalize().as_slice() == entry.sha1 {
            result.matching += 1;
        } else {
            result.changed += 1;
        }
    }
    Ok(result)
}

/// Reject DLC files that would replace different installed bytes before downloading a depot.
/// The commit transaction repeats this check after staging because game files can change.
pub fn check_additive_install(manifests: &[DepotManifest], install_dir: &Path) -> Result<()> {
    validate_manifest_set(manifests)?;
    let root = fs::canonicalize(install_dir)?;
    if !root.is_dir() {
        return Err("Game installation directory is missing".into());
    }
    let mut buffer = [0_u8; 64 * 1024];
    for manifest in manifests {
        for entry in &manifest.files {
            let Some(path) = existing_file(&root, &entry.path)? else {
                continue;
            };
            if fs::metadata(&path)?.len() != entry.size {
                return Err(format!(
                    "DLC depot would replace an existing game file at {}; ordered overlay support is required",
                    entry.path.display()
                )
                .into());
            }
            let mut file = File::open(&path)?;
            let mut hasher = Sha1::new();
            loop {
                let size = file.read(&mut buffer)?;
                if size == 0 {
                    break;
                }
                hasher.update(&buffer[..size]);
            }
            if hasher.finalize().as_slice() != entry.sha1 {
                return Err(format!(
                    "DLC depot would replace an existing game file at {}; ordered overlay support is required",
                    entry.path.display()
                )
                .into());
            }
        }
    }
    Ok(())
}

pub(crate) fn collect_staged_files(
    root: &Path,
    relative: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !fs::canonicalize(&path)?.starts_with(root) {
            return Err("Staged depot contains a symbolic link or escaping path".into());
        }
        let relative_path = relative.join(entry.file_name());
        if metadata.is_dir() {
            collect_staged_files(root, &relative_path, files)?;
        } else if metadata.is_file() {
            files.insert(relative_path);
        } else {
            return Err("Staged depot contains a non-regular file".into());
        }
    }
    Ok(())
}

pub(crate) fn expected_files(manifests: &[DepotManifest]) -> Result<BTreeSet<PathBuf>> {
    if manifests.is_empty() {
        return Err("No depot manifests were selected for staging verification".into());
    }
    let mut expected = BTreeMap::<String, &ManifestFile>::new();
    for manifest in manifests {
        for file in &manifest.files {
            if file.path.as_os_str().is_empty()
                || file
                    .path
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err("Selected depot manifest has an unsafe file path".into());
            }
            let portable = file
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_lowercase();
            if let Some(previous) = expected.insert(portable, file)
                && (previous.path != file.path
                    || previous.size != file.size
                    || previous.sha1 != file.sha1)
            {
                return Err(format!(
                    "Selected depots require conflicting content at {}",
                    file.path.display()
                )
                .into());
            }
        }
    }
    for (portable, file) in &expected {
        let mut parent = portable.as_str();
        while let Some(split) = parent.rfind('/') {
            parent = &parent[..split];
            if expected.contains_key(parent) {
                return Err(format!(
                    "Selected depots use a file as a directory at {}",
                    file.path.display()
                )
                .into());
            }
        }
    }
    Ok(expected
        .values()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>())
}

/// Check selected manifest paths and reject different-content or Windows case conflicts.
pub fn validate_manifest_set(manifests: &[DepotManifest]) -> Result<()> {
    expected_files(manifests).map(|_| ())
}

/// Verify the complete staged payload against every selected depot manifest.
/// Reject unlisted files and conflicting path ownership before any game file is changed.
pub fn verify_staged_exact(manifests: &[DepotManifest], staged_dir: &Path) -> Result<()> {
    let allowed = expected_files(manifests)?;
    let metadata = fs::symlink_metadata(staged_dir)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("DLC staging payload is not a regular directory".into());
    }
    let root = fs::canonicalize(staged_dir)?;
    let mut staged = BTreeSet::new();
    collect_staged_files(&root, Path::new(""), &mut staged)?;
    if let Some(extra) = staged.difference(&allowed).next() {
        return Err(format!(
            "Staged depot contains an unlisted file: {}",
            extra.display()
        )
        .into());
    }
    for manifest in manifests {
        let result = verify_installed(manifest, &root)?;
        if !result.complete() {
            return Err(format!(
                "Staged depot {} does not match manifest {} ({} missing, {} changed)",
                manifest.depot_id, manifest.manifest_id, result.missing, result.changed
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncrypt;

    #[test]
    fn selected_manifests_reject_file_directory_and_case_conflicts() {
        let manifest = |depot_id, path: &str| DepotManifest {
            depot_id,
            manifest_id: u64::from(depot_id),
            files: vec![ManifestFile {
                path: PathBuf::from(path),
                size: 4,
                sha1: Sha1::digest(b"data").into(),
            }],
        };
        let child = manifest(101, "data/addon.bin");
        let parent_file = manifest(102, "data");
        let error = validate_manifest_set(&[child.clone(), parent_file])
            .unwrap_err()
            .to_string();
        assert!(error.contains("file as a directory"), "{error}");

        let case_conflict = manifest(103, "DATA/addon.bin");
        let error = validate_manifest_set(&[child, case_conflict])
            .unwrap_err()
            .to_string();
        assert!(error.contains("conflicting content"), "{error}");
    }

    #[test]
    fn additive_preflight_rejects_overlays_before_download() {
        let root = std::env::temp_dir().join(format!(
            "azdl-dlc-additive-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let manifest = DepotManifest {
            depot_id: 200,
            manifest_id: 300,
            files: vec![ManifestFile {
                path: PathBuf::from("content/addon.bin"),
                size: 3,
                sha1: Sha1::digest(b"new").into(),
            }],
        };
        check_additive_install(std::slice::from_ref(&manifest), &root).unwrap();
        fs::create_dir(root.join("content")).unwrap();
        fs::write(root.join("content/addon.bin"), b"new").unwrap();
        check_additive_install(std::slice::from_ref(&manifest), &root).unwrap();
        fs::write(root.join("content/addon.bin"), b"old").unwrap();
        let error = check_additive_install(&[manifest], &root)
            .unwrap_err()
            .to_string();
        assert!(error.contains("ordered overlay support"), "{error}");
        assert_eq!(fs::read(root.join("content/addon.bin")).unwrap(), b"old");
        fs::remove_dir_all(root).unwrap();
    }

    fn encode_varint(mut value: u64) -> Vec<u8> {
        let mut output = Vec::new();
        while value >= 128 {
            output.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        output.push(value as u8);
        output
    }

    fn number(field: u8, value: u64) -> Vec<u8> {
        let mut output = vec![field << 3];
        output.extend(encode_varint(value));
        output
    }

    fn bytes(field: u8, value: &[u8]) -> Vec<u8> {
        let mut output = vec![(field << 3) | 2];
        output.extend(encode_varint(value.len() as u64));
        output.extend(value);
        output
    }

    fn section(magic: u32, content: &[u8]) -> Vec<u8> {
        let mut output = magic.to_le_bytes().to_vec();
        output.extend((content.len() as u32).to_le_bytes());
        output.extend(content);
        output
    }

    fn fixture(path: &str, body: &[u8]) -> Vec<u8> {
        let mut file = bytes(1, path.as_bytes());
        file.extend(number(2, body.len() as u64));
        file.extend(bytes(5, &Sha1::digest(body)));
        let payload = bytes(1, &file);
        let mut metadata = number(1, 200);
        metadata.extend(number(2, 300));
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&(payload.len() as u32).to_le_bytes());
        hasher.update(&payload);
        metadata.extend(number(9, u64::from(hasher.finalize())));
        let mut result = section(PAYLOAD_MAGIC, &payload);
        result.extend(section(METADATA_MAGIC, &metadata));
        result.extend(section(SIGNATURE_MAGIC, &[]));
        result.extend(END_MAGIC.to_le_bytes());
        result
    }

    fn encrypted_fixture(path: &str, flags: u64, key: &[u8; 32], padded: bool) -> Vec<u8> {
        let cipher = Aes256::new_from_slice(key).unwrap();
        let mut previous = [7u8; 16];
        let mut encrypted_iv = GenericArray::clone_from_slice(&previous);
        cipher.encrypt_block(&mut encrypted_iv);
        let mut encrypted = encrypted_iv.to_vec();
        let mut plain = path.as_bytes().to_vec();
        plain.extend([0, 0]);
        let padding = 16 - plain.len() % 16;
        plain.extend(std::iter::repeat_n(padding as u8, padding));
        for chunk in plain.chunks_exact(16) {
            let mut block = GenericArray::clone_from_slice(chunk);
            for (byte, prior) in block.iter_mut().zip(previous) {
                *byte ^= prior;
            }
            cipher.encrypt_block(&mut block);
            encrypted.extend_from_slice(&block);
            previous.copy_from_slice(&block);
        }
        let mut encoded = if padded {
            STANDARD.encode(encrypted)
        } else {
            STANDARD_NO_PAD.encode(encrypted)
        };
        encoded.insert_str(8, " \r\n");
        let mut file = bytes(1, encoded.as_bytes());
        file.extend(number(2, 7));
        file.extend(number(3, flags));
        file.extend(bytes(4, &Sha1::digest(path.to_lowercase().as_bytes())));
        file.extend(bytes(5, &Sha1::digest(b"content")));
        let payload = bytes(1, &file);
        let mut metadata = number(1, 200);
        metadata.extend(number(2, 300));
        metadata.extend(number(4, 1));
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&(payload.len() as u32).to_le_bytes());
        hasher.update(&payload);
        metadata.extend(number(8, u64::from(hasher.finalize())));
        metadata.extend(number(9, 0));
        let mut result = section(PAYLOAD_MAGIC, &payload);
        result.extend(section(METADATA_MAGIC, &metadata));
        result.extend(section(SIGNATURE_MAGIC, &[]));
        result.extend(END_MAGIC.to_le_bytes());
        result
    }

    #[test]
    fn encrypted_manifest_decryption_preserves_security_checks() {
        let root = std::env::temp_dir().join(format!(
            "azdl-encrypted-manifest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let source = root.join("depot.manifest");
        let key = [0x11; 32];
        let key_hex = "11".repeat(32);
        for padded in [true, false] {
            fs::write(
                &source,
                encrypted_fixture("data\\addon.bin", 0, &key, padded),
            )
            .unwrap();
            assert!(read_manifest(&source, 200, 300).is_err());
            assert!(read_manifest_with_key(&source, 200, 300, "invalid").is_err());
            assert!(read_manifest_with_key(&source, 200, 300, &"22".repeat(32)).is_err());
            let manifest = read_manifest_with_key(&source, 200, 300, &key_hex).unwrap();
            assert_eq!(manifest.files[0].path, Path::new("data/addon.bin"));
            let expected_hash: [u8; 20] = Sha1::digest(b"content").into();
            assert_eq!(manifest.files[0].sha1, expected_hash);
        }
        fs::write(&source, fixture("data/addon.bin", b"content")).unwrap();
        assert!(read_manifest_with_key(&source, 200, 300, "unused").is_ok());
        let original = encrypted_fixture("data/addon.bin", 0, &key, true);
        let mut cursor = 0;
        let payload = super::section(&original, &mut cursor, PAYLOAD_MAGIC).unwrap();
        let mut file = match fields(payload).unwrap()[0].1 {
            Field::Bytes(value) => value.to_vec(),
            _ => panic!("Expected a file mapping"),
        };
        let name_hash = Sha1::digest(b"data/addon.bin");
        let offset = file
            .windows(name_hash.len())
            .position(|window| window == &name_hash[..])
            .unwrap();
        file[offset] ^= 1;
        assert!(
            parse_file(&file, Some(&Aes256::new_from_slice(&key).unwrap()))
                .unwrap_err()
                .to_string()
                .contains("filename does not match")
        );
        fs::write(&source, encrypted_fixture("../escape", 0, &key, true)).unwrap();
        assert!(read_manifest_with_key(&source, 200, 300, &key_hex).is_err());
        fs::write(
            &source,
            encrypted_fixture("data/addon.bin", SYMLINK_FLAG, &key, true),
        )
        .unwrap();
        assert!(read_manifest_with_key(&source, 200, 300, &key_hex).is_err());
        let mut corrupted = encrypted_fixture("data/addon.bin", 0, &key, true);
        corrupted[12] ^= 1;
        fs::write(&source, corrupted).unwrap();
        assert!(
            read_manifest_with_key(&source, 200, 300, &key_hex)
                .unwrap_err()
                .to_string()
                .contains("CRC")
        );
        assert!(decrypt_name(b"%%%", &Aes256::new_from_slice(&key).unwrap()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_rejects_payload_changed_after_crc_was_recorded() {
        let root = std::env::temp_dir().join(format!(
            "azdl-manifest-crc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let source = root.join("depot.manifest");
        let mut bytes = fixture("data/addon.bin", b"content");
        fs::write(&source, &bytes).unwrap();
        read_manifest(&source, 200, 300).unwrap();
        let index = bytes
            .windows(b"data/addon.bin".len())
            .position(|window| window == b"data/addon.bin")
            .unwrap();
        bytes[index] = b'D';
        fs::write(&source, bytes).unwrap();
        let error = read_manifest(&source, 200, 300).unwrap_err().to_string();
        assert!(error.contains("payload CRC"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_manifest_verification_and_path_rejection() {
        let root = std::env::temp_dir().join(format!(
            "azdl-manifest-test-{}-{}",
            std::process::id(),
            crate::jobs::now().unwrap()
        ));
        fs::create_dir_all(root.join("game/data")).unwrap();
        let source = root.join("depot.manifest");
        fs::write(&source, fixture("data\\addon.bin", b"content")).unwrap();
        let manifest = read_manifest(&source, 200, 300).unwrap();
        assert_eq!(manifest.files[0].path, Path::new("data/addon.bin"));
        assert_eq!(
            verify_installed(&manifest, &root.join("game"))
                .unwrap()
                .missing,
            1
        );
        fs::write(root.join("game/data/addon.bin"), b"content").unwrap();
        assert!(
            verify_installed(&manifest, &root.join("game"))
                .unwrap()
                .complete()
        );
        fs::write(root.join("game/data/addon.bin"), b"changed").unwrap();
        assert_eq!(
            verify_installed(&manifest, &root.join("game"))
                .unwrap()
                .changed,
            1
        );
        assert!(read_manifest(&source, 201, 300).is_err());
        fs::write(&source, fixture("../escape", b"content")).unwrap();
        assert!(read_manifest(&source, 200, 300).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_verification_rejects_missing_changed_extra_and_conflicting_files() {
        let root = std::env::temp_dir().join(format!(
            "azdl-staged-manifest-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("payload/data")).unwrap();
        let source = root.join("depot.manifest");
        fs::write(&source, fixture("data/addon.bin", b"content")).unwrap();
        let manifest = read_manifest(&source, 200, 300).unwrap();
        let payload = root.join("payload");
        validate_manifest_set(std::slice::from_ref(&manifest)).unwrap();

        let missing = verify_staged_exact(std::slice::from_ref(&manifest), &payload)
            .unwrap_err()
            .to_string();
        assert!(missing.contains("1 missing"), "{missing}");
        fs::write(payload.join("data/addon.bin"), b"changed").unwrap();
        let changed = verify_staged_exact(std::slice::from_ref(&manifest), &payload)
            .unwrap_err()
            .to_string();
        assert!(changed.contains("1 changed"), "{changed}");
        fs::write(payload.join("data/addon.bin"), b"content").unwrap();
        verify_staged_exact(&[manifest.clone(), manifest.clone()], &payload).unwrap();

        let mut conflict = manifest.clone();
        conflict.depot_id = 201;
        conflict.files[0].sha1 = Sha1::digest(b"other").into();
        assert!(validate_manifest_set(&[manifest.clone(), conflict.clone()]).is_err());
        let error = verify_staged_exact(&[manifest.clone(), conflict], &payload)
            .unwrap_err()
            .to_string();
        assert!(error.contains("conflicting content"), "{error}");

        fs::write(payload.join("unlisted.bin"), b"extra").unwrap();
        let extra = verify_staged_exact(&[manifest], &payload)
            .unwrap_err()
            .to_string();
        assert!(extra.contains("unlisted file"), "{extra}");
        #[cfg(unix)]
        {
            fs::remove_file(payload.join("unlisted.bin")).unwrap();
            std::os::unix::fs::symlink(&source, payload.join("linked.manifest")).unwrap();
            let linked =
                verify_staged_exact(&[read_manifest(&source, 200, 300).unwrap()], &payload)
                    .unwrap_err()
                    .to_string();
            assert!(linked.contains("symbolic link"), "{linked}");
        }
        fs::remove_dir_all(root).unwrap();
    }
}
