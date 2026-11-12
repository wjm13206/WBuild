//! Build orchestration and user-facing reporting.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use walkdir::WalkDir;

use crate::cache::Cache;
use crate::config::{Kind, LoadedConfig, Target};
use crate::exec::{self, ExecContext, ExecOptions, StepOutcome, StepStatus};
use crate::hash::FileHasher;
use crate::plan::{Plan, Planner, StepKind};
use crate::state::{now_unix, BuildLock, State, StepRecord};
use crate::toolchain::Toolchain;
use crate::util;

#[derive(Debug, Clone, Default)]
pub struct BuildRequest {
    pub profile: Option<String>,
    pub targets: Vec<String>,
    pub jobs: Option<usize>,
    pub force: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub quiet: bool,
    pub no_cache: bool,
    pub timings: bool,
    pub explain: bool,
    pub print_commands: bool,
}

#[derive(Debug, Default)]
pub struct BuildSummary {
    pub built: usize,
    pub cached: usize,
    pub up_to_date: usize,
    pub failed: usize,
    pub not_run: usize,
    pub duration: Duration,
    pub artifacts: Vec<(String, PathBuf, bool)>,
}

impl BuildSummary {
    pub fn success(&self) -> bool {
        self.failed == 0
    }
}

struct Painter {
    enabled: bool,
}

impl Painter {
    fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }
    fn green(&self, text: &str) -> String {
        self.paint("32", text)
    }
    fn red(&self, text: &str) -> String {
        self.paint("31", text)
    }
    fn cyan(&self, text: &str) -> String {
        self.paint("36", text)
    }
    fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }
}

pub fn build(loaded: &LoadedConfig, req: &BuildRequest) -> Result<BuildSummary> {
    let profile = loaded.profile(req.profile.as_deref())?;
    let state_dir = loaded.state_dir();
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("cannot create state directory `{}`", state_dir.display()))?;
    let _lock = BuildLock::acquire(&state_dir)?;

    let state_path = state_dir.join("state.json");
    let mut state = State::load(&state_path)?;

    let toolchain = Toolchain::detect_cached(&loaded.config.compiler, &loaded.root, &state_dir)?;
    let plan = Planner::build(loaded, &profile, &toolchain, &req.targets)?;

    let jobs = req.jobs.unwrap_or(loaded.config.build.jobs);
    let jobs = if jobs == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        jobs
    };

    let cache_enabled = loaded.config.build.cache && !req.no_cache && !req.force;
    let ctx = ExecContext {
        root: loaded.root.clone(),
        state: &state,
        cache: Cache::new(&state_dir, cache_enabled),
        hasher: FileHasher::open(&state_dir),
        toolchain_fp: toolchain.fingerprint.clone(),
        options: ExecOptions {
            jobs,
            force: req.force,
            dry_run: req.dry_run,
            verbose: req.verbose,
            quiet: req.quiet,
            print_commands: req.print_commands,
            explain: req.explain,
            no_cache: req.no_cache,
        },
    };

    let painter = Painter::new(util::use_color() && !req.quiet);
    if !req.quiet {
        println!(
            "{} {}  {}  {}  {}",
            painter.bold("WBuild"),
            env!("CARGO_PKG_VERSION"),
            painter.dim("|"),
            painter.cyan(&format!("profile {}", profile.name)),
            painter.dim(&format!("| {} steps | {} jobs", plan.total_steps(), jobs))
        );
        if !req.targets.is_empty() {
            println!("  targets: {}", req.targets.join(", "));
        }
        println!();
    }

    let mut counts = Counts::default();
    let interactive = !req.quiet;
    let total = plan.total_steps();

    let report = exec::run(&plan, &ctx, |outcome, done, total| {
        if !interactive {
            return;
        }
        print_outcome(&painter, outcome, done, total, req);
        counts.observe(outcome);
    });

    // Persist the file hash memo before the executor context is torn down.
    // A no-op build computes no new hashes, so this writes nothing.
    if !req.dry_run {
        if let Err(err) = ctx.hasher.persist() {
            if req.verbose {
                eprintln!("warning: {err:#}");
            }
        }
    }

    // Merge successful step records into the persistent state.
    if !req.dry_run {
        let mut dirty = false;
        for (id, outcome) in report.outcomes.iter().enumerate() {
            let Some(outcome) = outcome else {
                continue;
            };
            if !matches!(outcome.status, StepStatus::Built | StepStatus::Cached) {
                continue;
            }
            let step = &plan.steps[id];
            let mut outputs = vec![util::display_path(&loaded.root, &step.output)];
            for extra in &step.extra_outputs {
                outputs.push(util::display_path(&loaded.root, extra));
            }
            state.insert(
                util::display_path(&loaded.root, &step.output),
                StepRecord {
                    fingerprint: outcome.fingerprint.clone(),
                    deps: outcome.deps.clone(),
                    outputs,
                    duration_ms: outcome.duration.as_millis() as u64,
                    updated: now_unix(),
                },
            );
            dirty = true;
        }
        // A no-op build leaves every record untouched. Writing the state file
        // back would only rewrite identical bytes, so skip it entirely.
        let keep: HashSet<String> = plan
            .steps
            .iter()
            .map(|step| util::display_path(&loaded.root, &step.output))
            .collect();
        let before = state.steps.len();
        state.prune(&loaded.root, &keep);
        dirty |= state.steps.len() != before;
        if dirty {
            state.save(&state_path)?;
        }
    }

    let summary = summarize(&plan, &report, loaded, req, counts, total);

    if req.timings || loaded.config.build.timings {
        write_timings(&plan, &report, &profile.name, &state_dir)?;
    }

    if interactive {
        print_summary(&painter, &summary, req);
    }

    Ok(summary)
}

