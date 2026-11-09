//! Parallel step executor.
//!
//! The executor runs a dependency-aware worker pool. Steps are pure
//! `(program, args, env)` descriptions; dependencies are resolved by the DAG,
//! and incrementality/caching decisions are made per step.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::unbounded;

use crate::cache::Cache;
use crate::depfile;
use crate::hash::{FileHasher, Fingerprint};
use crate::plan::{DepSource, Plan, Step, StepKind};
use crate::state::State;
use crate::util;

#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub jobs: usize,
    pub force: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub quiet: bool,
    pub print_commands: bool,
    pub explain: bool,
    pub no_cache: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Built,
    Cached,
    UpToDate,
    DryRun,
    Failed,
}

#[derive(Debug, Clone)]
pub struct StepOutcome {
    pub step_id: usize,
    pub target: String,
    pub label: String,
    pub status: StepStatus,
    pub duration: Duration,
    pub fingerprint: String,
    pub deps: Vec<String>,
    pub output: String,
    pub exit_code: Option<i32>,
    pub command: String,
    pub reason: String,
}

impl StepOutcome {
    pub fn succeeded(&self) -> bool {
        self.status != StepStatus::Failed
    }
}

pub struct ExecContext<'a> {
    pub root: PathBuf,
    pub state: &'a State,
    pub cache: Cache,
    pub hasher: FileHasher,
    pub toolchain_fp: String,
    pub options: ExecOptions,
}

pub struct RunReport {
    pub outcomes: Vec<Option<StepOutcome>>,
    pub elapsed: Duration,
    pub failed: bool,
}

/// Execute all steps in `plan`, honouring dependencies and the job limit.
///
/// `on_done` is called from the coordinating thread, one step at a time, which
/// keeps reporting deterministic even with many workers.
pub fn run<'a, F>(plan: &'a Plan, ctx: &'a ExecContext<'a>, mut on_done: F) -> RunReport
where
    F: FnMut(&StepOutcome, usize, usize),
{
    let started = Instant::now();
    let total = plan.steps.len();
    let jobs = effective_jobs(ctx.options.jobs, total);

    let (job_tx, job_rx) = unbounded::<usize>();
    let (res_tx, res_rx) = unbounded::<StepOutcome>();

    let mut indegree = plan.dag.indegree.clone();
    let priority = critical_priorities(&plan.dag, total);
    let mut ready: BinaryHeap<(usize, Reverse<usize>)> = (0..total)
        .filter(|&i| indegree[i] == 0)
        .map(|i| (priority[i], Reverse(i)))
        .collect();
    let mut results: Vec<Option<StepOutcome>> = (0..total).map(|_| None).collect();
    let mut running = 0usize;
    let mut completed = 0usize;
    let mut failed = false;

    // Scoped threads let every worker borrow the plan and execution context
    // directly. This avoids cloning the whole step list and state on every
    // build, which used to dominate startup time on large projects.
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(jobs);
        for _ in 0..jobs {
            let job_rx = job_rx.clone();
            let res_tx = res_tx.clone();
            handles.push(scope.spawn(move || {
                while let Ok(id) = job_rx.recv() {
                    let outcome = execute_step(&plan.steps[id], ctx);
                    if res_tx.send(outcome).is_err() {
                        break;
                    }
                }
            }));
        }
        drop(job_rx);
        drop(res_tx);

        loop {
            while !failed && running < jobs {
                let Some((_, Reverse(id))) = ready.pop() else {
                    break;
                };
                if job_tx.send(id).is_err() {
                    break;
                }
                running += 1;
            }

            if running == 0 {
                break;
            }

            let Ok(outcome) = res_rx.recv() else {
                break;
            };
            running -= 1;
            completed += 1;

            let id = outcome.step_id;
            let ok = outcome.succeeded();
            if !ok {
                failed = true;
            }
            on_done(&outcome, completed, total);
            results[id] = Some(outcome);

            if ok {
                for &next in &plan.dag.dependents[id] {
                    indegree[next] -= 1;
                    if indegree[next] == 0 {
                        ready.push((priority[next], Reverse(next)));
                    }
                }
            }
        }

        drop(job_tx);
        for handle in handles {
            let _ = handle.join();
        }
    });

    // Drain any results still in flight (normally none).
    while let Ok(outcome) = res_rx.try_recv() {
        let id = outcome.step_id;
        if results[id].is_none() {
            if !outcome.succeeded() {
                failed = true;
            }
            on_done(&outcome, completed, total);
            results[id] = Some(outcome);
        }
    }

    RunReport {
        outcomes: results,
        elapsed: started.elapsed(),
        failed,
    }
}

