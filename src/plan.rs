//! Build plan construction: turn a configuration into a DAG of concrete steps.
//!
//! The planner is the only place that knows how to translate semantic settings
//! (optimization level, target kind, ...) into compiler arguments. It never
//! invokes a shell.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;

use crate::config::{Kind, LoadedConfig, OptLevel, ResolvedProfile, Target};
use crate::graph::Dag;
use crate::toolchain::{object_extension, triple_os, Family, Tool, Toolchain};
use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    CCompile,
    CxxCompile,
    AsmCompile,
    Rustc,
    Link,
    Archive,
    Cargo,
    Copy,
    Custom,
}

impl StepKind {
    pub fn tag(self) -> &'static str {
        match self {
            StepKind::CCompile => "CC",
            StepKind::CxxCompile => "CXX",
            StepKind::AsmCompile => "AS",
            StepKind::Rustc => "RUSTC",
            StepKind::Link => "LINK",
            StepKind::Archive => "AR",
            StepKind::Cargo => "CARGO",
            StepKind::Copy => "CP",
            StepKind::Custom => "RUN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepSource {
    None,
    Makefile,
    MsvcIncludes,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub id: usize,
    pub target: String,
    pub kind: StepKind,
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub workdir: PathBuf,
    /// Explicit inputs hashed for the fingerprint.
    pub inputs: Vec<PathBuf>,
    pub output: PathBuf,
    pub extra_outputs: Vec<PathBuf>,
    pub depfile: Option<PathBuf>,
    pub dep_source: DepSource,
    pub cacheable: bool,
    pub cache_ext: &'static str,
    /// Step ids that must complete before this step starts.
    pub deps: Vec<usize>,
    /// For `StepKind::Copy`, the file to copy.
    pub copy_from: Option<PathBuf>,
}

impl Step {
    pub fn command_line(&self) -> String {
        let mut out = String::new();
        if !self.env.is_empty() {
            for (k, v) in &self.env {
                out.push_str(k);
                out.push('=');
                out.push_str(v);
                out.push(' ');
            }
        }
        out.push_str(&self.program.display().to_string());
        for arg in &self.args {
            out.push(' ');
            if arg.contains(' ') {
                out.push('"');
                out.push_str(arg);
                out.push('"');
            } else {
                out.push_str(arg);
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct Artifact {
    pub output: PathBuf,
    /// Everything a dependent target must link against.
    pub link_inputs: Vec<PathBuf>,
    pub shared_dirs: Vec<PathBuf>,
    pub final_step: usize,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub dag: Dag,
    pub artifacts: BTreeMap<String, Artifact>,
    /// Targets in dependency order.
    pub order: Vec<String>,
    pub profile_dir: PathBuf,
}

impl Plan {
    pub fn total_steps(&self) -> usize {
        self.steps.len()
    }
}

pub struct Planner<'a> {
    loaded: &'a LoadedConfig,
    profile: &'a ResolvedProfile,
    tc: &'a Toolchain,
    steps: Vec<Step>,
    artifacts: BTreeMap<String, Artifact>,
    profile_dir: PathBuf,
}

impl<'a> Planner<'a> {
    pub fn build(
        loaded: &'a LoadedConfig,
        profile: &'a ResolvedProfile,
        tc: &'a Toolchain,
        selection: &[String],
    ) -> Result<Plan> {
        let mut planner = Planner {
            loaded,
            profile,
            tc,
            steps: Vec::new(),
            artifacts: BTreeMap::new(),
            profile_dir: loaded.out_dir.join(&profile.name),
        };

        let order = planner.target_order()?;
        let needed = planner.needed_targets(&order, selection)?;

        for name in &order {
            if !needed.contains(name) {
                continue;
            }
            let target = loaded
                .config
                .target
                .get(name)
                .expect("validated target exists")
                .clone();
            planner.plan_target(name, &target)?;
        }

        // Dependencies are collected while planning.
        let dag = Dag::new(planner.steps.len(), planner.dep_lists())?;
        dag.topo_order()?;

        Ok(Plan {
            steps: planner.steps,
            dag,
            artifacts: planner.artifacts,
            order: order.into_iter().filter(|n| needed.contains(n)).collect(),
            profile_dir: planner.profile_dir,
        })
    }

    fn dep_lists(&self) -> Vec<Vec<usize>> {
        self.steps.iter().map(|s| s.deps.clone()).collect()
    }

    fn target_order(&self) -> Result<Vec<String>> {
        let names: Vec<String> = self.loaded.config.target.keys().cloned().collect();
        let index: BTreeMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();

        let mut deps = Vec::with_capacity(names.len());
        for name in &names {
            let target = &self.loaded.config.target[name];
            let mut list = Vec::new();
            for link in &target.links {
                match index.get(link.as_str()) {
                    Some(&i) => list.push(i),
                    None => bail!(
                        "target `{name}` links to unknown target `{link}` \
                         (available: {})",
                        names.join(", ")
                    ),
                }
            }
            deps.push(list);
        }

        let dag = Dag::new(names.len(), deps)?;
        let order = dag.topo_order().with_context(|| {
            "target dependency cycle; check the `links` fields in [target.*]".to_string()
        })?;
        Ok(order.into_iter().map(|i| names[i].clone()).collect())
    }

    fn needed_targets(&self, order: &[String], selection: &[String]) -> Result<BTreeSet<String>> {
        if selection.is_empty() {
            return Ok(order.iter().cloned().collect());
        }
        let mut needed = BTreeSet::new();
        for name in selection {
            if !self.loaded.config.target.contains_key(name) {
                bail!(
                    "unknown target `{name}` (available: {})",
                    self.loaded
                        .config
                        .target
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            self.mark_needed(name, &mut needed);
        }
        Ok(needed)
    }

    fn mark_needed(&self, name: &str, needed: &mut BTreeSet<String>) {
        if !needed.insert(name.to_string()) {
            return;
        }
        if let Some(target) = self.loaded.config.target.get(name) {
            for link in &target.links {
                if self.loaded.config.target.contains_key(link) {
                    self.mark_needed(link, needed);
                }
            }
        }
    }

    fn plan_target(&mut self, name: &str, target: &Target) -> Result<()> {
        match target.kind {
            Kind::Rust => {
                let id = self.plan_rust(name, target)?;
                let output = self.steps[id].output.clone();
                let mut link_inputs = Vec::new();
                let mut shared_dirs = Vec::new();
                for dep in &target.links {
                    let artifact = self.artifact(dep, name)?;
                    link_inputs.extend(artifact.link_inputs.iter().cloned());
                    shared_dirs.extend(artifact.shared_dirs.iter().cloned());
                }
                let is_rlib = output.extension().map(|e| e == "rlib").unwrap_or(false);
                let mut inputs = Vec::new();
                if is_rlib {
                    inputs.push(output.clone());
                }
                link_inputs.extend(inputs.iter().cloned());
                self.artifacts.insert(
                    name.to_string(),
                    Artifact {
                        output,
                        link_inputs,
                        shared_dirs,
                        final_step: id,
                        kind: Kind::Rust,
                    },
                );
                Ok(())
            }
            Kind::Cargo => {
                let (build_id, copy_id, output) = self.plan_cargo(name, target)?;
                let mut link_inputs = Vec::new();
                let mut dep_steps = Vec::new();
                for dep in &target.links {
                    let artifact = self.artifact(dep, name)?;
                    dep_steps.push(artifact.final_step);
                    link_inputs.extend(artifact.link_inputs.iter().cloned());
                }
                self.steps[build_id].deps.extend(dep_steps);
                let mut final_step = copy_id;
                let _ = build_id;
                if copy_id == build_id {
                    final_step = build_id;
                }
                self.artifacts.insert(
                    name.to_string(),
                    Artifact {
                        output,
                        link_inputs,
                        shared_dirs: Vec::new(),
                        final_step,
                        kind: Kind::Cargo,
                    },
                );
                Ok(())
            }
            Kind::Object => self.plan_object_target(name, target),
            Kind::Custom => self.plan_custom(name, target),
            Kind::Exe | Kind::Staticlib | Kind::Sharedlib => {
                self.plan_cc_target(name, target)
            }
        }
    }

    fn artifact(&self, name: &str, requester: &str) -> Result<Artifact> {
        self.artifacts.get(name).cloned().with_context(|| {
            format!("target `{requester}` depends on `{name}` which was not planned yet")
        })
    }

    fn compile_sources_to_objects(
        &mut self,
        name: &str,
        target: &Target,
        is_shared: bool,
    ) -> Result<(Vec<PathBuf>, Vec<usize>, String)> {
        let sources = expand_sources(self.loaded, name, target)?;
        if sources.is_empty() {
            bail!(
                "target `{name}`: no source files matched {:?}",
                target.sources
            );
        }
        let os = self.target_os(target);
        let object_dir = self.profile_dir.join("obj").join(sanitize_name(name));
        let mut objects = Vec::new();
        let mut compile_ids = Vec::new();

        for source in &sources {
            let lang = language_of(source).with_context(|| {
                format!(
                    "target `{name}`: unsupported source file `{}`",
                    source.display()
                )
            })?;
            if lang == Lang::Rust {
                bail!(
                    "target `{name}`: `{}` is a Rust source; use `kind = \"rust\"`",
                    source.display()
                );
            }
            let rel = source
                .strip_prefix(&self.loaded.root)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| PathBuf::from(source.file_name().unwrap_or_default()));
            let ext = match lang {
                Lang::AsmNasm => "o",
                _ => object_extension(self.primary_family()),
            };
            let object = object_dir.join(&rel).with_extension(ext);
            let depfile = self
                .loaded
                .state_dir()
                .join("deps")
                .join(sanitize_name(name))
                .join(&rel)
                .with_extension("d");
            let id = self.make_compile(
                name, target, source, lang, &object, &depfile, is_shared, &os,
            )?;
            objects.push(object);
            compile_ids.push(id);
        }
        Ok((objects, compile_ids, os))
    }

    /// `kind = "object"`: compile every source but stop before linking.
    fn plan_object_target(&mut self, name: &str, target: &Target) -> Result<()> {
        let (objects, compile_ids, _os) =
            self.compile_sources_to_objects(name, target, false)?;

        let mut dep_final_steps = Vec::new();
        for dep in &target.links {
            let artifact = self.artifact(dep, name)?;
            dep_final_steps.push(artifact.final_step);
        }
        for id in &compile_ids {
            self.steps[*id].deps.extend(dep_final_steps.iter().copied());
        }

        if target.output.is_some() {
            bail!("target `{name}`: `output` is not supported for object targets");
        }
        let output = objects
            .first()
            .cloned()
            .expect("compile_sources_to_objects returns at least one object");
        let final_step = *compile_ids.last().expect("at least one compile step");
        self.artifacts.insert(
            name.to_string(),
            Artifact {
                output,
                link_inputs: objects,
                shared_dirs: Vec::new(),
                final_step,
                kind: Kind::Object,
            },
        );
        Ok(())
    }

    /// `kind = "custom"`: run a fixed program/argument vector. No shell is
    /// involved; `inputs` and `sources` are hashed, `outputs` are tracked.
    fn plan_custom(&mut self, name: &str, target: &Target) -> Result<()> {
        let program_text = target
            .program
            .as_deref()
            .with_context(|| format!("target `{name}`: custom target requires `program`"))?;
        let extra_paths: Vec<PathBuf> = self
            .loaded
            .config
            .compiler
            .extra_paths
            .iter()
            .map(|p| util::abs_path(&self.loaded.root, p))
            .collect();
        let program = resolve_program(self.loaded, program_text, &extra_paths)
            .with_context(|| format!("target `{name}`: cannot resolve `program`"))?;

        let outputs: Vec<PathBuf> = target
            .outputs
            .iter()
            .map(|p| util::abs_path(&self.loaded.root, p))
            .collect();
        let output = outputs
            .first()
            .cloned()
            .with_context(|| format!("target `{name}`: custom target requires `outputs`"))?;
        let extra_outputs = outputs[1..].to_vec();

        let mut patterns = target.inputs.clone();
        patterns.extend(target.sources.iter().cloned());
        let inputs = expand_inputs(self.loaded, name, &patterns)?;

        let mut deps = Vec::new();
        for dep in &target.links {
            deps.push(self.artifact(dep, name)?.final_step);
        }

        let id = self.steps.len();
        let label = format!("RUN {}", self.arg(&output));
        self.steps.push(Step {
            id,
            target: name.to_string(),
            kind: StepKind::Custom,
            label,
            program,
            args: target.args.clone(),
            env: target.env.clone(),
            workdir: self.loaded.root.clone(),
            inputs,
            output: output.clone(),
            extra_outputs,
            depfile: None,
            dep_source: DepSource::None,
            cacheable: true,
            cache_ext: "custom",
            deps,
            copy_from: None,
        });
        self.artifacts.insert(
            name.to_string(),
            Artifact {
                output,
                link_inputs: Vec::new(),
                shared_dirs: Vec::new(),
                final_step: id,
                kind: Kind::Custom,
            },
        );
        Ok(())
    }

    fn plan_cc_target(&mut self, name: &str, target: &Target) -> Result<()> {
        let is_shared = target.kind == Kind::Sharedlib;
        let (objects, compile_ids, os) =
            self.compile_sources_to_objects(name, target, is_shared)?;

        // Dependency targets.
        let mut dep_final_steps = Vec::new();
        let mut dep_link_inputs = Vec::new();
        let mut dep_shared_dirs = Vec::new();
        for dep in &target.links {
            let artifact = self.artifact(dep, name)?;
            if artifact.kind == Kind::Exe {
                bail!("target `{name}` cannot link to executable target `{dep}`");
            }
            dep_final_steps.push(artifact.final_step);
            dep_link_inputs.extend(artifact.link_inputs.iter().cloned());
            dep_shared_dirs.extend(artifact.shared_dirs.iter().cloned());
        }

        // Consumer translation units must not start before their dependency
        // targets are finished: a dependency may generate headers or sources
        // that these compilations read.
        for id in &compile_ids {
            self.steps[*id].deps.extend(dep_final_steps.iter().copied());
        }

        let (link_id, link_inputs, shared_dirs, extra_outputs) = self.make_final(
            name,
            target,
            &objects,
            &dep_link_inputs,
            &dep_shared_dirs,
            &os,
        )?;
        let mut deps = compile_ids;
        deps.extend(dep_final_steps);
        self.steps[link_id].deps = deps;

        let output = self.steps[link_id].output.clone();
        let _ = extra_outputs;
        self.artifacts.insert(
            name.to_string(),
            Artifact {
                output,
                link_inputs,
                shared_dirs,
                final_step: link_id,
                kind: target.kind,
            },
        );
        Ok(())
    }

    fn make_compile(
        &mut self,
        target_name: &str,
        target: &Target,
        source: &Path,
        lang: Lang,
        object: &Path,
        depfile: &Path,
        is_shared: bool,
        os: &str,
    ) -> Result<usize> {
        let (tool, kind) = match lang {
            Lang::C => (self.tc.require_cc()?, StepKind::CCompile),
            Lang::Cxx => (self.tc.require_cxx()?, StepKind::CxxCompile),
            Lang::AsmGas => (self.tc.require_cc()?, StepKind::AsmCompile),
            Lang::AsmNasm => (self.tc.require_asm()?, StepKind::AsmCompile),
            Lang::Rust => unreachable!(),
        };
        let family = tool.family;
        if matches!(lang, Lang::AsmGas) && family.is_msvc() {
            bail!(
                "target `{target_name}`: MSVC cannot assemble `{}`; use NASM (.asm) or clang",
                source.display()
            );
        }

        let includes = self.include_dirs(target);
        let defines = self.defines(target);
        let cflags = self.flags_for(lang, target);
        let src_arg = self.arg(source);
        let obj_arg = self.arg(object);
        let dep_arg = self.arg(depfile);

        let mut args: Vec<String> = Vec::new();
        let mut dep_source = DepSource::None;

        match lang {
            Lang::AsmNasm => {
                args.push("-f".into());
                args.push(nasm_format(os).into());
                args.push("-o".into());
                args.push(obj_arg.clone());
                args.push(src_arg.clone());
                args.push("-Ox".into());
                for inc in &includes {
                    args.push(format!("-I{}", self.arg(inc)));
                }
                for def in &defines {
                    args.push(format!("-D{def}"));
                }
                if self.profile.debug {
                    args.push("-g".into());
                }
                args.extend(cflags);
            }
            _ if family.is_msvc() => {
                args.push("/nologo".into());
                args.push("/c".into());
                args.push(src_arg.clone());
                args.push(format!("/Fo{obj_arg}"));
                args.push(opt_flag(family, self.profile.opt));
                if self.profile.debug {
                    args.push("/Zi".into());
                }
                if self.profile.lto {
                    args.push("/GL".into());
                }
                if lang == Lang::Cxx {
                    args.push("/EHsc".into());
                    if let Some(std) = &target.cxx_std {
                        args.push(format!("/std:c++{}", normalize_std(std)));
                    }
                }
                for inc in &includes {
                    args.push(format!("/I{}", self.arg(inc)));
                }
                for def in &defines {
                    args.push(format!("/D{def}"));
                }
                args.extend(cflags);
                args.push("/showIncludes".into());
                dep_source = DepSource::MsvcIncludes;
            }
            _ => {
                args.push("-c".into());
                args.push(src_arg.clone());
                args.push("-o".into());
                args.push(obj_arg.clone());
                args.push(opt_flag(family, self.profile.opt));
                if self.profile.debug {
                    args.push("-g".into());
                }
                if self.profile.lto {
                    args.push("-flto".into());
                }
                if lang == Lang::Cxx {
                    if let Some(std) = &target.cxx_std {
                        args.push(format!("-std=c++{}", normalize_std(std)));
                    }
                }
                if is_shared && os != "windows" {
                    args.push("-fPIC".into());
                }
                for inc in &includes {
                    args.push(format!("-I{}", self.arg(inc)));
                }
                for def in &defines {
                    args.push(format!("-D{def}"));
                }
                args.extend(cflags);
                args.push("-MMD".into());
                args.push("-MF".into());
                args.push(dep_arg.clone());
                args.push("-MP".into());
                dep_source = DepSource::Makefile;
            }
        }

        let label = format!("{} {}", kind.tag(), self.arg(source));
        let id = self.steps.len();
        self.steps.push(Step {
            id,
            target: target_name.to_string(),
            kind,
            label,
            program: tool.path.clone(),
            args,
            env: target.env.clone(),
            workdir: self.loaded.root.clone(),
            inputs: vec![source.to_path_buf()],
            output: object.to_path_buf(),
            extra_outputs: Vec::new(),
            depfile: if dep_source == DepSource::None {
                None
            } else {
                Some(depfile.to_path_buf())
            },
            dep_source,
            cacheable: true,
            cache_ext: if family.is_msvc() { "obj" } else { "o" },
            deps: Vec::new(),
            copy_from: None,
        });
        Ok(id)
    }

    fn make_final(
        &mut self,
        name: &str,
        target: &Target,
        objects: &[PathBuf],
        dep_link_inputs: &[PathBuf],
        dep_shared_dirs: &[PathBuf],
        os: &str,
    ) -> Result<(usize, Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>)> {
        let output = self.default_output(name, target, os);
        let mut inputs: Vec<PathBuf> = objects.to_vec();
        inputs.extend(dep_link_inputs.iter().cloned());

        match target.kind {
            Kind::Staticlib => {
                let ar = self.tc.require_ar()?;
                let mut args = Vec::new();
                if ar.family.is_msvc() {
                    args.push("/nologo".into());
                    args.push(format!("/OUT:{}", self.arg(&output)));
                } else {
                    args.push("rcsD".into());
                    args.push(self.arg(&output));
                }
                for obj in objects {
                    args.push(self.arg(obj));
                }
                let label = format!("AR  {}", self.arg(&output));
                let id = self.push_step(
                    name,
                    target,
                    StepKind::Archive,
                    label,
                    ar,
                    args,
                    output.clone(),
                    Vec::new(),
                    None,
                    DepSource::None,
                    true,
                    "a",
                    &inputs,
                );
                let mut link_inputs = vec![output.clone()];
                link_inputs.extend(dep_link_inputs.iter().cloned());
                Ok((
                    id,
                    dedupe_paths(link_inputs),
                    dep_shared_dirs.to_vec(),
                    Vec::new(),
                ))
            }
            Kind::Exe | Kind::Sharedlib => {
                let linker = self.tc.require_linker()?;
                let family = linker.family;
                let shared = target.kind == Kind::Sharedlib;
                let mut args = Vec::new();
                let mut extra_outputs = Vec::new();
                let mut import_lib = None;

                if family.is_msvc() {
                    args.push("/nologo".into());
                    if self.profile.debug {
                        args.push("/Zi".into());
                    }
                    if shared {
                        args.push("/LD".into());
                    }
                    for input in &inputs {
                        args.push(self.arg(input));
                    }
                    args.push(format!("/Fe:{}", self.arg(&output)));
                    args.push("/link".into());
                    if self.profile.lto {
                        args.push("/LTCG".into());
                    }
                    for flag in self.ldflags(target) {
                        args.push(flag);
                    }
                    for dir in self.lib_dirs(target) {
                        args.push(format!("/LIBPATH:{}", self.arg(&dir)));
                    }
                    for lib in self.libs(target) {
                        args.push(format!("{lib}.lib"));
                    }
                    if shared {
                        let implib = output.with_extension("lib");
                        extra_outputs.push(implib.clone());
                        import_lib = Some(implib);
                    }
                } else {
                    for input in &inputs {
                        args.push(self.arg(input));
                    }
                    args.push("-o".into());
                    args.push(self.arg(&output));
                    if shared {
                        args.push("-shared".into());
                    }
                    if self.profile.lto {
                        args.push("-flto".into());
                    }
                    for flag in self.ldflags(target) {
                        args.push(flag);
                    }
                    for dir in self.lib_dirs(target) {
                        args.push(format!("-L{}", self.arg(&dir)));
                    }
                    for lib in self.libs(target) {
                        args.push(format!("-l{lib}"));
                    }
                    if os != "windows" {
                        for dir in dep_shared_dirs {
                            args.push(format!("-Wl,-rpath,{}", self.arg(dir)));
                        }
                    }
                    if shared && os == "windows" {
                        let implib = import_library_path(&output);
                        args.push(format!("-Wl,--out-implib,{}", self.arg(&implib)));
                        extra_outputs.push(implib.clone());
                        import_lib = Some(implib);
                    }
                }

                let label = format!("LINK {}", self.arg(&output));
                let cacheable = extra_outputs.is_empty();
                let id = self.push_step(
                    name,
                    target,
                    StepKind::Link,
                    label,
                    linker,
                    args,
                    output.clone(),
                    extra_outputs.clone(),
                    None,
                    DepSource::None,
                    cacheable,
                    "out",
                    &inputs,
                );

                let mut link_inputs = Vec::new();
                if let Some(import_lib) = import_lib {
                    link_inputs.push(import_lib);
                } else {
                    link_inputs.push(output.clone());
                }
                link_inputs.extend(dep_link_inputs.iter().cloned());

                let mut shared_dirs = dep_shared_dirs.to_vec();
                if shared && os != "windows" {
                    shared_dirs.push(
                        output
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| self.loaded.root.clone()),
                    );
                }
                if target.kind == Kind::Exe {
                    // Executables are not linkable by other targets.
                    return Ok((id, Vec::new(), shared_dirs, extra_outputs));
                }
                Ok((id, dedupe_paths(link_inputs), shared_dirs, extra_outputs))
            }
            _ => unreachable!("make_final called for {:?}", target.kind),
        }
    }

    fn push_step(
        &mut self,
        target_name: &str,
        target: &Target,
        kind: StepKind,
        label: String,
        tool: &Tool,
        args: Vec<String>,
        output: PathBuf,
        extra_outputs: Vec<PathBuf>,
        depfile: Option<PathBuf>,
        dep_source: DepSource,
        cacheable: bool,
        cache_ext: &'static str,
        inputs: &[PathBuf],
    ) -> usize {
        let id = self.steps.len();
        self.steps.push(Step {
            id,
            target: target_name.to_string(),
            kind,
            label,
            program: tool.path.clone(),
            args,
            env: target.env.clone(),
            workdir: self.loaded.root.clone(),
            inputs: inputs.to_vec(),
            output,
            extra_outputs,
            depfile,
            dep_source,
            cacheable,
            cache_ext,
            deps: Vec::new(),
            copy_from: None,
        });
        id
    }

    fn plan_rust(&mut self, name: &str, target: &Target) -> Result<usize> {
        let rustc = self.tc.require_rustc()?;
        let sources = expand_sources(self.loaded, name, target)?;
        if sources.len() != 1 {
            bail!(
                "target `{name}`: Rust targets need exactly one crate root, found {}",
                sources.len()
            );
        }
        let crate_root = &sources[0];
        let crate_name = name.replace(['-', '.'], "_");
        let crate_type = target
            .crate_type
            .clone()
            .unwrap_or_else(|| "bin".to_string());
        let os = self.target_os(target);
        let output = match &target.output {
            Some(o) => util::abs_path(&self.loaded.out_dir, o),
            None => {
                let dir = self.profile_dir.clone();
                match crate_type.as_str() {
                    "staticlib" => dir.join(if self.primary_family().is_msvc() {
                        format!("{name}.lib")
                    } else {
                        format!("lib{name}.a")
                    }),
                    "cdylib" => dir.join(if os == "windows" {
                        format!("{name}.dll")
                    } else if os == "macos" {
                        format!("lib{name}.dylib")
                    } else {
                        format!("lib{name}.so")
                    }),
                    "lib" | "rlib" => dir.join(format!("lib{name}.rlib")),
                    "dylib" => dir.join(format!("lib{name}{}", dylib_suffix(&os))),
                    _ => dir.join(format!("{name}{}", exe_suffix(&os))),
                }
            }
        };

        let depfile = self
            .loaded
            .state_dir()
            .join("deps")
            .join(format!("{}.d", sanitize_name(name)));

        let mut args: Vec<String> = vec![
            self.arg(crate_root),
            "--crate-name".into(),
            crate_name.clone(),
            "--crate-type".into(),
            crate_type.clone(),
            "--edition".into(),
            target.edition.clone().unwrap_or_else(|| "2021".to_string()),
            "-o".into(),
            self.arg(&output),
            format!("--emit=dep-info={},link", self.arg(&depfile)),
            format!("-Copt-level={}", self.profile.opt.as_str()),
        ];
        if self.profile.debug {
            args.push("-Cdebuginfo=2".into());
        }
        if self.profile.lto {
            args.push("-Clto=thin".into());
        }
        if let Some(triple) = target.target.as_ref().or(self.tc.target.as_ref()) {
            args.push("--target".into());
            args.push(triple.clone());
        }

        // Extern crates produced by dependent WBuild targets.
        let mut dep_steps = Vec::new();
        for dep in &target.links {
            let artifact = self.artifact(dep, name)?;
            dep_steps.push(artifact.final_step);
            for input in &artifact.link_inputs {
                if input
                    .extension()
                    .map(|e| e == "rlib" || e == "so" || e == "dylib" || e == "dll")
                    .unwrap_or(false)
                {
                    let dep_name = dep.replace(['-', '.'], "_");
                    args.push("--extern".into());
                    args.push(format!("{dep_name}={}", self.arg(input)));
                }
            }
        }

        for def in self.defines(target) {
            args.push(format!("--cfg={def}"));
        }
        for flag in self
            .loaded
            .config
            .compiler
            .rustflags
            .iter()
            .chain(self.profile.rustflags.iter())
            .chain(target.rustflags.iter())
        {
            args.push(flag.clone());
        }

        let label = format!("RUSTC {}", self.arg(crate_root));
        let id = self.steps.len();
        self.steps.push(Step {
            id,
            target: name.to_string(),
            kind: StepKind::Rustc,
            label,
            program: rustc.path.clone(),
            args,
            env: target.env.clone(),
            workdir: self.loaded.root.clone(),
            inputs: vec![crate_root.clone()],
            output,
            extra_outputs: Vec::new(),
            depfile: Some(depfile),
            dep_source: DepSource::Makefile,
            cacheable: true,
            cache_ext: "rust",
            deps: Vec::new(),
            copy_from: None,
        });
        self.steps[id].deps = dep_steps;
        Ok(id)
    }

    fn plan_cargo(&mut self, name: &str, target: &Target) -> Result<(usize, usize, PathBuf)> {
        let manifest = target
            .manifest
            .as_ref()
            .context("cargo target requires `manifest`")?;
        let manifest = util::abs_path(&self.loaded.root, manifest);
        if !manifest.is_file() {
            bail!(
                "target `{name}`: manifest `{}` does not exist",
                manifest.display()
            );
        }
        let extra_paths: Vec<PathBuf> = self
            .loaded
            .config
            .compiler
            .extra_paths
            .iter()
            .map(|p| util::abs_path(&self.loaded.root, p))
            .collect();
        let cargo = util::find_program("cargo", &extra_paths).with_context(|| {
            "no `cargo` found on PATH; install the Rust toolchain or add it to `compiler.extra_paths`"
        })?;
        let cargo_version = probe_version(&cargo);

        let release = self.profile.name == "release" || self.profile.opt != OptLevel::O0;
        let artifact = cargo_artifact(&cargo, &manifest, name, target, release, self)?;
        let output = match &target.output {
            Some(o) => util::abs_path(&self.loaded.out_dir, o),
            None => self.default_cargo_output(name, release, &self.target_os(target)),
        };

        let mut args: Vec<String> = vec![
            "build".into(),
            "--manifest-path".into(),
            self.arg(&manifest),
        ];
        if release {
            args.push("--release".into());
        }
        if let Some(triple) = target.target.as_ref().or(self.tc.target.as_ref()) {
            args.push("--target".into());
            args.push(triple.clone());
        }
        if !target.features.is_empty() {
            args.push("--features".into());
            args.push(target.features.join(","));
        }
        args.extend(target.cargo_args.iter().cloned());

        let inputs = cargo_inputs(&manifest);
        let build_id = self.steps.len();
        self.steps.push(Step {
            id: build_id,
            target: name.to_string(),
            kind: StepKind::Cargo,
            label: format!("CARGO build {name}"),
            program: cargo,
            args,
            env: target.env.clone(),
            workdir: self.loaded.root.clone(),
            inputs,
            output: artifact.clone(),
            extra_outputs: Vec::new(),
            depfile: None,
            dep_source: DepSource::None,
            cacheable: false,
            cache_ext: "cargo",
            deps: Vec::new(),
            copy_from: None,
        });

        // Ensure the stable output location.
        if output == artifact {
            return Ok((build_id, build_id, output));
        }
        let copy_id = self.steps.len();
        self.steps.push(Step {
            id: copy_id,
            target: name.to_string(),
            kind: StepKind::Copy,
            label: format!("CP  {}", self.arg(&output)),
            program: PathBuf::from("wbuild"),
            args: Vec::new(),
            env: BTreeMap::new(),
            workdir: self.loaded.root.clone(),
            inputs: vec![artifact.clone()],
            output: output.clone(),
            extra_outputs: Vec::new(),
            depfile: None,
            dep_source: DepSource::None,
            cacheable: true,
            cache_ext: "copy",
            deps: Vec::new(),
            copy_from: Some(artifact.clone()),
        });
        self.steps[copy_id].deps = vec![build_id];
        let _ = cargo_version;
        Ok((build_id, copy_id, output))
    }

    fn default_cargo_output(&self, name: &str, release: bool, os: &str) -> PathBuf {
        let profile_dir = if release { "release" } else { "debug" };
        self.loaded
            .out_dir
            .join(profile_dir)
            .join(format!("{name}{}", exe_suffix(os)))
    }

    fn default_output(&self, name: &str, target: &Target, os: &str) -> PathBuf {
        if let Some(output) = &target.output {
            return util::abs_path(&self.loaded.out_dir, output);
        }
        let dir = self.profile_dir.clone();
        let msvc = self.primary_family().is_msvc();
        match target.kind {
            Kind::Exe => dir.join(format!("{name}{}", exe_suffix(os))),
            Kind::Staticlib => dir.join(if msvc {
                format!("{name}.lib")
            } else {
                format!("lib{name}.a")
            }),
            Kind::Sharedlib => dir.join(if os == "windows" {
                format!("{name}.dll")
            } else if os == "macos" {
                format!("lib{name}.dylib")
            } else {
                format!("lib{name}.so")
            }),
            _ => dir.join(name),
        }
    }

    fn primary_family(&self) -> Family {
        self.tc
            .cc
            .as_ref()
            .or(self.tc.cxx.as_ref())
            .or(self.tc.linker.as_ref())
            .map(|t| t.family)
            .unwrap_or(Family::Gcc)
    }

    fn target_os(&self, target: &Target) -> String {
        target
            .target
            .as_deref()
            .or(self.tc.target.as_deref())
            .and_then(triple_os)
            .unwrap_or(self.tc.host_os)
            .to_string()
    }

    fn include_dirs(&self, target: &Target) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let add = |dirs: &mut Vec<PathBuf>, dir: &str| {
            let abs = util::abs_path(&self.loaded.root, dir);
            if !dirs.iter().any(|d| d == &abs) {
                dirs.push(abs);
            }
        };
        for dir in self
            .loaded
            .config
            .compiler
            .include
            .iter()
            .chain(target.include.iter())
        {
            add(&mut dirs, dir);
        }

        // Include directories of linked targets are propagated so that a
        // consumer can `#include` its dependency's public headers.
        let mut stack: Vec<String> = target.links.clone();
        let mut visited: HashSet<String> = HashSet::new();
        while let Some(dep_name) = stack.pop() {
            if !visited.insert(dep_name.clone()) {
                continue;
            }
            if let Some(dep) = self.loaded.config.target.get(&dep_name) {
                for dir in &dep.include {
                    add(&mut dirs, dir);
                }
                stack.extend(dep.links.iter().cloned());
            }
        }
        dirs
    }

    fn defines(&self, target: &Target) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for def in self
            .loaded
            .config
            .compiler
            .defines
            .iter()
            .chain(self.profile.defines.iter())
            .chain(target.defines.iter())
        {
            let def = def.trim();
            if !def.is_empty() && seen.insert(def.to_string()) {
                out.push(def.to_string());
            }
        }
        out
    }

    fn flags_for(&self, lang: Lang, target: &Target) -> Vec<String> {
        let mut out = Vec::new();
        let compiler = &self.loaded.config.compiler;
        match lang {
            Lang::C => out.extend(compiler.cflags.iter().cloned()),
            Lang::Cxx => {
                out.extend(compiler.cxxflags.iter().cloned());
                out.extend(compiler.cflags.iter().cloned());
            }
            Lang::AsmGas | Lang::AsmNasm => out.extend(compiler.asmflags.iter().cloned()),
            Lang::Rust => {}
        }
        match lang {
            Lang::C => out.extend(self.profile.cflags.iter().cloned()),
            Lang::Cxx => {
                out.extend(self.profile.cxxflags.iter().cloned());
                out.extend(self.profile.cflags.iter().cloned());
            }
            Lang::AsmGas | Lang::AsmNasm => out.extend(self.profile.asmflags.iter().cloned()),
            Lang::Rust => {}
        }
        match lang {
            Lang::C => out.extend(target.cflags.iter().cloned()),
            Lang::Cxx => {
                out.extend(target.cxxflags.iter().cloned());
                out.extend(target.cflags.iter().cloned());
            }
            Lang::AsmGas | Lang::AsmNasm => out.extend(target.asmflags.iter().cloned()),
            Lang::Rust => {}
        }
        out
    }

    fn ldflags(&self, target: &Target) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.loaded.config.compiler.ldflags.iter().cloned());
        out.extend(self.profile.ldflags.iter().cloned());
        out.extend(target.ldflags.iter().cloned());
        if let Some(sysroot) = &self.tc.sysroot {
            let family = self.primary_family();
            if !family.is_msvc() {
                out.push(format!("--sysroot={}", self.arg(sysroot)));
            }
        }
        out
    }