#[derive(Default)]
struct Counts {
    built: usize,
    cached: usize,
    up_to_date: usize,
    failed: usize,
}

impl Counts {
    fn observe(&mut self, outcome: &StepOutcome) {
        match outcome.status {
            StepStatus::Built => self.built += 1,
            StepStatus::Cached => self.cached += 1,
            StepStatus::UpToDate => self.up_to_date += 1,
            StepStatus::Failed => self.failed += 1,
            StepStatus::DryRun => {}
        }
    }
}

fn summarize(
    plan: &Plan,
    report: &exec::RunReport,
    loaded: &LoadedConfig,
    req: &BuildRequest,
    counts: Counts,
    total: usize,
) -> BuildSummary {
    let mut observed = Counts::default();
    let mut not_run = 0;
    for outcome in &report.outcomes {
        match outcome {
            Some(outcome) => observed.observe(outcome),
            None => not_run += 1,
        }
    }
    let _ = counts;
    let _ = total;
    let artifacts = plan
        .artifacts
        .iter()
        .map(|(name, artifact)| {
            (
                name.clone(),
                artifact.output.clone(),
                artifact.output.exists(),
            )
        })
        .collect();
    let _ = req;
    let _ = loaded;
    BuildSummary {
        built: observed.built,
        cached: observed.cached,
        up_to_date: observed.up_to_date,
        failed: observed.failed,
        not_run,
        duration: report.elapsed,
        artifacts,
    }
}

