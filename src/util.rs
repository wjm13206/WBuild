//! Small platform helpers shared by the rest of the crate.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Monotonic counter used to build unique temporary file names.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Limits how many metadata queries may be in flight at once.
///
/// File-system filters (antivirus, indexing, virtualization) serialize metadata
/// calls below the application, and under a dozen concurrent workers each call
/// then costs several times more than it does serially. A small cap keeps the
/// total time close to the serial cost instead of collapsing throughput.
#[cfg(windows)]
struct StampSlot;

/// How many metadata queries may run at once by default.
#[cfg(windows)]
const MAX_IN_FLIGHT: usize = 4;

#[cfg(windows)]
impl StampSlot {
    fn acquire() -> StampSlotRelease {
        use std::sync::{Condvar, Mutex, OnceLock};

        static GATE: OnceLock<(Mutex<usize>, Condvar, usize)> = OnceLock::new();
        let gate = GATE.get_or_init(|| (Mutex::new(0), Condvar::new(), MAX_IN_FLIGHT.max(1)));

        let mut active = gate.0.lock().unwrap_or_else(|err| err.into_inner());
        while *active >= gate.2 {
            active = gate
                .1
                .wait(active)
                .unwrap_or_else(|err| err.into_inner());
        }
        *active += 1;
        drop(active);

        StampSlotRelease(&gate.0, &gate.1)
    }
}

/// Releases the slot taken by [`StampSlot::acquire`].
#[cfg(windows)]
struct StampSlotRelease(
    &'static std::sync::Mutex<usize>,
    &'static std::sync::Condvar,
);

#[cfg(windows)]
impl Drop for StampSlotRelease {
    fn drop(&mut self) {
        let mut active = self.0.lock().unwrap_or_else(|err| err.into_inner());
        *active -= 1;
        self.1.notify_one();
    }
}

/// Cheap identity stamp of a file: `(size, mtime)`.
///
/// The touch-update loop in an incremental build stats every input, so this
/// deliberately avoids `fs::metadata`: on Windows the standard library opens a
/// handle for it, which costs an order of magnitude more than the single
/// `GetFileAttributesExW` query used here. The returned mtime is an opaque
/// tick value that is only ever compared for equality, never interpreted.
pub fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        /// `FILETIME` is two 32-bit halves and, importantly, 4-byte aligned.
        /// Modelling it as a `u64` would insert padding and shift every
        /// following field of `WIN32_FILE_ATTRIBUTE_DATA`.
        #[repr(C)]
        struct FileTime {
            low: u32,
            high: u32,
        }

        impl FileTime {
            fn ticks(&self) -> u64 {
                ((self.high as u64) << 32) | self.low as u64
            }
        }

        // WIN32_FILE_ATTRIBUTE_DATA, level 0 (GetFileExInfoStandard).
        #[repr(C)]
        struct FileAttributeData {
            file_attributes: u32,
            creation_time: FileTime,
            last_access_time: FileTime,
            last_write_time: FileTime,
            file_size_high: u32,
            file_size_low: u32,
        }

        // The Win32 structure is exactly 36 bytes; a mismatch would mean the
        // layout above is wrong and the call would scribble over the stack.
        const _: () = assert!(std::mem::size_of::<FileAttributeData>() == 36);

        extern "system" {
            fn GetFileAttributesExW(
                name: *const u16,
                level: i32,
                info: *mut FileAttributeData,
            ) -> i32;
        }

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut data = std::mem::MaybeUninit::<FileAttributeData>::uninit();
        // SAFETY: `wide` is a NUL-terminated UTF-16 path and `data` is a valid
        // out pointer for the documented structure.
        let ok = unsafe {
            let _slot = StampSlot::acquire();
            GetFileAttributesExW(wide.as_ptr(), 0, data.as_mut_ptr())
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: the call succeeded, so the structure is fully initialized.
        let data = unsafe { data.assume_init() };
        let size = ((data.file_size_high as u64) << 32) | data.file_size_low as u64;
        Some((size, data.last_write_time.ticks()))
    }
    #[cfg(not(windows))]
    {
        let meta = fs::metadata(path).ok()?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
            .unwrap_or(0);
        Some((meta.len(), mtime))
    }
}

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

/// Directories on `PATH`, split once per process. Toolchain detection probes a
/// dozen candidate names on every build, so re-splitting the variable each time
/// would just be wasted work.
fn path_dirs() -> &'static [PathBuf] {
    use std::sync::OnceLock;
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default()
    })
}

/// Locate an executable on `PATH` (and optional extra directories).
pub fn find_program(name: &str, extra_paths: &[PathBuf]) -> Option<PathBuf> {
    let has_sep = name.contains('/') || name.contains('\\');
    if has_sep {
        let p = PathBuf::from(name);
        return if p.is_file() { Some(p) } else { None };
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

    for dir in extra_paths.iter().chain(path_dirs()) {
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
