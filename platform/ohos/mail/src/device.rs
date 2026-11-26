//! The name Flectar Mail Sync lists this device by, read from the system
//! parameters through Basic Services Kit's `deviceinfo.h`. ArkTS's
//! `deviceInfo.marketName` and `deviceInfo.productModel` read the same ones.

use core::ffi::{CStr, c_char};

#[link(name = "deviceinfo_ndk.z", kind = "dylib")]
unsafe extern "C" {
    /// The market name, such as "HUAWEI MateBook Pro".
    fn OH_GetMarketName() -> *const c_char;
    /// The product model, such as "HAD-W32".
    fn OH_GetProductModel() -> *const c_char;
    /// The kind of device, such as "phone", "tablet" or "2in1".
    fn OH_GetDeviceType() -> *const c_char;
}

/// The device's market name, or its product model when it has none. Android
/// names a device by its model the same way.
pub(super) fn name() -> Option<String> {
    // SAFETY: the call takes nothing and returns null or a NUL-terminated
    // string that the system keeps.
    let market = unsafe { text(OH_GetMarketName()) };
    // SAFETY: as above.
    market.or_else(|| unsafe { text(OH_GetProductModel()) })
}

/// Whether Scan Kit's scanner runs here: on phones and tablets, not on
/// computers.
pub(super) fn scans_qr() -> bool {
    // SAFETY: the call takes nothing and returns null or a NUL-terminated
    // string that the system keeps.
    let kind = unsafe { text(OH_GetDeviceType()) };
    matches!(kind.as_deref(), Some("phone" | "tablet"))
}

/// A system string without surrounding space, if it says anything.
///
/// # Safety
///
/// `value` is null or points to a NUL-terminated string.
unsafe fn text(value: *const c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: the caller's contract.
    let value = unsafe { CStr::from_ptr(value) }.to_string_lossy();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
