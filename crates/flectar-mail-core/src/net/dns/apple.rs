//! macOS and iOS: DNS-SD's `DNSServiceQueryRecord`, which answers through
//! the system resolver with its per-domain, VPN and encrypted DNS settings.
//! Apple documents it for both, unlike the configuration store other
//! resolvers read.

use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_char, c_int, c_void};
use std::time::Instant;

#[repr(C)]
struct ServiceRef {
    _private: [u8; 0],
}

type Reply = unsafe extern "C" fn(
    *mut ServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    u16,
    u16,
    u16,
    *const c_void,
    u32,
    *mut c_void,
);

// dns_sd.h, in the system library.
unsafe extern "C" {
    fn DNSServiceQueryRecord(
        service: *mut *mut ServiceRef,
        flags: u32,
        interface_index: u32,
        full_name: *const c_char,
        record_type: u16,
        record_class: u16,
        reply: Reply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(service: *mut ServiceRef) -> c_int;
    fn DNSServiceProcessResult(service: *mut ServiceRef) -> i32;
    fn DNSServiceRefDeallocate(service: *mut ServiceRef);
}

const NO_ERROR: i32 = 0;
const FLAG_MORE_COMING: u32 = 0x1;
const FLAG_ADD: u32 = 0x2;
/// Ends the query with an error when no answer comes.
const FLAG_TIMEOUT: u32 = 0x1_0000;

/// What the replies brought. The reply callback only gets a shared
/// reference, so the fields change through cells.
#[derive(Default)]
struct Answers {
    exchanges: RefCell<Vec<(u16, String)>>,
    done: Cell<bool>,
}

unsafe extern "C" fn reply(
    _service: *mut ServiceRef,
    flags: u32,
    _interface_index: u32,
    error: i32,
    _full_name: *const c_char,
    record_type: u16,
    _record_class: u16,
    length: u16,
    data: *const c_void,
    _ttl: u32,
    context: *mut c_void,
) {
    // SAFETY: `context` is the Answers that `exchanges` passed in, which
    // outlives the query.
    let answers = unsafe { &*context.cast_const().cast::<Answers>() };
    if error != NO_ERROR {
        answers.done.set(true);
        return;
    }
    if flags & FLAG_ADD != 0 && record_type == super::TYPE_MX && !data.is_null() {
        // SAFETY: DNS-SD hands over `length` bytes of record data.
        let data = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), usize::from(length)) };
        answers
            .exchanges
            .borrow_mut()
            .extend(super::exchange_in_record(data));
    }
    if flags & FLAG_MORE_COMING == 0 {
        answers.done.set(true);
    }
}

pub(super) fn exchanges(domain: &str) -> Option<Vec<(u16, String)>> {
    let name = CString::new(domain).ok()?;
    let answers = Answers::default();
    let mut service = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated, and `answers` outlives the query,
    // which is deallocated below before this function returns.
    let started = unsafe {
        DNSServiceQueryRecord(
            &mut service,
            FLAG_TIMEOUT,
            0,
            name.as_ptr(),
            super::TYPE_MX,
            super::CLASS_IN,
            reply,
            (&raw const answers).cast_mut().cast(),
        )
    };
    if started != NO_ERROR || service.is_null() {
        return None;
    }
    // SAFETY: a live service reference.
    let socket = unsafe { DNSServiceRefSockFD(service) };
    let deadline = Instant::now() + super::TIMEOUT;
    while !answers.done.get() && socket >= 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut ready = libc::pollfd {
            fd: socket,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = c_int::try_from(left.as_millis()).unwrap_or(c_int::MAX);
        // SAFETY: one pollfd that lives on this stack frame.
        let polled = unsafe { libc::poll(&mut ready, 1, millis) };
        if polled < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        // SAFETY: a live service reference whose socket has a reply.
        if polled <= 0 || unsafe { DNSServiceProcessResult(service) } != NO_ERROR {
            break;
        }
    }
    // SAFETY: the reference from DNSServiceQueryRecord, freed once.
    unsafe { DNSServiceRefDeallocate(service) };
    Some(answers.exchanges.into_inner())
}
