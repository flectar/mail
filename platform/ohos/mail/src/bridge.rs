//! ArkTS bridge: native code calls closures installed from the UI page.
//!
//! The messenger window is an ArkTS XComponent page. Services that only exist
//! as ArkTS APIs (opening the system browser through a Want, the system file
//! picker) cannot be reached from native code directly. The page installs
//! closures with `setBridge({ ... })` on the module object handed to its
//! `onLoad` callback, and this module wraps each closure in a NAPI thread-safe
//! function so any thread can invoke it.
//!
//! Request/response calls (file pickers) additionally use
//! `submitBridgeResult(requestId, path, error)`, which the page calls back into
//! the module when the interactive flow finishes.

use core::ffi::c_void;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use slint_ohos_backend::ffi::{self, napi_env, napi_value};
use slint_ohos_backend::hold::PresentationHold;
use tokio::sync::oneshot;

/// Thread-safe handles to the ArkTS closures; `0` until installed.
static OPEN_URL: AtomicUsize = AtomicUsize::new(0);
static PICK_OPEN: AtomicUsize = AtomicUsize::new(0);
static PICK_OPEN_MANY: AtomicUsize = AtomicUsize::new(0);
static PICK_SAVE: AtomicUsize = AtomicUsize::new(0);
static SHOW_IME: AtomicUsize = AtomicUsize::new(0);
static HIDE_IME: AtomicUsize = AtomicUsize::new(0);
static IME_UPDATE_CURSOR: AtomicUsize = AtomicUsize::new(0);
static REQUEST_PASTEBOARD: AtomicUsize = AtomicUsize::new(0);

/// Request ids for the callback-based picker flows.
static NEXT_REQUEST: AtomicI64 = AtomicI64::new(1);

/// A picker's reported path and picked name, or why it failed.
type PickerReply = Result<(String, String), String>;

/// One open picker: where its result goes, and the hold that keeps the Slint
/// thread from presenting while the picker covers the window.
struct Pending {
    sender: oneshot::Sender<PickerReply>,
    _hold: PresentationHold,
}

fn pending() -> &'static Mutex<HashMap<i64, Pending>> {
    static PENDING: OnceLock<Mutex<HashMap<i64, Pending>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Removes the request entry, and with it the presentation hold, when the
/// awaiting future goes away (cancellation, timeout); a late page-side result
/// is then dropped.
struct PendingGuard(i64);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = pending().lock() {
            map.remove(&self.0);
        }
    }
}

