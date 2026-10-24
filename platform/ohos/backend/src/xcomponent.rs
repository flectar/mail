//! XComponent callbacks: surface lifecycle, touch and key input.
//!
//! The callback table is registered from the NAPI module `Init` (see the demo
//! cdylib) and every callback runs on the OHOS main thread. They never touch
//! Slint; they only convert the native input into [`BackendEvent`]s and hand
//! them to the Slint thread through the channel installed by
//! [`crate::start`].

use core::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::event::{BackendEvent, SendPtr, TouchPhase};
use crate::ffi;
use crate::present::SurfaceGate;

static EVENTS: OnceLock<Mutex<Sender<BackendEvent>>> = OnceLock::new();
static GATE: OnceLock<Arc<SurfaceGate>> = OnceLock::new();
/// Whether the window is currently visible. Presenting into a hidden window
/// can block on the compositor's buffer queue, so frames are skipped until
/// the surface is shown again.
static VISIBLE: OnceLock<Arc<AtomicBool>> = OnceLock::new();

/// Touch diagnostics: log the first few events so input delivery is visible in
/// hilog without flooding it.
static TOUCH_LOGGED: AtomicU32 = AtomicU32::new(0);
const TOUCH_LOG_LIMIT: u32 = 40;

/// Wheel diagnostics: log the first few scroll events with the raw platform
/// value, so the unit and sign can be checked against a device.
static SCROLL_LOGGED: AtomicU32 = AtomicU32::new(0);
const SCROLL_LOG_LIMIT: u32 = 10;

/// A mouse wheel notch reports 15 degrees per scroll step. Slint scrolls 50
/// logical pixels per wheel line (the same step the desktop backend uses), so
/// a notch moves about three text lines.
const LOGICAL_PX_PER_WHEEL_DEGREE: f32 = 50.0 / 15.0;

/// Modifier state used for character translation: OHOS key events carry only
/// key codes, not text.
static KEY_SHIFT: AtomicBool = AtomicBool::new(false);
static KEY_CAPS_LOCK: AtomicBool = AtomicBool::new(false);

/// Key codes currently held down, for auto-repeat detection.
static KEYS_DOWN: OnceLock<Mutex<Vec<i32>>> = OnceLock::new();

fn keys_down() -> &'static Mutex<Vec<i32>> {
    KEYS_DOWN.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record a key-down. Returns `true` when the key was already held, i.e. the
/// event is an auto-repeat.
fn mark_key_down(code: i32) -> bool {
    match keys_down().lock() {
        Ok(mut keys) => {
            if keys.contains(&code) {
                true
            } else {
                keys.push(code);
                false
            }
        }
        Err(_) => false,
    }
}

fn mark_key_up(code: i32) {
    if let Ok(mut keys) = keys_down().lock() {
        keys.retain(|held| *held != code);
    }
}

/// How long `OnSurfaceDestroyed` may block the OHOS main thread waiting for the
/// Slint thread to acknowledge the teardown (one frame is ~16 ms).
const DESTROY_ACK_TIMEOUT: Duration = Duration::from_millis(500);

/// Install the channel and gate used by the callbacks. Must run on the OHOS
/// main thread before the first surface callback can fire.
pub fn install(tx: Sender<BackendEvent>, gate: Arc<SurfaceGate>, visible: Arc<AtomicBool>) {
    let _ = EVENTS.set(Mutex::new(tx));
    let _ = GATE.set(gate);
    let _ = VISIBLE.set(visible);
}

pub(crate) fn send(event: BackendEvent) {
    if let Some(tx) = EVENTS.get()
        && let Ok(tx) = tx.lock()
    {
        let _ = tx.send(event);
    }
}

