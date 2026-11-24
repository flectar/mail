//! Kernel objects only the current user can open, such as the named pipes the
//! app listens on, and checks on who serves a pipe the app opens. Without a
//! security descriptor of its own, a pipe gets the default one, which lets
//! every user and anonymous clients open it for reading. Pipe names are shared
//! by every user of the computer, so a pipe the app opens by name may be
//! another user's.

use std::{ffi::c_void, io, mem::size_of, os::windows::io::AsRawHandle};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GENERIC_ALL, HANDLE},
    Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, EqualSid, GetLengthSid,
        GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, SECURITY_ATTRIBUTES,
        SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenSessionId,
        TokenUser,
    },
    System::{
        Pipes::GetNamedPipeServerProcessId,
        SystemServices::SECURITY_DESCRIPTOR_REVISION,
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

/// The user a process runs as: a `TOKEN_USER` and the SID it points into.
struct TokenUserBuffer(Vec<u64>);

impl TokenUserBuffer {
    fn of_token(token: HANDLE) -> io::Result<Self> {
        let mut length = 0u32;
        // SAFETY: asks only for the size, with no buffer.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut length) };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        // u64 elements align the TOKEN_USER the call writes.
        let mut buffer = vec![0u64; (length as usize).div_ceil(size_of::<u64>())];
        // SAFETY: `buffer` holds at least `length` bytes.
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(buffer))
    }

    fn sid(&self) -> *mut c_void {
        // SAFETY: the buffer holds the TOKEN_USER `of_token` read.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

/// A process's access token, closed when dropped.
struct Token(HANDLE);

impl Token {
    fn of_process(process: HANDLE) -> io::Result<Self> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `process` is a valid handle and `token` an output location.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(token))
    }

    fn of_current_process() -> io::Result<Self> {
        // SAFETY: the pseudo handle of this process needs no closing.
        Self::of_process(unsafe { GetCurrentProcess() })
    }

    fn of_process_id(process_id: u32) -> io::Result<Self> {
        // SAFETY: a plain call; the handle is closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let token = Self::of_process(process);
        // SAFETY: opened above and not used after this.
        unsafe { CloseHandle(process) };
        token
    }

    fn user(&self) -> io::Result<TokenUserBuffer> {
        TokenUserBuffer::of_token(self.0)
    }

    /// The Remote Desktop session the process runs in.
    fn session(&self) -> io::Result<u32> {
        let mut session = 0u32;
        let mut length = 0u32;
        // SAFETY: `session` holds the four bytes the call writes.
        if unsafe {
            GetTokenInformation(
                self.0,
                TokenSessionId,
                (&raw mut session).cast(),
                size_of::<u32>() as u32,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(session)
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: opened by `of_process` and not used after this.
        unsafe { CloseHandle(self.0) };
    }
}

/// A security descriptor whose only entry gives the current user full access.
pub(crate) struct CurrentUserOnly {
    _user: TokenUserBuffer,
    _acl: Vec<u64>,
    descriptor: Box<SECURITY_DESCRIPTOR>,
}

// SAFETY: the buffers are owned and never change after `new`, and the
// descriptor's pointers only point into them.
unsafe impl Send for CurrentUserOnly {}
unsafe impl Sync for CurrentUserOnly {}

impl CurrentUserOnly {
    pub(crate) fn new() -> io::Result<Self> {
        let user = Token::of_current_process()?.user()?;
        let sid = user.sid();
        // SAFETY: `sid` is the token's valid SID.
        let sid_length = unsafe { GetLengthSid(sid) } as usize;
        let acl_length =
            size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_length;
        let mut acl = vec![0u64; acl_length.div_ceil(size_of::<u64>())];
        let acl_pointer = acl.as_mut_ptr().cast::<ACL>();
        // SAFETY: `acl` holds `acl_length` bytes and `sid` is valid.
        if unsafe {
            InitializeAcl(acl_pointer, acl_length as u32, ACL_REVISION) == 0
                || AddAccessAllowedAce(acl_pointer, ACL_REVISION, GENERIC_ALL, sid) == 0
        } {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: an all-zero SECURITY_DESCRIPTOR is plain data, initialized
        // right below.
        let mut descriptor = Box::new(unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() });
        let descriptor_pointer: *mut SECURITY_DESCRIPTOR = &mut *descriptor;
        // SAFETY: the descriptor and the ACL it points to live as long as
        // `self`.
        if unsafe {
            InitializeSecurityDescriptor(descriptor_pointer.cast(), SECURITY_DESCRIPTOR_REVISION)
                == 0
                || SetSecurityDescriptorDacl(descriptor_pointer.cast(), 1, acl_pointer, 0) == 0
        } {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _user: user,
            _acl: acl,
            descriptor,
        })
    }

    /// Attributes to create an object with this descriptor. They point into
    /// `self`, so use them while it lives.
    pub(crate) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::from_ref::<SECURITY_DESCRIPTOR>(&self.descriptor)
                .cast_mut()
                .cast(),
            bInheritHandle: 0,
        }
    }
}

/// Which process the app may talk to over a pipe it opens.
#[derive(Clone, Copy)]
pub(crate) enum Peer {
    /// Any process of the current user.
    SameUser,
    /// A process of the current user in this process's session, whose
    /// windows the same person sees.
    SameSession,
}

/// The id of the process serving `pipe`, once it is known to be a `peer`.
pub(crate) fn pipe_server(pipe: &impl AsRawHandle, peer: Peer) -> io::Result<u32> {
    let mut process_id = 0;
    // SAFETY: `pipe` is an open pipe handle and `process_id` a valid output
    // location.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle() as HANDLE, &mut process_id) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let theirs = Token::of_process_id(process_id)?;
    let ours = Token::of_current_process()?;
    let (their_user, our_user) = (theirs.user()?, ours.user()?);
    // SAFETY: both SIDs come from their tokens.
    if unsafe { EqualSid(their_user.sid(), our_user.sid()) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the pipe belongs to another user",
        ));
    }
    if matches!(peer, Peer::SameSession) && theirs.session()? != ours.session()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the pipe belongs to another session",
        ));
    }
    Ok(process_id)
}
