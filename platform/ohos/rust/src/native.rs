//! Small synchronous ABI. Native code retains the window and serializes buffer access.
#[cfg(test)]
pub(crate) static TEST_IME_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
pub(crate) static TEST_IME_SHOW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(target_env = "ohos")]
use std::ffi::{CString, c_char};

#[cfg(target_env = "ohos")]
unsafe extern "C" {
    fn feihong_native_present_rgba_damage(
        pixels: *const u8,
        width: u32,
        height: u32,
        generation: u64,
        dirty_x: i32,
        dirty_y: i32,
        dirty_width: u32,
        dirty_height: u32,
    ) -> i32;
    fn feihong_native_set_performance_diagnostics(enabled: bool);
    fn feihong_native_ui_qos(interactive: bool) -> i32;
    fn feihong_request_frame() -> i32;
    fn feihong_native_ime_update(
        enabled: bool,
        text: *const u16,
        length: usize,
        cursor: i32,
        anchor: i32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        input_type: i32,
    );
    fn feihong_native_ime_show();
    fn feihong_native_clipboard_set(text: *const c_char);
    fn feihong_native_clipboard_get(text: *mut c_char, capacity: usize) -> usize;
    fn feihong_native_log(message: *const c_char);
}

pub fn present(
    generation: u64,
    pixels: &[crate::framebuffer::OpaqueRgba],
    width: u32,
    height: u32,
    damage: &slint::platform::software_renderer::PhysicalRegion,
) -> bool {
    #[cfg(target_env = "ohos")]
    // OpaqueRgba is four packed RGBA bytes. Native copies before returning.
    unsafe {
        let origin = damage.bounding_box_origin();
        let size = damage.bounding_box_size();
        feihong_native_present_rgba_damage(
            crate::framebuffer::OpaqueRgba::as_bytes(pixels).as_ptr(),
            width,
            height,
            generation,
            origin.x,
            origin.y,
            size.width,
            size.height,
        ) == 0
    }
    #[cfg(not(target_env = "ohos"))]
    {
        let _ = damage;
        let _ = (generation, pixels, width, height);
        true
    }
}

pub fn set_performance_diagnostics(enabled: bool) {
    #[cfg(target_env = "ohos")]
    unsafe {
        feihong_native_set_performance_diagnostics(enabled);
    }
    #[cfg(not(target_env = "ohos"))]
    let _ = enabled;
}

pub fn set_ui_interactive(interactive: bool) {
    #[cfg(target_env = "ohos")]
    unsafe {
        // Scheduling API failure is logged by native code and must not prevent drawing.
        let _ = feihong_native_ui_qos(interactive);
    }
    #[cfg(not(target_env = "ohos"))]
    let _ = interactive;
}

pub fn request_frame() -> bool {
    #[cfg(target_env = "ohos")]
    unsafe {
        feihong_request_frame() == 0
    }
    #[cfg(not(target_env = "ohos"))]
    {
        false
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ime_update(
    enabled: bool,
    text: &str,
    cursor: i32,
    anchor: i32,
    rect: [f32; 4],
    input_type: i32,
) {
    #[cfg(test)]
    TEST_IME_ENABLED.store(enabled, std::sync::atomic::Ordering::SeqCst);
    #[cfg(target_env = "ohos")]
    {
        let text: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            feihong_native_ime_update(
                enabled,
                text.as_ptr(),
                text.len(),
                cursor,
                anchor,
                rect[0],
                rect[1],
                rect[2],
                rect[3],
                input_type,
            );
        }
    }
    #[cfg(not(target_env = "ohos"))]
    {
        let _ = (enabled, text, cursor, anchor, rect, input_type);
    }
}

pub fn ime_show() {
    #[cfg(test)]
    TEST_IME_SHOW_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    #[cfg(target_env = "ohos")]
    unsafe {
        feihong_native_ime_show();
    }
}

pub fn clipboard_set(text: &str) {
    #[cfg(target_env = "ohos")]
    if let Ok(text) = CString::new(text) {
        unsafe { feihong_native_clipboard_set(text.as_ptr()) }
    }
    #[cfg(not(target_env = "ohos"))]
    {
        let _ = text;
    }
}

pub fn clipboard_get() -> Option<String> {
    #[cfg(target_env = "ohos")]
    unsafe {
        let length = feihong_native_clipboard_get(std::ptr::null_mut(), 0);
        if length > 16 * 1024 * 1024 {
            return None;
        }
        let mut bytes = vec![0u8; length + 1];
        let actual = feihong_native_clipboard_get(bytes.as_mut_ptr().cast(), bytes.len());
        if actual > length {
            return None;
        }
        bytes.truncate(actual);
        String::from_utf8(bytes).ok()
    }
    #[cfg(not(target_env = "ohos"))]
    {
        None
    }
}

pub fn log(message: &str) {
    #[cfg(target_env = "ohos")]
    if let Ok(message) = CString::new(message) {
        unsafe { feihong_native_log(message.as_ptr()) }
    }
    #[cfg(not(target_env = "ohos"))]
    eprintln!("{message}");
}
