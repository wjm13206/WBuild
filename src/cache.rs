//! Content-addressed local build cache.
//!
//! Objects are stored under `state_dir/cache/objects/<xx>/<fingerprint>.<ext>`
//! together with a sidecar listing the discovered dependencies. Entries are
//! immutable; a hit is restored with an atomic copy.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One cached variant of a step. `entry` is the fingerprint under which the
/// artifact is stored; `hash` identifies the set and contents of the discovered
/// dependencies at the time the artifact was produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub hash: String,
    pub deps: Vec<String>,
    pub entry: String,
    /// Fingerprints of the extra outputs, in step order.
    #[serde(default)]
    pub extras: Vec<String>,
}

/// Sidecar metadata used to detect corrupted cache objects.
#[derive(Debug, Serialize, Deserialize)]
struct ObjectMeta {
    /// BLAKE3 of the stored bytes.
    hash: String,
    /// Stored length, checked before hashing to fail fast on truncation.
    len: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    entries: Vec<CacheEntry>,
}

#[derive(Debug)]
pub struct Cache {
    root: PathBuf,
    enabled: bool,
}

impl Cache {
    pub fn new(state_dir: &Path, enabled: bool) -> Self {
        Self {
            root: state_dir.join("cache"),
            enabled,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn entry(&self, fingerprint: &str, ext: &str) -> PathBuf {
        let shard = fingerprint.get(0..2).unwrap_or("00");
        self.root
            .join("objects")
            .join(shard)
            .join(format!("{fingerprint}.{ext}"))
    }

    fn deps_entry(&self, fingerprint: &str) -> PathBuf {
        let shard = fingerprint.get(0..2).unwrap_or("00");
        self.root
            .join("index")
            .join(shard)
            .join(format!("{fingerprint}.json"))
    }

    fn meta_entry(&self, fingerprint: &str, ext: &str) -> PathBuf {
        let shard = fingerprint.get(0..2).unwrap_or("00");
        self.root
            .join("objects")
            .join(shard)
            .join(format!("{fingerprint}.{ext}.meta"))
    }

    fn read_meta(&self, fingerprint: &str, ext: &str) -> Option<ObjectMeta> {
        let text = fs::read_to_string(self.meta_entry(fingerprint, ext)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Remove an object whose bytes no longer match its metadata.
    fn discard(&self, fingerprint: &str, ext: &str) {
        let _ = fs::remove_file(self.entry(fingerprint, ext));
        let _ = fs::remove_file(self.meta_entry(fingerprint, ext));
    }

    /// Look up a cached artifact. Returns `None` when caching is disabled, when
    /// the object is missing, or when its contents fail verification.
    pub fn artifact(&self, fingerprint: &str, ext: &str) -> Option<PathBuf> {
        if !self.enabled {
            return None;
        }
        let path = self.entry(fingerprint, ext);
        if !path.is_file() {
            return None;
        }
        let Some(meta) = self.read_meta(fingerprint, ext) else {
            // Objects written by older versions have no metadata; treat them as
            // a miss so the next build re-creates them with a checksum.
            return None;
        };
        let len = fs::metadata(&path).ok()?.len();
        if len != meta.len {
            self.discard(fingerprint, ext);
            return None;
        }
        match crate::hash::hash_file(&path) {
            Ok(hash) if hash == meta.hash => Some(path),
            _ => {
                self.discard(fingerprint, ext);
                None
            }
        }
    }

    /// Store an artifact under its fingerprint and record its checksum.
    pub fn store(&self, fingerprint: &str, ext: &str, src: &Path) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let dst = self.entry(fingerprint, ext);
        if dst.is_file() && self.read_meta(fingerprint, ext).is_some() {
            return Ok(());
        }
        crate::util::atomic_copy(src, &dst)
            .with_context(|| format!("cannot store cache entry `{}`", dst.display()))?;
        let hash = crate::hash::hash_file(&dst)
            .with_context(|| format!("cannot checksum cache entry `{}`", dst.display()))?;
        let len = fs::metadata(&dst)?.len();
        let meta = ObjectMeta { hash, len };
        let text = serde_json::to_string(&meta)?;
        crate::util::atomic_write(&self.meta_entry(fingerprint, ext), text.as_bytes())
            .with_context(|| format!("cannot store cache metadata for `{}`", dst.display()))
    }

    /// All cached variants of a step, keyed by its dependency-independent
    /// command fingerprint. The caller validates each variant's `hash`
    /// against the current dependency contents before using it.
    pub fn index(&self, command: &str) -> Vec<CacheEntry> {
        if !self.enabled {
            return Vec::new();
        }
        let Ok(text) = fs::read_to_string(self.deps_entry(command)) else {
            return Vec::new();
        };
        serde_json::from_str::<Index>(&text)
            .map(|index| index.entries)
            .unwrap_or_default()
    }

    /// Insert or replace a cached variant in the command index.
    pub fn upsert(&self, command: &str, entry: CacheEntry) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.deps_entry(command);
        let mut index = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Index>(&text).ok())
            .unwrap_or_default();
        index.entries.retain(|e| e.hash != entry.hash);
        index.entries.push(entry);
        let text = serde_json::to_string(&index)?;
        crate::util::atomic_write(&path, text.as_bytes())
            .with_context(|| format!("cannot store cache index `{}`", path.display()))
    }

    pub fn clear(&self) -> io::Result<()> {
        if self.root.exists() {
            fs::remove_dir_all(&self.root)?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_restores_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path(), true);
        let src = dir.path().join("a.o");
        fs::write(&src, b"object").unwrap();
        cache.store("abcdef1234", "o", &src).unwrap();
        let hit = cache.artifact("abcdef1234", "o").unwrap();
        assert_eq!(fs::read(hit).unwrap(), b"object");
        assert!(cache.artifact("ffff", "o").is_none());
    }

    #[test]
    fn disabled_cache_never_hits() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path(), false);
        let src = dir.path().join("a.o");
        fs::write(&src, b"object").unwrap();
        cache.store("abcdef", "o", &src).unwrap();
        assert!(cache.artifact("abcdef", "o").is_none());
    }

    #[test]
    fn corrupted_objects_are_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path(), true);
        let src = dir.path().join("a.o");
        fs::write(&src, b"object").unwrap();
        cache.store("abcdef1234", "o", &src).unwrap();
        assert!(cache.artifact("abcdef1234", "o").is_some());

        // Same length, different bytes: only the checksum can catch this.
        fs::write(cache.entry("abcdef1234", "o"), b"0bject").unwrap();
        assert!(cache.artifact("abcdef1234", "o").is_none());
        assert!(!cache.entry("abcdef1234", "o").exists());
        assert!(!cache.meta_entry("abcdef1234", "o").exists());

        // Objects without metadata are treated as misses instead of trusted.
        cache.store("abcdef1234", "o", &src).unwrap();
        fs::remove_file(cache.meta_entry("abcdef1234", "o")).unwrap();
        assert!(cache.artifact("abcdef1234", "o").is_none());
    }

    #[test]
    fn index_tracks_dependency_variants() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path(), true);
        let first = CacheEntry {
            hash: "h1".into(),
            deps: vec!["a.h".into()],
            entry: "aaaa1111".into(),
            extras: Vec::new(),
        };
        cache.upsert("command", first.clone()).unwrap();
        assert_eq!(cache.index("command").len(), 1);
        let second = CacheEntry {
            hash: "h2".into(),
            deps: vec!["a.h".into(), "b.h".into()],
            entry: "bbbb2222".into(),
            extras: Vec::new(),
        };
        cache.upsert("command", second).unwrap();
        assert_eq!(cache.index("command").len(), 2);
        // Re-inserting the same dependency hash replaces that variant.
        cache.upsert("command", first).unwrap();
        assert_eq!(cache.index("command").len(), 2);
    }
}
