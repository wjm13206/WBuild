//! A tiny polling watcher: rebuild when source files change.
//!
//! Polling keeps the dependency footprint at zero and is more than fast enough
//! for typical projects (a directory walk per interval, no hashing).

use std::collections::BTreeMap;
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use walkdir::WalkDir;

use crate::config::LoadedConfig;
use crate::engine::{self, BuildRequest};

type Snapshot = BTreeMap<String, (Option<SystemTime>, u64)>;

pub fn watch(loaded: &LoadedConfig, req: &BuildRequest) -> Result<()> {
    let interval = Duration::from_millis(500);
    println!(
        "Watching `{}` for changes (Ctrl+C to stop)...",
        loaded.root.display()
    );

    let mut last = snapshot(loaded);
    loop {
        thread::sleep(interval);
        let current = snapshot(loaded);
        if current == last {
            continue;
        }
        match engine::build(loaded, req) {
            Ok(summary) if !summary.success() => {
                eprintln!("build failed; waiting for changes...");
            }
            Ok(_) => {}
            Err(err) => {
                eprintln!("error: {err:#}");
                eprintln!("waiting for changes...");
            }
        }
        // Take a fresh snapshot after building so generated files in the
        // source tree do not immediately trigger another cycle.
        last = snapshot(loaded);
    }
}

fn snapshot(loaded: &LoadedConfig) -> Snapshot {
    let state_dir = loaded.state_dir();
    let mut map = Snapshot::new();
    let walker = WalkDir::new(&loaded.root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| keep(entry.path(), &loaded.out_dir, &state_dir));

    for entry in walker.filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let modified = meta.modified().ok();
        map.insert(
            entry.path().to_string_lossy().to_string(),
            (modified, meta.len()),
        );
    }
    map
}

fn keep(path: &Path, out_dir: &Path, state_dir: &Path) -> bool {
    if path.starts_with(out_dir) || path.starts_with(state_dir) {
        return false;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name == ".git" || name == "node_modules" || name == ".svn" || name == ".hg" {
        return false;
    }
    true
}