/// Register the surface and touch callbacks on one XComponent instance.
/// Returns the OHOS status code (`0` = success).
///
/// # Safety
///
/// `component` must be the live `OH_NativeXComponent` provided by the
/// XComponent loader on the main thread (the NAPI
/// `__NATIVE_XCOMPONENT_OBJ__` external). The callback table is registered
/// for the process lifetime.
pub unsafe fn register(component: *mut ffi::OH_NativeXComponent) -> i32 {
    // The native side keeps the pointer, so the table must live for the process
    // lifetime. Registration happens once per process.
    let callbacks: &'static mut ffi::OH_NativeXComponent_Callback =
        Box::leak(Box::new(ffi::OH_NativeXComponent_Callback {
            OnSurfaceCreated: Some(on_surface_created),
            OnSurfaceChanged: Some(on_surface_changed),
            OnSurfaceDestroyed: Some(on_surface_destroyed),
            DispatchTouchEvent: Some(dispatch_touch_event),
        }));
    // SAFETY: `component` is a live instance provided by the XComponent loader
    // on the main thread; the callback table is `'static`.
    let rc = unsafe { ffi::OH_NativeXComponent_RegisterCallback(component, callbacks) };
    crate::log::info(&format!("register XComponent callbacks: rc={rc}"));
    // SAFETY: same live component; the callback has the required C signature
    // and only reads the key event handed to it by the loader.
    let rc_key =
        unsafe { ffi::OH_NativeXComponent_RegisterKeyEventCallback(component, on_key_event) };
    crate::log::info(&format!("register XComponent key callback: rc={rc_key}"));
    // SAFETY: same live component; the callbacks have the required C signature.
    let rc_show =
        unsafe { ffi::OH_NativeXComponent_RegisterSurfaceShowCallback(component, on_surface_show) };
    let rc_hide =
        unsafe { ffi::OH_NativeXComponent_RegisterSurfaceHideCallback(component, on_surface_hide) };
    crate::log::info(&format!(
        "register XComponent surface show/hide callbacks: {rc_show}/{rc_hide}"
    ));
    // Wheel and touchpad scrolling: the XComponent UI input callback is the
    // only native path for axis events (touch reaches DispatchTouchEvent, but
    // axis events do not).
    // SAFETY: same live component; the callback has the required C signature
    // and only reads the axis event handed to it by the loader.
    let rc_axis = unsafe {
        ffi::OH_NativeXComponent_RegisterUIInputEventCallback(
            component,
            on_axis_event,
            ffi::ARKUI_UIINPUTEVENT_TYPE_AXIS,
        )
    };
    crate::log::info(&format!("register XComponent axis callback: rc={rc_axis}"));
    rc
}

unsafe extern "C" fn on_surface_created(
    component: *mut ffi::OH_NativeXComponent,
    window: *mut c_void,
) {
    surface_event(component, window);
}

unsafe extern "C" fn on_surface_changed(
    component: *mut ffi::OH_NativeXComponent,
    window: *mut c_void,
) {
    surface_event(component, window);
}

unsafe extern "C" fn on_surface_show(
    _component: *mut ffi::OH_NativeXComponent,
    _window: *mut c_void,
) {
    if let Some(visible) = VISIBLE.get() {
        visible.store(true, Ordering::Relaxed);
    }
    if let Some(gate) = GATE.get() {
        gate.surface_up();
    }
    send(BackendEvent::SurfaceShown);
}

unsafe extern "C" fn on_surface_hide(
    _component: *mut ffi::OH_NativeXComponent,
    _window: *mut c_void,
) {
    // Only stop *submitting* frames: a hidden window can stop the compositor
    // from consuming buffers, and a blocking buffer request would freeze the
    // render thread. The surface gate is left alone here, so a missed show
    // callback cannot leave the window permanently undrawn.
    if let Some(visible) = VISIBLE.get() {
        visible.store(false, Ordering::Relaxed);
    }
    send(BackendEvent::SurfaceHidden);
}

fn surface_event(component: *mut ffi::OH_NativeXComponent, window: *mut c_void) {
    if window.is_null() {
        return;
    }
    if let Some(visible) = VISIBLE.get() {
        visible.store(true, Ordering::Relaxed);
    }
    let mut width: u64 = 0;
    let mut height: u64 = 0;
    // SAFETY: both pointers are provided by the XComponent callback contract.
    let rc = unsafe {
        ffi::OH_NativeXComponent_GetXComponentSize(component, window, &mut width, &mut height)
    };
    if rc != 0 || width == 0 || height == 0 {
        return;
    }
    crate::log::info(&format!("surface {width}x{height}"));
    send(BackendEvent::Surface {
        window: SendPtr(window),
        width: width.min(u32::MAX as u64) as u32,
        height: height.min(u32::MAX as u64) as u32,
    });
}

unsafe extern "C" fn on_surface_destroyed(
    _component: *mut ffi::OH_NativeXComponent,
    _window: *mut c_void,
) {
    let Some(gate) = GATE.get().cloned() else {
        return;
    };
    let live = gate.begin_destroy();
    send(BackendEvent::SurfaceDestroyed);
    if live {
        gate.wait_acked(DESTROY_ACK_TIMEOUT);
    }
}

