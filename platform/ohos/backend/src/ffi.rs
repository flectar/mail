//! Raw FFI surface for the OpenHarmony APIs this backend uses.
//!
//! Every structure and signature below mirrors the OHOS NDK headers shipped in
//! `$OHOS_SDK/native/sysroot/usr/include` (SDK 26.0.0.18):
//!
//! - `ace/xcomponent/native_interface_xcomponent.h` (XComponent surface + touch)
//! - `native_window/external_window.h` (`OHNativeWindow*` buffer queue)
//! - `native_window/buffer_handle.h` (`BufferHandle`)
//! - `native_buffer/buffer_common.h` (`OH_NativeBuffer_Format` values)
//! - NAPI module registration (napi/native_api.h)
//!
//! Everything is declared by hand so the backend has no C build step: the
//! symbols resolve at dynamic link time from the platform libraries.
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use core::ffi::{c_char, c_int, c_void};

// ---------------------------------------------------------------------------
// NAPI (only the module-registration path; no exported JS functions yet)
// ---------------------------------------------------------------------------

pub type napi_env = *mut c_void;
pub type napi_value = *mut c_void;
pub type napi_status = c_int;

pub const NAPI_OK: napi_status = 0;

/// Opaque argument bag of a native JS callback.
pub type napi_callback_info = *mut c_void;
/// Signature of a native function exposed to JS through `napi_create_function`.
pub type napi_callback =
    unsafe extern "C" fn(env: napi_env, info: napi_callback_info) -> napi_value;
/// Handle to a thread-safe function used to call JS from other threads.
pub type napi_threadsafe_function = *mut c_void;
pub type napi_finalize =
    unsafe extern "C" fn(env: napi_env, finalize_data: *mut c_void, finalize_hint: *mut c_void);
/// Callback invoked on the JS thread by `napi_call_threadsafe_function`.
pub type napi_threadsafe_function_call_js = unsafe extern "C" fn(
    env: napi_env,
    js_callback: napi_value,
    context: *mut c_void,
    data: *mut c_void,
);

#[repr(C)]
pub struct NapiModule {
    /// NAPI version; the C++ templates on this SDK use `1`.
    pub nm_version: c_int,
    pub nm_flags: u32,
    pub nm_filename: *const c_char,
    pub nm_register_func:
        Option<unsafe extern "C" fn(env: napi_env, exports: napi_value) -> napi_value>,
    pub nm_modname: *const c_char,
    pub nm_priv: *mut c_void,
    pub reserved: [*mut c_void; 4],
}

