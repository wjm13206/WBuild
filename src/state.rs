//! Persistent build state (fingerprints and discovered dependencies).

use std::collections::BTreeMap;
use std::fs;
use std::io;
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
        let text = serde_json::to_string_pretty(self)?;
        crate::util::atomic_write(path, text.as_bytes())
            .with_context(|| format!("cannot write state file `{}`", path.display()))
    }

    pub fn record(&self, key: &str) -> Option<&StepRecord> {
        self.steps.get(key)
    }

    pub fn insert(&mut self, key: String, record: StepRecord) {
        self.steps.insert(key, record);
    }

    /// Drop records whose outputs no longer exist.
    pub fn prune(&mut self, root: &Path) {
        let root = root.to_path_buf();
        self.steps.retain(|_, record| {
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
pub struct BuildLock {
    path: PathBuf,
}

impl BuildLock {
    pub fn acquire(state_dir: &Path) -> Result<Self> {
        fs::create_dir_all(state_dir)
            .with_context(|| format!("cannot create state directory `{}`", state_dir.display()))?;
        let path = state_dir.join("lock");
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                let _ = writeln!(file, "pid={}", std::process::id());
                Ok(Self { path })
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                anyhow::bail!(
                    "another WBuild process holds the lock `{}`; \
                     if no build is running, delete the lock file",
                    path.display()
                )
            }
            Err(err) => {
                Err(err).with_context(|| format!("cannot create lock `{}`", path.display()))
            }
        }
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
