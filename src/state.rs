//! Persistent build state (fingerprints and discovered dependencies).

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepRecord {
    /// Fingerprint computed after the last successful build.
    pub fingerprint: String,
    /// Files discovered as dependencies (source and headers), relative to the
    /// project root when possible.
    #[serde(default)]
    pub deps: Vec<String>,
    /// Outputs produced by this step.
    #[serde(default)]
    pub outputs: Vec<String>,
    /// Wall-clock duration of the last build of this step.
    #[serde(default)]
    pub duration_ms: u64,
    /// Unix timestamp of the last successful build.
    #[serde(default)]
    pub updated: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub steps: BTreeMap<String, StepRecord>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            steps: BTreeMap::new(),
        }
    }
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read state file `{}`", path.display()))?;
        match serde_json::from_str::<State>(&text) {
            Ok(state) if state.version == STATE_VERSION => Ok(state),
            Ok(_) => {
                // Forward/backward incompatible state is discarded; the build
                // will simply rebuild everything.
                Ok(Self::default())
            }
            Err(_) => Ok(Self::default()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        // Compact JSON keeps state files small; they are machine data, and on
        // large projects this noticeably reduces write and parse time.
        let bytes = serde_json::to_vec(self)?;
        crate::util::atomic_write(path, &bytes)
            .with_context(|| format!("cannot write state file `{}`", path.display()))
    }

    pub fn record(&self, key: &str) -> Option<&StepRecord> {
        self.steps.get(key)
    }

    pub fn insert(&mut self, key: String, record: StepRecord) {
        self.steps.insert(key, record);
    }

    /// Drop records whose outputs no longer exist. Records in `keep` belong to
    /// the current plan and were just validated, so they skip the stat calls.
    pub fn prune(&mut self, root: &Path, keep: &HashSet<String>) {
        let root = root.to_path_buf();
        self.steps.retain(|key, record| {
            if keep.contains(key) {
                return true;
            }
            record.outputs.iter().any(|out| {
                let path = if Path::new(out).is_absolute() {
                    PathBuf::from(out)
                } else {
                    root.join(out)
                };
                path.exists()
            })
        });
    }

    pub fn clear(&mut self) {
        self.steps.clear();
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// An exclusive lock preventing concurrent builds from corrupting state.
///
/// The lock is an OS advisory file lock rather than a marker file: the
/// operating system releases it when the process exits, so a crashed or
/// killed build never leaves a stale lock behind that has to be deleted by
/// hand. The lock file itself is kept on disk between builds.
pub struct BuildLock {
    _file: fs::File,
}

impl BuildLock {
    pub fn acquire(state_dir: &Path) -> Result<Self> {
        use std::io::{Seek, SeekFrom, Write};

        fs::create_dir_all(state_dir)
            .with_context(|| format!("cannot create state directory `{}`", state_dir.display()))?;
        let path = state_dir.join("lock");
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("cannot open lock file `{}`", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                anyhow::bail!(
                    "another WBuild process is already building this project (lock `{}`)",
                    path.display()
                )
            }
            Err(fs::TryLockError::Error(err)) => {
                return Err(err)
                    .with_context(|| format!("cannot acquire lock `{}`", path.display()))
            }
        }
        // Record the owner for diagnostics. Failure to write is not fatal for
        // the lock itself, but it should still be reported.
        file.set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
            .and_then(|()| writeln!(file, "pid={}", std::process::id()))
            .with_context(|| format!("cannot write lock owner to `{}`", path.display()))?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = BuildLock::acquire(dir.path()).unwrap();
        assert!(BuildLock::acquire(dir.path()).is_err());
        drop(first);
        let second = BuildLock::acquire(dir.path());
        assert!(second.is_ok());
    }
}
