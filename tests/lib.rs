//! Library-level tests: these exercise the public API directly instead of
//! spawning the CLI, so embedders can rely on the same behaviour.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use wbuild::cache::{Cache, CacheEntry};
use wbuild::config::{Config, OptLevel};
use wbuild::hash;
use wbuild::plan::{Planner, StepKind};
use wbuild::toolchain::Toolchain;

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("wbuild.toml"),
        r#"
[project]
name = "libdemo"

[target.app]
kind = "exe"
sources = ["src/*.c"]
include = ["core/include"]
links = ["core"]

[target.core]
kind = "staticlib"
sources = ["core/*.c"]
include = ["core/include"]
"#,
    );
    write(
        &dir.path().join("src/main.c"),
        "#include \"core.h\"\nint main(void) { return core_value() == 42 ? 0 : 1; }\n",
    );
    write(
        &dir.path().join("core/core.c"),
        "#include \"core.h\"\nint core_value(void) { return 42; }\n",
    );
    write(
        &dir.path().join("core/include/core.h"),
        "#ifndef CORE_H\n#define CORE_H\nint core_value(void);\n#endif\n",
    );
    dir
}

#[test]
fn config_loads_and_resolves_profiles() {
    let dir = project();
    let loaded = Config::load(Some(&dir.path().join("wbuild.toml"))).unwrap();

    assert_eq!(loaded.config.project.name, "libdemo");
    assert_eq!(
        loaded.root.canonicalize().unwrap(),
        dir.path().canonicalize().unwrap()
    );
    assert_eq!(loaded.out_dir, dir.path().join("build"));

    let debug = loaded.config.resolve_profile("debug").unwrap();
    assert_eq!(debug.name, "debug");
    assert_eq!(debug.opt, OptLevel::O0);
    assert!(debug.debug);

    let release = loaded.config.resolve_profile("release").unwrap();
    assert_eq!(release.name, "release");
    assert_eq!(release.opt, OptLevel::O2);
    assert!(!release.debug);
    assert!(release.defines.iter().any(|d| d == "NDEBUG"));
}

#[test]
fn planner_builds_a_valid_dag() {
    let dir = project();
    let loaded = Config::load(Some(&dir.path().join("wbuild.toml"))).unwrap();
    let profile = loaded.config.resolve_profile("debug").unwrap();
    let Ok(tc) = Toolchain::detect(&loaded.config.compiler, &loaded.root) else {
        eprintln!("skipping: no compiler toolchain detected");
        return;
    };
    if tc.cc.is_none() {
        eprintln!("skipping: no C compiler detected");
        return;
    }

    let plan = Planner::build(&loaded, &profile, &tc, &[]).unwrap();
    // Two compilations, one archive and one link.
    assert_eq!(plan.steps.len(), 4);
    assert!(plan.artifacts.contains_key("app"));
    assert!(plan.artifacts.contains_key("core"));

    let order = plan.dag.topo_order().unwrap();
    assert_eq!(order.len(), 4);
    let position: HashMap<usize, usize> = order
        .iter()
        .copied()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();

    let mut links = 0;
    for (id, step) in plan.steps.iter().enumerate() {
        for dep in &step.deps {
            assert!(
                position[dep] < position[&id],
                "step {id} ({}) scheduled before its dependency {dep}",
                step.label
            );
        }
        if matches!(step.kind, StepKind::Link) {
            links += 1;
            assert!(!step.deps.is_empty(), "link step must depend on objects");
        }
    }
    assert_eq!(links, 1);

    let compile = plan
        .steps
        .iter()
        .find(|s| matches!(s.kind, StepKind::CCompile))
        .unwrap();
    assert!(
        compile.args.iter().any(|a| a == "-MMD"),
        "expected GCC/Clang dependency generation, got {:?}",
        compile.args
    );
}

