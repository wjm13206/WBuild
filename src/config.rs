//! TOML configuration model and loader.
//!
//! The configuration stays intentionally small: a project block, a few global
//! knobs, compiler hints, profiles and a map of build targets. Everything is
//! statically typed and validated before any compilation happens.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer};

use crate::util;

pub const DEFAULT_CONFIG: &str = "wbuild.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub project: Project,
    #[serde(default)]
    pub build: Build,
    #[serde(default)]
    pub compiler: Compiler,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Build targets, declared as `[target.<name>]`.
    #[serde(default)]
    pub target: BTreeMap<String, Target>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    /// Project root, relative to the configuration file. Defaults to `.`.
    #[serde(default = "default_dot")]
    pub root: String,
    /// Directory for build artifacts. Defaults to `build`.
    #[serde(default = "default_out_dir")]
    pub out_dir: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    #[serde(default = "default_profile")]
    pub profile: String,
    /// Number of parallel jobs. `0` means "detect from the machine".
    #[serde(default)]
    pub jobs: usize,
    /// Enable the content-addressed local cache.
    #[serde(default = "default_true")]
    pub cache: bool,
    /// Directory for state, cache and dependency files. Defaults to `.wbuild`.
    #[serde(default = "default_state_dir")]
    pub state_dir: String,
    /// Collect a `timings.json` performance report.
    #[serde(default)]
    pub timings: bool,
}

impl Default for Build {
    fn default() -> Self {
        Self {
            profile: default_profile(),
            jobs: 0,
            cache: true,
            state_dir: default_state_dir(),
            timings: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compiler {
    #[serde(default = "auto")]
    pub cc: String,
    #[serde(default = "auto")]
    pub cxx: String,
    #[serde(default = "auto")]
    pub rustc: String,
    #[serde(default = "auto")]
    pub asm: String,
    #[serde(default = "auto")]
    pub ar: String,
    #[serde(default = "auto")]
    pub linker: String,
    /// Target triple for cross compilation.
    #[serde(default)]
    pub target: Option<String>,
    /// Cross compilation sysroot.
    #[serde(default)]
    pub sysroot: Option<String>,
    /// Extra directories searched before `PATH` when detecting tools.
    #[serde(default)]
    pub extra_paths: Vec<String>,
    #[serde(default)]
    pub defines: Vec<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub cflags: Vec<String>,
    #[serde(default)]
    pub cxxflags: Vec<String>,
    #[serde(default)]
    pub asmflags: Vec<String>,
    #[serde(default)]
    pub rustflags: Vec<String>,
    #[serde(default)]
    pub ldflags: Vec<String>,
}

impl Default for Compiler {
    fn default() -> Self {
        Self {
            cc: auto(),
            cxx: auto(),
            rustc: auto(),
            asm: auto(),
            ar: auto(),
            linker: auto(),
            target: None,
            sysroot: None,
            extra_paths: Vec::new(),
            defines: Vec::new(),
            include: Vec::new(),
            cflags: Vec::new(),
            cxxflags: Vec::new(),
            asmflags: Vec::new(),
            rustflags: Vec::new(),
            ldflags: Vec::new(),
        }
    }
}

/// Optimization levels. Accepts both `opt = 2` and `opt = "2"` / `"s"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    O0,
    O1,
    O2,
    O3,
    Os,
    Oz,
}

impl OptLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            OptLevel::O0 => "0",
            OptLevel::O1 => "1",
            OptLevel::O2 => "2",
            OptLevel::O3 => "3",
            OptLevel::Os => "s",
            OptLevel::Oz => "z",
        }
    }
}

