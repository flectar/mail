//! Desktop uses a short-lived renderer process; mobile uses one bounded native
//! worker with cooperative cancellation. Both load only the packaged PDFium
//! library on first preview and return one bounded RGBA page.
//! No PDF scripts, links, forms actions, or document attachments are executed.
//! The desktop renderer runs in a sandbox, built from Landlock and seccomp on
//! Linux, Apple's sandbox on macOS and an AppContainer on Windows.
#[cfg(desktop)]
use std::sync::Arc;

pub(crate) const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_EDGE: u32 = 2880;
#[cfg(desktop)]
const MAGIC: &[u8; 4] = b"FPD1";
/// The argument that starts the desktop executable as the PDF worker.
#[cfg(desktop)]
const WORKER: &str = "--render-pdf-page";

#[derive(Debug)]
pub(crate) struct Page {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub count: u32,
    pub index: u32,
}

fn dimensions(width: u32, height: u32) -> Result<usize, String> {
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
        return Err("PDF page exceeds the preview pixel limit.".into());
    }
    Ok(width as usize * height as usize * 4)
}

#[cfg(desktop)]
pub(crate) async fn render(data: Arc<Vec<u8>>, index: u32, zoom: u32) -> Result<Page, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if data.len() > MAX_DOCUMENT_BYTES || !(50..=200).contains(&zoom) {
        return Err("This PDF exceeds the preview limit. Download it to view locally.".into());
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let library = library_path(&executable)?;
    if !library.is_file() {
        return Err("The PDF preview component is missing from this installation. Download the file to view it locally.".into());
    }
    // Do not pass credentials, profile paths, PATH or dynamic-loader
    // overrides to PDFium.
    #[cfg(not(target_os = "windows"))]
    let mut child = tokio::process::Command::new(&executable)
        .arg(WORKER)
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    // Without its sandbox the worker doesn't start at all.
    #[cfg(target_os = "windows")]
    let mut child = windows_worker::Worker::spawn(&executable, &[&executable, &library], WORKER)
        .map_err(|error| {
            tracing::warn!(%error, "Could not start the sandboxed PDF renderer");
            "Unable to preview this PDF safely on this device. Download it to view locally."
                .to_string()
        })?;
    let work = async {
        let mut input = child.stdin.take().ok_or("Missing PDF input pipe")?;
        input
            .write_all(&index.to_le_bytes())
            .await
            .map_err(|e| e.to_string())?;
        input
            .write_all(&zoom.to_le_bytes())
            .await
            .map_err(|e| e.to_string())?;
        input
            .write_all(&(data.len() as u32).to_le_bytes())
            .await
            .map_err(|e| e.to_string())?;
        input.write_all(&data).await.map_err(|e| e.to_string())?;
        input.flush().await.map_err(|e| e.to_string())?;
        drop(input);
        let mut output = child.stdout.take().ok_or("Missing PDF output pipe")?;
        let mut header = [0u8; 16];
        output.read_exact(&mut header).await.map_err(|_| {
            "Unable to preview this PDF safely on this device. Download it to view locally."
                .to_string()
        })?;
        let number = |start| u32::from_le_bytes(header[start..start + 4].try_into().unwrap());
        let (count, width, height) = (number(4), number(8), number(12));
        if &header[..4] != MAGIC || count == 0 || index >= count {
            return Err("Invalid response from the PDF renderer.".into());
        }
        let mut pixels = vec![0u8; dimensions(width, height)?];
        output
            .read_exact(&mut pixels)
            .await
            .map_err(|e| e.to_string())?;
        if !child.wait().await.map_err(|e| e.to_string())?.success() {
            return Err("The PDF renderer could not finish this page.".into());
        }
        Ok(Page {
            pixels,
            width,
            height,
            count,
            index,
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), work)
        .await
        .map_err(|_| {
            "This PDF page took too long to render. Download it to view locally.".to_string()
        })?
}

#[cfg(any(test, mobile))]
#[path = "pdf_preview_mobile.rs"]
mod mobile;
#[cfg(target_os = "windows")]
#[path = "pdf_preview_windows.rs"]
mod windows_worker;
#[cfg(mobile)]
pub use mobile::cancel_active;
#[cfg(mobile)]
pub(crate) use mobile::render;

/// Called by the thin desktop launcher BEFORE PlatformContext or the UI exists.
/// The reserved argument alone enables the worker; documents arrive only via stdin.
#[cfg(desktop)]
pub fn run_worker_if_requested() -> Option<i32> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(WORKER)) {
        return None;
    }
    Some(if worker().is_ok() { 0 } else { 1 })
}

