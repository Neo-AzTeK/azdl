//! Persistent directory sizes, refreshed by one background scanner.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const RECONCILE_SECONDS: u64 = 6 * 60 * 60;
const RETRY_SECONDS: u64 = 60;

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    bytes: u64,
    scanned_unix: u64,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<PathBuf, Entry>,
    pending: HashSet<PathBuf>,
    failed: HashMap<PathBuf, u64>,
    running: bool,
    generation: u64,
}

pub struct SizeCache {
    path: PathBuf,
    state: Mutex<CacheState>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl SizeCache {
    pub fn new(data_dir: &Path) -> Self {
        let path = data_dir.join("directory-sizes.json");
        let entries: Vec<Entry> = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            state: Mutex::new(CacheState {
                entries: entries
                    .into_iter()
                    .map(|entry| (entry.path.clone(), entry))
                    .collect(),
                ..CacheState::default()
            }),
        }
    }

    /// Return the last known size immediately; never walk a game directory here.
    pub fn get(self: &Arc<Self>, path: &Path, updated: impl Fn() + Send + 'static) -> Option<u64> {
        let mut state = self.state.lock().ok()?;
        let cached = state.entries.get(path);
        let bytes = cached.map(|entry| entry.bytes);
        let expired = cached
            .is_none_or(|entry| now().saturating_sub(entry.scanned_unix) >= RECONCILE_SECONDS);
        let retry = state
            .failed
            .get(path)
            .is_none_or(|time| now().saturating_sub(*time) >= RETRY_SECONDS);
        if expired && retry {
            state.pending.insert(path.to_path_buf());
            if !state.running {
                state.running = true;
                let cache = Arc::clone(self);
                if std::thread::Builder::new()
                    .name("azdl-directory-sizes".into())
                    .spawn(move || {
                        cache.scan_pending();
                        updated();
                    })
                    .is_err()
                {
                    state.running = false;
                }
            }
        }
        bytes
    }

    /// Content mutations invalidate observations, including an already running scan.
    pub fn invalidate_all(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.generation = state.generation.wrapping_add(1);
            state.failed.clear();
            for entry in state.entries.values_mut() {
                entry.scanned_unix = 0;
            }
            let _ = self.persist(&state);
        }
    }

    pub fn invalidate(&self, path: &Path) {
        if let Ok(mut state) = self.state.lock() {
            state.generation = state.generation.wrapping_add(1);
            state.failed.remove(path);
            if let Some(entry) = state.entries.get_mut(path) {
                entry.scanned_unix = 0;
            }
            let _ = self.persist(&state);
        }
    }

    pub fn retain(&self, paths: impl Iterator<Item = PathBuf>) {
        let paths: HashSet<_> = paths.collect();
        if let Ok(mut state) = self.state.lock() {
            let old_len = state.entries.len();
            let old_pending = state.pending.len();
            state.entries.retain(|path, _| paths.contains(path));
            state.pending.retain(|path| paths.contains(path));
            state.failed.retain(|path, _| paths.contains(path));
            if state.entries.len() != old_len || state.pending.len() != old_pending {
                state.generation = state.generation.wrapping_add(1);
                let _ = self.persist(&state);
            }
        }
    }

    fn scan_pending(&self) {
        loop {
            let (path, generation) = {
                let Ok(mut state) = self.state.lock() else {
                    return;
                };
                let Some(path) = state.pending.iter().next().cloned() else {
                    let _ = self.persist(&state);
                    state.running = false;
                    return;
                };
                (path, state.generation)
            };
            let result = directory_size(&path);
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if state.generation != generation {
                continue;
            }
            state.pending.remove(&path);
            match result {
                Ok(bytes) => {
                    state.failed.remove(&path);
                    state.entries.insert(
                        path.clone(),
                        Entry {
                            path,
                            bytes,
                            scanned_unix: now(),
                        },
                    );
                }
                Err(_) => {
                    state.failed.insert(path, now());
                }
            }
        }
    }

    fn persist(&self, state: &CacheState) -> io::Result<()> {
        // The launcher data directory already exists; do not recreate a removed root.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = self
            .path
            .with_extension(format!("{}.{nonce}.tmp", std::process::id()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            serde_json::to_writer(&mut file, &state.entries.values().collect::<Vec<_>>())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
}

fn directory_size(path: &Path) -> io::Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        let bytes = if metadata.is_file() {
            metadata.len()
        } else if metadata.is_dir() {
            directory_size(&path)?
        } else {
            0
        };
        total = total
            .checked_add(bytes)
            .ok_or_else(|| io::Error::other("directory size overflow"))?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn sizes_survive_restart_and_refresh_after_invalidation() {
        let root = std::env::temp_dir().join(format!("azdl-sizes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let game = root.join("game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("data"), b"game").unwrap();
        let cache = Arc::new(SizeCache::new(&root));
        let (tx, rx) = mpsc::channel();
        assert_eq!(
            cache.get(&game, move || {
                let _ = tx.send(());
            }),
            None
        );
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let restarted = Arc::new(SizeCache::new(&root));
        assert_eq!(
            restarted.get(&game, || panic!("fresh persisted size must not scan")),
            Some(4)
        );
        fs::write(game.join("data"), b"changed").unwrap();
        assert_eq!(restarted.get(&game, || {}), Some(4));
        restarted.invalidate_all();
        let (tx, rx) = mpsc::channel();
        assert_eq!(
            restarted.get(&game, move || {
                let _ = tx.send(());
            }),
            Some(4)
        );
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(restarted.get(&game, || {}), Some(7));
        fs::remove_dir_all(root).unwrap();
    }
}