impl<'de> Deserialize<'de> for OptLevel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OptVisitor;
        impl Visitor<'_> for OptVisitor {
            type Value = OptLevel;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an optimization level: 0, 1, 2, 3, s or z")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<OptLevel, E> {
                match v {
                    0 => Ok(OptLevel::O0),
                    1 => Ok(OptLevel::O1),
                    2 => Ok(OptLevel::O2),
                    3 => Ok(OptLevel::O3),
                    _ => Err(E::custom("optimization level must be 0..=3")),
                }
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<OptLevel, E> {
                self.visit_u64(v as u64)
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<OptLevel, E> {
                match v {
                    "0" => Ok(OptLevel::O0),
                    "1" => Ok(OptLevel::O1),
                    "2" => Ok(OptLevel::O2),
                    "3" => Ok(OptLevel::O3),
                    "s" | "S" => Ok(OptLevel::Os),
                    "z" | "Z" => Ok(OptLevel::Oz),
                    other => Err(E::custom(format!(
                        "unknown optimization level `{other}` (use 0, 1, 2, 3, s or z)"
                    ))),
                }
            }
        }
        deserializer.deserialize_any(OptVisitor)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Inherit unset fields from another profile (`release`, `debug`, ...).
    pub extends: Option<String>,
    pub opt: Option<OptLevel>,
    /// Emit debug information.
    pub debug: Option<bool>,
    pub lto: Option<bool>,
    #[serde(default)]
    pub defines: Vec<String>,
    #[serde(default)]
    pub cflags: Vec<String>,
    #[serde(default)]
    pub cxxflags: Vec<String>,
    #[serde(default)]
    pub asmflags: Vec<String>,
    #[serde(default)]
    pub rustflags: Vec<String>,
    #[serde(default)]
    pub ldflags: Vec<String>,
}

/// Fully resolved profile after applying built-in defaults and inheritance.
#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub name: String,
    pub opt: OptLevel,
    pub debug: bool,
    pub lto: bool,
    pub defines: Vec<String>,
    pub cflags: Vec<String>,
    pub cxxflags: Vec<String>,
    pub asmflags: Vec<String>,
    pub rustflags: Vec<String>,
    pub ldflags: Vec<String>,
}

impl Config {
    /// Load a configuration file. When `explicit` is `None` we walk up from the
    /// current directory looking for `wbuild.toml`.
    pub fn load(explicit: Option<&Path>) -> Result<LoadedConfig> {
        let path = match explicit {
            Some(p) => {
                let p = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    util::current_dir().join(p)
                };
                if !p.is_file() {
                    bail!("configuration file `{}` does not exist", p.display());
                }
                util::normalize(&p)
            }
            None => find_config()?,
        };

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read `{}`", path.display()))?;
        let mut config: Config = toml::from_str(&text)
            .with_context(|| format!("invalid TOML in `{}`", path.display()))?;
        config.validate()?;

        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let root = util::normalize(&util::abs_path(&dir, &config.project.root));
        let out_dir = util::abs_path(&root, &config.project.out_dir);