#[cfg(desktop)]
fn worker() -> Result<(), Box<dyn std::error::Error>> {
    use pdfium_render::prelude::*;
    use std::io::{Read, Write};
    #[cfg(unix)]
    close_inherited_descriptors()?;
    // Resource limits affect this child only. The parent's wall-clock deadline
    // additionally covers native code that stalls without consuming CPU time.
    #[cfg(unix)]
    unsafe {
        let cpu = libc::rlimit {
            rlim_cur: 15,
            rlim_max: 15,
        };
        let core = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0
            || libc::setrlimit(libc::RLIMIT_CORE, &core) != 0
        {
            return Err("Could not constrain PDF renderer".into());
        }
        #[cfg(linux_desktop)]
        {
            let memory = libc::rlimit {
                rlim_cur: 768 * 1024 * 1024,
                rlim_max: 768 * 1024 * 1024,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &memory) != 0 {
                return Err("Could not limit PDF memory".into());
            }
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
        }
    }
    let mut input = std::io::stdin().lock();
    let mut header = [0u8; 12];
    input.read_exact(&mut header)?;
    let number = |start| u32::from_le_bytes(header[start..start + 4].try_into().unwrap());
    let (index, zoom, length) = (number(0), number(4), number(8) as usize);
    if length > MAX_DOCUMENT_BYTES || !(50..=200).contains(&zoom) {
        return Err("Invalid PDF request".into());
    }
    let mut data = vec![0; length];
    input.read_exact(&mut data)?;
    let executable = std::env::current_exe()?;
    let library = library_path(&executable)?;
    let pdfium = Pdfium::new(Pdfium::bind_to_library(library)?);
    #[cfg(any(linux_desktop, target_os = "macos"))]
    sandbox()?;
    let document = pdfium.load_pdf_from_byte_slice(&data, None)?;
    let count = u32::try_from(document.pages().len())?;
    if index >= count {
        return Err("PDF page out of range".into());
    }
    let page = document.pages().get(index.try_into()?)?;
    let config = PdfRenderConfig::new()
        .set_target_width((1440 * zoom / 100) as i32)
        .set_maximum_width(MAX_EDGE as i32)
        .set_maximum_height(MAX_EDGE as i32);
    let bitmap = page.render_with_config(&config)?;
    let (width, height) = (bitmap.width() as u32, bitmap.height() as u32);
    let expected = dimensions(width, height)?;
    let pixels = bitmap.as_rgba_bytes();
    if pixels.len() != expected {
        return Err("Invalid PDF bitmap".into());
    }
    let mut output = std::io::stdout().lock();
    output.write_all(MAGIC)?;
    for value in [count, width, height] {
        output.write_all(&value.to_le_bytes())?;
    }
    output.write_all(&pixels)?;
    output.flush()?;
    Ok(())
}