// NAPI lives in libace_napi.z.so on the OHOS platform.
#[link(name = "ace_napi.z", kind = "dylib")]
unsafe extern "C" {
    pub fn napi_get_named_property(
        env: napi_env,
        object: napi_value,
        utf8name: *const c_char,
        result: *mut napi_value,
    ) -> napi_status;
    pub fn napi_get_value_external(
        env: napi_env,
        value: napi_value,
        result: *mut *mut c_void,
    ) -> napi_status;
    /// The XComponent module object is an ArkTS object with a wrapped native
    /// pointer: the documented way to unpack it is `napi_unwrap` (see the
    /// OpenHarmony native-window/XComponent guidelines for API 12+), not
    /// `napi_get_value_external`.
    pub fn napi_unwrap(env: napi_env, object: napi_value, result: *mut *mut c_void) -> napi_status;
    pub fn napi_module_register(module: *mut NapiModule);
    /// Read the arguments of a native JS callback (used by `setBridge`).
    pub fn napi_get_cb_info(
        env: napi_env,
        cbinfo: napi_callback_info,
        argc: *mut usize,
        argv: *mut napi_value,
        this_arg: *mut napi_value,
        data: *mut *mut c_void,
    ) -> napi_status;
    /// Expose a native function to JS (`setBridge`).
    pub fn napi_create_function(
        env: napi_env,
        utf8name: *const c_char,
        length: usize,
        cb: napi_callback,
        data: *mut c_void,
        result: *mut napi_value,
    ) -> napi_status;
    pub fn napi_set_named_property(
        env: napi_env,
        object: napi_value,
        utf8name: *const c_char,
        value: napi_value,
    ) -> napi_status;
    pub fn napi_create_string_utf8(
        env: napi_env,
        string: *const c_char,
        length: usize,
        result: *mut napi_value,
    ) -> napi_status;
    pub fn napi_get_undefined(env: napi_env, result: *mut napi_value) -> napi_status;
    pub fn napi_create_int64(env: napi_env, value: i64, result: *mut napi_value) -> napi_status;
    pub fn napi_get_value_int64(env: napi_env, value: napi_value, result: *mut i64) -> napi_status;
    /// With `buf == NULL` and `bufsize == 0` the required length (excluding the
    /// NUL terminator) is written to `result`.
    pub fn napi_get_value_string_utf8(
        env: napi_env,
        value: napi_value,
        buf: *mut c_char,
        bufsize: usize,
        result: *mut usize,
    ) -> napi_status;
    /// Wrap a JS function so it can be invoked from any thread (the Slint
    /// thread opening the system browser, in this backend).
    pub fn napi_create_threadsafe_function(
        env: napi_env,
        func: napi_value,
        async_resource: napi_value,
        async_resource_name: napi_value,
        max_queue_size: usize,
        initial_thread_count: usize,
        thread_finalize_data: *mut c_void,
        thread_finalize_cb: Option<napi_finalize>,
        context: *mut c_void,
        call_js_cb: Option<napi_threadsafe_function_call_js>,
        result: *mut napi_threadsafe_function,
    ) -> napi_status;
    pub fn napi_call_threadsafe_function(
        func: napi_threadsafe_function,
        data: *mut c_void,
        is_blocking: c_int,
    ) -> napi_status;
    pub fn napi_release_threadsafe_function(
        func: napi_threadsafe_function,
        mode: c_int,
    ) -> napi_status;
    pub fn napi_call_function(
        env: napi_env,
        recv: napi_value,
        func: napi_value,
        argc: usize,
        argv: *const napi_value,
        result: *mut napi_value,
    ) -> napi_status;
}

/// Property name under which the XComponent hands the native instance to the
/// module `Init` (see `native_interface_xcomponent.h`).
pub const OH_NATIVE_XCOMPONENT_OBJ: &[u8] = b"__NATIVE_XCOMPONENT_OBJ__\0";

// ---------------------------------------------------------------------------
// Pasteboard (database/pasteboard/oh_pasteboard.h, database/udmf/{udmf,uds}.h)
// ---------------------------------------------------------------------------

/// Opaque system pasteboard instance.
#[repr(C)]
pub struct OH_Pasteboard {
    _private: [u8; 0],
}

/// Opaque UDMF data container (one pasteboard item).
#[repr(C)]
pub struct OH_UdmfData {
    _private: [u8; 0],
}

/// Opaque UDMF record (one representation inside an item).
#[repr(C)]
pub struct OH_UdmfRecord {
    _private: [u8; 0],
}

/// Opaque unified data structure holding plain text.
#[repr(C)]
pub struct OH_UdsPlainText {
    _private: [u8; 0],
}

// The pasteboard client builds on the UDMF data model; both live in the OHOS
// NDK (`libpasteboard.so`, `libudmf.so`).
#[link(name = "pasteboard", kind = "dylib")]
unsafe extern "C" {
    pub fn OH_Pasteboard_Create() -> *mut OH_Pasteboard;
    pub fn OH_Pasteboard_Destroy(pasteboard: *mut OH_Pasteboard);
    /// Reading requires `ohos.permission.READ_PASTEBOARD`; the returned data is
    /// owned by the caller and must be destroyed with `OH_UdmfData_Destroy`.
    pub fn OH_Pasteboard_GetData(
        pasteboard: *mut OH_Pasteboard,
        status: *mut c_int,
    ) -> *mut OH_UdmfData;
    pub fn OH_Pasteboard_SetData(pasteboard: *mut OH_Pasteboard, data: *mut OH_UdmfData) -> c_int;
}

