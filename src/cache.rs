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

/// One cached variant of a step. `entry` is the content fingerprint under
/// which the artifact is stored; `hash` identifies the set and contents of the
/// discovered dependencies at the time the artifact was produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub hash: String,
    pub deps: Vec<String>,
    pub entry: String,
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

    /// Look up a cached artifact. Returns `None` when caching is disabled.
    pub fn artifact(&self, fingerprint: &str, ext: &str) -> Option<PathBuf> {
        if !self.enabled {
            return None;
        }
        let path = self.entry(fingerprint, ext);
        if path.is_file() {
            Some(path)
        } else {
            None
        }
    }

    /// Store an artifact under its fingerprint.
    pub fn store(&self, fingerprint: &str, ext: &str, src: &Path) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let dst = self.entry(fingerprint, ext);
        if dst.is_file() {
            return Ok(());
        }
        crate::util::atomic_copy(src, &dst)
            .with_context(|| format!("cannot store cache entry `{}`", dst.display()))
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
    fn index_tracks_dependency_variants() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path(), true);
        let first = CacheEntry {
            hash: "h1".into(),
            deps: vec!["a.h".into()],
            entry: "aaaa1111".into(),
        };
        cache.upsert("command", first.clone()).unwrap();
        assert_eq!(cache.index("command").len(), 1);
        let second = CacheEntry {
            hash: "h2".into(),
            deps: vec!["a.h".into(), "b.h".into()],
            entry: "bbbb2222".into(),
        };
        cache.upsert("command", second).unwrap();
        assert_eq!(cache.index("command").len(), 2);
        // Re-inserting the same dependency hash replaces that variant.
        cache.upsert("command", first).unwrap();
        assert_eq!(cache.index("command").len(), 2);
    }
}
