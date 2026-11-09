//! Small platform helpers shared by the rest of the crate.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Monotonic counter used to build unique temporary file names.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Make `path` absolute relative to `base` without touching the filesystem.
pub fn abs_path(base: &Path, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// Lexically normalize a path: resolve `.` and `..` components without
/// requiring the path to exist. Keeps the path absolute when the input is.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop normal components. For a relative path that starts
                // with `..` we must keep those components.
                match out.components().next_back() {
                    Some(Component::Normal(_)) => {
                        out.pop();
                    }
                    Some(Component::Prefix(_)) | Some(Component::RootDir) => {}
                    _ => out.push(Component::ParentDir),
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Express `path` relative to `base` when possible, using forward slashes.
///
/// The returned string is stable across platforms which keeps fingerprints and
/// log output readable.
pub fn display_path(base: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(base).unwrap_or(path);
    let mut text = rel.to_string_lossy().replace('\\', "/");
    if text.is_empty() {
        text = ".".to_string();
    }
    text
}

/// Normalize a path for fingerprinting/logging.
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// A unique temporary sibling path for `path`.
pub fn temp_sibling(path: &Path) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = format!(
        ".{}.wbuild-tmp-{}-{}",
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string()),
        pid,
        n
    );
    path.with_file_name(name)
}

/// Atomically write bytes to `path` (write temp file, then rename).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = temp_sibling(path);
    fs::write(&tmp, bytes)?;
    // On Windows a rename over an existing file can fail; remove first.
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Atomically copy `src` to `dst`, preserving permissions where possible.
pub fn atomic_copy(src: &Path, dst: &Path) -> io::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = temp_sibling(dst);
    fs::copy(src, &tmp)?;
    if dst.exists() {
        let _ = fs::remove_file(dst);
    }
    match fs::rename(&tmp, dst) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Best-effort executable permission bit (no-op on Windows).
#[cfg(unix)]
pub fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
pub fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// True when a file name looks like an executable on the current platform.
pub fn exe_suffix(host_os: &str) -> &'static str {
    if host_os == "windows" {
        ".exe"
    } else {
        ""
    }
}

/// Locate an executable on `PATH` (and optional extra directories).
pub fn find_program(name: &str, extra_paths: &[PathBuf]) -> Option<PathBuf> {
    let has_sep = name.contains('/') || name.contains('\\');
    if has_sep {
        let p = PathBuf::from(name);
        return if p.is_file() { Some(p) } else { None };
    }

    let mut dirs: Vec<PathBuf> = extra_paths.to_vec();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }

    let candidates: Vec<String> = if cfg!(windows) {
        if name.to_ascii_lowercase().ends_with(".exe") {
            vec![name.to_string()]
        } else {
            vec![format!("{name}.exe"), name.to_string()]
        }
    } else {
        vec![name.to_string()]
    };

    for dir in dirs {
        for candidate in &candidates {
            let full = dir.join(candidate);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}

/// Run a program capturing stdout/stderr. No shell is involved.
pub fn run_capture(
    program: &Path,
    args: &[&OsStr],
    workdir: Option<&Path>,
) -> io::Result<std::process::Output> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    if let Some(dir) = workdir {
        cmd.current_dir(dir);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.output()
}

/// Human friendly duration: `12ms`, `1.24s`.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 1.0 {
        format!("{secs:.2}s")
    } else {
        format!("{}ms", d.as_millis())
    }
}

/// Shorten a single line for inline logging.
pub fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim().to_string();
    const MAX: usize = 160;
    if line.chars().count() > MAX {
        let truncated: String = line.chars().take(MAX - 3).collect();
        format!("{truncated}...")
    } else {
        line
    }
}

/// Return `true` when the environment requests colored output.
pub fn use_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if matches!(std::env::var("CLICOLOR").as_deref(), Ok("0")) {
        return false;
    }
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

/// Enable ANSI escape processing on legacy Windows consoles.
pub fn enable_ansi() {
    #[cfg(windows)]
    {
        // MINIMAL: use a tiny FFI shim instead of pulling in the windows crate.
        #[link(name = "kernel32")]
        extern "system" {
            fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
            fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
            fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
        }
        const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
        const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;
        const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
        unsafe {
            for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                let handle = GetStdHandle(which);
                if handle.is_null() {
                    continue;
                }
                let mut mode = 0u32;
                if GetConsoleMode(handle, &mut mode) != 0 {
                    let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                }
            }
        }
    }
}

/// Resolve a possibly relative path against the current directory.
pub fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