fn effective_jobs(requested: usize, total: usize) -> usize {
    let detected = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let jobs = if requested == 0 { detected } else { requested };
    jobs.clamp(1, total.max(1))
}

/// Priority of every step: the length of the longest chain of dependents that
/// still has to run. Starting the longest chain first keeps the critical path
/// busy and leaves short leaf steps for the tail of the build.
fn critical_priorities(dag: &crate::graph::Dag, total: usize) -> Vec<usize> {
    let mut priority = vec![0usize; total];
    let order = dag.topo_order().unwrap_or_else(|_| (0..total).collect());
    for &id in order.iter().rev() {
        let best = dag.dependents[id]
            .iter()
            .map(|&next| priority[next] + 1)
            .max()
            .unwrap_or(0);
        priority[id] = best;
    }
    priority
}

fn execute_step(step: &Step, ctx: &ExecContext<'_>) -> StepOutcome {
    let started = Instant::now();
    // Building the full command line is only useful when it will be shown.
    let command = if ctx.options.verbose || ctx.options.print_commands {
        step.command_line()
    } else {
        String::new()
    };
    let base = |status: StepStatus, reason: &str| StepOutcome {
        step_id: step.id,
        target: step.target.clone(),
        label: step.label.clone(),
        status,
        duration: started.elapsed(),
        fingerprint: String::new(),
        deps: Vec::new(),
        output: String::new(),
        exit_code: None,
        command: command.clone(),
        reason: reason.to_string(),
    };

    let key = util::display_path(&ctx.root, &step.output);
    let record = ctx.state.record(&key);
    let prev_deps: &[String] = record.map(|r| r.deps.as_slice()).unwrap_or(&[]);

    let fp_pre = match compute_fingerprint(step, prev_deps, ctx) {
        Ok(fp) => fp,
        Err(err) => {
            let mut outcome = base(StepStatus::Failed, &format!("{err:#}"));
            outcome.output = format!("{err:#}");
            return outcome;
        }
    };

    // Stat the outputs at most once per step; the answer is reused by both the
    // rebuild reason and the up-to-date decision.
    let outputs_present = !ctx.options.force && record.is_some() && outputs_exist(step);
    let reason = if ctx.options.force {
        "forced rebuild"
    } else if record.is_none() {
        "first build"
    } else if !outputs_present {
        "output missing"
    } else {
        "inputs or command changed"
    };

    if !ctx.options.force {
        if let Some(record) = record {
            if record.fingerprint == fp_pre && outputs_present {
                let mut outcome = base(StepStatus::UpToDate, "up to date");
                outcome.fingerprint = fp_pre;
                // The persisted record already holds this step's dependency
                // list, so there is no need to copy it into the outcome.
                return outcome;
            }
        }
    }

    if ctx.options.dry_run {
        let mut outcome = base(StepStatus::DryRun, reason);
        outcome.fingerprint = fp_pre;
        outcome.deps = prev_deps.to_vec();
        return outcome;
    }

    // Cache lookup. The command fingerprint deliberately excludes discovered
    // dependencies: they are validated separately against the cached variant's
    // dependency hash, so a cache entry survives `wbuild clean` and can be
    // reused whenever the same headers are back in play. It is computed once
    // and reused when storing the result below.
    let cache_enabled = ctx.cache.enabled() && !ctx.options.force && !ctx.options.no_cache;
    let command_fp = if cache_enabled && step.cacheable {
        Some(compute_fingerprint(step, &[], ctx).unwrap_or_default())
    } else {
        None
    };
    if let Some(command_fp) = &command_fp {
        for entry in ctx.cache.index(command_fp) {
            if deps_fingerprint(&entry.deps, ctx) != entry.hash {
                continue;
            }
            if entry.extras.len() != step.extra_outputs.len() {
                continue;
            }
            let Some(cached) = ctx.cache.artifact(&entry.entry, step.cache_ext) else {
                continue;
            };
            let mut cached_extras = Vec::with_capacity(entry.extras.len());
            let mut extras_ok = true;
            for fingerprint in &entry.extras {
                match ctx.cache.artifact(fingerprint, step.cache_ext) {
                    Some(path) => cached_extras.push(path),
                    None => {
                        extras_ok = false;
                        break;
                    }
                }
            }
            if !extras_ok {
                continue;
            }
            match restore_cached(step, &cached, &cached_extras) {
                Ok(()) if outputs_exist(step) => {
                    let fp_post = if entry.deps.as_slice() == prev_deps {
                        fp_pre.clone()
                    } else {
                        compute_fingerprint(step, &entry.deps, ctx)
                            .unwrap_or_else(|_| entry.entry.clone())
                    };
                    let mut outcome = base(StepStatus::Cached, "cache hit");
                    outcome.fingerprint = fp_post;
                    outcome.deps = entry.deps;
                    return outcome;
                }
                Ok(()) => {
                    // Restored files did not land where the step expects them.
                }
                Err(err) => {
                    // Corrupt/unreadable cache entry: fall through to a real build.
                    if ctx.options.verbose {
                        eprintln!("warning: cache restore failed: {err:#}");
                    }
                }
            }
        }
    }

    if let Err(err) = ensure_parents(step) {
        let mut outcome = base(StepStatus::Failed, "cannot create output directory");
        outcome.output = format!("{err:#}");
        return outcome;
    }

    // Execute.
    let (output_text, exit_code, success) = if step.kind == StepKind::Copy {
        match copy_step(step) {
            Ok(()) => (String::new(), Some(0), true),
            Err(err) => (format!("{err:#}"), None, false),
        }
    } else {
        match run_command(step) {
            Ok(out) => {
                let mut text = String::new();
                text.push_str(&String::from_utf8_lossy(&out.stdout));
                if !out.stderr.is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&String::from_utf8_lossy(&out.stderr));
                }
                (text, out.status.code(), out.status.success())
            }
            Err(err) => (
                format!("failed to start `{}`: {err}", step.program.display()),
                None,
                false,
            ),
        }
    };

    if !success {
        let mut outcome = base(StepStatus::Failed, "command failed");
        outcome.fingerprint = fp_pre;
        outcome.exit_code = exit_code;
        let cleaned = if step.dep_source == DepSource::MsvcIncludes {
            depfile::strip_msvc_includes(&output_text)
        } else {
            output_text
        };
        outcome.output = cleaned;
        return outcome;
    }

    // Discover implicit dependencies.
    let mut deps = match collect_deps(step, &output_text, &ctx.root) {
        Ok(deps) => deps,
        Err(err) => {
            let mut outcome = base(StepStatus::Failed, "cannot parse dependencies");
            outcome.fingerprint = fp_pre;
            outcome.output = format!("{err:#}");
            return outcome;
        }
    };
    deps.sort();
    deps.dedup();

    let fp_post = if deps.as_slice() == prev_deps {
        // Common case: the dependency set did not change, so the fingerprint
        // computed before the build is still valid.
        fp_pre.clone()
    } else {
        match compute_fingerprint(step, &deps, ctx) {
            Ok(fp) => fp,
            Err(err) => {
                let mut outcome = base(StepStatus::Failed, &format!("{err:#}"));
                outcome.output = format!("{err:#}");
                return outcome;
            }
        }
    };

    if let Some(command_fp) = &command_fp {
        let mut extras = Vec::new();
        let mut extras_ok = true;
        for (index, extra) in step.extra_outputs.iter().enumerate() {
            let fingerprint = format!("{fp_post}-x{index}");
            match ctx.cache.store(&fingerprint, step.cache_ext, extra) {
                Ok(()) => extras.push(fingerprint),
                Err(err) => {
                    extras_ok = false;
                    if ctx.options.verbose {
                        eprintln!("warning: {err:#}");
                    }
                    break;
                }
            }
        }
        if let Err(err) = ctx.cache.store(&fp_post, step.cache_ext, &step.output) {
            if ctx.options.verbose {
                eprintln!("warning: {err:#}");
            }
        }
        if extras_ok {
            let entry = crate::cache::CacheEntry {
                hash: deps_fingerprint(&deps, ctx),
                deps: deps.clone(),
                entry: fp_post.clone(),
                extras,
            };
            if let Err(err) = ctx.cache.upsert(command_fp, entry) {
                if ctx.options.verbose {
                    eprintln!("warning: {err:#}");
                }
            }
        }
    }

    let mut outcome = base(StepStatus::Built, reason);
    outcome.fingerprint = fp_post;
    outcome.deps = deps;
    outcome.exit_code = exit_code;
    outcome.output = if step.dep_source == DepSource::MsvcIncludes {
        depfile::strip_msvc_includes(&output_text)
    } else {
        output_text
    };
    outcome
}