/// `setBridge(bridge)` implementation, exported on the module object for the
/// ArkTS page (`XComponent` `onLoad` hands over the module's exports).
pub(super) unsafe extern "C" fn set_bridge(
    env: napi_env,
    info: ffi::napi_callback_info,
) -> napi_value {
    // SAFETY: the NAPI runtime provides a live `env`/`info` pair for this
    // callback, and `argv` has room for the single `bridge` argument.
    unsafe {
        let mut argc: usize = 1;
        let mut argv: [napi_value; 1] = [core::ptr::null_mut()];
        let rc = ffi::napi_get_cb_info(
            env,
            info,
            &mut argc,
            argv.as_mut_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        if rc != ffi::NAPI_OK || argc < 1 || argv[0].is_null() {
            slint_ohos_backend::ohos_error!("mail: setBridge: missing bridge object (rc={rc})");
            return undefined(env);
        }
        install_property(env, argv[0], b"openUrl\0", &OPEN_URL, call_open_url);
        install_property(env, argv[0], b"pickOpen\0", &PICK_OPEN, call_pick_open);
        install_property(
            env,
            argv[0],
            b"pickOpenMany\0",
            &PICK_OPEN_MANY,
            call_pick_open_many,
        );
        install_property(env, argv[0], b"pickSave\0", &PICK_SAVE, call_pick_save);
        install_property(env, argv[0], b"showIme\0", &SHOW_IME, call_bool_arg);
        install_property(env, argv[0], b"hideIme\0", &HIDE_IME, call_zero_arg);
        install_property(
            env,
            argv[0],
            b"imeUpdateCursor\0",
            &IME_UPDATE_CURSOR,
            call_cursor_update,
        );
        install_property(
            env,
            argv[0],
            b"requestPasteboard\0",
            &REQUEST_PASTEBOARD,
            call_zero_arg,
        );
        undefined(env)
    }
}

/// `submitBridgeResult(requestId, path, name, error)` implementation, exported
/// for the page's asynchronous flows.
pub(super) unsafe extern "C" fn submit_bridge_result(
    env: napi_env,
    info: ffi::napi_callback_info,
) -> napi_value {
    // SAFETY: live `env`/`info` from the NAPI runtime; `argv` holds the four
    // expected arguments.
    unsafe {
        let mut argc: usize = 4;
        let mut argv: [napi_value; 4] = [core::ptr::null_mut(); 4];
        let rc = ffi::napi_get_cb_info(
            env,
            info,
            &mut argc,
            argv.as_mut_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        if rc != ffi::NAPI_OK || argc < 1 {
            slint_ohos_backend::ohos_error!("mail: submitBridgeResult: bad arguments (rc={rc})");
            return boolean(env, false);
        }
        let id = read_int64(env, argv[0]).unwrap_or(0);
        let path = if argc > 1 {
            read_string(env, argv[1]).unwrap_or_default()
        } else {
            String::new()
        };
        let name = if argc > 2 {
            read_string(env, argv[2]).unwrap_or_default()
        } else {
            String::new()
        };
        let error = if argc > 3 {
            read_string(env, argv[3]).unwrap_or_default()
        } else {
            String::new()
        };
        let request = pending().lock().ok().and_then(|mut map| map.remove(&id));
        let accepted = match request {
            Some(Pending { sender, .. }) => {
                // Empty path + empty error means "the user cancelled".
                let outcome = if !path.is_empty() || error.is_empty() {
                    Ok((path, name))
                } else {
                    Err(error)
                };
                sender.send(outcome).is_ok()
            }
            None => {
                slint_ohos_backend::ohos_info!(
                    "mail: bridge result for an unknown request id (dropped)"
                );
                false
            }
        };
        // The page deletes a staged import when the request timed out or was
        // cancelled before the result arrived; otherwise it would leak a copy
        // in the cache directory.
        boolean(env, accepted)
    }
}

/// Create a JS boolean return value for a NAPI callback.
unsafe fn boolean(env: napi_env, value: bool) -> napi_value {
    let mut result: napi_value = core::ptr::null_mut();
    // SAFETY: NAPI call on the JS thread.
    unsafe { ffi::napi_get_boolean(env, value, &mut result) };
    result
}

/// Open `url` in the system browser. Callable from any thread.
pub(super) fn open_url(url: &str) -> Result<(), String> {
    let url = CString::new(url).map_err(|_| "authorization address is malformed".to_owned())?;
    call_closure(&OPEN_URL, "openUrl", url)
}

/// Registers a picker request. Its entry holds presentation until the page
/// reports the result or the request is dropped, whichever thread awaits it.
fn register_picker() -> Result<(i64, oneshot::Receiver<PickerReply>), String> {
    let id = NEXT_REQUEST.fetch_add(1, Ordering::SeqCst);
    let (sender, receiver) = oneshot::channel();
    let request = Pending {
        sender,
        _hold: PresentationHold::acquire(),
    };
    pending()
        .lock()
        .map_err(|_| "the picker registry is poisoned".to_owned())?
        .insert(id, request);
    Ok((id, receiver))
}

/// One picked document: the private staged path and its original display name.
pub(super) struct PickedFile {
    pub(super) path: String,
    pub(super) name: String,
}

/// Run the system file picker for one document. `Ok(None)` = user cancelled.
pub(super) async fn pick_open() -> Result<Option<PickedFile>, String> {
    let (id, receiver) = register_picker()?;
    let guard = PendingGuard(id);
    call_closure(&PICK_OPEN, "pickOpen", id)?;
    let (path, name) = await_result(receiver).await?;
    drop(guard);
    Ok((!path.is_empty()).then_some(PickedFile { path, name }))
}

/// Arguments of one multi-file picker request.
struct OpenManyRequest {
    id: i64,
    limit: i64,
}

/// Run the system file picker for several documents. The ArkTS page serializes
/// the staged files as `[{ "path": ..., "name": ... }]` in the normal bridge
/// result, keeping the presentation hold and timeout behavior unchanged.
pub(super) async fn pick_open_many(limit: usize) -> Result<Vec<PickedFile>, String> {
    let (id, receiver) = register_picker()?;
    let guard = PendingGuard(id);
    call_closure(
        &PICK_OPEN_MANY,
        "pickOpenMany",
        OpenManyRequest {
            id,
            limit: limit.clamp(1, 64) as i64,
        },
    )?;
    let (json, _name) = await_result(receiver).await?;
    drop(guard);
    if json.is_empty() {
        return Ok(Vec::new());
    }
    let values: Vec<serde_json::Value> = serde_json::from_str(&json)
        .map_err(|error| format!("the file picker returned invalid metadata: {error}"))?;
    let mut files = Vec::with_capacity(values.len());
    for value in values {
        let path = value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        let name = value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        files.push(PickedFile {
            path: path.to_owned(),
            name: name.to_owned(),
        });
    }
    Ok(files)
}

/// Ask the system file picker for a destination and copy `source_path` there.
/// `Ok(None)` = user cancelled.
pub(super) async fn pick_save(
    source_path: String,
    suggested_name: String,
) -> Result<Option<String>, String> {
    let (id, receiver) = register_picker()?;
    let guard = PendingGuard(id);
    let request = SaveRequest {
        id,
        source: source_path,
        name: suggested_name,
    };
    call_closure(&PICK_SAVE, "pickSave", request)?;
    let (path, _name) = await_result(receiver).await?;
    drop(guard);
    Ok((!path.is_empty()).then_some(path))
}

/// Arguments of one `imeUpdateCursor` call.
struct CursorUpdate {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

/// Arguments of one `pickSave` call, boxed into the thread-safe-function data.
struct SaveRequest {
    id: i64,
    source: String,
    name: String,
}

async fn await_result(receiver: oneshot::Receiver<PickerReply>) -> PickerReply {
    match tokio::time::timeout(Duration::from_secs(600), receiver).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("the file picker was closed".to_owned()),
        Err(_) => Err("the file picker timed out".to_owned()),
    }
}

/// Raise the system keyboard through the ArkTS hook. Callable from any thread.
pub(super) fn show_ime(password: bool) {
    let _ = call_closure(&SHOW_IME, "showIme", password);
}

/// Lower the system keyboard through the ArkTS hook.
pub(super) fn hide_ime() {
    let _ = call_closure(&HIDE_IME, "hideIme", ());
}

/// Forward the caret rectangle to the ArkTS input-method controller.
pub(super) fn update_cursor(left: f32, top: f32, width: f32, height: f32) {
    let _ = call_closure(
        &IME_UPDATE_CURSOR,
        "imeUpdateCursor",
        CursorUpdate {
            left,
            top,
            width,
            height,
        },
    );
}

/// Ask the page to request the pasteboard permission (called from the
/// clipboard fallback on a denied read).
pub(super) fn request_pasteboard() {
    let _ = call_closure(&REQUEST_PASTEBOARD, "requestPasteboard", ());
}

/// `setClipboardText(text)`: the ArkTS page read the system pasteboard on the
/// UI thread. Empty text invalidates the cache.
pub(super) unsafe extern "C" fn set_clipboard_text(
    env: napi_env,
    info: ffi::napi_callback_info,
) -> napi_value {
    // SAFETY: live `env`/`info` from the NAPI runtime.
    unsafe {
        let mut argc: usize = 1;
        let mut argv: [napi_value; 1] = [core::ptr::null_mut()];
        let rc = ffi::napi_get_cb_info(
            env,
            info,
            &mut argc,
            argv.as_mut_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        if rc == ffi::NAPI_OK && argc >= 1 {
            let text = read_string(env, argv[0]).unwrap_or_default();
            slint_ohos_backend::clipboard::set_cached_text((!text.is_empty()).then_some(text));
        }
        undefined(env)
    }
}

/// `imeCommit(text)`: committed text from the soft keyboard.
pub(super) unsafe extern "C" fn ime_commit(
    env: napi_env,
    info: ffi::napi_callback_info,
) -> napi_value {
    // SAFETY: live `env`/`info` from the NAPI runtime.
    unsafe {
        let mut argc: usize = 1;
        let mut argv: [napi_value; 1] = [core::ptr::null_mut()];
        let rc = ffi::napi_get_cb_info(
            env,
            info,
            &mut argc,
            argv.as_mut_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        if rc == ffi::NAPI_OK
            && argc >= 1
            && let Some(text) = read_string(env, argv[0])
        {
            slint_ohos_backend::ime::commit_text(&text);
        }
        undefined(env)
    }
}

/// `imeDelete(count)`: the soft keyboard deleted `count` characters.
pub(super) unsafe extern "C" fn ime_delete(
    env: napi_env,
    info: ffi::napi_callback_info,
) -> napi_value {
    // SAFETY: live `env`/`info` from the NAPI runtime.
    unsafe {
        let mut argc: usize = 1;
        let mut argv: [napi_value; 1] = [core::ptr::null_mut()];
        let rc = ffi::napi_get_cb_info(
            env,
            info,
            &mut argc,
            argv.as_mut_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        let count = if rc == ffi::NAPI_OK && argc >= 1 {
            // JS numbers arrive as doubles through the int64 reader's error
            // path; read as int64 when possible and fall back to one.
            read_int64(env, argv[0]).unwrap_or(1).clamp(1, 64) as usize
        } else {
            1
        };
        slint_ohos_backend::ime::delete_left(count);
        undefined(env)
    }
}

/// `setColorScheme(dark)`: report the system light/dark preference.
pub(super) unsafe extern "C" fn set_color_scheme(
    env: napi_env,
    info: ffi::napi_callback_info,
) -> napi_value {
    // SAFETY: live NAPI env/info; argv holds one boolean.
    unsafe {
        let mut argc: usize = 1;
        let mut argv: [napi_value; 1] = [core::ptr::null_mut()];
        if ffi::napi_get_cb_info(
            env,
            info,
            &mut argc,
            argv.as_mut_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        ) == ffi::NAPI_OK
            && argc >= 1
        {
            let mut dark = false;
            if ffi::napi_get_value_bool(env, argv[0], &mut dark) == ffi::NAPI_OK {
                slint_ohos_backend::set_color_scheme(dark);
            }
        }
        undefined(env)
    }
}

/// `suspendPreview()`: the app is leaving the foreground; release any preview
/// resource the core keeps alive.
pub(super) unsafe extern "C" fn suspend_preview(
    env: napi_env,
    _info: ffi::napi_callback_info,
) -> napi_value {
    flectar_mail::suspend_file_preview();
    undefined(env)
}

/// `imeFunction(key)`: the action key of the soft keyboard was pressed.
pub(super) unsafe extern "C" fn ime_function(
    env: napi_env,
    info: ffi::napi_callback_info,
) -> napi_value {
    // SAFETY: live `env`/`info` from the NAPI runtime.
    unsafe {
        let mut argc: usize = 1;
        let mut argv: [napi_value; 1] = [core::ptr::null_mut()];
        let rc = ffi::napi_get_cb_info(
            env,
            info,
            &mut argc,
            argv.as_mut_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        if rc == ffi::NAPI_OK && argc >= 1 {
            // `sendFunctionKey` fires only for the keyboard's action key. The
            // reported EnterKeyType varies by keyboard and field, so any of
            // them means "activate": send Return.
            slint_ohos_backend::ime::function_key_enter();
        }
        undefined(env)
    }
}

/// Queue one thread-safe call to an ArkTS closure, boxing `payload` for the
/// trampoline (which owns and drops it).
fn call_closure<T>(slot: &AtomicUsize, name: &str, payload: T) -> Result<(), String> {
    let handle = slot.load(Ordering::SeqCst);
    let data = Box::into_raw(Box::new(payload)).cast::<c_void>();
    if handle == 0 {
        // The trampoline will never run; reclaim the payload here.
        unsafe { drop(Box::from_raw(data.cast::<T>())) };
        return Err(format!("the OHOS window bridge has no {name} closure"));
    }
    // Non-blocking (0): queue the call and return; the trampoline owns `data`.
    let rc = unsafe {
        ffi::napi_call_threadsafe_function(handle as ffi::napi_threadsafe_function, data, 0)
    };
    if rc != ffi::NAPI_OK {
        // The trampoline will never see the data; reclaim it here.
        unsafe { drop(Box::from_raw(data.cast::<T>())) };
        return Err(format!("the OHOS window bridge rejected {name} ({rc})"));
    }
    Ok(())
}

fn undefined(env: napi_env) -> napi_value {
    let mut value: napi_value = core::ptr::null_mut();
    // SAFETY: plain NAPI call on the JS thread.
    unsafe { ffi::napi_get_undefined(env, &mut value) };
    value
}

/// Wrap one property of the bridge object in a thread-safe function and store
/// its handle in `slot`.
unsafe fn install_property(
    env: napi_env,
    bridge: napi_value,
    name: &'static [u8],
    slot: &'static AtomicUsize,
    trampoline: ffi::napi_threadsafe_function_call_js,
) {
    let label = std::str::from_utf8(&name[..name.len() - 1]).unwrap_or("bridge");
    // SAFETY: NAPI calls run on the JS thread; every handle is borrowed for the
    // duration of the call.
    let handle = unsafe {
        let mut closure: napi_value = core::ptr::null_mut();
        let rc = ffi::napi_get_named_property(env, bridge, name.as_ptr().cast(), &mut closure);
        if rc != ffi::NAPI_OK || closure.is_null() {
            slint_ohos_backend::ohos_info!(
                "mail: setBridge: no {label} closure (rc={rc}); that service stays disabled"
            );
            return;
        }
        let mut resource: napi_value = core::ptr::null_mut();
        ffi::napi_create_string_utf8(env, name.as_ptr().cast(), name.len() - 1, &mut resource);
        let mut handle: ffi::napi_threadsafe_function = core::ptr::null_mut();
        let rc = ffi::napi_create_threadsafe_function(
            env,
            closure,
            core::ptr::null_mut(),
            resource,
            0,
            1,
            core::ptr::null_mut(),
            None,
            core::ptr::null_mut(),
            Some(trampoline),
            &mut handle,
        );
        if rc != ffi::NAPI_OK || handle.is_null() {
            slint_ohos_backend::ohos_error!("mail: setBridge: {label} is not callable (rc={rc})");
            return;
        }
        handle
    };
    let previous = slot.swap(handle as usize, Ordering::SeqCst);
    if previous != 0 {
        // A page reload installed a fresh closure; drop this handle's reference
        // (the initial JS thread keeps the previous function alive).
        unsafe {
            ffi::napi_release_threadsafe_function(previous as ffi::napi_threadsafe_function, 0);
        }
    }
    slint_ohos_backend::ohos_info!("mail: setBridge: {label} installed");
}

/// JS-thread half of [`open_url`].
unsafe extern "C" fn call_open_url(
    env: napi_env,
    js_callback: napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: `data` is the `CString` boxed in `open_url`; this callback owns it.
    let url = unsafe { Box::from_raw(data.cast::<CString>()) };
    // SAFETY: NAPI calls are made on the JS thread with live handles.
    unsafe {
        let Some(argument) = string_value(env, url.as_bytes()) else {
            return;
        };
        call_with_args(env, js_callback, &[argument], "openUrl");
    }
}

/// JS-thread half of [`pick_open`].
unsafe extern "C" fn call_pick_open(
    env: napi_env,
    js_callback: napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: `data` is the `i64` boxed in `pick_open`; this callback owns it.
    let id = unsafe { Box::from_raw(data.cast::<i64>()) };
    // SAFETY: NAPI calls are made on the JS thread with live handles.
    unsafe {
        let Some(argument) = int_value(env, *id) else {
            return;
        };
        call_with_args(env, js_callback, &[argument], "pickOpen");
    }
}

/// JS-thread half of [`pick_open_many`] (request id and selection limit).
unsafe extern "C" fn call_pick_open_many(
    env: napi_env,
    js_callback: napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: `data` is the `OpenManyRequest` boxed in `pick_open_many`; this
    // callback owns it.
    let request = unsafe { Box::from_raw(data.cast::<OpenManyRequest>()) };
    // SAFETY: NAPI calls are made on the JS thread with live handles.
    unsafe {
        let (Some(id), Some(limit)) = (int_value(env, request.id), int_value(env, request.limit))
        else {
            return;
        };
        call_with_args(env, js_callback, &[id, limit], "pickOpenMany");
    }
}

/// JS-thread half of [`pick_save`].
unsafe extern "C" fn call_pick_save(
    env: napi_env,
    js_callback: napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: `data` is the `SaveRequest` boxed in `pick_save`; this callback
    // owns it.
    let request = unsafe { Box::from_raw(data.cast::<SaveRequest>()) };
    // SAFETY: NAPI calls are made on the JS thread with live handles, and the
    // argument strings outlive the call.
    unsafe {
        let (Some(id), Some(source), Some(name)) = (
            int_value(env, request.id),
            string_value(env, request.source.as_bytes()),
            string_value(env, request.name.as_bytes()),
        ) else {
            return;
        };
        call_with_args(env, js_callback, &[id, source, name], "pickSave");
    }
}

/// JS-thread half of [`show_ime`] (one boolean: password field).
unsafe extern "C" fn call_bool_arg(
    env: napi_env,
    js_callback: napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: `data` is the `bool` payload boxed by `call_closure`; this
    // callback owns it.
    let password = unsafe { Box::from_raw(data.cast::<bool>()) };
    let mut value: napi_value = core::ptr::null_mut();
    // SAFETY: NAPI call on the JS thread with a live boolean payload.
    let rc = unsafe { ffi::napi_get_boolean(env, *password, &mut value) };
    if rc == ffi::NAPI_OK {
        // SAFETY: NAPI call on the JS thread with live handles.
        unsafe { call_with_args(env, js_callback, &[value], "showIme") };
    }
}

/// JS-thread half of [`update_cursor`] (four doubles).
unsafe extern "C" fn call_cursor_update(
    env: napi_env,
    js_callback: napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: `data` is the `CursorUpdate` payload boxed by `call_closure`;
    // this callback owns it.
    let update = unsafe { Box::from_raw(data.cast::<CursorUpdate>()) };
    let mut args: [napi_value; 4] = [core::ptr::null_mut(); 4];
    let values = [
        update.left as f64,
        update.top as f64,
        update.width as f64,
        update.height as f64,
    ];
    for (index, value) in values.into_iter().enumerate() {
        // SAFETY: NAPI call on the JS thread; `args` outlives the call.
        let rc = unsafe { ffi::napi_create_double(env, value, &mut args[index]) };
        if rc != ffi::NAPI_OK {
            return;
        }
    }
    // SAFETY: NAPI call on the JS thread with live handles.
    unsafe { call_with_args(env, js_callback, &args, "imeUpdateCursor") };
}

/// JS-thread half of [`hide_ime`] and [`request_pasteboard`] (zero arguments).
unsafe extern "C" fn call_zero_arg(
    env: napi_env,
    js_callback: napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: `data` is the `()` payload boxed by `call_closure`; this callback
    // owns it.
    let _payload = unsafe { Box::from_raw(data.cast::<()>()) };
    // SAFETY: NAPI call on the JS thread.
    unsafe { call_with_args(env, js_callback, &[], "notice") };
}

unsafe fn string_value(env: napi_env, bytes: &[u8]) -> Option<napi_value> {
    let mut value: napi_value = core::ptr::null_mut();
    // SAFETY: NAPI call with a byte slice that stays alive for the call.
    let rc = unsafe {
        ffi::napi_create_string_utf8(env, bytes.as_ptr().cast(), bytes.len(), &mut value)
    };
    (rc == ffi::NAPI_OK).then_some(value)
}

unsafe fn int_value(env: napi_env, number: i64) -> Option<napi_value> {
    let mut value: napi_value = core::ptr::null_mut();
    // SAFETY: plain NAPI call.
    let rc = unsafe { ffi::napi_create_int64(env, number, &mut value) };
    (rc == ffi::NAPI_OK).then_some(value)
}

unsafe fn call_with_args(env: napi_env, js_callback: napi_value, args: &[napi_value], label: &str) {
    if js_callback.is_null() {
        return;
    }
    // SAFETY: NAPI calls on the JS thread; `args` outlives the call.
    unsafe {
        let mut recv: napi_value = core::ptr::null_mut();
        ffi::napi_get_undefined(env, &mut recv);
        let mut result: napi_value = core::ptr::null_mut();
        let rc = ffi::napi_call_function(
            env,
            recv,
            js_callback,
            args.len(),
            args.as_ptr(),
            &mut result,
        );
        if rc != ffi::NAPI_OK {
            slint_ohos_backend::ohos_error!("mail: {label} call failed (rc={rc})");
        }
    }
}

unsafe fn read_int64(env: napi_env, value: napi_value) -> Option<i64> {
    let mut number: i64 = 0;
    // SAFETY: plain NAPI call.
    let rc = unsafe { ffi::napi_get_value_int64(env, value, &mut number) };
    (rc == ffi::NAPI_OK).then_some(number)
}

unsafe fn read_string(env: napi_env, value: napi_value) -> Option<String> {
    // SAFETY: NAPI two-step string read; the buffer matches the reported size.
    unsafe {
        let mut length: usize = 0;
        if ffi::napi_get_value_string_utf8(env, value, core::ptr::null_mut(), 0, &mut length)
            != ffi::NAPI_OK
        {
            return None;
        }
        let mut buffer = vec![0u8; length + 1];
        if ffi::napi_get_value_string_utf8(
            env,
            value,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut length,
        ) != ffi::NAPI_OK
        {
            return None;
        }
        buffer.truncate(length);
        String::from_utf8(buffer).ok()
    }
}
