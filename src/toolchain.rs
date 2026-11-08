//! Compiler and toolchain detection.
//!
//! Detection is deliberately side-effect free (apart from running `--version`)
//! and always yields concrete executable paths. The planner never guesses how to
//! invoke a tool; it asks the `Toolchain`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::Compiler;
use crate::hash::Fingerprint;
use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Gcc,
    Clang,
    Msvc,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Gcc => "gcc",
            Family::Clang => "clang",
            Family::Msvc => "msvc",
        }
    }

    pub fn is_msvc(self) -> bool {
        matches!(self, Family::Msvc)
    }
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub path: PathBuf,
    pub family: Family,
    pub version: String,
}

impl Tool {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Debug, Clone)]
pub struct Toolchain {
    pub cc: Option<Tool>,
    pub cxx: Option<Tool>,
    pub asm: Option<Tool>,
    pub ar: Option<Tool>,
    pub rustc: Option<Tool>,
    pub linker: Option<Tool>,
    /// Triple used for cross compilation, if any.
    pub target: Option<String>,
    pub sysroot: Option<PathBuf>,
    pub host_os: &'static str,
    /// A stable identity of the whole toolchain used in fingerprints.
    pub fingerprint: String,
}

impl Toolchain {
    pub fn detect(compiler: &Compiler, root: &Path) -> Result<Self> {
        let extra: Vec<PathBuf> = compiler
            .extra_paths
            .iter()
            .map(|p| util::abs_path(root, p))
            .collect();

        let target = compiler.target.clone();
        let sysroot = compiler.sysroot.as_ref().map(|s| util::abs_path(root, s));

        let cc = resolve_tool(
            &compiler.cc,
            "WBUILD_CC",
            &cc_candidates(target.as_deref()),
            &extra,
        );
        let cxx = resolve_tool(
            &compiler.cxx,
            "WBUILD_CXX",
            &cxx_candidates(target.as_deref()),
            &extra,
        );
        let linker = if compiler.linker == "auto" {
            cc.clone()
        } else {
            resolve_tool(&compiler.linker, "WBUILD_LINKER", &[], &extra)
        };
        let asm = resolve_tool(&compiler.asm, "WBUILD_ASM", &["nasm".to_string()], &extra);
        let rustc = resolve_tool(
            &compiler.rustc,
            "WBUILD_RUSTC",
            &["rustc".to_string()],
            &extra,
        );
        let ar = if compiler.ar == "auto" {
            let family = cc.as_ref().map(|t| t.family);
            let candidates: Vec<String> = match family {
                Some(Family::Msvc) => vec!["lib".to_string()],
                _ => {
                    let mut v = Vec::new();
                    if let Some(triple) = &target {
                        v.push(format!("{triple}-ar"));
                    }
                    v.push("ar".to_string());
                    v.push("llvm-ar".to_string());
                    v
                }
            };
            resolve_tool("auto", "WBUILD_AR", &candidates, &extra)
        } else {
            resolve_tool(&compiler.ar, "WBUILD_AR", &[], &extra)
        };

        let host_os = std::env::consts::OS;

        let mut fp = Fingerprint::new().tag("wbuild-toolchain-v1");
        fp.text(host_os);
        if let Some(t) = &target {
            fp.text(t);
        }
        if let Some(s) = &sysroot {
            fp.path(s);
        }
        for tool in [&cc, &cxx, &asm, &ar, &rustc, &linker]
            .into_iter()
            .flatten()
        {
            fp.text(tool.family.as_str());
            fp.text(&tool.version);
            fp.text(&tool.name());
        }

        Ok(Self {
            cc,
            cxx,
            asm,
            ar,
            rustc,
            linker,
            target,
            sysroot,
            host_os,
            fingerprint: fp.finish(),
        })
    }

    pub fn require_cc(&self) -> Result<&Tool> {
        self.cc
            .as_ref()
            .context("no C compiler found; install gcc/clang or set `[compiler] cc = \"<path>\"`")
    }

    pub fn require_cxx(&self) -> Result<&Tool> {
        self.cxx.as_ref().context(
            "no C++ compiler found; install g++/clang++ or set `[compiler] cxx = \"<path>\"`",
        )
    }

    pub fn require_asm(&self) -> Result<&Tool> {
        self.asm
            .as_ref()
            .context("no assembler found; install nasm or set `[compiler] asm = \"<path>\"`")
    }

    pub fn require_ar(&self) -> Result<&Tool> {
        self.ar
            .as_ref()
            .context("no archiver found; install ar/llvm-ar or set `[compiler] ar = \"<path>\"`")
    }

    pub fn require_rustc(&self) -> Result<&Tool> {
        self.rustc
            .as_ref()
            .context("no Rust compiler found; install rustc or set `[compiler] rustc = \"<path>\"`")
    }

    pub fn require_linker(&self) -> Result<&Tool> {
        self.linker
            .as_ref()
            .context("no linker driver found; set `[compiler] linker = \"<path>\"`")
    }

    /// Human readable detection summary for `wbuild doctor`.
    pub fn describe(&self) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        let mut push = |label: &str, tool: &Option<Tool>| {
            let value = match tool {
                Some(t) => format!(
                    "{} ({}, {})",
                    t.path.display(),
                    t.family.as_str(),
                    t.version
                ),
                None => "<not found>".to_string(),
            };
            rows.push((label.to_string(), value));
        };
        push("cc", &self.cc);
        push("cxx", &self.cxx);
        push("linker", &self.linker);
        push("ar", &self.ar);
        push("asm", &self.asm);
        push("rustc", &self.rustc);
        rows.push((
            "target".to_string(),
            self.target.clone().unwrap_or_else(|| "native".to_string()),
        ));
        rows.push((
            "sysroot".to_string(),
            self.sysroot
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<default>".to_string()),
        ));
        rows
    }
}

