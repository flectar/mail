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
use tokio::sync::oneshot;

/// Thread-safe handles to the ArkTS closures; `0` until installed.
static OPEN_URL: AtomicUsize = AtomicUsize::new(0);
static PICK_OPEN: AtomicUsize = AtomicUsize::new(0);
static PICK_SAVE: AtomicUsize = AtomicUsize::new(0);
static SHOW_IME: AtomicUsize = AtomicUsize::new(0);
static HIDE_IME: AtomicUsize = AtomicUsize::new(0);
static NOTIFY: AtomicUsize = AtomicUsize::new(0);
static REQUEST_PASTEBOARD: AtomicUsize = AtomicUsize::new(0);
static OPEN_PRIVACY: AtomicUsize = AtomicUsize::new(0);
static WITHDRAW_PRIVACY: AtomicUsize = AtomicUsize::new(0);

/// Request ids for the callback-based picker flows.
static NEXT_REQUEST: AtomicI64 = AtomicI64::new(1);

type PendingSender = oneshot::Sender<Result<String, String>>;

fn pending() -> &'static Mutex<HashMap<i64, PendingSender>> {
    static PENDING: OnceLock<Mutex<HashMap<i64, PendingSender>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Removes the request entry when the awaiting future goes away (cancellation,
/// timeout); a late page-side result is then dropped.
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
        install_property(env, argv[0], b"pickSave\0", &PICK_SAVE, call_pick_save);
        install_property(env, argv[0], b"showIme\0", &SHOW_IME, call_zero_arg);
        install_property(env, argv[0], b"hideIme\0", &HIDE_IME, call_zero_arg);
        install_property(env, argv[0], b"notify\0", &NOTIFY, call_notify);
        install_property(
            env,
            argv[0],
            b"requestPasteboard\0",
            &REQUEST_PASTEBOARD,
            call_zero_arg,
        );
        install_property(
            env,
            argv[0],
            b"openPrivacyPolicy\0",
            &OPEN_PRIVACY,
            call_privacy,
        );
        install_property(
            env,
            argv[0],
            b"withdrawPrivacyConsent\0",
            &WITHDRAW_PRIVACY,
            call_zero_arg,
        );
        undefined(env)
    }
}

/// `submitBridgeResult(requestId, path, error)` implementation, exported for
/// the page's asynchronous flows.
pub(super) unsafe extern "C" fn submit_bridge_result(
    env: napi_env,
    info: ffi::napi_callback_info,
) -> napi_value {
    // SAFETY: live `env`/`info` from the NAPI runtime; `argv` holds the three
    // expected arguments.
    unsafe {
        let mut argc: usize = 3;
        let mut argv: [napi_value; 3] = [core::ptr::null_mut(); 3];
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
            return undefined(env);
        }
        let id = read_int64(env, argv[0]).unwrap_or(0);
        let path = if argc > 1 {
            read_string(env, argv[1]).unwrap_or_default()
        } else {
            String::new()
        };
        let error = if argc > 2 {
            read_string(env, argv[2]).unwrap_or_default()
        } else {
            String::new()
        };
        let sender = pending().lock().ok().and_then(|mut map| map.remove(&id));
        match sender {
            Some(sender) => {
                // Empty path + empty error means "the user cancelled".
                let outcome = if !path.is_empty() || error.is_empty() {
                    Ok(path)
                } else {
                    Err(error)
                };
                let _ = sender.send(outcome);
            }
            None => slint_ohos_backend::ohos_info!(
                "mail: bridge result for an unknown request id (dropped)"
            ),
        }
        undefined(env)
    }
}

/// Open `url` in the system browser. Callable from any thread.
pub(super) fn open_url(url: &str) -> Result<(), String> {
    // The privacy policy ships as a local page until the website edition
    // covers the application-specific disclosures; keep the URL in the shared
    // UI so other hosts open the published policy. The shared UI appends the
    // configured language as `?lang=`.
    const PRIVACY_PREFIX: &str = "https://flectar.com/legal/privacy/";
    if url.starts_with(PRIVACY_PREFIX) {
        if url.contains("consent=withdraw") {
            return call_closure(&WITHDRAW_PRIVACY, "withdrawPrivacyConsent", ());
        }
        let language = url
            .split_once("?lang=")
            .map(|(_, rest)| rest.split('&').next().unwrap_or(""))
            .filter(|value| !value.is_empty())
            .unwrap_or("system")
            .to_owned();
        return call_closure(&OPEN_PRIVACY, "openPrivacyPolicy", language);
    }
    let url = CString::new(url).map_err(|_| "authorization address is malformed".to_owned())?;
    call_closure(&OPEN_URL, "openUrl", url)
}