#[link(name = "udmf", kind = "dylib")]
unsafe extern "C" {
    pub fn OH_UdmfData_Create() -> *mut OH_UdmfData;
    pub fn OH_UdmfData_Destroy(data: *mut OH_UdmfData);
    pub fn OH_UdmfData_AddRecord(data: *mut OH_UdmfData, record: *mut OH_UdmfRecord) -> c_int;
    /// Fills `plain_text` with the primary plain-text entry of `data`.
    pub fn OH_UdmfData_GetPrimaryPlainText(
        data: *mut OH_UdmfData,
        plain_text: *mut OH_UdsPlainText,
    ) -> c_int;
    pub fn OH_UdmfRecord_Create() -> *mut OH_UdmfRecord;
    pub fn OH_UdmfRecord_Destroy(record: *mut OH_UdmfRecord);
    /// The record copies the plain-text structure; the caller still owns and
    /// destroys its `OH_UdsPlainText` afterwards.
    pub fn OH_UdmfRecord_AddPlainText(
        record: *mut OH_UdmfRecord,
        plain_text: *mut OH_UdsPlainText,
    ) -> c_int;
    pub fn OH_UdsPlainText_Create() -> *mut OH_UdsPlainText;
    pub fn OH_UdsPlainText_Destroy(plain_text: *mut OH_UdsPlainText);
    pub fn OH_UdsPlainText_SetContent(
        plain_text: *mut OH_UdsPlainText,
        content: *const c_char,
    ) -> c_int;
    /// Returns a pointer into the structure; valid until it is destroyed.
    pub fn OH_UdsPlainText_GetContent(plain_text: *mut OH_UdsPlainText) -> *const c_char;
}

// ---------------------------------------------------------------------------
// XComponent
// ---------------------------------------------------------------------------

/// Opaque ArkUI native XComponent instance.
#[repr(C)]
pub struct OH_NativeXComponent {
    _private: [u8; 0],
}

/// Opaque `OHNativeWindow` (also referenced as `NativeWindow`).
pub type OHNativeWindow = c_void;

/// Opaque `OHNativeWindowBuffer`.
#[repr(C)]
pub struct OHNativeWindowBuffer {
    _private: [u8; 0],
}

pub const OH_NATIVEXCOMPONENT_DOWN: c_int = 0;
pub const OH_NATIVEXCOMPONENT_UP: c_int = 1;
pub const OH_NATIVEXCOMPONENT_MOVE: c_int = 2;
pub const OH_NATIVEXCOMPONENT_CANCEL: c_int = 3;
pub const OH_NATIVEXCOMPONENT_UNKNOWN: c_int = 4;

pub const OH_NATIVE_XCOMPONENT_MAX_TOUCH_POINTS_NUMBER: usize = 10;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OH_NativeXComponent_TouchPoint {
    pub id: i32,
    pub screenX: f32,
    pub screenY: f32,
    pub x: f32,
    pub y: f32,
    pub type_: c_int,
    pub size: f64,
    pub force: f32,
    pub timeStamp: i64,
    pub isPressed: bool,
}

#[repr(C)]
pub struct OH_NativeXComponent_TouchEvent {
    pub id: i32,
    pub screenX: f32,
    pub screenY: f32,
    pub x: f32,
    pub y: f32,
    pub type_: c_int,
    pub size: f64,
    pub force: f32,
    pub deviceId: i64,
    pub timeStamp: i64,
    pub touchPoints: [OH_NativeXComponent_TouchPoint; OH_NATIVE_XCOMPONENT_MAX_TOUCH_POINTS_NUMBER],
    pub numPoints: u32,
}

#[repr(C)]
pub struct OH_NativeXComponent_Callback {
    pub OnSurfaceCreated:
        Option<unsafe extern "C" fn(component: *mut OH_NativeXComponent, window: *mut c_void)>,
    pub OnSurfaceChanged:
        Option<unsafe extern "C" fn(component: *mut OH_NativeXComponent, window: *mut c_void)>,
    pub OnSurfaceDestroyed:
        Option<unsafe extern "C" fn(component: *mut OH_NativeXComponent, window: *mut c_void)>,
    pub DispatchTouchEvent:
        Option<unsafe extern "C" fn(component: *mut OH_NativeXComponent, window: *mut c_void)>,
}

