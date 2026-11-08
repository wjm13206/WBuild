//! BLAKE3 based content hashing and fingerprint construction.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context, Result};

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

#[derive(Clone)]
struct CachedHash {
    mtime: Option<SystemTime>,
    size: u64,
    hash: String,
}

/// A small in-process memoizer that avoids re-reading unchanged files during a
/// single build. Comparing (mtime, size) is enough to catch edits; BLAKE3 then
/// guarantees content correctness across builds.
pub struct FileHasher {
    cache: Mutex<HashMap<PathBuf, CachedHash>>,
}

impl Default for FileHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl FileHasher {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn hash(&self, path: &Path) -> Result<String> {
        let meta = fs::metadata(path)
            .with_context(|| format!("cannot read input `{}`", path.display()))?;
        let mtime = meta.modified().ok();
        let size = meta.len();

        if let Some(cached) = self.cache.lock().unwrap().get(path) {
            if cached.size == size && cached.mtime == mtime {
                return Ok(cached.hash.clone());
            }
        }

        let hash =
            hash_file(path).with_context(|| format!("cannot hash input `{}`", path.display()))?;
        self.cache.lock().unwrap().insert(
            path.to_path_buf(),
            CachedHash {
                mtime,
                size,
                hash: hash.clone(),
            },
        );
        Ok(hash)
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