fn outputs_exist(step: &Step) -> bool {
    step.output.is_file() && step.extra_outputs.iter().all(|p| p.is_file())
}

fn ensure_parents(step: &Step) -> std::io::Result<()> {
    fs::create_dir_all(&step.workdir)?;
    if let Some(parent) = step.output.parent() {
        fs::create_dir_all(parent)?;
    }
    for extra in &step.extra_outputs {
        if let Some(parent) = extra.parent() {
            fs::create_dir_all(parent)?;
        }
    }
    if let Some(stdout) = &step.stdout {
        if let Some(parent) = stdout.parent() {
            fs::create_dir_all(parent)?;
        }
    }
    if let Some(depfile) = &step.depfile {
        if let Some(parent) = depfile.parent() {
            fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

fn run_command(step: &Step) -> std::io::Result<std::process::Output> {
    let mut cmd = std::process::Command::new(&step.program);
    cmd.args(&step.args);
    cmd.current_dir(&step.workdir);
    for (k, v) in &step.env {
        cmd.env(k, v);
    }
    cmd.stdin(std::process::Stdio::null());
    if let Some(stdout) = &step.stdout {
        let file = fs::File::create(stdout)?;
        cmd.stdout(std::process::Stdio::from(file));
    }
    cmd.output()
}

fn copy_step(step: &Step) -> Result<()> {
    let from = step
        .copy_from
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("internal error: copy step without source"))?;
    util::atomic_copy(from, &step.output)?;
    util::make_executable(&step.output)?;
    Ok(())
}

fn restore_cached(step: &Step, cached: &Path, cached_extras: &[PathBuf]) -> Result<()> {
    util::atomic_copy(cached, &step.output)?;
    for (extra, cached_extra) in step.extra_outputs.iter().zip(cached_extras) {
        util::atomic_copy(cached_extra, extra)?;
    }
    // Only executables need the extra permission bit; setting it on objects is
    // harmless on Unix.
    if matches!(step.kind, StepKind::Link | StepKind::Rustc) {
        util::make_executable(&step.output)?;
    }
    Ok(())
}

/// Compute the fingerprint for a step given its dependency set (stored paths).
fn compute_fingerprint(step: &Step, deps: &[String], ctx: &ExecContext<'_>) -> Result<String> {
    let mut fp = Fingerprint::new().tag("wbuild-step-v2");
    fp.text(step.toolchain_fp.as_deref().unwrap_or(&ctx.toolchain_fp));
    fp.text(step.kind.tag());
    fp.path(&step.program);
    fp.args(&step.args);
    for (key, value) in &step.env {
        fp.text(key);
        fp.text(value);
    }
    fp.path(&step.workdir);
    fp.path(&step.output);
    for extra in &step.extra_outputs {
        fp.path(extra);
    }
    if let Some(stdout) = &step.stdout {
        fp.path(stdout);
    }
    for input in &step.inputs {
        fp.path(input);
        fp.text(&ctx.hasher.hash_opt(input));
    }
    for dep in deps {
        let path = resolve_dep(&ctx.root, dep);
        fp.text(dep);
        fp.text(&ctx.hasher.hash_opt(&path));
    }
    if let Some(depfile) = &step.depfile {
        fp.path(depfile);
    }
    Ok(fp.finish())
}

/// Fingerprint of the discovered dependency set and the contents of every
/// dependency. Used to validate cached variants whose command fingerprint is
/// unchanged but whose headers may have moved on.
fn deps_fingerprint(deps: &[String], ctx: &ExecContext<'_>) -> String {
    let mut fp = Fingerprint::new().tag("wbuild-deps-v1");
    for dep in deps {
        let path = resolve_dep(&ctx.root, dep);
        fp.text(dep);
        fp.text(&ctx.hasher.hash_opt(&path));
    }
    fp.finish()
}

fn resolve_dep(root: &Path, dep: &str) -> PathBuf {
    let path = PathBuf::from(dep);
    if path.is_absolute() {
        path
    } else {
        util::normalize(&root.join(path))
    }
}

fn collect_deps(step: &Step, command_output: &str, root: &Path) -> Result<Vec<String>> {
    let raw: Vec<String> = match step.dep_source {
        DepSource::None => Vec::new(),
        DepSource::Makefile => {
            let Some(depfile_path) = &step.depfile else {
                return Ok(Vec::new());
            };
            let text = fs::read_to_string(depfile_path).map_err(|err| {
                anyhow::anyhow!(
                    "cannot read dependency file `{}`: {err}",
                    depfile_path.display()
                )
            })?;
            depfile::parse_make_deps(&text)
        }
        DepSource::MsvcIncludes => depfile::parse_msvc_includes(command_output),
    };

    let mut out = Vec::with_capacity(raw.len());
    for dep in raw {
        let path = PathBuf::from(&dep);
        let abs = if path.is_absolute() {
            util::normalize(&path)
        } else {
            util::normalize(&step.workdir.join(path))
        };
        out.push(util::display_path(root, &abs));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_paths_are_relativized() {
        let root = Path::new("C:/proj").to_path_buf();
        let deps = ["C:/proj/src/a.h".to_string()];
        let resolved = resolve_dep(&root, &deps[0]);
        assert!(resolved.ends_with("a.h"));
    }

    #[test]
    fn effective_jobs_is_bounded() {
        assert_eq!(effective_jobs(4, 2), 2);
        assert_eq!(effective_jobs(1, 10), 1);
        assert!(effective_jobs(0, 10) >= 1);
    }

    #[test]
    fn priorities_follow_longest_dependent_chain() {
        let dag = crate::graph::Dag::new(4, vec![vec![], vec![], vec![0, 1], vec![2]]).unwrap();
        let priority = critical_priorities(&dag, 4);
        assert_eq!(priority[3], 0);
        assert_eq!(priority[2], 1);
        assert_eq!(priority[0], 2);
        assert_eq!(priority[1], 2);
    }
}