    fn lib_dirs(&self, target: &Target) -> Vec<PathBuf> {
        target
            .lib_dirs
            .iter()
            .map(|d| util::abs_path(&self.loaded.root, d))
            .collect()
    }

    fn libs(&self, target: &Target) -> Vec<String> {
        target.libs.clone()
    }

    fn arg(&self, path: &Path) -> String {
        util::display_path(&self.loaded.root, path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    C,
    Cxx,
    AsmGas,
    AsmNasm,
    Rust,
}

pub fn language_of(path: &Path) -> Option<Lang> {
    let ext = path.extension()?.to_string_lossy();
    match ext.to_ascii_lowercase().as_str() {
        "c" => Some(Lang::C),
        "cc" | "cpp" | "cxx" | "c++" => Some(Lang::Cxx),
        "s" => Some(Lang::AsmGas),
        "asm" => Some(Lang::AsmNasm),
        "rs" => Some(Lang::Rust),
        _ => None,
    }
}

fn opt_flag(family: Family, opt: OptLevel) -> String {
    if family.is_msvc() {
        match opt {
            OptLevel::O0 => "/Od".to_string(),
            OptLevel::O1 => "/O1".to_string(),
            OptLevel::O2 => "/O2".to_string(),
            OptLevel::O3 => "/Ox".to_string(),
            OptLevel::Os => "/Os".to_string(),
            OptLevel::Oz => "/Oz".to_string(),
        }
    } else {
        format!("-O{}", opt.as_str())
    }
}

fn normalize_std(std: &str) -> String {
    std.trim_start_matches("c++").to_string()
}

fn nasm_format(os: &str) -> &'static str {
    match os {
        "windows" => "win64",
        "macos" => "macho64",
        _ => "elf64",
    }
}

fn exe_suffix(os: &str) -> &'static str {
    if os == "windows" {
        ".exe"
    } else {
        ""
    }
}

