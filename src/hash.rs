//! BLAKE3 based content hashing and fingerprint construction.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Number of independently locked hash maps. Builds of large projects call
/// `hash` hundreds of thousands of times, so spreading the lookups across
/// shards removes most of the mutex contention between compiler workers.
const SHARDS: usize = 16;

/// Version of the on-disk hash memo. Bump when its semantics change.
const INDEX_VERSION: u32 = 2;
const INDEX_FILE: &str = "hashes.json";

/// Hash an in-memory byte slice.
pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Hash a file's contents.
pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut file = fs::File::open(path)?;
    let mut buf = [0u8; 64 * 1024];
    use std::io::Read;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// A memoized file hash together with the cheap stamp it was computed from.
///
/// The stamp (modification time and size) is what an incremental rebuild
/// usually checks anyway. Trusting it as a validation key means an unchanged
/// file is never opened during a no-op build, while still being re-hashed as
/// soon as it is touched.
///
/// The stamp is also memoized on its own, so a path is stat'ed at most once per
/// build even when it is both an input and an output of different steps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HashEntry {
    /// Stamp the persisted hash was computed from.
    index_stamp: Option<(u64, u64)>,
    hash: String,
    /// Stamp observed by this process, once `looked` is set.
    stamp: Option<(u64, u64)>,
    looked: bool,
}

impl HashEntry {
    /// True when the memoized hash still describes the file as it is now.
    fn matches(&self, stamp: (u64, u64)) -> bool {
        !self.hash.is_empty() && self.index_stamp == Some(stamp) && self.stamp == Some(stamp)
    }
}

/// Serialized form of the hash memo, keyed by normalized path.
#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    files: BTreeMap<String, SerializedEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SerializedEntry {
    #[serde(default)]
    mtime: Option<u64>,
    size: u64,
    hash: String,
}

/// A small in-process memoizer that avoids re-reading unchanged files during a
/// single build.
///
/// Each file's content is hashed at most once per build while its stamp stays
/// the same. When the memo is backed by `state_dir/hashes.json`, hashes survive
/// between builds too: a no-op rebuild only stats its inputs instead of reading
/// them. A build reserves the right to assume inputs do not change underneath
/// it, so a path is stamped at most once as well; anything that rewrites a file
/// during the build (the executor, for its step outputs) must call
/// [`FileHasher::invalidate`] first.
pub struct FileHasher {
    shards: Vec<Mutex<HashMap<PathBuf, HashEntry>>>,
    index_path: Option<PathBuf>,
    dirty: AtomicBool,
}

impl Default for FileHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl FileHasher {
    /// In-memory-only hasher, used by tests and one-off tools.
    pub fn new() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
            index_path: None,
            dirty: AtomicBool::new(false),
        }
    }

    /// Load the persistent hash memo from `state_dir`, falling back to an empty
    /// memo when it is missing or unreadable.
    pub fn open(state_dir: &Path) -> Self {
        let path = state_dir.join(INDEX_FILE);
        let mut hasher = Self::new();
        hasher.index_path = Some(path.clone());

        let Ok(text) = fs::read_to_string(&path) else {
            return hasher;
        };
        let Ok(index) = serde_json::from_str::<IndexFile>(&text) else {
            return hasher;
        };
        if index.version != INDEX_VERSION {
            return hasher;
        }
        for (key, entry) in index.files {
            let shard = hasher.shard(Path::new(&key));
            shard.lock().unwrap().insert(
                PathBuf::from(key),
                HashEntry {
                    index_stamp: entry.mtime.map(|mtime| (entry.size, mtime)),
                    hash: entry.hash,
                    stamp: None,
                    looked: false,
                },
            );
        }
        hasher
    }

    /// Write the memo back when at least one hash was computed. No-op builds
    /// never write anything, which keeps them free of extra disk traffic.
    pub fn persist(&self) -> Result<()> {
        let Some(path) = &self.index_path else {
            return Ok(());
        };
        if !self.dirty.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut files = BTreeMap::new();
        for shard in &self.shards {
            for (key, entry) in shard.lock().unwrap().iter() {
                let Some((size, mtime)) = entry.index_stamp else {
                    continue;
                };
                if entry.hash.is_empty() {
                    continue;
                }
                files.insert(
                    crate::util::path_key(key),
                    SerializedEntry {
                        mtime: Some(mtime),
                        size,
                        hash: entry.hash.clone(),
                    },
                );
            }
        }
        let index = IndexFile {
            version: INDEX_VERSION,
            files,
        };
        let bytes = serde_json::to_vec(&index)?;
        crate::util::atomic_write(path, &bytes)
            .with_context(|| format!("cannot write hash index `{}`", path.display()))?;
        self.dirty.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn shard(&self, path: &Path) -> &Mutex<HashMap<PathBuf, HashEntry>> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
        let idx = (hasher.finish() as usize) % self.shards.len();
        &self.shards[idx]
    }

    /// Stamp of a path, memoized for the lifetime of this build.
    ///
    /// Returns `None` when the path cannot be stat'ed (typically: it does not
    /// exist). The result is cached, including the negative answer, so callers
    /// that probe the same file repeatedly pay for one syscall.
    fn ensure_stamp(&self, path: &Path) -> Option<(u64, u64)> {
        {
            let cache = self.shard(path).lock().unwrap();
            if let Some(entry) = cache.get(path) {
                if entry.looked {
                    return entry.stamp;
                }
            }
        }

        let stamp = crate::util::file_stamp(path);

        let mut cache = self.shard(path).lock().unwrap();
        let entry = cache.entry(path.to_path_buf()).or_default();
        entry.looked = true;
        entry.stamp = stamp;
        stamp
    }

    /// Whether a path exists, answered from the stamp memo when possible.
    pub fn exists(&self, path: &Path) -> bool {
        self.ensure_stamp(path).is_some()
    }

    pub fn hash(&self, path: &Path) -> Result<String> {
        let Some(stamp) = self.ensure_stamp(path) else {
            anyhow::bail!("cannot read input `{}`", path.display());
        };

        {
            let cache = self.shard(path).lock().unwrap();
            if let Some(entry) = cache.get(path) {
                if entry.matches(stamp) {
                    return Ok(entry.hash.clone());
                }
            }
        }

        let hash =
            hash_file(path).with_context(|| format!("cannot hash input `{}`", path.display()))?;
        {
            let mut cache = self.shard(path).lock().unwrap();
            let entry = cache.entry(path.to_path_buf()).or_default();
            entry.index_stamp = Some(stamp);
            entry.hash = hash.clone();
            entry.stamp = Some(stamp);
            entry.looked = true;
        }
        self.dirty.store(true, Ordering::Relaxed);
        Ok(hash)
    }

    /// Drop the verified state of a path a step is about to rewrite.
    ///
    /// Only the in-memory trust flag is cleared: the memoized hash stays, and
    /// the next lookup re-stats before deciding whether it is still valid.
    pub fn invalidate(&self, path: &Path) {
        if let Some(entry) = self.shard(path).lock().unwrap().get_mut(path) {
            entry.looked = false;
            entry.stamp = None;
        }
    }

    /// Hash a file, mapping "not found" to a stable marker instead of an error.
    /// Used for dependency files that may legitimately disappear.
    pub fn hash_opt(&self, path: &Path) -> String {
        match self.hash(path) {
            Ok(h) => h,
            Err(_) => "<missing>".to_string(),
        }
    }
}