unsafe extern "C" fn dispatch_touch_event(
    component: *mut ffi::OH_NativeXComponent,
    window: *mut c_void,
) {
    // Any input means the window is interactive again; re-arm presentation in
    // case the surface show callback was missed (modal dialogs, focus moves).
    if let Some(visible) = VISIBLE.get() {
        visible.store(true, Ordering::Relaxed);
    }
    // SAFETY: zeroed is a valid bit pattern for this POD struct.
    let mut touch: ffi::OH_NativeXComponent_TouchEvent = unsafe { core::mem::zeroed() };
    // SAFETY: pointers provided by the callback contract.
    let rc = unsafe { ffi::OH_NativeXComponent_GetTouchEvent(component, window, &mut touch) };
    if rc != 0 {
        return;
    }
    let phase = match touch.type_ {
        ffi::OH_NATIVEXCOMPONENT_DOWN => TouchPhase::Down,
        ffi::OH_NATIVEXCOMPONENT_UP => TouchPhase::Up,
        ffi::OH_NATIVEXCOMPONENT_MOVE => TouchPhase::Move,
        ffi::OH_NATIVEXCOMPONENT_CANCEL => TouchPhase::Cancel,
        _ => return,
    };
    let seen = TOUCH_LOGGED.fetch_add(1, Ordering::Relaxed);
    if seen < TOUCH_LOG_LIMIT {
        crate::log::info(&format!(
            "touch #{seen}: {phase:?} at ({:.0}, {:.0})",
            touch.x, touch.y
        ));
    }
    send(BackendEvent::Touch {
        x: touch.x,
        y: touch.y,
        phase,
    });
}

/// Wheel and touchpad scrolling arrives only through the XComponent UI input
/// callback; the touch callback never sees axis events.
unsafe extern "C" fn on_axis_event(
    _component: *mut ffi::OH_NativeXComponent,
    event: *mut ffi::ArkUI_UIInputEvent,
    _type: i32,
) {
    if event.is_null() {
        return;
    }
    // Any input means the window is interactive again (see the touch handler).
    if let Some(visible) = VISIBLE.get() {
        visible.store(true, Ordering::Relaxed);
    }
    // SAFETY: `event` is a live axis event owned by the loader for the
    // duration of this callback.
    let action = unsafe { ffi::OH_ArkUI_AxisEvent_GetAxisAction(event) };
    // BEGIN and UPDATE carry the incremental value; END and CANCEL do not.
    if action != ffi::UI_AXIS_EVENT_ACTION_BEGIN && action != ffi::UI_AXIS_EVENT_ACTION_UPDATE {
        return;
    }
    let vertical = unsafe { ffi::OH_ArkUI_AxisEvent_GetVerticalAxisValue(event) } as f32;
    let horizontal = unsafe { ffi::OH_ArkUI_AxisEvent_GetHorizontalAxisValue(event) } as f32;
    if vertical == 0.0 && horizontal == 0.0 {
        return;
    }
    let x = unsafe { ffi::OH_ArkUI_PointerEvent_GetX(event) };
    let y = unsafe { ffi::OH_ArkUI_PointerEvent_GetY(event) };
    let seen = SCROLL_LOGGED.fetch_add(1, Ordering::Relaxed);
    if seen < SCROLL_LOG_LIMIT {
        crate::log::info(&format!(
            "scroll #{seen}: action={action} raw=({horizontal:.2}, {vertical:.2}) at ({x:.0}, {y:.0})"
        ));
    }
    // OHOS reports positive axis values toward the end of the content (wheel
    // down / touchpad swipe up with natural scrolling), the opposite of
    // Slint's positive delta (toward the start), so invert both axes.
    send(BackendEvent::Scroll {
        x,
        y,
        delta_x: -horizontal * LOGICAL_PX_PER_WHEEL_DEGREE,
        delta_y: -vertical * LOGICAL_PX_PER_WHEEL_DEGREE,
    });
}

unsafe extern "C" fn on_key_event(component: *mut ffi::OH_NativeXComponent, _window: *mut c_void) {
    if let Some(visible) = VISIBLE.get() {
        visible.store(true, Ordering::Relaxed);
    }
    let mut event: *mut ffi::OH_NativeXComponent_KeyEvent = core::ptr::null_mut();
    // SAFETY: pointers provided by the callback contract.
    if unsafe { ffi::OH_NativeXComponent_GetKeyEvent(component, &mut event) } != 0
        || event.is_null()
    {
        return;
    }
    let mut action: i32 = -1;
    let mut code: i32 = -1;
    // SAFETY: `event` is a live instance owned by the XComponent for the
    // duration of this callback.
    let ok = unsafe {
        ffi::OH_NativeXComponent_GetKeyEventAction(event, &mut action) == 0
            && ffi::OH_NativeXComponent_GetKeyEventCode(event, &mut code) == 0
    };
    if !ok || action < 0 {
        return;
    }
    let down = action == ffi::OH_NATIVEXCOMPONENT_KEY_ACTION_DOWN;
    match code {
        ffi::KEY_SHIFT_LEFT | ffi::KEY_SHIFT_RIGHT => KEY_SHIFT.store(down, Ordering::Relaxed),
        ffi::KEY_CAPS_LOCK if down => {
            KEY_CAPS_LOCK.fetch_xor(true, Ordering::Relaxed);
        }
        _ => {}
    }
    let repeat = if down {
        mark_key_down(code)
    } else {
        mark_key_up(code);
        false
    };
    let Some(text) = translate_key(code) else {
        return;
    };
    send(BackendEvent::Key { text, down, repeat });
}

