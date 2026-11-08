//! Command line interface.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::config::Config;
use crate::engine::{self, BuildRequest};
use crate::util;

#[derive(Debug, Parser)]
#[command(
    name = "wbuild",
    version,
    about = "WBuild - Fast. Simple. Incremental.",
    long_about = None,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Change to this directory before doing anything.
    #[arg(short = 'C', long = "directory", global = true, value_name = "DIR")]
    pub directory: Option<PathBuf>,

    /// Use a specific configuration file.
    #[arg(short = 'f', long = "file", global = true, value_name = "FILE")]
    pub file: Option<PathBuf>,

    /// Number of parallel jobs (0 = detect).
    #[arg(short = 'j', long = "jobs", global = true, value_name = "N")]
    pub jobs: Option<usize>,

    /// Build profile (debug, release or a custom profile).
    #[arg(short = 'p', long = "profile", global = true, value_name = "NAME")]
    pub profile: Option<String>,

    /// Increase logging verbosity (repeat for more).
    #[arg(short = 'v', long = "verbose", global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Only print errors and the final summary.
    #[arg(short = 'q', long = "quiet", global = true)]
    pub quiet: bool,

    /// Disable colored output.
    #[arg(long = "no-color", global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Build targets (all targets by default).
    Build(BuildArgs),
    /// Clean, then build.
    Rebuild(BuildArgs),
    /// Remove build artifacts.
    Clean(CleanArgs),
    /// List configured targets.
    List,
    /// Show detected toolchains and environment information.
    Doctor,
    /// Emit the build step graph in Graphviz DOT format.
    Graph(GraphArgs),
    /// Watch sources and rebuild on change.
    Watch(BuildArgs),
    /// Build a target and run its executable, forwarding trailing arguments.
    Run(RunArgs),
    /// Build and run every target marked with `test = true`.
    Test(BuildArgs),
    /// Print the resolved configuration without building.
    Configure,
    /// Show the slowest steps from the last `--timings` build.
    Profile,
    /// Inspect or clear the local build cache.
    Cache(CacheArgs),
    /// Print version and host information.
    Version,
    /// Create a starter project.
    Init(InitArgs),
}

#[derive(Debug, Args, Default)]
pub struct BuildArgs {
    /// Targets to build (default: all).
    pub targets: Vec<String>,

    /// Rebuild everything, ignoring fingerprints and cache.
    #[arg(long)]
    pub force: bool,

    /// Print commands without executing them.
    #[arg(long)]
    pub dry_run: bool,

    /// Write `.wbuild/timings.json` and print the slowest steps.
    #[arg(long)]
    pub timings: bool,

    /// Explain why steps were rebuilt.
    #[arg(long)]
    pub explain: bool,

    /// Print the exact command line of every step.
    #[arg(long = "print-commands")]
    pub print_commands: bool,

    /// Disable the local object cache for this invocation.
    #[arg(long = "no-cache")]
    pub no_cache: bool,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    /// Also remove the local cache and all incremental state.
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(flatten)]
    pub build: BuildArgs,

    /// Arguments for the program; everything after `--` is forwarded verbatim.
    #[arg(last = true, allow_hyphen_values = true, value_name = "ARGS")]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CacheActionArg {
    /// Show the number and size of cached artifacts.
    Stats,
    /// Delete every cached artifact and index entry.
    Clean,
    /// Print the cache directory.
    Dir,
}

#[derive(Debug, Args)]
pub struct CacheArgs {
    /// What to do with the cache (default: show statistics).
    #[arg(value_enum, default_value_t = CacheActionArg::Stats)]
    pub action: CacheActionArg,
}

#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Write the graph to a file instead of stdout.
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Directory to initialize (default: current directory).
    #[arg(default_value = ".")]
    pub directory: PathBuf,

    /// Project name (default: directory name).
    #[arg(long)]
    pub name: Option<String>,

    /// Overwrite an existing `wbuild.toml`.
    #[arg(long)]
    pub force: bool,
}

/// Run the CLI. Returns the process exit code.
pub fn run(cli: Cli) -> Result<i32> {
    util::enable_ansi();
    if cli.no_color {
        std::env::set_var("NO_COLOR", "1");
    }
    if let Some(dir) = &cli.directory {
        std::env::set_current_dir(dir)
            .with_context(|| format!("cannot change directory to `{}`", dir.display()))?;
    }

    match &cli.command {
        Commands::Init(args) => {
            engine::init(&args.directory, args.name.as_deref(), args.force)?;
            Ok(0)
        }
        Commands::Build(args) => run_build(&cli, args, false),
        Commands::Rebuild(args) => run_build(&cli, args, true),
        Commands::Watch(args) => {
            let loaded = load(&cli)?;
            let req = request(&cli, args, false);
            crate::watch::watch(&loaded, &req)?;
            Ok(0)
        }
        Commands::Run(args) => {
            let loaded = load(&cli)?;
            let req = request(&cli, &args.build, false);
            engine::run(&loaded, &req, &args.args)
        }
        Commands::Test(args) => {
            let loaded = load(&cli)?;
            let req = request(&cli, args, false);
            engine::test(&loaded, &req)
        }
        Commands::Configure => {
            let loaded = load(&cli)?;
            engine::configure(&loaded, cli.profile.as_deref())?;
            Ok(0)
        }
        Commands::Profile => {
            let loaded = load(&cli)?;
            engine::profile_report(&loaded)?;
            Ok(0)
        }
        Commands::Cache(args) => {
            let loaded = load(&cli)?;
            let action = match args.action {
                CacheActionArg::Stats => engine::CacheAction::Stats,
                CacheActionArg::Clean => engine::CacheAction::Clean,
                CacheActionArg::Dir => engine::CacheAction::Dir,
            };
            engine::cache(&loaded, action)?;
            Ok(0)
        }
        Commands::Version => {
            engine::version();
            Ok(0)
        }
        Commands::Clean(args) => {
            let loaded = load(&cli)?;
            engine::clean(&loaded, args.all)?;
            Ok(0)
        }
        Commands::List => {
            let loaded = load(&cli)?;
            engine::list(&loaded)?;
            Ok(0)
        }
        Commands::Doctor => {
            let loaded = load(&cli)?;
            engine::doctor(&loaded)?;
            Ok(0)
        }
        Commands::Graph(args) => {
            let loaded = load(&cli)?;
            let output = args
                .output
                .as_ref()
                .map(|p| util::abs_path(&util::current_dir(), p));
            engine::graph(&loaded, cli.profile.as_deref(), output.as_deref())?;
            Ok(0)
        }
    }
}

fn load(cli: &Cli) -> Result<crate::config::LoadedConfig> {
    Config::load(cli.file.as_deref())
}

fn run_build(cli: &Cli, args: &BuildArgs, force: bool) -> Result<i32> {
    let loaded = load(cli)?;
    let req = request(cli, args, force);
    let summary = engine::build(&loaded, &req)?;
    Ok(if summary.success() { 0 } else { 1 })
}

fn request(cli: &Cli, args: &BuildArgs, force: bool) -> BuildRequest {
    BuildRequest {
        profile: cli.profile.clone(),
        targets: args.targets.clone(),
        jobs: cli.jobs,
        force: force || args.force,
        dry_run: args.dry_run,
        verbose: cli.verbose > 0,
        quiet: cli.quiet,
        no_cache: args.no_cache,
        timings: args.timings,
        explain: args.explain,
        print_commands: args.print_commands,
    }
}
