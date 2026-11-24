//! Linux, BSD and OpenHarmony: the C library's `res_query`, which follows
//! the system's resolver settings. It is looked up when first needed, as
//! Thunderbird does, because glibc before 2.34 keeps it in libresolv rather
//! than in the C library itself.

use std::ffi::{CString, c_char, c_int, c_uchar};
use std::sync::OnceLock;

type ResQuery = unsafe extern "C" fn(*const c_char, c_int, c_int, *mut c_uchar, c_int) -> c_int;

fn res_query() -> Option<ResQuery> {
    // The function's address, found once.
    static ADDRESS: OnceLock<Option<usize>> = OnceLock::new();
    let address = (*ADDRESS.get_or_init(|| {
        let names = [c"res_query", c"__res_query"];
        // SAFETY: dlsym and dlopen take NUL-terminated names, and a handle
        // that dlopen returned stays loaded for the life of the process.
        unsafe {
            for name in names {
                let symbol = libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr());
                if !symbol.is_null() {
                    return Some(symbol as usize);
                }
            }
            for library in [c"libresolv.so.2", c"libresolv.so"] {
                let handle = libc::dlopen(library.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
                if handle.is_null() {
                    continue;
                }
                for name in names {
                    let symbol = libc::dlsym(handle, name.as_ptr());
                    if !symbol.is_null() {
                        return Some(symbol as usize);
                    }
                }
            }
        }
        None
    }))?;
    // SAFETY: the symbol is res_query, whose C signature ResQuery mirrors.
    Some(unsafe { std::mem::transmute::<usize, ResQuery>(address) })
}

pub(super) fn exchanges(domain: &str) -> Option<Vec<(u16, String)>> {
    let query = res_query()?;
    let name = CString::new(domain).ok()?;
    let mut answer = vec![0_u8; 4096];
    let capacity = c_int::try_from(answer.len()).ok()?;
    // SAFETY: `name` is NUL-terminated and `answer` holds `capacity` bytes.
    // res_query keeps its state per thread in glibc and musl.
    let length = unsafe {
        query(
            name.as_ptr(),
            c_int::from(super::CLASS_IN),
            c_int::from(super::TYPE_MX),
            answer.as_mut_ptr(),
            capacity,
        )
    };
    // A longer answer than the buffer comes back cut off.
    let length = usize::try_from(length).ok()?;
    super::exchanges_in_message(answer.get(..length)?)
}