/// Incremental fingerprint builder.
///
/// The fingerprint answer is: "would this exact command, given these exact
/// input bytes and this compiler identity, produce the same output?"
pub struct Fingerprint {
    hasher: blake3::Hasher,
}

impl Default for Fingerprint {
    fn default() -> Self {
        Self::new()
    }
}

impl Fingerprint {
    pub fn new() -> Self {
        Self {
            hasher: blake3::Hasher::new(),
        }
    }

    /// Tag the fingerprint with a domain separator.
    pub fn tag(mut self, tag: &str) -> Self {
        self.hasher.update(tag.as_bytes());
        self.hasher.update(&[0]);
        self
    }

    pub fn text(&mut self, text: &str) {
        self.hasher.update(text.as_bytes());
        self.hasher.update(&[0]);
    }

    pub fn path(&mut self, path: &Path) {
        self.text(&crate::util::path_key(path));
    }

    pub fn file(&mut self, hasher: &FileHasher, path: &Path) -> Result<()> {
        let hash = hasher.hash(path)?;
        self.hasher.update(hash.as_bytes());
        self.hasher.update(&[0]);
        Ok(())
    }

    /// Include a command argument vector.
    pub fn args(&mut self, args: &[String]) {
        for arg in args {
            self.text(arg);
        }
    }

    pub fn finish(self) -> String {
        self.hasher.finalize().to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn hash_is_stable_and_sensitive() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let mut f = fs::File::create(&a).unwrap();
        f.write_all(b"hello").unwrap();
        drop(f);

        let h1 = hash_file(&a).unwrap();
        let h2 = hash_file(&a).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);

        fs::write(&a, b"world").unwrap();
        assert_ne!(h1, hash_file(&a).unwrap());
    }

    #[test]
    fn hasher_memoizes_by_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, b"one").unwrap();
        let hasher = FileHasher::new();
        let first = hasher.hash(&a).unwrap();
        let second = hasher.hash(&a).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn hasher_rehashes_after_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, b"one").unwrap();
        let hasher = FileHasher::new();
        let first = hasher.hash(&a).unwrap();
        // Same length, different contents: only the stamp can catch this, so the
        // file must be re-read once the memo is invalidated (which is what the
        // executor does before a step rewrites its own outputs).
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&a, b"two").unwrap();
        assert_eq!(first, hasher.hash(&a).unwrap(), "stamps are memoized");
        hasher.invalidate(&a);
        let second = hasher.hash(&a).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn index_rehashes_when_stamp_changes() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, b"one").unwrap();
        let hasher = FileHasher::open(dir.path());
        let first = hasher.hash(&a).unwrap();
        hasher.persist().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&a, b"two").unwrap();

        // A new build only trusts the persisted hash while the stamp matches.
        let next = FileHasher::open(dir.path());
        let second = next.hash(&a).unwrap();
        assert_ne!(first, second);
        assert!(next.dirty.load(Ordering::Relaxed));
    }

    #[test]
    fn index_round_trips_between_builds() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, b"hello").unwrap();

        let first = FileHasher::open(dir.path());
        let before = first.hash(&a).unwrap();
        first.persist().unwrap();

        let second = FileHasher::open(dir.path());
        let after = second.hash(&a).unwrap();
        assert_eq!(before, after);
        // Nothing was re-hashed, so there is nothing to write back.
        assert!(!second.dirty.load(Ordering::Relaxed));
    }

    #[test]
    fn fingerprint_changes_with_args() {
        let mut a = Fingerprint::new();
        a.text("cc");
        a.args(&["-c".into(), "x.c".into()]);
        let mut b = Fingerprint::new();
        b.text("cc");
        b.args(&["-c".into(), "y.c".into()]);
        assert_ne!(a.finish(), b.finish());
    }
}