fn dylib_suffix(os: &str) -> &'static str {
    if os == "macos" {
        ".dylib"
    } else if os == "windows" {
        ".dll"
    } else {
        ".so"
    }
}

fn import_library_path(dll: &Path) -> PathBuf {
    let mut name = dll
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "lib".to_string());
    name.push_str(".dll.a");
    dll.with_file_name(name)
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            out.push(path);
        }
    }
    out
}

fn expand_sources(
    loaded: &LoadedConfig,
    target_name: &str,
    target: &Target,
) -> Result<Vec<PathBuf>> {
    expand_paths(
        loaded,
        target_name,
        &target.sources,
        &target.exclude,
        true,
    )
}

fn expand_inputs(
    loaded: &LoadedConfig,
    target_name: &str,
    patterns: &[String],
) -> Result<Vec<PathBuf>> {
    expand_paths(loaded, target_name, patterns, &[], false)
}

/// Returns true when `path` is declared in the `outputs` of a custom target
/// that the consumer links directly. Such files may legitimately not exist
/// before the build starts.
fn is_declared_output(loaded: &LoadedConfig, consumer: &str, path: &Path) -> bool {
    let path = util::normalize(path);
    let Some(target) = loaded.config.target.get(consumer) else {
        return false;
    };
    target.links.iter().any(|dep_name| {
        loaded.config.target.get(dep_name).is_some_and(|dep| {
            dep.kind == Kind::Custom
                && dep
                    .outputs
                    .iter()
                    .any(|out| util::normalize(&util::abs_path(&loaded.root, out)) == path)
        })
    })
}

