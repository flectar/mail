//! Windows runs the PDF worker in an AppContainer without capabilities, so a
//! PDFium bug can't reach the network, the user's files or other processes.
//! A job object also caps its memory, CPU time and process count, keeps it
//! away from the clipboard and other programs' windows, and ends it when the
//! job is closed. The worker inherits no handles but its two pipes.
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT, LocalFree,
    SetHandleInformation, WAIT_OBJECT_0, WIN32_ERROR,
};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid,
    FreeSid, GetAce, GetTokenInformation, NO_INHERITANCE, PSID, SECURITY_CAPABILITIES,
    SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TokenIsAppContainer, WinBuiltinAnyPackageSid,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_EXECUTE, FILE_GENERIC_READ};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOB_OBJECT_LIMIT_PROCESS_TIME, JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS,
    JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES,
    JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
    JOB_OBJECT_UILIMIT_WRITECLIPBOARD, JOBOBJECT_BASIC_UI_RESTRICTIONS,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

/// The worker's AppContainer. The same name always gives the same SID, so
/// files that were opened to it stay open to it.
const CONTAINER: &str = "Flectar.Mail.PdfPreview";
/// The access the worker needs to its own executable and to PDFium.
const READ_AND_EXECUTE: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;

/// A running worker. Dropping it ends the process.
pub(super) struct Worker {
    process: Arc<OwnedHandle>,
    job: OwnedHandle,
    pub(super) stdin: Option<tokio::fs::File>,
    pub(super) stdout: Option<tokio::fs::File>,
}

impl Worker {
    /// Starts `executable` with `argument` in the AppContainer, after letting
    /// the AppContainer read and run `files`, the executable and the
    /// libraries it loads. It fails rather than start the worker unconfined.
    pub(super) fn spawn(
        executable: &Path,
        files: &[&Path],
        argument: &str,
    ) -> Result<Self, String> {
        let container = Sid::container()?;
        for file in files {
            allow(file, &container)?;
        }
        let job = job()?;
        let (child_input, input) = pipe()?;
        let (output, child_output) = pipe()?;
        let application = wide(executable.as_os_str());
        let mut command_line: Vec<u16> = "\""
            .encode_utf16()
            .chain(executable.as_os_str().encode_wide())
            .chain("\" ".encode_utf16())
            .chain(argument.encode_utf16())
            .chain([0])
            .collect();
        // Windows needs SystemRoot to load system libraries, and it won't
        // start an AppContainer without LOCALAPPDATA, which it points at the
        // AppContainer's own folder. The worker gets no other variables, so no
        // credentials, PATH or library search overrides. The names are in the
        // sorted order Windows expects.
        let mut environment = Vec::new();
        for name in ["LOCALAPPDATA", "SystemRoot"] {
            if let Some(value) = std::env::var_os(name) {
                environment.extend(name.encode_utf16().chain([u16::from(b'=')]));
                environment.extend(value.encode_wide().chain([0]));
            }
        }
        environment.extend([0, 0]);
        // The AppContainer can read the system folder, wherever the app's
        // own working folder is.
        let directory = system_directory()?;
        let mut capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: container.0,
            ..Default::default()
        };
        let inherited: [HANDLE; 2] = [child_input.as_raw_handle(), child_output.as_raw_handle()];
        let mut attributes = Attributes::new(2)?;
        attributes.set(
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            (&raw mut capabilities).cast(),
            size_of::<SECURITY_CAPABILITIES>(),
        )?;
        attributes.set(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            inherited.as_ptr().cast_mut().cast(),
            size_of_val(&inherited),
        )?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = child_input.as_raw_handle();
        startup.StartupInfo.hStdOutput = child_output.as_raw_handle();
        startup.lpAttributeList = attributes.list();
        let mut information = PROCESS_INFORMATION::default();
        // SAFETY: every pointer refers to a live local that outlives the call.
        // The pipe ends become inheritable only for this call, and the handle
        // list keeps the worker from inheriting any other handle.
        let created = unsafe {
            for handle in inherited {
                if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0 {
                    return Err(last_error());
                }
            }
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_UNICODE_ENVIRONMENT
                    | CREATE_NO_WINDOW
                    | CREATE_SUSPENDED,
                environment.as_ptr().cast(),
                directory.as_ptr(),
                (&raw const startup).cast(),
                &mut information,
            )
        };
        if created == 0 {
            return Err(last_error());
        }
        // The worker holds its own copies of its pipe ends now.
        drop((child_input, child_output, attributes));
        // SAFETY: CreateProcessW returned these handles for this caller to own.
        let (process, thread) = unsafe {
            (
                OwnedHandle::from_raw_handle(information.hProcess),
                OwnedHandle::from_raw_handle(information.hThread),
            )
        };
        let process = Arc::new(process);
        let worker = Self {
            process,
            job,
            stdin: Some(tokio::fs::File::from_std(input.into())),
            stdout: Some(tokio::fs::File::from_std(output.into())),
        };
        // The worker hasn't run yet. It starts only in the job and only
        // once Windows reports it is in the AppContainer.
        // SAFETY: both handles are open, and the process handle has all access.
        unsafe {
            if AssignProcessToJobObject(worker.job.as_raw_handle(), worker.process.as_raw_handle())
                == 0
            {
                return Err(last_error());
            }
            if !in_app_container(worker.process.as_raw_handle())? {
                return Err("The PDF renderer did not start in its sandbox.".into());
            }
            if ResumeThread(thread.as_raw_handle()) == u32::MAX {
                return Err(last_error());
            }
        }
        Ok(worker)
    }

    pub(super) async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        let process = self.process.clone();
        tokio::task::spawn_blocking(move || {
            // SAFETY: the handle stays open while this closure owns a reference.
            // The caller's deadline drops the worker, which ends the process,
            // so this wait is bounded too.
            unsafe {
                if WaitForSingleObject(process.as_raw_handle(), 30_000) != WAIT_OBJECT_0 {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "The PDF renderer did not exit.",
                    ));
                }
                let mut code = 0;
                if GetExitCodeProcess(process.as_raw_handle(), &mut code) == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(std::process::ExitStatus::from_raw(code))
            }
        })
        .await
        .map_err(io::Error::other)?
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Closing the job ends the worker as well. Ending it here first
        // breaks its pipes before anything waits on them.
        // SAFETY: the process handle is open and has terminate access.
        unsafe {
            TerminateProcess(self.process.as_raw_handle(), 1);
        }
    }
}