/// Opaque key event from `OH_NativeXComponent_GetKeyEvent`.
#[repr(C)]
pub struct OH_NativeXComponent_KeyEvent {
    _private: [u8; 0],
}

/// Opaque UI input event from `OH_NativeXComponent_RegisterUIInputEventCallback`
/// (arkui/ui_input_event.h).
#[repr(C)]
pub struct ArkUI_UIInputEvent {
    _private: [u8; 0],
}

/// `ArkUI_UIInputEvent_Type`: the callback registration only supports axis
/// events on an XComponent.
pub const ARKUI_UIINPUTEVENT_TYPE_AXIS: i32 = 2;

/// `UI_AXIS_EVENT_ACTION_*`: the BEGIN and UPDATE phases carry an incremental
/// wheel/touchpad scroll value.
pub const UI_AXIS_EVENT_ACTION_BEGIN: i32 = 1;
pub const UI_AXIS_EVENT_ACTION_UPDATE: i32 = 2;

pub const OH_NATIVEXCOMPONENT_KEY_ACTION_DOWN: i32 = 0;
pub const OH_NATIVEXCOMPONENT_KEY_ACTION_UP: i32 = 1;

/// `OH_NativeXComponent_KeyCode` values we translate (native_xcomponent_key_event.h).
pub const KEY_0: i32 = 2000;
/// `KEY_9` is 2009; digits are offset by this base.
pub const KEY_DIGIT_BASE: i32 = KEY_0;
pub const KEY_A: i32 = 2017;
pub const KEY_Z: i32 = 2042;
pub const KEY_COMMA: i32 = 2043;
pub const KEY_PERIOD: i32 = 2044;
pub const KEY_ALT_LEFT: i32 = 2045;
pub const KEY_ALT_RIGHT: i32 = 2046;
pub const KEY_SHIFT_LEFT: i32 = 2047;
pub const KEY_SHIFT_RIGHT: i32 = 2048;
pub const KEY_TAB: i32 = 2049;
pub const KEY_SPACE: i32 = 2050;
pub const KEY_ENTER: i32 = 2054;
pub const KEY_DEL: i32 = 2055;
pub const KEY_GRAVE: i32 = 2056;
pub const KEY_MINUS: i32 = 2057;
pub const KEY_EQUALS: i32 = 2058;
pub const KEY_LEFT_BRACKET: i32 = 2059;
pub const KEY_RIGHT_BRACKET: i32 = 2060;
pub const KEY_BACKSLASH: i32 = 2061;
pub const KEY_SEMICOLON: i32 = 2062;
pub const KEY_APOSTROPHE: i32 = 2063;
pub const KEY_SLASH: i32 = 2064;
pub const KEY_AT: i32 = 2065;
pub const KEY_PLUS: i32 = 2066;
pub const KEY_PAGE_UP: i32 = 2068;
pub const KEY_PAGE_DOWN: i32 = 2069;
pub const KEY_ESCAPE: i32 = 2070;
pub const KEY_FORWARD_DEL: i32 = 2071;
pub const KEY_CTRL_LEFT: i32 = 2072;
pub const KEY_CTRL_RIGHT: i32 = 2073;
pub const KEY_CAPS_LOCK: i32 = 2074;
pub const KEY_META_LEFT: i32 = 2076;
pub const KEY_META_RIGHT: i32 = 2077;
pub const KEY_MOVE_HOME: i32 = 2081;
pub const KEY_MOVE_END: i32 = 2082;
pub const KEY_DPAD_UP: i32 = 2012;
pub const KEY_DPAD_DOWN: i32 = 2013;
pub const KEY_DPAD_LEFT: i32 = 2014;
pub const KEY_DPAD_RIGHT: i32 = 2015;