fn print_outcome(
    painter: &Painter,
    outcome: &StepOutcome,
    done: usize,
    total: usize,
    req: &BuildRequest,
) {
    let width = total.to_string().len().max(1);
    let progress = format!("[{:>width$}/{}]", done, total, width = width);
    let status = match outcome.status {
        StepStatus::Built => painter.green("built"),
        StepStatus::Cached => painter.cyan("cached"),
        StepStatus::UpToDate => painter.dim("up-to-date"),
        StepStatus::DryRun => painter.cyan("would build"),
        StepStatus::Failed => painter.red("FAILED"),
    };
    let duration = util::format_duration(outcome.duration);
    let label = truncate(&outcome.label, 64);

    println!(
        "{} {:<70} {:>8}  {}",
        painter.dim(&progress),
        label,
        painter.dim(&duration),
        status
    );

    if (req.verbose || req.print_commands) && outcome.status != StepStatus::UpToDate {
        println!("      {}", painter.dim(&format!("$ {}", outcome.command)));
    }
    if req.explain && outcome.status == StepStatus::Built {
        println!(
            "      {}",
            painter.dim(&format!("reason: {}", outcome.reason))
        );
    }
    if outcome.status == StepStatus::Failed {
        if let Some(code) = outcome.exit_code {
            println!("      {}", painter.red(&format!("exit code {code}")));
        }
        for line in outcome.output.lines() {
            println!("      {line}");
        }
        println!();
    } else if req.verbose && !outcome.output.trim().is_empty() {
        for line in outcome.output.lines() {
            println!("      {}", painter.dim(line));
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

fn print_summary(painter: &Painter, summary: &BuildSummary, req: &BuildRequest) {
    println!();
    if summary.success() {
        let mut parts = Vec::new();
        if summary.built > 0 {
            parts.push(format!("{} built", summary.built));
        }
        if summary.cached > 0 {
            parts.push(format!("{} cached", summary.cached));
        }
        if summary.up_to_date > 0 {
            parts.push(format!("{} up-to-date", summary.up_to_date));
        }
        if parts.is_empty() {
            parts.push("nothing to do".to_string());
        }
        println!(
            "{} {} in {}",
            painter.green("Finished"),
            parts.join(", "),
            painter.bold(&util::format_duration(summary.duration))
        );
        if !req.quiet {
            for (name, path, exists) in &summary.artifacts {
                let marker = if *exists {
                    painter.green("->")
                } else {
                    painter.dim("->")
                };
                println!(
                    "  {} {} {}",
                    marker,
                    painter.bold(name),
                    painter.dim(&path.display().to_string())
                );
            }
        }
    } else {
        let mut text = format!("{} step(s) failed", summary.failed);
        if summary.not_run > 0 {
            text.push_str(&format!(", {} not run", summary.not_run));
        }
        println!("{} {}", painter.red("error:"), text);
    }
}

#[derive(Serialize)]
struct TimingReport {
    profile: String,
    elapsed_ms: u128,
    steps: Vec<TimingStep>,
}

#[derive(Serialize, Clone)]
struct TimingStep {
    label: String,
    target: String,
    status: &'static str,
    duration_ms: u128,
}

fn status_name(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Built => "built",
        StepStatus::Cached => "cached",
        StepStatus::UpToDate => "up-to-date",
        StepStatus::DryRun => "dry-run",
        StepStatus::Failed => "failed",
    }
}

fn write_timings(
    plan: &Plan,
    report: &exec::RunReport,
    profile: &str,
    state_dir: &Path,
) -> Result<()> {
    let mut steps: Vec<TimingStep> = report
        .outcomes
        .iter()
        .flatten()
        .map(|outcome| TimingStep {
            label: outcome.label.clone(),
            target: outcome.target.clone(),
            status: status_name(outcome.status),
            duration_ms: outcome.duration.as_millis(),
        })
        .collect();
    steps.sort_by_key(|s| std::cmp::Reverse(s.duration_ms));
    let timing = TimingReport {
        profile: profile.to_string(),
        elapsed_ms: report.elapsed.as_millis(),
        steps: steps.clone(),
    };
    let path = state_dir.join("timings.json");
    let text = serde_json::to_string_pretty(&timing)?;
    util::atomic_write(&path, text.as_bytes())?;

    println!();
    println!("Top steps:");
    for step in steps.iter().take(10) {
        if step.duration_ms == 0 {
            continue;
        }
        println!(
            "  {:>8}  {:<50} {}",
            format!("{}ms", step.duration_ms),
            step.label,
            step.status
        );
    }
    println!("  timings written to {}", path.display());
    let _ = plan;
    Ok(())
}

pub fn clean(loaded: &LoadedConfig, all: bool) -> Result<()> {
    let out_dir = &loaded.out_dir;
    let state_dir = loaded.state_dir();
    let mut removed = Vec::new();

    if out_dir.exists() {
        fs::remove_dir_all(out_dir)
            .with_context(|| format!("cannot remove `{}`", out_dir.display()))?;
        removed.push(out_dir.clone());
    }
    if all {
        if state_dir.exists() {
            fs::remove_dir_all(&state_dir)
                .with_context(|| format!("cannot remove `{}`", state_dir.display()))?;
            removed.push(state_dir.clone());
        }
    } else {
        let state_file = state_dir.join("state.json");
        if state_file.exists() {
            fs::remove_file(&state_file)?;
            removed.push(state_file);
        }
    }

    if removed.is_empty() {
        println!("nothing to clean");
    } else {
        for path in removed {
            println!("removed {}", path.display());
        }
    }
    Ok(())
}

pub fn list(loaded: &LoadedConfig) -> Result<()> {
    let painter = Painter::new(util::use_color());
    println!(
        "{} {} ({})",
        painter.bold(&loaded.config.project.name),
        painter.dim(&format!("v{}", loaded.config.project.version)),
        loaded.path.display()
    );
    println!();
    for (name, target) in &loaded.config.target {
        let links = if target.links.is_empty() {
            String::new()
        } else {
            format!(" -> {}", target.links.join(", "))
        };
        println!(
            "  {:<20} {:<10} {}{}",
            painter.bold(name),
            target.kind.as_str(),
            target.sources.join(", "),
            painter.dim(&links)
        );
    }
    Ok(())
}

pub fn doctor(loaded: &LoadedConfig) -> Result<()> {
    let toolchain = Toolchain::detect(&loaded.config.compiler, &loaded.root)?;
    let painter = Painter::new(util::use_color());
    let _ = &painter;
    println!("WBuild {}", env!("CARGO_PKG_VERSION"));
    println!("  config    {}", loaded.path.display());
    println!("  root      {}", loaded.root.display());
    println!("  out_dir   {}", loaded.out_dir.display());
    println!(
        "  host      {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("  profile   {}", loaded.config.build.profile);
    println!();
    for (label, value) in toolchain.describe() {
        println!("  {:<8} {}", label, value);
    }
    Ok(())
}

pub fn graph(loaded: &LoadedConfig, profile: Option<&str>, output: Option<&Path>) -> Result<()> {
    let profile = loaded.profile(profile)?;
    let toolchain = Toolchain::detect(&loaded.config.compiler, &loaded.root)?;
    let plan = Planner::build(loaded, &profile, &toolchain, &[])?;

    let mut dot = String::from(
        "digraph wbuild {\n  rankdir=LR;\n  node [shape=box, fontname=\"monospace\"];\n",
    );
    for step in &plan.steps {
        dot.push_str(&format!(
            "  s{} [label=\"{}\"];\n",
            step.id,
            escape_dot(&step.label)
        ));
    }
    for (id, deps) in plan.dag.deps.iter().enumerate() {
        for dep in deps {
            dot.push_str(&format!("  s{dep} -> s{id};\n"));
        }
    }
    dot.push_str("}\n");

    match output {
        Some(path) => {
            util::atomic_write(path, dot.as_bytes())?;
            println!("graph written to {}", path.display());
        }
        None => print!("{dot}"),
    }
    Ok(())
}

fn escape_dot(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn init(directory: &Path, name: Option<&str>, force: bool) -> Result<()> {
    let dir = util::abs_path(&util::current_dir(), directory);
    fs::create_dir_all(&dir)?;
    let config_path = dir.join("wbuild.toml");
    if config_path.exists() && !force {
        bail!(
            "`{}` already exists; pass --force to overwrite",
            config_path.display()
        );
    }
    let project_name = name.map(str::to_string).unwrap_or_else(|| {
        dir.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "app".to_string())
    });

    let config = format!(
        r#"[project]
name = "{project_name}"
version = "0.1.0"
out_dir = "build"

[build]
profile = "debug"
# cache = true
# jobs = 0

[target.app]
kind = "exe"
sources = ["src/**/*.c"]
include = ["include"]

# [target.libcore]
# kind = "staticlib"
# sources = ["core/**/*.c"]
# include = ["core/include"]
#
# [target.app2]
# kind = "exe"
# sources = ["src2/**/*.c"]
# links = ["libcore"]

# [profiles.release]
# opt = 2
# debug = false
"#
    );
    fs::create_dir_all(dir.join("src"))?;
    let main_c = dir.join("src").join("main.c");
    if !main_c.exists() || force {
        fs::write(
            &main_c,
            "#include <stdio.h>\n\nint main(void) {\n    printf(\"Hello from WBuild!\\n\");\n    return 0;\n}\n",
        )?;
    }
    fs::write(&config_path, config)?;
    println!("created {}", config_path.display());
    println!("created {}", main_c.display());
    println!("run `wbuild build` to build the project");
    Ok(())
}

/// Look up a target's artifact path without building.
pub fn artifact_paths(
    loaded: &LoadedConfig,
    profile: Option<&str>,
) -> Result<BTreeMap<String, PathBuf>> {
    let profile = loaded.profile(profile)?;
    let toolchain = Toolchain::detect(&loaded.config.compiler, &loaded.root)?;
    let plan = Planner::build(loaded, &profile, &toolchain, &[])?;
    Ok(plan
        .artifacts
        .into_iter()
        .map(|(k, v)| (k, v.output))
        .collect())
}

/// Exposed for tests and tooling: number of steps in a plan.
pub fn plan_steps(loaded: &LoadedConfig, profile: Option<&str>) -> Result<usize> {
    let profile = loaded.profile(profile)?;
    let toolchain = Toolchain::detect(&loaded.config.compiler, &loaded.root)?;
    Ok(Planner::build(loaded, &profile, &toolchain, &[])?.total_steps())
}

/// Not all step kinds produce a linkable artifact; this is used by the graph
/// renderer to keep node labels short.
pub fn is_link_step(kind: StepKind) -> bool {
    matches!(kind, StepKind::Link | StepKind::Archive)
}

fn target_is_runnable(target: &Target) -> bool {
    match target.kind {
        Kind::Exe | Kind::Cargo | Kind::Custom => true,
        Kind::Rust => matches!(target.crate_type.as_deref(), None | Some("bin")),
        _ => false,
    }
}

/// Targets that produce a directly executable artifact (as opposed to a custom
/// target, whose `program` is executed instead).
fn target_has_artifact(target: &Target) -> bool {
    match target.kind {
        Kind::Exe | Kind::Cargo => true,
        Kind::Rust => matches!(target.crate_type.as_deref(), None | Some("bin")),
        _ => false,
    }
}

fn default_runnable_target(loaded: &LoadedConfig) -> Result<String> {
    for (name, target) in &loaded.config.target {
        if target_has_artifact(target) {
            return Ok(name.clone());
        }
    }
    bail!(
        "no executable targets configured; declare one with `kind = \"exe\"` \
         or pass a target name"
    )
}

fn find_artifact<'a>(summary: &'a BuildSummary, name: &str) -> Option<&'a (String, PathBuf, bool)> {
    summary
        .artifacts
        .iter()
        .find(|(target, _, _)| target == name)
}

/// Build the selected target, then execute its artifact with `args`.
pub fn run(loaded: &LoadedConfig, req: &BuildRequest, args: &[String]) -> Result<i32> {
    if req.targets.len() > 1 {
        bail!(
            "`run` accepts at most one target (got: {})",
            req.targets.join(", ")
        );
    }
    let selected = match req.targets.first() {
        Some(name) => {
            if !loaded.config.target.contains_key(name) {
                bail!("unknown target `{name}`");
            }
            name.clone()
        }
        None => default_runnable_target(loaded)?,
    };
    let target = loaded
        .config
        .target
        .get(&selected)
        .expect("target existence checked above");
    if !target_is_runnable(target) {
        bail!(
            "target `{selected}` is of kind `{}`, which cannot be executed",
            target.kind.as_str()
        );
    }

    if target.kind == Kind::Custom {
        // Build only the custom target's dependencies, then run its program
        // with the configured arguments. Running the custom step itself would
        // execute the program twice (once during the build, once here).
        let mut dep_req = req.clone();
        dep_req.targets = target.links.clone();
        if !dep_req.targets.is_empty() {
            let summary = build(loaded, &dep_req)?;
            if !summary.success() {
                return Ok(1);
            }
        }
        if req.dry_run {
            return Ok(0);
        }
        let compiler = crate::plan::compiler_for(loaded, target)?;
        let extra_paths: Vec<PathBuf> = compiler
            .extra_paths
            .iter()
            .map(|path| util::abs_path(&loaded.root, path))
            .collect();
        let program_text = target
            .program
            .as_deref()
            .context("custom target requires `program`")?;
        let program = crate::plan::resolve_program(loaded, program_text, &extra_paths)?;
        let workdir = target
            .workdir
            .as_ref()
            .map(|dir| util::abs_path(&loaded.root, dir))
            .unwrap_or_else(|| loaded.root.clone());
        println!("running {} {}", selected, program.display());
        let mut command = Command::new(&program);
        command.args(&target.args).args(args).current_dir(&workdir);
        for (key, value) in &target.env {
            command.env(key, value);
        }
        let status = command
            .status()
            .with_context(|| format!("cannot execute `{}`", program.display()))?;
        return Ok(status.code().unwrap_or(1));
    }

    let summary = build(loaded, req)?;
    if !summary.success() {
        return Ok(1);
    }
    let path = find_artifact(&summary, &selected)
        .map(|(_, path, _)| path.clone())
        .with_context(|| format!("target `{selected}` produced no artifact"))?;
    if !path.is_file() {
        bail!("artifact `{}` does not exist", path.display());
    }

    println!("running {} {}", selected, path.display());
    let mut command = Command::new(&path);
    command.args(args).current_dir(&loaded.root);
    for (key, value) in &target.env {
        command.env(key, value);
    }
    let status = command
        .status()
        .with_context(|| format!("cannot execute `{}`", path.display()))?;
    Ok(status.code().unwrap_or(1))
}

/// Build, then run every target marked with `test = true`.
pub fn test(loaded: &LoadedConfig, req: &BuildRequest) -> Result<i32> {
    let summary = build(loaded, req)?;
    if !summary.success() {
        return Ok(1);
    }

    let names: Vec<String> = loaded
        .config
        .target
        .iter()
        .filter(|(_, target)| target.test && target_has_artifact(target))
        .map(|(name, _)| name.clone())
        .collect();
    if names.is_empty() {
        bail!(
            "no test targets configured; mark at least one executable target with \
             `test = true`"
        );
    }

    let mut ran = 0usize;
    let mut failed = 0usize;
    for name in names {
        let Some((_, path, exists)) = find_artifact(&summary, &name) else {
            continue;
        };
        if !exists {
            eprintln!("test {name}: artifact `{}` is missing", path.display());
            failed += 1;
            continue;
        }
        let target = &loaded.config.target[&name];
        println!("test {name} ({})", path.display());
        let mut command = Command::new(path);
        command.current_dir(&loaded.root);
        for (key, value) in &target.env {
            command.env(key, value);
        }
        match command.status() {
            Ok(status) if status.success() => println!("  ok"),
            Ok(status) => {
                failed += 1;
                println!("  FAILED (exit code {:?})", status.code());
            }
            Err(err) => {
                failed += 1;
                println!("  FAILED ({err})");
            }
        }
        ran += 1;
    }

    if ran == 0 {
        bail!("none of the configured test targets were selected");
    }
    println!();
    if failed == 0 {
        println!("all {ran} test target(s) passed");
        Ok(0)
    } else {
        println!("{failed} of {ran} test target(s) failed");
        Ok(1)
    }
}

/// Print the fully resolved configuration without building anything.
pub fn configure(loaded: &LoadedConfig, requested: Option<&str>) -> Result<()> {
    let profile = loaded.profile(requested)?;
    let painter = Painter::new(util::use_color());
    println!(
        "{} {}",
        painter.bold(&loaded.config.project.name),
        painter.dim(&format!("v{}", loaded.config.project.version))
    );
    println!("  config    {}", loaded.path.display());
    println!("  root      {}", loaded.root.display());
    println!("  out_dir   {}", loaded.out_dir.display());
    println!(
        "  profile   {} (opt={}, debug={}, lto={})",
        painter.cyan(&profile.name),
        profile.opt.as_str(),
        profile.debug,
        profile.lto
    );
    println!(
        "  cache     {}",
        if loaded.config.build.cache {
            "enabled"
        } else {
            "disabled"
        }
    );
    if !profile.defines.is_empty() {
        println!("  defines   {}", profile.defines.join(" "));
    }

    println!();
    println!("targets:");
    for (name, target) in &loaded.config.target {
        let mut tags = vec![target.kind.as_str()];
        if target.test {
            tags.push("test");
        }
        let detail = match target.kind {
            Kind::Custom => {
                let mut outputs = target.outputs.clone();
                if let Some(stdout) = &target.stdout {
                    outputs.push(format!("{stdout} (stdout)"));
                }
                format!(
                    "{} -> {}",
                    target.program.as_deref().unwrap_or("<missing program>"),
                    outputs.join(", ")
                )
            }
            Kind::Cargo => target
                .manifest
                .clone()
                .unwrap_or_else(|| "<missing manifest>".to_string()),
            Kind::Rust => target
                .sources
                .first()
                .cloned()
                .unwrap_or_else(|| "<no crate root>".to_string()),
            _ => format!("{} source(s)", target.sources.len()),
        };
        println!(
            "  {:<18} {:<12} {}",
            painter.bold(name),
            painter.dim(&tags.join(",")),
            detail
        );
    }
    Ok(())
}

/// Print the slowest steps from the last `--timings` build.
pub fn profile_report(loaded: &LoadedConfig) -> Result<()> {
    let path = loaded.state_dir().join("timings.json");
    if !path.is_file() {
        bail!(
            "no timing report at `{}`; run `wbuild build --timings` first",
            path.display()
        );
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("cannot read `{}`", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).with_context(|| "cannot parse timing report")?;
    let profile = json
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let elapsed = json.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);
    println!(
        "profile {}  total {}  ({})",
        profile,
        util::format_duration(Duration::from_millis(elapsed)),
        path.display()
    );

    let steps = json
        .get("steps")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if steps.is_empty() {
        println!("  no steps recorded");
        return Ok(());
    }
    println!();
    for step in steps.iter().take(20) {
        let label = step
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>");
        let target = step.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let status = step.get("status").and_then(|v| v.as_str()).unwrap_or("?");
        let ms = step
            .get("duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        println!(
            "  {:>9}  {:<48} {:<10} {}",
            format!("{ms}ms"),
            truncate(label, 48),
            status,
            target
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheAction {
    Stats,
    Clean,
    Dir,
}

/// Inspect or clear the content-addressed cache.
pub fn cache(loaded: &LoadedConfig, action: CacheAction) -> Result<()> {
    let state_dir = loaded.state_dir();
    let cache = Cache::new(&state_dir, true);
    let root = cache.root();

    match action {
        CacheAction::Dir => println!("{}", root.display()),
        CacheAction::Clean => {
            cache
                .clear()
                .with_context(|| format!("cannot clear cache `{}`", root.display()))?;
            println!("cleared {}", root.display());
        }
        CacheAction::Stats => {
            if !root.exists() {
                println!("cache is empty ({})", root.display());
                return Ok(());
            }
            let mut objects = 0u64;
            let mut bytes = 0u64;
            let mut indexes = 0u64;
            for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
                match rel.components().next().map(|c| c.as_os_str()) {
                    Some(first) if first == "objects" => {
                        objects += 1;
                        bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                    }
                    Some(first) if first == "index" => indexes += 1,
                    _ => {}
                }
            }
            println!("cache     {}", root.display());
            println!("  objects {} ({})", objects, format_bytes(bytes));
            println!("  index   {indexes} command(s)");
        }
    }
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Print version and host information; works outside a project.
pub fn version() {
    println!("WBuild {}", env!("CARGO_PKG_VERSION"));
    println!(
        "  host      {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("  cache     content-addressed local cache");
}