/// A SID that FreeSid releases.
struct Sid(PSID);

impl Sid {
    fn container() -> Result<Self, String> {
        let name = wide(OsStr::new(CONTAINER));
        let mut sid = null_mut();
        // SAFETY: the name is NUL terminated and the SID returned is ours.
        unsafe {
            if CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                null(),
                0,
                &mut sid,
            ) >= 0
            {
                return Ok(Self(sid));
            }
            // The profile exists after the first preview. Windows runs an
            // AppContainer by its SID, which the name alone gives.
            let derived = DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid);
            if derived < 0 {
                return Err(format!(
                    "Could not set up the PDF sandbox: {}",
                    io::Error::from_raw_os_error(derived)
                ));
            }
        }
        Ok(Self(sid))
    }
}

impl Drop for Sid {
    fn drop(&mut self) {
        // SAFETY: the SID came from CreateAppContainerProfile or
        // DeriveAppContainerSidFromAppContainerName, which say to free it so.
        unsafe {
            FreeSid(self.0);
        }
    }
}

/// Memory that LocalFree releases.
struct Local<T>(*mut T);

impl<T> Drop for Local<T> {
    fn drop(&mut self) {
        // SAFETY: the pointer is null or came from an API that returns
        // LocalAlloc memory.
        unsafe {
            LocalFree(self.0.cast());
        }
    }
}

/// Lets the AppContainer read and run `file`. Files that every AppContainer
/// may already read, as under Program Files or in a Microsoft Store install,
/// keep their permissions. Those folders also refuse changes from the user,
/// so a refusal leaves the file as it is, and the worker can't start if the
/// AppContainer can't read it.
fn allow(file: &Path, container: &Sid) -> Result<(), String> {
    let name = wide(file.as_os_str());
    let mut acl = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: the name is NUL terminated, and the security descriptor that
    // holds the ACL is freed after its last use.
    unsafe {
        let status = GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        );
        if status != ERROR_SUCCESS {
            return Err(os_error(status));
        }
        let _descriptor = Local(descriptor);
        let mut any_package = [0u32; SECURITY_MAX_SID_SIZE.div_ceil(4) as usize];
        let mut size = SECURITY_MAX_SID_SIZE;
        let any_package: PSID = any_package.as_mut_ptr().cast();
        if CreateWellKnownSid(WinBuiltinAnyPackageSid, null_mut(), any_package, &mut size) == 0 {
            return Err(last_error());
        }
        if allows(acl, container.0) || allows(acl, any_package) {
            return Ok(());
        }
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: READ_AND_EXECUTE,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: container.0.cast(),
            },
        };
        let mut updated = null_mut();
        let status = SetEntriesInAclW(1, &access, acl, &mut updated);
        if status != ERROR_SUCCESS {
            return Err(os_error(status));
        }
        let updated = Local(updated);
        match SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            updated.0,
            null(),
        ) {
            ERROR_SUCCESS | ERROR_ACCESS_DENIED => Ok(()),
            status => Err(os_error(status)),
        }
    }
}