        Ok(LoadedConfig {
            path,
            dir,
            root,
            out_dir,
            config,
        })
    }

    fn validate(&mut self) -> Result<()> {
        if self.project.name.trim().is_empty() {
            bail!("`project.name` must not be empty");
        }
        if self.project.out_dir.trim().is_empty() {
            bail!("`project.out_dir` must not be empty");
        }
        if self.build.state_dir.trim().is_empty() {
            bail!("`build.state_dir` must not be empty");
        }
        if self.target.is_empty() {
            bail!("no build targets declared; add at least one `[target.<name>]` table");
        }
        for (name, target) in &self.target {
            if name.trim().is_empty() {
                bail!("target names must not be empty");
            }
            target.validate(name)?;
        }
        // Profile references must resolve.
        for profile in self.profiles.keys() {
            self.resolve_profile(profile)?;
        }
        Ok(())
    }

    /// Resolve a profile, applying built-in defaults and `extends` chains.
    pub fn resolve_profile(&self, name: &str) -> Result<ResolvedProfile> {
        let mut chain = Vec::new();
        self.resolve_profile_inner(name, &mut chain)
    }

    fn resolve_profile_inner(
        &self,
        name: &str,
        chain: &mut Vec<String>,
    ) -> Result<ResolvedProfile> {
        if chain.iter().any(|n| n == name) {
            bail!(
                "profile inheritance cycle: {} -> {}",
                chain.join(" -> "),
                name
            );
        }
        if chain.len() > 32 {
            bail!("profile inheritance is too deep at `{name}`");
        }
        chain.push(name.to_string());
        let result = self.build_profile(name, chain);
        chain.pop();
        result
    }

    fn build_profile(&self, name: &str, chain: &mut Vec<String>) -> Result<ResolvedProfile> {
        let user = self.profiles.get(name);
        let (mut base_opt, mut base_debug, mut base_lto, base_defines) = match name {
            "debug" => (OptLevel::O0, true, false, Vec::<String>::new()),
            "release" => (OptLevel::O2, false, false, vec!["NDEBUG".to_string()]),
            _ => (OptLevel::O0, true, false, Vec::<String>::new()),
        };

        let mut extends_flags = Profile::default();
        if let Some(user) = user {
            if let Some(parent) = &user.extends {
                let parent_profile = self.resolve_profile_inner(parent, chain)?;
                base_opt = parent_profile.opt;
                base_debug = parent_profile.debug;
                base_lto = parent_profile.lto;
                extends_flags = Profile {
                    extends: None,
                    opt: None,
                    debug: None,
                    lto: None,
                    defines: parent_profile.defines,
                    cflags: parent_profile.cflags,
                    cxxflags: parent_profile.cxxflags,
                    asmflags: parent_profile.asmflags,
                    rustflags: parent_profile.rustflags,
                    ldflags: parent_profile.ldflags,
                };
            }
        }

        let mut flags = extends_flags;
        if let Some(user) = user {
            if let Some(opt) = user.opt {
                base_opt = opt;
            }
            if let Some(debug) = user.debug {
                base_debug = debug;
            }
            if let Some(lto) = user.lto {
                base_lto = lto;
            }
            flags.defines.extend(user.defines.iter().cloned());
            flags.cflags.extend(user.cflags.iter().cloned());
            flags.cxxflags.extend(user.cxxflags.iter().cloned());
            flags.asmflags.extend(user.asmflags.iter().cloned());
            flags.rustflags.extend(user.rustflags.iter().cloned());
            flags.ldflags.extend(user.ldflags.iter().cloned());
        }

        let mut defines = base_defines;
        defines.extend(flags.defines);

        Ok(ResolvedProfile {
            name: name.to_string(),
            opt: base_opt,
            debug: base_debug,
            lto: base_lto,
            defines,
            cflags: flags.cflags,
            cxxflags: flags.cxxflags,
            asmflags: flags.asmflags,
            rustflags: flags.rustflags,
            ldflags: flags.ldflags,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Exe,
    Staticlib,
    Sharedlib,
    /// Compile the sources into object files without linking them.
    Object,
    Rust,
    Cargo,
    /// Run a user-provided program with a fixed argument vector.
    Custom,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Exe => "exe",
            Kind::Staticlib => "staticlib",
            Kind::Sharedlib => "sharedlib",
            Kind::Object => "object",
            Kind::Rust => "rust",
            Kind::Cargo => "cargo",
            Kind::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(default = "default_kind")]
    pub kind: Kind,
    /// Source globs, directories or individual files.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Globs excluded from `sources`.
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub defines: Vec<String>,
    #[serde(default)]
    pub cflags: Vec<String>,
    #[serde(default)]
    pub cxxflags: Vec<String>,
    #[serde(default)]
    pub asmflags: Vec<String>,
    #[serde(default)]
    pub rustflags: Vec<String>,
    #[serde(default)]
    pub ldflags: Vec<String>,
    /// Other WBuild targets this target depends on.
    #[serde(default)]
    pub links: Vec<String>,
    /// System libraries, e.g. `["m", "pthread"]`.
    #[serde(default)]
    pub libs: Vec<String>,
    #[serde(default)]
    pub lib_dirs: Vec<String>,
    /// C++ standard, e.g. `"17"`.
    pub cxx_std: Option<String>,
    /// Explicit artifact name or path.
    pub output: Option<String>,
    /// Per-target target triple override (cross compilation).
    pub target: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    // Rust specific.
    pub edition: Option<String>,
    pub crate_type: Option<String>,
    // Cargo specific.
    pub manifest: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub cargo_args: Vec<String>,
    // Custom target: run `program` with `args`, reading `inputs` and producing
    // `outputs`. The first output is the primary artifact.
    pub program: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    /// Mark this target as a test executable. `wbuild test` builds and runs
    /// every target with `test = true`.
    #[serde(default)]
    pub test: bool,
}

impl Default for Target {
    fn default() -> Self {
        Self {
            kind: Kind::Exe,
            sources: Vec::new(),
            exclude: Vec::new(),
            include: Vec::new(),
            defines: Vec::new(),
            cflags: Vec::new(),
            cxxflags: Vec::new(),
            asmflags: Vec::new(),
            rustflags: Vec::new(),
            ldflags: Vec::new(),
            links: Vec::new(),
            libs: Vec::new(),
            lib_dirs: Vec::new(),
            cxx_std: None,
            output: None,
            target: None,
            env: BTreeMap::new(),
            edition: None,
            crate_type: None,
            manifest: None,
            features: Vec::new(),
            cargo_args: Vec::new(),
            program: None,
            args: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            test: false,
        }
    }
}

impl Target {
    fn validate(&self, name: &str) -> Result<()> {
        for dep in &self.links {
            if dep == name {
                bail!("target `{name}` cannot link to itself");
            }
        }
        match self.kind {
            Kind::Custom => {
                if self
                    .program
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
                {
                    bail!("target `{name}`: kind \"custom\" requires `program`");
                }
                if self.outputs.is_empty() {
                    bail!("target `{name}`: kind \"custom\" requires at least one output");
                }
            }
            Kind::Cargo => {
                if self.manifest.is_none() {
                    bail!("target `{name}`: kind \"cargo\" requires `manifest`");
                }
            }
            Kind::Rust => {
                if self.sources.len() != 1 {
                    bail!(
                        "target `{name}`: kind \"rust\" requires exactly one source file (the crate root)"
                    );
                }
            }
            _ => {
                if self.sources.is_empty() {
                    bail!("target `{name}`: `sources` must not be empty");
                }
            }
        }
        Ok(())
    }
}

/// The configuration plus resolved filesystem locations.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// Path of `wbuild.toml`.
    pub path: PathBuf,
    /// Directory containing the configuration file.
    pub dir: PathBuf,
    /// Project root (where source paths are resolved).
    pub root: PathBuf,
    /// Build artifact directory (absolute).
    pub out_dir: PathBuf,
    pub config: Config,
}