/// Translate an OHOS key code into Slint's key-text representation (US
/// layout). Returns `None` for keys the backend does not handle.
fn translate_key(code: i32) -> Option<slint::SharedString> {
    use slint::platform::Key;

    let shift = KEY_SHIFT.load(Ordering::Relaxed);
    let caps = KEY_CAPS_LOCK.load(Ordering::Relaxed);

    let printable = |unshifted: &str, shifted: &str| {
        slint::SharedString::from(if shift { shifted } else { unshifted })
    };

    let text: slint::SharedString = match code {
        ffi::KEY_DIGIT_BASE..=2009 => {
            const UNSHIFTED: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
            const SHIFTED: [&str; 10] = [")", "!", "@", "#", "$", "%", "^", "&", "*", "("];
            let index = (code - ffi::KEY_DIGIT_BASE) as usize;
            printable(UNSHIFTED[index], SHIFTED[index])
        }
        ffi::KEY_A..=ffi::KEY_Z => {
            let ch = (b'a' + (code - ffi::KEY_A) as u8) as char;
            let ch = if shift ^ caps {
                ch.to_ascii_uppercase()
            } else {
                ch
            };
            ch.to_string().into()
        }
        ffi::KEY_COMMA => printable(",", "<"),
        ffi::KEY_PERIOD => printable(".", ">"),
        ffi::KEY_GRAVE => printable("`", "~"),
        ffi::KEY_MINUS => printable("-", "_"),
        ffi::KEY_EQUALS => printable("=", "+"),
        ffi::KEY_LEFT_BRACKET => printable("[", "{"),
        ffi::KEY_RIGHT_BRACKET => printable("]", "}"),
        ffi::KEY_BACKSLASH => printable("\\", "|"),
        ffi::KEY_SEMICOLON => printable(";", ":"),
        ffi::KEY_APOSTROPHE => printable("'", "\""),
        ffi::KEY_SLASH => printable("/", "?"),
        ffi::KEY_AT => slint::SharedString::from("@"),
        ffi::KEY_SPACE => Key::Space.into(),
        ffi::KEY_ENTER => Key::Return.into(),
        ffi::KEY_DEL => Key::Backspace.into(),
        ffi::KEY_FORWARD_DEL => Key::Delete.into(),
        ffi::KEY_TAB => Key::Tab.into(),
        ffi::KEY_ESCAPE => Key::Escape.into(),
        ffi::KEY_DPAD_UP => Key::UpArrow.into(),
        ffi::KEY_DPAD_DOWN => Key::DownArrow.into(),
        ffi::KEY_DPAD_LEFT => Key::LeftArrow.into(),
        ffi::KEY_DPAD_RIGHT => Key::RightArrow.into(),
        ffi::KEY_MOVE_HOME => Key::Home.into(),
        ffi::KEY_MOVE_END => Key::End.into(),
        ffi::KEY_PAGE_UP => Key::PageUp.into(),
        ffi::KEY_PAGE_DOWN => Key::PageDown.into(),
        ffi::KEY_SHIFT_LEFT => Key::Shift.into(),
        ffi::KEY_SHIFT_RIGHT => Key::ShiftR.into(),
        ffi::KEY_CTRL_LEFT => Key::Control.into(),
        ffi::KEY_CTRL_RIGHT => Key::ControlR.into(),
        ffi::KEY_ALT_LEFT => Key::Alt.into(),
        ffi::KEY_ALT_RIGHT => Key::AltGr.into(),
        ffi::KEY_META_LEFT => Key::Meta.into(),
        ffi::KEY_META_RIGHT => Key::MetaR.into(),
        ffi::KEY_CAPS_LOCK => Key::CapsLock.into(),
        _ => return None,
    };
    Some(text)
}