fn expand_paths(
    loaded: &LoadedConfig,
    target_name: &str,
    patterns: &[String],
    exclude: &[String],
    only_languages: bool,
) -> Result<Vec<PathBuf>> {
    let root = &loaded.root;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut explicit_missing: Vec<String> = Vec::new();
    let accepted = |path: &Path| !only_languages || language_of(path).is_some();

    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        let full = util::abs_path(root, pattern);

        if full.is_dir() {
            for entry in WalkDir::new(&full).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() && accepted(entry.path()) {
                    files.push(entry.path().to_path_buf());
                }
            }
            continue;
        }

        if has_glob_meta(pattern) {
            let pattern_text = format!(
                "{}/{}",
                root.to_string_lossy()
                    .replace('\\', "/")
                    .trim_end_matches('/'),
                pattern.replace('\\', "/")
            );
            let entries = glob::glob(&pattern_text)
                .with_context(|| format!("invalid input pattern `{pattern}`"))?;
            for entry in entries {
                let path = entry.with_context(|| format!("error expanding `{pattern}`"))?;
                if path.is_file() && accepted(&path) {
                    files.push(path);
                }
            }
            continue;
        }

        if full.is_file() {
            files.push(full);
        } else if !is_declared_output(loaded, target_name, &full) {
            explicit_missing.push(pattern.to_string());
        } else {
            // The file will be produced by a custom target in this build, so it
            // is allowed to be absent while planning. Consumers depend on that
            // target through `links`.
            files.push(full);
        }
    }

    if !explicit_missing.is_empty() {
        bail!(
            "target `{}`: input path(s) not found: {}",
            target_name,
            explicit_missing.join(", ")
        );
    }

    // Apply exclusions.
    if !exclude.is_empty() {
        let mut excluded: HashSet<PathBuf> = HashSet::new();
        for pattern in exclude {
            let full = util::abs_path(root, pattern);
            if has_glob_meta(pattern) {
                let pattern_text = format!(
                    "{}/{}",
                    root.to_string_lossy()
                        .replace('\\', "/")
                        .trim_end_matches('/'),
                    pattern.replace('\\', "/")
                );
                if let Ok(entries) = glob::glob(&pattern_text) {
                    for entry in entries.flatten() {
                        excluded.insert(util::normalize(&entry));
                    }
                }
            } else if full.is_dir() {
                for entry in WalkDir::new(&full).into_iter().filter_map(|e| e.ok()) {
                    if entry.file_type().is_file() {
                        excluded.insert(util::normalize(entry.path()));
                    }
                }
            } else {
                excluded.insert(util::normalize(&full));
            }
        }
        files.retain(|f| !excluded.contains(&util::normalize(f)));
    }

    files.sort_by_key(|p| util::path_key(p));
    files.dedup();
    Ok(files)
}

