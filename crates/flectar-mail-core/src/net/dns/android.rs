//! Android: the platform resolver (`android_res_nquery`), which applies the
//! device's Private DNS and per-network settings. It came with Android 10
//! (API 29), so it is looked up at run time, and older versions skip the
//! lookup rather than send plain DNS around Private DNS.

use std::ffi::{CString, c_char, c_int};
use std::sync::OnceLock;
use std::time::Instant;

type Query = unsafe extern "C" fn(u64, *const c_char, c_int, c_int, u32) -> c_int;
type Answer = unsafe extern "C" fn(c_int, *mut c_int, *mut u8, usize) -> c_int;
type Cancel = unsafe extern "C" fn(c_int);

struct Api {
    query: Query,
    answer: Answer,
    cancel: Cancel,
}

/// The device's default network (`NETWORK_UNSPECIFIED`).
const DEFAULT_NETWORK: u64 = 0;

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: libandroid.so is a public NDK library that stays loaded,
        // and each symbol has the C signature of its alias above
        // (android/multinetwork.h).
        unsafe {
            let library = libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW);
            if library.is_null() {
                return None;
            }
            let symbol = |name: &std::ffi::CStr| {
                let symbol = libc::dlsym(library, name.as_ptr());
                (!symbol.is_null()).then_some(symbol)
            };
            Some(Api {
                query: std::mem::transmute::<*mut libc::c_void, Query>(symbol(
                    c"android_res_nquery",
                )?),
                answer: std::mem::transmute::<*mut libc::c_void, Answer>(symbol(
                    c"android_res_nresult",
                )?),
                cancel: std::mem::transmute::<*mut libc::c_void, Cancel>(symbol(
                    c"android_res_cancel",
                )?),
            })
        }
    })
    .as_ref()
}

pub(super) fn exchanges(domain: &str) -> Option<Vec<(u16, String)>> {
    let api = api()?;
    let name = CString::new(domain).ok()?;
    // SAFETY: `name` is NUL-terminated.
    let fd = unsafe {
        (api.query)(
            DEFAULT_NETWORK,
            name.as_ptr(),
            c_int::from(super::CLASS_IN),
            c_int::from(super::TYPE_MX),
            0,
        )
    };
    if fd < 0 {
        return None;
    }
    let deadline = Instant::now() + super::TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = c_int::try_from(left.as_millis()).unwrap_or(c_int::MAX);
        // SAFETY: one pollfd that lives on this stack frame.
        let polled = unsafe { libc::poll(&mut ready, 1, millis) };
        if polled > 0 {
            break;
        }
        let interrupted =
            polled < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
        if !interrupted || left.is_zero() {
            // SAFETY: the query's descriptor, not read yet; cancel closes it.
            unsafe { (api.cancel)(fd) };
            return None;
        }
    }
    let mut code = 0;
    let mut answer = vec![0_u8; 4096];
    // SAFETY: `answer` holds `answer.len()` bytes; the call closes `fd`.
    let length = unsafe { (api.answer)(fd, &mut code, answer.as_mut_ptr(), answer.len()) };
    let length = usize::try_from(length).ok()?;
    if code != 0 {
        return None;
    }
    super::exchanges_in_message(answer.get(..length)?)
}