/// Whether `acl` lets `sid` read and run a file. A missing ACL lets anyone.
///
/// # Safety
/// `acl` is null or a valid ACL, and `sid` is a valid SID.
unsafe fn allows(acl: *const ACL, sid: PSID) -> bool {
    if acl.is_null() {
        return true;
    }
    // SAFETY: the caller passes a valid ACL, so GetAce returns its entries.
    unsafe {
        (0..u32::from((*acl).AceCount)).any(|index| {
            let mut entry = null_mut();
            if GetAce(acl, index, &mut entry) == 0 {
                return false;
            }
            let header = &*entry.cast::<ACE_HEADER>();
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
                return false;
            }
            let allowed = entry.cast::<ACCESS_ALLOWED_ACE>();
            (*allowed).Mask & READ_AND_EXECUTE == READ_AND_EXECUTE
                && EqualSid((&raw const (*allowed).SidStart).cast_mut().cast(), sid) != 0
        })
    }
}

/// Whether the process runs in an AppContainer.
///
/// # Safety
/// `process` is an open process handle with query access.
unsafe fn in_app_container(process: HANDLE) -> Result<bool, String> {
    let mut token = null_mut();
    // SAFETY: the token handle is owned as soon as it is returned.
    unsafe {
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return Err(last_error());
        }
        let token = OwnedHandle::from_raw_handle(token);
        let mut value = 0u32;
        let mut size = 0;
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenIsAppContainer,
            (&raw mut value).cast(),
            size_of::<u32>() as u32,
            &mut size,
        ) == 0
        {
            return Err(last_error());
        }
        Ok(value != 0)
    }
}

/// The job that caps the worker. Closing it ends the worker.
fn job() -> Result<OwnedHandle, String> {
    // SAFETY: the job handle is owned at once, including on every error path.
    unsafe {
        let raw = CreateJobObjectW(null(), null());
        if raw.is_null() {
            return Err(last_error());
        }
        let job = OwnedHandle::from_raw_handle(raw);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_PROCESS_TIME;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        limits.BasicLimitInformation.PerProcessUserTimeLimit = 15 * 10_000_000;
        limits.ProcessMemoryLimit = 768 * 1024 * 1024;
        let interface = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_EXITWINDOWS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
        };
        if SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of_val(&limits) as u32,
        ) == 0
            || SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicUIRestrictions,
                (&raw const interface).cast(),
                size_of_val(&interface) as u32,
            ) == 0
        {
            return Err(last_error());
        }
        Ok(job)
    }
}

/// An anonymous pipe as its read and write ends. Neither end is inheritable.
fn pipe() -> Result<(OwnedHandle, OwnedHandle), String> {
    let (mut read, mut write) = (null_mut(), null_mut());
    // SAFETY: both ends are owned as soon as the pipe exists.
    unsafe {
        if CreatePipe(&mut read, &mut write, null(), 0) == 0 {
            return Err(last_error());
        }
        Ok((
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        ))
    }
}

/// A list of process creation attributes. The values it points to must
/// outlive the process creation.
struct Attributes(Vec<usize>);

impl Attributes {
    fn new(count: u32) -> Result<Self, String> {
        let mut size = 0;
        // SAFETY: the first call only reports the size the list needs, and
        // the second fills a buffer of that size, aligned for pointers.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size);
            let mut list = Self(vec![0; size.div_ceil(size_of::<usize>())]);
            if InitializeProcThreadAttributeList(list.list(), count, 0, &mut size) == 0 {
                list.0 = Vec::new();
                return Err(last_error());
            }
            Ok(list)
        }
    }

    fn list(&mut self) -> *mut core::ffi::c_void {
        self.0.as_mut_ptr().cast()
    }

    fn set(
        &mut self,
        attribute: u32,
        value: *mut core::ffi::c_void,
        size: usize,
    ) -> Result<(), String> {
        // SAFETY: the list was initialized, and the caller keeps the value alive.
        if unsafe {
            UpdateProcThreadAttribute(
                self.list(),
                0,
                attribute as usize,
                value,
                size,
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(last_error());
        }
        Ok(())
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            // SAFETY: the list was initialized.
            unsafe { DeleteProcThreadAttributeList(self.list()) };
        }
    }
}

/// The system folder, such as C:\Windows\System32, NUL terminated.
fn system_directory() -> Result<Vec<u16>, String> {
    let mut buffer = vec![0u16; 260];
    loop {
        // SAFETY: the buffer holds as many characters as it says.
        let length =
            unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if length == 0 {
            return Err(last_error());
        }
        if length < buffer.len() {
            buffer.truncate(length + 1);
            return Ok(buffer);
        }
        buffer.resize(length + 1, 0);
    }
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

fn os_error(status: WIN32_ERROR) -> String {
    io::Error::from_raw_os_error(status as i32).to_string()
}

fn last_error() -> String {
    io::Error::last_os_error().to_string()
}
