//! WBuild - a fast, simple and incremental build system.
//!
//! The crate is organized as a small pipeline:
//!
//! ```text
//! config  ->  toolchain  ->  planner  ->  executor
//! (TOML)      (detect)       (DAG)        (parallel workers)
//! ```
//!
//! Every build step is a pure command description (program + argument vector),
//! never a shell string. Incrementality is driven by BLAKE3 fingerprints that
//! combine the command line, compiler identity and the contents of every known
//! input file.

pub mod cache;
pub mod cli;
pub mod config;
pub mod depfile;
pub mod engine;
pub mod exec;
pub mod graph;
pub mod hash;
pub mod plan;
pub mod state;
pub mod toolchain;
pub mod util;
pub mod watch;