/// Resolve a custom target's `program`. Paths are resolved relative to the
/// project root; bare names are looked up on `PATH`.
fn resolve_program(
    loaded: &LoadedConfig,
    program: &str,
    extra_paths: &[PathBuf],
) -> Result<PathBuf> {
    let looks_like_path =
        program.contains('/') || program.contains('\\') || program.starts_with('.');
    if looks_like_path {
        let path = util::abs_path(&loaded.root, program);
        if path.is_file() {
            return Ok(path);
        }
        if cfg!(windows) && path.extension().is_none() {
            let exe = path.with_extension("exe");
            if exe.is_file() {
                return Ok(exe);
            }
        }
        bail!("`{program}` does not exist");
    }
    util::find_program(program, extra_paths)
        .with_context(|| format!("`{program}` not found on PATH; set `compiler.extra_paths`"))
}

fn has_glob_meta(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

fn probe_version(program: &Path) -> String {
    use std::ffi::OsStr;
    if let Ok(output) = util::run_capture(program, &[OsStr::new("--version")], None) {
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        if let Some(line) = text.lines().find(|l| !l.trim().is_empty()) {
            return line.trim().to_string();
        }
    }
    "unknown".to_string()
}

fn cargo_inputs(manifest: &Path) -> Vec<PathBuf> {
    let mut inputs = vec![manifest.to_path_buf()];
    let dir = manifest.parent().unwrap_or(Path::new("."));
    let lock = dir.join("Cargo.lock");
    if lock.is_file() {
        inputs.push(lock);
    }
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != "target" && name != ".git" && name != ".wbuild"
        })
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            let ext = entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if ext == "rs" || entry.file_name() == "Cargo.toml" {
                inputs.push(entry.path().to_path_buf());
            }
        }
    }
    inputs.sort_by_key(|p| util::path_key(p));
    inputs.dedup();
    inputs
}