impl LoadedConfig {
    pub fn state_dir(&self) -> PathBuf {
        util::abs_path(&self.root, &self.config.build.state_dir)
    }

    pub fn profile(&self, requested: Option<&str>) -> Result<ResolvedProfile> {
        let name = requested.unwrap_or(&self.config.build.profile);
        self.config.resolve_profile(name)
    }
}

fn find_config() -> Result<PathBuf> {
    let mut dir = util::current_dir();
    loop {
        let candidate = dir.join(DEFAULT_CONFIG);
        if candidate.is_file() {
            return Ok(util::normalize(&candidate));
        }
        if !dir.pop() {
            break;
        }
    }
    bail!(
        "could not find `{DEFAULT_CONFIG}` in the current directory or any parent; \
         run `wbuild init` to create one"
    )
}

fn auto() -> String {
    "auto".to_string()
}
fn default_version() -> String {
    "0.1.0".to_string()
}
fn default_dot() -> String {
    ".".to_string()
}
fn default_out_dir() -> String {
    "build".to_string()
}
fn default_profile() -> String {
    "debug".to_string()
}
fn default_true() -> bool {
    true
}
fn default_state_dir() -> String {
    ".wbuild".to_string()
}
fn default_kind() -> Kind {
    Kind::Exe
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[project]
name = "demo"

[build]
profile = "release"

[target.app]
kind = "exe"
sources = ["src/*.c"]
links = ["core"]

[target.core]
kind = "staticlib"
sources = ["core/*.c"]

[profiles.release]
opt = 3
defines = ["FAST=1"]
"#;

    #[test]
    fn parses_sample_config() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.project.name, "demo");
        assert_eq!(cfg.target.len(), 2);
        assert_eq!(cfg.target["app"].links, vec!["core"]);
        let profile = cfg.resolve_profile("release").unwrap();
        assert_eq!(profile.opt, OptLevel::O3);
        assert!(profile.defines.contains(&"FAST=1".to_string()));
        assert!(profile.defines.contains(&"NDEBUG".to_string()));
    }

    #[test]
    fn profile_inheritance_works() {
        let cfg: Config = toml::from_str(
            r#"
[project]
name = "demo"
[target.a]
sources = ["a.c"]
[profiles.release]
opt = 2
[profiles.sanitize]
extends = "debug"
cflags = ["-fsanitize=address"]
"#,
        )
        .unwrap();
        let profile = cfg.resolve_profile("sanitize").unwrap();
        assert_eq!(profile.opt, OptLevel::O0);
        assert!(profile.debug);
        assert_eq!(profile.cflags, vec!["-fsanitize=address"]);
    }

    #[test]
    fn profile_cycle_is_rejected() {
        let cfg: Config = toml::from_str(
            r#"
[project]
name = "demo"
[target.a]
sources = ["a.c"]
[profiles.a]
extends = "b"
[profiles.b]
extends = "a"
"#,
        )
        .unwrap();
        assert!(cfg.resolve_profile("a").is_err());
    }

    #[test]
    fn opt_level_accepts_strings() {
        let cfg: Config = toml::from_str(
            r#"
[project]
name = "demo"
[target.a]
sources = ["a.c"]
[profiles.weird]
opt = "s"
"#,
        )
        .unwrap();
        assert_eq!(cfg.resolve_profile("weird").unwrap().opt, OptLevel::Os);
    }

    #[test]
    fn parses_object_and_custom_targets() {
        let cfg: Config = toml::from_str(
            r#"
[project]
name = "demo"

[target.obj]
kind = "object"
sources = ["src/a.c"]

[target.gen]
kind = "custom"
program = "tool"
args = ["--in", "a.txt", "--out", "gen/b.c"]
inputs = ["a.txt"]
outputs = ["gen/b.c", "gen/b.h"]

[target.suite]
kind = "exe"
sources = ["tests/*.c"]
test = true
"#,
        )
        .unwrap();
        assert_eq!(cfg.target["obj"].kind, Kind::Object);
        let custom = &cfg.target["gen"];
        assert_eq!(custom.kind, Kind::Custom);
        assert_eq!(custom.program.as_deref(), Some("tool"));
        assert_eq!(custom.outputs.len(), 2);
        assert!(cfg.target["suite"].test);
    }

    #[test]
    fn custom_target_requires_program_and_outputs() {
        let missing_program: Config = toml::from_str(
            r#"
[project]
name = "demo"
[target.gen]
kind = "custom"
outputs = ["out.c"]
"#,
        )
        .unwrap();
        assert!(missing_program.target["gen"].validate("gen").is_err());

        let missing_output: Config = toml::from_str(
            r#"
[project]
name = "demo"
[target.gen]
kind = "custom"
program = "tool"
"#,
        )
        .unwrap();
        assert!(missing_output.target["gen"].validate("gen").is_err());
    }
}