fn cc_candidates(target: Option<&str>) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(t) = target {
        v.push(format!("{t}-gcc"));
        v.push(format!("{t}-clang"));
        v.push(format!("{t}-cc"));
    }
    if cfg!(windows) {
        v.extend(["gcc", "clang", "cc", "cl"].map(String::from));
    } else {
        v.extend(["cc", "gcc", "clang"].map(String::from));
    }
    v
}

fn cxx_candidates(target: Option<&str>) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(t) = target {
        v.push(format!("{t}-g++"));
        v.push(format!("{t}-clang++"));
        v.push(format!("{t}-c++"));
    }
    if cfg!(windows) {
        v.extend(["g++", "clang++", "c++", "cl"].map(String::from));
    } else {
        v.extend(["c++", "g++", "clang++"].map(String::from));
    }
    v
}

fn resolve_tool(
    configured: &str,
    env_var: &str,
    auto_candidates: &[String],
    extra_paths: &[PathBuf],
) -> Option<Tool> {
    let requested = std::env::var(env_var)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| configured.to_string());
    let requested = requested.trim();

    if requested != "auto" {
        let found = find_tool(requested, extra_paths).or_else(|| {
            // A bare name that was not found: report nothing, detection is
            // best-effort. The planner emits a precise error if it is needed.
            if requested.contains('/') || requested.contains('\\') {
                let p = PathBuf::from(requested);
                if p.is_file() {
                    probe(&p)
                } else {
                    None
                }
            } else {
                None
            }
        });
        return found;
    }

    for candidate in auto_candidates {
        if let Some(tool) = find_tool(candidate, extra_paths) {
            return Some(tool);
        }
    }
    None
}

fn find_tool(name: &str, extra_paths: &[PathBuf]) -> Option<Tool> {
    let path = util::find_program(name, extra_paths)?;
    probe(&path)
}

fn probe(path: &Path) -> Option<Tool> {
    let family = family_from_name(path);

    let version = probe_version(path).unwrap_or_else(|| "unknown".to_string());
    let family = family_from_version(&version).unwrap_or(family);

    Some(Tool {
        path: path.to_path_buf(),
        family,
        version,
    })
}

fn probe_version(path: &Path) -> Option<String> {
    // Most compilers accept `--version`; MSVC's cl needs to be run bare.
    let attempts: [&[&OsStr]; 2] = [&[OsStr::new("--version")], &[]];
    for args in attempts {
        let output = match util::run_capture(path, args, None) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let mut text = String::new();
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        if text.trim().is_empty() {
            text.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        if !line.is_empty() {
            let mut line = line.to_string();
            if line.len() > 200 {
                line.truncate(200);
            }
            return Some(line);
        }
    }
    None
}

fn family_from_name(path: &Path) -> Family {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name == "cl" || name == "clang-cl" {
        Family::Msvc
    } else if name.contains("clang") {
        Family::Clang
    } else {
        Family::Gcc
    }
}

fn family_from_version(version: &str) -> Option<Family> {
    let v = version.to_ascii_lowercase();
    if v.contains("clang") {
        Some(Family::Clang)
    } else if v.contains("microsoft") && (v.contains("c/c++") || v.contains("compil")) {
        Some(Family::Msvc)
    } else if v.contains("gcc") || v.contains("free software foundation") || v.contains("gnu") {
        Some(Family::Gcc)
    } else {
        None
    }
}

/// Compute the object file extension for a family.
pub fn object_extension(family: Family) -> &'static str {
    if family.is_msvc() {
        "obj"
    } else {
        "o"
    }
}

/// Return the platform component of a target triple such as
/// `x86_64-pc-windows-msvc` -> `windows`.
pub fn triple_os(triple: &str) -> Option<&str> {
    let lower = triple.to_ascii_lowercase();
    if lower.contains("windows") {
        Some("windows")
    } else if lower.contains("darwin") || lower.contains("apple") {
        Some("macos")
    } else if lower.contains("linux") || lower.contains("android") {
        Some("linux")
    } else if lower.contains("freebsd") || lower.contains("openbsd") || lower.contains("netbsd") {
        Some("bsd")
    } else {
        None
    }
}

/// Validate that a configured target triple looks plausible.
pub fn validate_triple(triple: &str) -> Result<()> {
    if triple.trim().is_empty() {
        bail!("target triple must not be empty");
    }
    if triple.chars().any(|c| c.is_whitespace()) {
        bail!("target triple `{triple}` must not contain whitespace");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_families_from_versions() {
        assert_eq!(family_from_version("gcc (GCC) 13.2.0"), Some(Family::Gcc));
        assert_eq!(
            family_from_version("clang version 18.1.0"),
            Some(Family::Clang)
        );
        assert_eq!(
            family_from_version("Microsoft (R) C/C++ Optimizing Compiler Version 19.40"),
            Some(Family::Msvc)
        );
    }

    #[test]
    fn detects_triple_os() {
        assert_eq!(triple_os("x86_64-pc-windows-msvc"), Some("windows"));
        assert_eq!(triple_os("aarch64-unknown-linux-gnu"), Some("linux"));
        assert_eq!(triple_os("aarch64-apple-darwin"), Some("macos"));
        assert_eq!(triple_os("wasm32-unknown-unknown"), None);
    }
}