// The XComponent native interface lives in libace_ndk.z.so.
#[link(name = "ace_ndk.z", kind = "dylib")]
unsafe extern "C" {
    pub fn OH_NativeXComponent_GetXComponentId(
        component: *mut OH_NativeXComponent,
        id: *mut c_char,
        size: *mut u64,
    ) -> i32;
    pub fn OH_NativeXComponent_GetXComponentSize(
        component: *mut OH_NativeXComponent,
        window: *const c_void,
        width: *mut u64,
        height: *mut u64,
    ) -> i32;
    pub fn OH_NativeXComponent_GetTouchEvent(
        component: *mut OH_NativeXComponent,
        window: *const c_void,
        touchEvent: *mut OH_NativeXComponent_TouchEvent,
    ) -> i32;
    pub fn OH_NativeXComponent_RegisterCallback(
        component: *mut OH_NativeXComponent,
        callback: *mut OH_NativeXComponent_Callback,
    ) -> i32;
    pub fn OH_NativeXComponent_RegisterKeyEventCallback(
        component: *mut OH_NativeXComponent,
        callback: unsafe extern "C" fn(component: *mut OH_NativeXComponent, window: *mut c_void),
    ) -> i32;
    pub fn OH_NativeXComponent_RegisterUIInputEventCallback(
        component: *mut OH_NativeXComponent,
        callback: unsafe extern "C" fn(
            component: *mut OH_NativeXComponent,
            event: *mut ArkUI_UIInputEvent,
            type_: i32,
        ),
        type_: i32,
    ) -> i32;
    /// Axis (mouse wheel / touchpad) accessors for the event handed to the UI
    /// input callback. Wheel values are in degrees (a notch reports 15
    /// degrees times the user's scroll-step setting); touchpad values are in
    /// pixels. Positive values scroll toward the end of the content (the
    /// opposite of Slint's positive delta).
    pub fn OH_ArkUI_AxisEvent_GetAxisAction(event: *const ArkUI_UIInputEvent) -> i32;
    pub fn OH_ArkUI_AxisEvent_GetVerticalAxisValue(event: *const ArkUI_UIInputEvent) -> f64;
    pub fn OH_ArkUI_AxisEvent_GetHorizontalAxisValue(event: *const ArkUI_UIInputEvent) -> f64;
    /// Position of a directional input event (touch, mouse or axis) relative
    /// to the component, in physical pixels.
    pub fn OH_ArkUI_PointerEvent_GetX(event: *const ArkUI_UIInputEvent) -> f32;
    pub fn OH_ArkUI_PointerEvent_GetY(event: *const ArkUI_UIInputEvent) -> f32;
    pub fn OH_NativeXComponent_RegisterSurfaceShowCallback(
        component: *mut OH_NativeXComponent,
        callback: unsafe extern "C" fn(component: *mut OH_NativeXComponent, window: *mut c_void),
    ) -> i32;
    pub fn OH_NativeXComponent_RegisterSurfaceHideCallback(
        component: *mut OH_NativeXComponent,
        callback: unsafe extern "C" fn(component: *mut OH_NativeXComponent, window: *mut c_void),
    ) -> i32;
    pub fn OH_NativeXComponent_GetKeyEvent(
        component: *mut OH_NativeXComponent,
        keyEvent: *mut *mut OH_NativeXComponent_KeyEvent,
    ) -> i32;
    pub fn OH_NativeXComponent_GetKeyEventAction(
        keyEvent: *mut OH_NativeXComponent_KeyEvent,
        action: *mut i32,
    ) -> i32;
    pub fn OH_NativeXComponent_GetKeyEventCode(
        keyEvent: *mut OH_NativeXComponent_KeyEvent,
        code: *mut i32,
    ) -> i32;
}

// ---------------------------------------------------------------------------
// NativeWindow buffer queue
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[repr(C)]
pub struct Region {
    pub rects: *mut Rect,
    pub rectNumber: i32,
}

/// `NativeWindowOperation` enum ordinals we use (external_window.h).
pub const SET_BUFFER_GEOMETRY: c_int = 0;
pub const SET_FORMAT: c_int = 3;

/// `OH_NativeBuffer_Format` value for RGBA8888 (buffer_common.h).
pub const NATIVEBUFFER_PIXEL_FMT_RGBA_8888: c_int = 12;