/// Run the system file picker for one document. `Ok(None)` = user cancelled.
pub(super) async fn pick_open() -> Result<Option<String>, String> {
    let id = NEXT_REQUEST.fetch_add(1, Ordering::SeqCst);
    let (sender, receiver) = oneshot::channel();
    pending()
        .lock()
        .map_err(|_| "the picker registry is poisoned".to_owned())?
        .insert(id, sender);
    let guard = PendingGuard(id);
    call_closure(&PICK_OPEN, "pickOpen", id)?;
    let path = await_result(receiver).await?;
    drop(guard);
    Ok((!path.is_empty()).then_some(path))
}

/// Ask the system file picker for a destination and copy `source_path` there.
/// `Ok(None)` = user cancelled.
pub(super) async fn pick_save(
    source_path: String,
    suggested_name: String,
) -> Result<Option<String>, String> {
    let id = NEXT_REQUEST.fetch_add(1, Ordering::SeqCst);
    let (sender, receiver) = oneshot::channel();
    pending()
        .lock()
        .map_err(|_| "the picker registry is poisoned".to_owned())?
        .insert(id, sender);
    let guard = PendingGuard(id);
    let request = SaveRequest {
        id,
        source: source_path,
        name: suggested_name,
    };
    call_closure(&PICK_SAVE, "pickSave", request)?;
    let path = await_result(receiver).await?;
    drop(guard);
    Ok((!path.is_empty()).then_some(path))
}

/// Arguments of one `pickSave` call, boxed into the thread-safe-function data.
struct SaveRequest {
    id: i64,
    source: String,
    name: String,
}

async fn await_result(
    receiver: oneshot::Receiver<Result<String, String>>,
) -> Result<String, String> {
    match tokio::time::timeout(Duration::from_secs(600), receiver).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("the file picker was closed".to_owned()),
        Err(_) => Err("the file picker timed out".to_owned()),
    }
}

/// Raise the system keyboard through the ArkTS hook. Callable from any thread.
pub(super) fn show_ime() {
    let _ = call_closure(&SHOW_IME, "showIme", ());
}

/// Lower the system keyboard through the ArkTS hook.
pub(super) fn hide_ime() {
    let _ = call_closure(&HIDE_IME, "hideIme", ());
}

/// Ask the page to request the pasteboard permission (called from the
/// clipboard fallback on a denied read).
pub(super) fn request_pasteboard() {
    let _ = call_closure(&REQUEST_PASTEBOARD, "requestPasteboard", ());
}

/// Publish a system notification through the ArkTS hook.
pub(super) fn notify(title: &str, body: &str) {
    let request = NotifyRequest {
        title: title.to_owned(),
        body: body.to_owned(),
    };
    let _ = call_closure(&NOTIFY, "notify", request);
}

/// Arguments of one `notify` call.
struct NotifyRequest {
    title: String,
    body: String,
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
            // FunctionKey.ENTER is 1 in the OHOS input-method enums.
            if read_int64(env, argv[0]).unwrap_or(0) == 1 {
                slint_ohos_backend::ime::function_key_enter();
            }
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

/// JS-thread half of [`show_ime`] and [`hide_ime`] (zero arguments).
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

/// JS-thread half of [`open_privacy_policy`] (one language argument).
unsafe extern "C" fn call_privacy(
    env: napi_env,
    js_callback: napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: `data` is the `String` boxed by `call_closure`; this callback
    // owns it.
    let language = unsafe { Box::from_raw(data.cast::<String>()) };
    // SAFETY: NAPI calls on the JS thread with live handles; the string bytes
    // outlive the call.
    unsafe {
        let Some(arg) = string_value(env, language.as_bytes()) else {
            return;
        };
        call_with_args(env, js_callback, &[arg], "openPrivacyPolicy");
    }
}

/// JS-thread half of [`notify`].
unsafe extern "C" fn call_notify(
    env: napi_env,
    js_callback: napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: `data` is the `NotifyRequest` boxed in `notify`; this callback
    // owns it.
    let request = unsafe { Box::from_raw(data.cast::<NotifyRequest>()) };
    // SAFETY: NAPI calls are made on the JS thread with live handles, and the
    // argument strings outlive the call.
    unsafe {
        let (Some(title), Some(body)) = (
            string_value(env, request.title.as_bytes()),
            string_value(env, request.body.as_bytes()),
        ) else {
            return;
        };
        call_with_args(env, js_callback, &[title, body], "notify");
    }
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
