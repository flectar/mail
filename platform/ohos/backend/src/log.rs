//! `hilog` logging plus a stderr mirror.
//!
//! Application `.so` output does not reliably reach `hilog`, so every message
//! goes through `OH_LOG_Print` (tag `slint-ohos`) and is also written to stderr
//! for hosts that capture it.

use core::ffi::c_char;

use crate::ffi;

/// OHOS hilog service domain. Must be within 0x0000..=0xFFFF (hilog silently
/// drops calls with an out-of-range domain — 0xD000D is invalid).
pub const DOMAIN: u32 = 0xD00D;
const TAG: &[u8] = b"slint-ohos\0";
const FORMAT: &[u8] = b"%{public}s\0";

/// `LogType::LOG_APP`
const LOG_APP: i32 = 0;
/// `LogLevel::LOG_INFO` / `LOG_ERROR`
const LOG_INFO: i32 = 4;
const LOG_ERROR: i32 = 6;

fn write(level: i32, message: &str) {
    let mut buffer = String::with_capacity(message.len() + 1);
    buffer.push_str(&message.replace('\0', "\u{FFFD}"));
    buffer.push('\0');
    // SAFETY: the format string is a static NUL-terminated literal and the
    // variadic argument is the matching `%{public}s` C string.
    unsafe {
        ffi::OH_LOG_Print(
            LOG_APP,
            level,
            DOMAIN,
            TAG.as_ptr().cast::<c_char>(),
            FORMAT.as_ptr().cast::<c_char>(),
            buffer.as_ptr().cast::<c_char>(),
        );
    }
}

/// Log at info level to hilog (and stderr).
pub fn info(message: &str) {
    write(LOG_INFO, message);
    eprintln!("[slint-ohos] {message}");
}

/// Log at error level to hilog (and stderr).
pub fn error(message: &str) {
    write(LOG_ERROR, message);
    eprintln!("[slint-ohos] ERROR: {message}");
}

/// `info!`-style macro using the crate's hilog logger.
#[macro_export]
macro_rules! ohos_info {
    ($($arg:tt)*) => { $crate::log::info(&format!($($arg)*)) };
}

/// `error!`-style macro using the crate's hilog logger.
#[macro_export]
macro_rules! ohos_error {
    ($($arg:tt)*) => { $crate::log::error(&format!($($arg)*)) };
}
