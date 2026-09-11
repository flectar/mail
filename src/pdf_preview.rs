//! Desktop uses a short-lived renderer process; mobile uses one bounded native
//! worker with cooperative cancellation. Both load only the packaged PDFium
//! library on first preview and return one bounded RGBA page.
//! No PDF scripts, links, forms actions, or document attachments are executed.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use std::sync::Arc;

pub(crate) const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_EDGE: u32 = 2880;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const MAGIC: &[u8; 4] = b"FPD1";

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

#[cfg(not(any(target_os = "android", target_os = "ios")))]
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
    let mut command = tokio::process::Command::new(executable);
    command
        .arg("--render-pdf-page")
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // Windows needs SystemRoot to resolve native system libraries. Do not pass
    // credentials, profile paths, PATH or dynamic-loader overrides to PDFium.
    #[cfg(target_os = "windows")]
    {
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    let _job = WindowsJob::constrain(child.id().ok_or("PDF worker has no process ID")?)?;
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

#[cfg(any(test, target_os = "android", target_os = "ios"))]
#[path = "pdf_preview_mobile.rs"]
mod mobile;
#[cfg(any(target_os = "android", target_os = "ios"))]
pub use mobile::cancel_active;
#[cfg(target_os = "android")]
pub use mobile::configure_android;
#[cfg(any(target_os = "android", target_os = "ios"))]
pub(crate) use mobile::render;

/// Called by the thin desktop launcher BEFORE PlatformContext or the UI exists.
/// The reserved argument alone enables the worker; documents arrive only via stdin.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn run_worker_if_requested() -> Option<i32> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("--render-pdf-page")) {
        return None;
    }
    Some(if worker().is_ok() { 0 } else { 1 })
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn worker() -> Result<(), Box<dyn std::error::Error>> {
    use pdfium_render::prelude::*;
    use std::io::{Read, Write};
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
        #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
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

// Files arrive through stdin; only system font directories remain readable.
// Fail closed if the kernel cannot enforce the sandbox. Seccomp complements
// Landlock by denying networking, process creation, ptrace and signal delivery.
#[cfg(target_os = "linux")]
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
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        return Err("PDF sandbox requires Linux Landlock support".into());
    }
    let mut calls = vec![
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_close,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_pread64,
        libc::SYS_lseek,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
        libc::SYS_openat,
        libc::SYS_faccessat,
        libc::SYS_faccessat2,
        libc::SYS_fcntl,
        libc::SYS_flock,
        libc::SYS_getdents64,
        libc::SYS_readlinkat,
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
    #[cfg(target_arch = "x86_64")]
    calls.extend([
        libc::SYS_open,
        libc::SYS_access,
        libc::SYS_stat,
        libc::SYS_lstat,
        libc::SYS_readlink,
    ]);
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

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn library_path(executable: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let directory = executable.parent().ok_or("Missing application directory")?;
    let name = pdfium_render::prelude::Pdfium::pdfium_platform_library_name();
    #[cfg(target_os = "linux")]
    {
        // Linux packages keep private libraries out of the executable directory.
        for libdir in ["../lib64/flectar-mail", "../lib/flectar-mail"] {
            let packaged = directory.join(libdir).join(&name);
            if packaged.is_file() {
                return Ok(packaged);
            }
        }
    }
    Ok(directory.join(name))
}

#[cfg(target_os = "windows")]
struct WindowsJob(std::os::windows::io::OwnedHandle);
#[cfg(target_os = "windows")]
impl WindowsJob {
    fn constrain(pid: u32) -> Result<Self, String> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::{JobObjects::*, Threading::*};
        // Both handles are immediately owned, including every error path.
        // Assignment happens before sending any document bytes to the child.
        unsafe {
            let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if raw.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let job = Self(OwnedHandle::from_raw_handle(raw));
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_PROCESS_TIME;
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
            limits.BasicLimitInformation.PerProcessUserTimeLimit = 15 * 10_000_000;
            limits.ProcessMemoryLimit = 768 * 1024 * 1024;
            if SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let raw = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if raw.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let process = OwnedHandle::from_raw_handle(raw);
            if AssignProcessToJobObject(job.0.as_raw_handle(), process.as_raw_handle()) == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(job)
        }
    }
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