#[test]
fn selecting_a_target_prunes_unrelated_work() {
    let dir = project();
    let loaded = Config::load(Some(&dir.path().join("wbuild.toml"))).unwrap();
    let profile = loaded.config.resolve_profile("debug").unwrap();
    let Ok(tc) = Toolchain::detect(&loaded.config.compiler, &loaded.root) else {
        eprintln!("skipping: no compiler toolchain detected");
        return;
    };
    if tc.cc.is_none() {
        eprintln!("skipping: no C compiler detected");
        return;
    }

    let plan = Planner::build(&loaded, &profile, &tc, &["core".to_string()]).unwrap();
    assert_eq!(plan.steps.len(), 2, "compile + archive only");
    assert!(plan.artifacts.contains_key("core"));
    assert!(!plan.artifacts.contains_key("app"));
}

#[test]
fn named_outputs_and_declared_programs_are_plannable() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("wbuild.toml"),
        r#"
[project]
name = "named"

[target.gen]
kind = "custom"
program = "cargo"
args = ["--version"]
outputs = ["gen/a.txt", "gen/b.o"]

[target.runner]
kind = "custom"
program = "gen/a.txt"
links = ["gen"]
outputs = ["runner.txt"]

[target.consumer]
kind = "custom"
program = "cargo"
args = ["--version"]
links = ["gen:gen/b.o"]
outputs = ["consumer.txt"]
"#,
    );
    let loaded = Config::load(Some(&dir.path().join("wbuild.toml"))).unwrap();
    let profile = loaded.config.resolve_profile("debug").unwrap();
    let tc = Toolchain::detect(&loaded.config.compiler, &loaded.root).unwrap();

    let plan = Planner::build(&loaded, &profile, &tc, &["consumer".to_string()]).unwrap();
    assert!(plan.artifacts.contains_key("gen"));
    assert!(plan.artifacts.contains_key("consumer"));
    assert!(!plan.artifacts.contains_key("runner"));
    assert!(plan.named.contains_key("gen:gen/b.o"));
    assert!(plan.named.contains_key("gen:b.o"));
    let consumer = plan
        .steps
        .iter()
        .find(|step| step.target == "consumer")
        .unwrap();
    assert_eq!(consumer.deps.len(), 1);

    // A custom `program` may point at another target's declared output even
    // though the file does not exist yet.
    let plan = Planner::build(&loaded, &profile, &tc, &["runner".to_string()]).unwrap();
    assert!(plan.artifacts.contains_key("runner"));
}

#[test]
fn cache_roundtrips_and_deduplicates_variants() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join(".wbuild");
    fs::create_dir_all(&state).unwrap();

    let cache = Cache::new(&state, true);
    let artifact = dir.path().join("out.o");
    fs::write(&artifact, b"object-bytes").unwrap();

    let fingerprint = "ab".repeat(32);
    cache.store(&fingerprint, "o", &artifact).unwrap();

    let restored = cache.artifact(&fingerprint, "o").unwrap();
    assert_eq!(fs::read(&restored).unwrap(), b"object-bytes");

    assert!(cache.index("cmd").is_empty());
    let entry = |hash: &str| CacheEntry {
        hash: hash.to_string(),
        deps: vec![format!("{hash}.h")],
        entry: fingerprint.clone(),
        extras: Vec::new(),
    };
    cache.upsert("cmd", entry("dep-a")).unwrap();
    cache.upsert("cmd", entry("dep-b")).unwrap();
    assert_eq!(cache.index("cmd").len(), 2);

    // Re-inserting the same dependency hash replaces, not duplicates.
    cache.upsert("cmd", entry("dep-a")).unwrap();
    assert_eq!(cache.index("cmd").len(), 2);

    cache.clear().unwrap();
    assert!(cache.artifact(&fingerprint, "o").is_none());
    assert!(cache.index("cmd").is_empty());
}

#[test]
fn hashing_is_content_addressed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.bin");
    fs::write(&file, b"hello").unwrap();

    let bytes = hash::hash_bytes(b"hello");
    assert_eq!(bytes.len(), 64);
    assert_eq!(bytes, hash::hash_file(&file).unwrap());

    fs::write(&file, b"world").unwrap();
    assert_ne!(bytes, hash::hash_file(&file).unwrap());

    let hasher = hash::FileHasher::new();
    let first = hasher.hash(&file).unwrap();
    let second = hasher.hash(&file).unwrap();
    assert_eq!(first, second, "memoized lookups stay stable");
    assert_eq!(hasher.hash_opt(&dir.path().join("missing")), "<missing>");
}