/// Closes every file descriptor above stderr that the worker inherited. The
/// sandbox still lets it read and write descriptors it holds, so a socket or
/// file the app opened without close-on-exec would stay within its reach.
/// The worker needs only stdin, stdout and stderr.
#[cfg(all(unix, desktop))]
fn close_inherited_descriptors() -> std::io::Result<()> {
    #[cfg(linux_desktop)]
    {
        // close_range(2), from Linux 5.9 on, closes them in one call.
        if unsafe { libc::syscall(libc::SYS_close_range, 3, libc::c_uint::MAX, 0) } == 0 {
            return Ok(());
        }
    }
    let listing = if cfg!(linux_desktop) {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let open: Vec<libc::c_int> = std::fs::read_dir(listing)?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .filter(|&descriptor| descriptor > 2)
        .collect();
    for descriptor in open {
        // The listing's own descriptor is among them, already closed.
        unsafe { libc::close(descriptor) };
    }
    Ok(())
}

// Files arrive through stdin. Landlock leaves only the system font directories
// readable. Kernels without Landlock, such as Raspberry Pi OS's, can't limit
// file access to those directories, so there seccomp blocks opening any file
// and PDFs that rely on installed fonts render with PDFium's built-in ones.
// Seccomp also denies networking, process creation, ptrace and signal delivery.
#[cfg(linux_desktop)]
fn sandbox() -> Result<(), Box<dyn std::error::Error>> {
    use landlock::{
        ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
        path_beneath_rules,
    };
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(ABI::V1))?
        .create()?
        .add_rules(path_beneath_rules(
            [
                "/usr/share/fonts",
                "/usr/local/share/fonts",
                "/etc/fonts",
                "/var/cache/fontconfig",
            ],
            AccessFs::from_read(ABI::V1),
        ))?
        .restrict_self()?;
    if !status.no_new_privs {
        return Err("PDF sandbox requires no_new_privs".into());
    }
    let read_fonts = match status.ruleset {
        RulesetStatus::FullyEnforced => true,
        RulesetStatus::NotEnforced => false,
        // Part of the file access would remain unrestricted.
        RulesetStatus::PartiallyEnforced => {
            return Err("PDF sandbox requires full Linux Landlock support".into());
        }
    };
    let mut calls = vec![
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_close,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_pread64,
        libc::SYS_lseek,
        libc::SYS_fstat,
        // glibc implements fstat() with newfstatat on an empty path.
        libc::SYS_newfstatat,
        libc::SYS_fcntl,
        libc::SYS_flock,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_futex,
        libc::SYS_clock_gettime,
        libc::SYS_gettimeofday,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_rt_sigreturn,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigaction,
        libc::SYS_sigaltstack,
        libc::SYS_getrandom,
        libc::SYS_sched_yield,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];
    if read_fonts {
        calls.extend([
            libc::SYS_openat,
            libc::SYS_statx,
            libc::SYS_faccessat,
            libc::SYS_faccessat2,
            libc::SYS_getdents64,
            libc::SYS_readlinkat,
        ]);
        #[cfg(target_arch = "x86_64")]
        calls.extend([
            libc::SYS_open,
            libc::SYS_access,
            libc::SYS_stat,
            libc::SYS_lstat,
            libc::SYS_readlink,
        ]);
    }
    let filter: BpfProgram = SeccompFilter::new(
        calls.into_iter().map(|number| (number, vec![])).collect(),
        SeccompAction::Errno(libc::EPERM as u32),
        SeccompAction::Allow,
        std::env::consts::ARCH.try_into()?,
    )?
    .try_into()?;
    seccompiler::apply_filter(&filter)?;
    Ok(())
}

// PDFium is loaded and the document read, so the worker needs nothing from
// the system but memory and the output pipe it already holds. The built-in
// pure computation profile denies opening files, networking, starting
// processes and reaching other processes' services. Fonts can't be read
// either, so PDFs that rely on installed fonts render with PDFium's built-in
// ones. A worker that can't be sandboxed stops before it parses anything.
#[cfg(target_os = "macos")]
pub(crate) fn sandbox() -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::{CStr, c_char, c_int};
    // These come from sandbox.h. Apple deprecated them, but the named profiles
    // still work, and they need no entitlements or separate helper app.
    unsafe extern "C" {
        static kSBXProfilePureComputation: c_char;
        fn sandbox_init(profile: *const c_char, flags: u64, error: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(error: *mut c_char);
    }
    const SANDBOX_NAMED: u64 = 1;
    let mut error = std::ptr::null_mut();
    // SAFETY: the profile is a NUL terminated name that the system library
    // exports, and an error message is freed the way sandbox.h says.
    unsafe {
        if sandbox_init(
            &raw const kSBXProfilePureComputation,
            SANDBOX_NAMED,
            &mut error,
        ) == 0
        {
            return Ok(());
        }
        let message = if error.is_null() {
            "unknown error".to_string()
        } else {
            let message = CStr::from_ptr(error).to_string_lossy().into_owned();
            sandbox_free_error(error);
            message
        };
        Err(format!("Could not sandbox the PDF renderer: {message}").into())
    }
}

#[cfg(desktop)]
fn library_path(executable: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let directory = executable.parent().ok_or("Missing application directory")?;
    let name = pdfium_render::prelude::Pdfium::pdfium_platform_library_name();
    #[cfg(linux_desktop)]
    {
        // Linux packages keep private libraries out of the executable directory.
        for libdir in ["../lib64/flectar-mail", "../lib/flectar-mail"] {
            let packaged = directory.join(libdir).join(&name);
            if packaged.is_file() {
                return Ok(packaged);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let packaged = directory.join("../Frameworks").join(&name);
        if packaged.is_file() {
            return Ok(packaged);
        }
    }
    Ok(directory.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_unbounded_renderer_dimensions() {
        assert!(dimensions(0, 10).is_err());
        assert!(dimensions(u32::MAX, u32::MAX).is_err());
        assert!(dimensions(MAX_EDGE + 1, 1).is_err());
        assert_eq!(dimensions(1440, 1920).unwrap(), 11_059_200);
    }
}