fn cargo_artifact(
    cargo: &Path,
    manifest: &Path,
    name: &str,
    target: &Target,
    release: bool,
    planner: &Planner<'_>,
) -> Result<PathBuf> {
    let args: Vec<OsString> = vec![
        "metadata".into(),
        "--format-version".into(),
        "1".into(),
        "--no-deps".into(),
        "--manifest-path".into(),
        manifest.as_os_str().to_os_string(),
    ];
    let refs: Vec<&std::ffi::OsStr> = args.iter().map(|s| s.as_os_str()).collect();
    let output = util::run_capture(cargo, &refs, Some(&planner.loaded.root))
        .with_context(|| "failed to run `cargo metadata`")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&text).with_context(|| "cannot parse `cargo metadata` output")?;

    let meta_dir = json
        .get("target_directory")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.parent().unwrap_or(Path::new(".")).join("target"));

    let package_name = target
        .output
        .clone()
        .or_else(|| {
            json.get("packages")
                .and_then(|p| p.as_array())
                .and_then(|pkgs| {
                    pkgs.iter()
                        .find(|pkg| pkg.get("name").and_then(|n| n.as_str()) == Some(name))
                })
                .and_then(|pkg| pkg.get("name").and_then(|n| n.as_str()).map(String::from))
        })
        .or_else(|| {
            json.get("packages")
                .and_then(|p| p.as_array())
                .and_then(|pkgs| pkgs.first())
                .and_then(|pkg| pkg.get("name").and_then(|n| n.as_str()).map(String::from))
        })
        .unwrap_or_else(|| name.to_string());

    let mut bin_name = target.output.clone().unwrap_or_else(|| {
        json.get("packages")
            .and_then(|p| p.as_array())
            .and_then(|pkgs| {
                pkgs.iter().find(|pkg| {
                    pkg.get("name").and_then(|n| n.as_str()) == Some(package_name.as_str())
                })
            })
            .and_then(|pkg| pkg.get("targets"))
            .and_then(|t| t.as_array())
            .and_then(|targets| {
                targets
                    .iter()
                    .find(|t| {
                        t.get("kind")
                            .and_then(|k| k.as_array())
                            .map(|kinds| kinds.iter().any(|k| k.as_str() == Some("bin")))
                            .unwrap_or(false)
                    })
                    .and_then(|t| t.get("name"))
                    .and_then(|n| n.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| package_name.clone())
    });
    if bin_name.is_empty() {
        bin_name = package_name;
    }

    let profile_dir = if release { "release" } else { "debug" };
    let os = target
        .target
        .as_deref()
        .or(planner.tc.target.as_deref())
        .and_then(triple_os)
        .unwrap_or(planner.tc.host_os);
    let mut path = meta_dir;
    if let Some(triple) = target.target.as_ref().or(planner.tc.target.as_ref()) {
        path = path.join(triple);
    }
    path = path.join(profile_dir);
    path = path.join(format!("{bin_name}{}", exe_suffix(os)));
    Ok(path)
}