/// `BufferHandle` from buffer_handle.h. `stride` is measured in BYTES.
#[repr(C)]
pub struct BufferHandle {
    pub fd: i32,
    pub width: i32,
    pub stride: i32,
    pub height: i32,
    pub size: i32,
    pub format: i32,
    pub usage: u64,
    pub virAddr: *mut c_void,
    pub key: i32,
    pub phyAddr: u64,
    pub reserveFds: u32,
    pub reserveInts: u32,
    // `int32_t reserve[0]` flexible array member follows; we never index it.
}

#[link(name = "native_window", kind = "dylib")]
unsafe extern "C" {
    pub fn OH_NativeWindow_NativeWindowRequestBuffer(
        window: *mut OHNativeWindow,
        buffer: *mut *mut OHNativeWindowBuffer,
        fenceFd: *mut c_int,
    ) -> i32;
    pub fn OH_NativeWindow_NativeWindowFlushBuffer(
        window: *mut OHNativeWindow,
        buffer: *mut OHNativeWindowBuffer,
        fenceFd: c_int,
        region: Region,
    ) -> i32;
    pub fn OH_NativeWindow_NativeWindowAbortBuffer(
        window: *mut OHNativeWindow,
        buffer: *mut OHNativeWindowBuffer,
    ) -> i32;
    pub fn OH_NativeWindow_NativeWindowHandleOpt(
        window: *mut OHNativeWindow,
        code: c_int,
        ...
    ) -> i32;
    pub fn OH_NativeWindow_GetBufferHandleFromNative(
        buffer: *mut OHNativeWindowBuffer,
    ) -> *mut BufferHandle;
}

// ---------------------------------------------------------------------------
// Display manager (density)
// ---------------------------------------------------------------------------

#[link(name = "native_display_manager", kind = "dylib")]
unsafe extern "C" {
    /// `window_manager/oh_display_manager.h`. Returns 0 on success.
    pub fn OH_NativeDisplayManager_GetDefaultDisplayDensityPixels(densityPixels: *mut f32) -> i32;
}

// ---------------------------------------------------------------------------
// hilog
// ---------------------------------------------------------------------------

#[link(name = "hilog_ndk.z", kind = "dylib")]
unsafe extern "C" {
    /// `hilog/log.h`: variadic printf-style logger backed by libhilog_ndk.z.so.
    pub fn OH_LOG_Print(
        logType: c_int,
        logLevel: c_int,
        domain: u32,
        tag: *const c_char,
        fmt: *const c_char,
        ...
    ) -> c_int;
}

// ---------------------------------------------------------------------------
// mmap / munmap from the OHOS musl libc
// ---------------------------------------------------------------------------

pub const PROT_READ: c_int = 0x1;
pub const PROT_WRITE: c_int = 0x2;
pub const MAP_SHARED: c_int = 0x01;

unsafe extern "C" {
    pub fn mmap(
        addr: *mut c_void,
        length: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    pub fn munmap(addr: *mut c_void, length: usize) -> c_int;
}

/// `mmap` failure return value.
pub const MAP_FAILED: *mut c_void = usize::MAX as *mut c_void;

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn touch_structs_match_the_c_layout() {
        // Values computed from native_interface_xcomponent.h for aarch64 musl:
        // TouchPoint: 4+4+4+4+4 (+4 pad) +8 +4 (+4 pad) +8 +1 (+7 pad) = 56.
        assert_eq!(size_of::<OH_NativeXComponent_TouchPoint>(), 56);
        assert_eq!(align_of::<OH_NativeXComponent_TouchPoint>(), 8);
        // TouchEvent: ... + 10 touchPoints + u32 numPoints (+4 pad) = 624.
        assert_eq!(size_of::<OH_NativeXComponent_TouchEvent>(), 624);
    }

    #[test]
    fn buffer_handle_matches_the_c_layout() {
        // 6 * i32 + u64 (8-aligned) + ptr + i32 (+4 pad) + u64 + 2 * u32 = 64.
        assert_eq!(size_of::<BufferHandle>(), 64);
    }
}
