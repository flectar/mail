//! Windows keeps the native frame and removes only the caption, following
//! Windows Terminal. The window still has `WS_THICKFRAME`, so DWM draws the
//! shadow and rounded corners and keeps the minimize/maximize animations,
//! Aero Snap, the system menu and the side and bottom resize borders.
//!
//! Hit-testing reports the caption buttons drawn over the toolbar as
//! `HTMINBUTTON`/`HTMAXBUTTON`/`HTCLOSE`. Reporting the maximize button is
//! what makes Windows 11 offer Snap Layouts on hover. Windows then sends
//! non-client mouse messages for the buttons, so this module drives their
//! hover and pressed states itself. Empty toolbar space stays client area:
//! Slint starts native moves from it, which keeps Aero Snap.
//!
//! Winit owns the window procedure and its user data, so the window is
//! subclassed through comctl32. A thread-local CBT hook subclasses the window
//! during creation, before the first `WM_NCCALCSIZE`, so the initial frame is
//! already correct when the window is first shown.

use super::{TitleBarAction, WindowFrame};
use crate::{AppWindow, WindowChrome};
use slint::ComponentHandle;
use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
use slint::winit_030::winit::window::WindowAttributes;
use std::cell::{Cell, RefCell};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, ScreenToClient,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETAUTOHIDEBAREX, ABM_GETSTATE, ABS_AUTOHIDE,
    APPBARDATA, DefSubclassProc, RemoveWindowSubclass, SHAppBarMessage, SetWindowSubclass,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GWL_STYLE, GetClassNameW, GetClientRect, GetWindowLongW, HCBT_CREATEWND,
    HHOOK, HTCLIENT, HTCLOSE, HTMAXBUTTON, HTMINBUTTON, HTTOP, HTTOPLEFT, HTTOPRIGHT,
    IsZoomed, NCCALCSIZE_PARAMS, PostMessageW, SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE,
    SM_CXPADDEDBORDER, SM_CYSIZEFRAME, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_CBT, WM_MOUSEMOVE, WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST,
    WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
    WM_SYSCOMMAND, WS_CAPTION,
};

/// Window class that identifies the main window during creation.
const CLASS_NAME: &str = "FlectarMailWindow";
/// Window class of composer windows.
const COMPOSER_CLASS_NAME: &str = "FlectarMailComposer";
/// Winit's class for windows created without the attributes hook.
const WINIT_CLASS_NAME: &str = "Window Class";
const SUBCLASS_ID: usize = 0x464d_6169; // "FMai"
/// Keep a maximized window off auto-hidden taskbars so the pointer can
/// still reveal them, as Windows Terminal does.
const AUTOHIDE_TASKBAR_GAP: i32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Minimize,
    Maximize,
    Close,
}

impl Control {
    fn from_hit(hit: usize) -> Option<Self> {
        match hit as u32 {
            HTMINBUTTON => Some(Self::Minimize),
            HTMAXBUTTON => Some(Self::Maximize),
            HTCLOSE => Some(Self::Close),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Maximize => "maximize",
            Self::Close => "close",
        }
    }
}

struct State {
    hwnd: Cell<HWND>,
    hook: Cell<HHOOK>,
    enabled: Cell<bool>,
    /// Logical caption-button geometry shared with Slint; zero height while
    /// the controls are hidden, for example in full screen.
    button_height: Cell<f32>,
    button_width: Cell<f32>,
    hovered: Cell<Option<Control>>,
    pressed: Cell<Option<Control>>,
    tracking: Cell<bool>,
    sync_scheduled: Cell<bool>,
    /// The hook was re-armed for a window Slint recreates after hiding it,
    /// which carries winit's default class instead of [`CLASS_NAME`].
    recreating: Cell<bool>,
    app: RefCell<Option<slint::Weak<AppWindow>>>,
}

thread_local! {
    static STATE: State = const { State {
        hwnd: Cell::new(null_mut()),
        hook: Cell::new(null_mut()),
        enabled: Cell::new(false),
        button_height: Cell::new(0.0),
        button_width: Cell::new(46.0),
        hovered: Cell::new(None),
        pressed: Cell::new(None),
        tracking: Cell::new(false),
        sync_scheduled: Cell::new(false),
        recreating: Cell::new(false),
        app: RefCell::new(None),
    } };
}

pub(super) fn window_attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes.with_class_name(CLASS_NAME)
}

/// Composer windows have their own class, so the creation hook never takes
/// one for the main window.
pub(super) fn composer_window_attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes.with_class_name(COMPOSER_CLASS_NAME)
}

pub(super) fn prepare(startup: WindowFrame) {
    STATE.with(|state| {
        state.enabled.set(startup == WindowFrame::Integrated);
        if state.hook.get().is_null() {
            // SAFETY: a thread-local CBT hook on the UI thread, removed once
            // the main window has been subclassed.
            let hook = unsafe {
                SetWindowsHookExW(WH_CBT, Some(creation_hook), null_mut(), GetCurrentThreadId())
            };
            state.hook.set(hook);
        }
    });
}

/// Subclass the main window again when Slint recreates it on show. Hiding to
/// the tray destroys the native window, which removes the subclass.
pub(super) fn rearm() {
    STATE.with(|state| {
        if !state.hwnd.get().is_null() || !state.hook.get().is_null() {
            return;
        }
        state.recreating.set(true);
        // SAFETY: a thread-local CBT hook on the UI thread, removed once the
        // recreated window has been subclassed.
        let hook = unsafe {
            SetWindowsHookExW(WH_CBT, Some(creation_hook), null_mut(), GetCurrentThreadId())
        };
        state.hook.set(hook);
    });
}

pub(super) fn register(app: &AppWindow) {
    STATE.with(|state| *state.app.borrow_mut() = Some(app.as_weak()));
    sync_geometry(app);
    let weak = app.as_weak();
    app.global::<WindowChrome>().on_geometry_changed(move || {
        if let Some(app) = weak.upgrade() {
            sync_geometry(&app);
        }
    });
}

fn sync_geometry(app: &AppWindow) {
    let chrome = app.global::<WindowChrome>();
    STATE.with(|state| {
        state.button_height.set(if chrome.get_controls_visible() {
            chrome.get_band_height()
        } else {
            0.0
        });
        state.button_width.set(chrome.get_caption_button_width());
    });
}

pub(super) fn title_bar_action(gesture: &str) -> Option<TitleBarAction> {
    match gesture {
        "double" => Some(TitleBarAction::ToggleMaximize),
        // The system menu opens when the right button is released.
        "secondary-release" => Some(TitleBarAction::Menu),
        _ => None,
    }
}

pub(super) fn set_enabled(enabled: bool) {
    let hwnd = STATE.with(|state| {
        if state.enabled.replace(enabled) == enabled {
            return None;
        }
        state.hovered.set(None);
        state.pressed.set(None);
        Some(state.hwnd.get()).filter(|hwnd| !hwnd.is_null())
    });
    let Some(hwnd) = hwnd else {
        return;
    };
    schedule_sync();
    // Recalculate the frame outside the current Slint callback; the resize
    // this causes is delivered through winit like any other.
    let hwnd = hwnd as usize;
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        // SAFETY: the handle belongs to this thread's main window; a stale
        // handle makes SetWindowPos fail without side effects.
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED
                    | SWP_NOMOVE
                    | SWP_NOSIZE
                    | SWP_NOZORDER
                    | SWP_NOOWNERZORDER
                    | SWP_NOACTIVATE,
            );
        }
    });
}

unsafe extern "system" fn creation_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HCBT_CREATEWND as i32 {
        let hwnd = wparam as HWND;
        if is_main_window_class(hwnd) {
            STATE.with(|state| {
                if state.hwnd.get().is_null() {
                    // SAFETY: hwnd is being created on this thread.
                    unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0) };
                    state.hwnd.set(hwnd);
                    state.recreating.set(false);
                }
            });
        }
    }
    // SAFETY: forwarding to the next hook as required.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn is_main_window_class(hwnd: HWND) -> bool {
    let mut name = [0u16; 64];
    // SAFETY: the buffer length is passed alongside the buffer.
    let length = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) };
    if length <= 0 {
        return false;
    }
    let name = String::from_utf16_lossy(&name[..length as usize]);
    name == CLASS_NAME
        || (name == WINIT_CLASS_NAME && STATE.with(|state| state.recreating.get()))
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    // The hook is only needed until the main window exists.
    let hook = STATE.with(|state| state.hook.replace(null_mut()));
    if !hook.is_null() {
        // SAFETY: the hook was installed by `prepare` on this thread.
        unsafe { UnhookWindowsHookEx(hook) };
    }
    if message == WM_NCDESTROY {
        STATE.with(|state| state.hwnd.set(null_mut()));
        // SAFETY: removing this module's own subclass from its window.
        unsafe { RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID) };
    } else if STATE.with(|state| state.enabled.get()) && has_caption(hwnd) {
        // SAFETY: called with the arguments of the message being processed.
        if let Some(result) = unsafe { handle(hwnd, message, wparam, lparam) } {
            return result;
        }
    }
    // SAFETY: forwarding to winit's window procedure.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

/// Full-screen windows have no caption style and keep default handling.
fn has_caption(hwnd: HWND) -> bool {
    // SAFETY: reading the style of a live window.
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    style & WS_CAPTION == WS_CAPTION
}

unsafe fn handle(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match message {
        WM_NCCALCSIZE if wparam != 0 => {
            // SAFETY: for WM_NCCALCSIZE with wparam TRUE, lparam points to
            // NCCALCSIZE_PARAMS owned by the caller for this message.
            let params = unsafe { &mut *(lparam as *mut NCCALCSIZE_PARAMS) };
            let top = params.rgrc[0].top;
            // Let the default frame compute the side and bottom borders, then
            // give the caption area back to the client.
            unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
            params.rgrc[0].top = top;
            // SAFETY: querying a live window.
            if unsafe { IsZoomed(hwnd) } != 0 {
                // A maximized window extends past the monitor by its frame.
                params.rgrc[0].top += resize_border(hwnd);
                leave_autohide_taskbars_reachable(hwnd, &mut params.rgrc[0]);
            }
            Some(0)
        }
        WM_NCHITTEST => {
            let hit = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
            if hit != HTCLIENT as LRESULT {
                return Some(hit);
            }
            Some(hit_test(hwnd, lparam) as LRESULT)
        }
        WM_NCMOUSEMOVE => {
            set_hovered(Control::from_hit(wparam));
            track_non_client_leave(hwnd);
            None
        }
        WM_NCMOUSELEAVE => {
            STATE.with(|state| state.tracking.set(false));
            set_hovered(None);
            set_pressed(None);
            None
        }
        WM_MOUSEMOVE => {
            if STATE.with(|state| state.hovered.get().is_some()) {
                set_hovered(None);
                set_pressed(None);
            }
            None
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK => {
            // Swallow presses on the drawn buttons; the default handling
            // would start modal tracking of classic caption buttons.
            let control = Control::from_hit(wparam)?;
            set_pressed(Some(control));
            Some(0)
        }
        WM_NCLBUTTONUP => {
            let pressed = STATE.with(|state| state.pressed.get());
            set_pressed(None);
            let control = Control::from_hit(wparam)?;
            if pressed == Some(control) {
                perform(hwnd, control);
            }
            Some(0)
        }
        _ => None,
    }
}

fn hit_test(hwnd: HWND, lparam: LPARAM) -> u32 {
    let mut point = POINT {
        x: (lparam & 0xffff) as i16 as i32,
        y: ((lparam >> 16) & 0xffff) as i16 as i32,
    };
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: both pointers reference locals for the duration of the call.
    unsafe {
        ScreenToClient(hwnd, &mut point);
        GetClientRect(hwnd, &mut client);
    }
    let (button_height, button_width) =
        STATE.with(|state| (state.button_height.get(), state.button_width.get()));
    let scale = scale_factor(hwnd);
    let button_height = (button_height * scale).round() as i32;
    let button_width = (button_width * scale).round() as i32;

    // The top resize border was given to the client area, so provide it here.
    // SAFETY: querying a live window.
    let maximized = unsafe { IsZoomed(hwnd) } != 0;
    let border = resize_border(hwnd);
    if !maximized && point.y < border {
        return if point.x < border {
            HTTOPLEFT
        } else if point.x >= client.right - border {
            HTTOPRIGHT
        } else {
            HTTOP
        };
    }
    if point.y >= button_height {
        return HTCLIENT;
    }
    let from_right = client.right - point.x;
    if from_right <= button_width {
        HTCLOSE
    } else if from_right <= button_width * 2 {
        HTMAXBUTTON
    } else if from_right <= button_width * 3 {
        HTMINBUTTON
    } else {
        HTCLIENT
    }
}

/// Slint honours SLINT_SCALE_FACTOR over the monitor DPI.
fn scale_factor(hwnd: HWND) -> f32 {
    std::env::var("SLINT_SCALE_FACTOR")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|scale| *scale > 0.0)
        // SAFETY: querying a live window.
        .unwrap_or_else(|| unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0)
}

/// Height of the sizing border Windows would place above the caption.
fn resize_border(hwnd: HWND) -> i32 {
    // SAFETY: querying system metrics for the window's DPI.
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
    }
}

fn leave_autohide_taskbars_reachable(hwnd: HWND, client: &mut RECT) {
    // SAFETY: APPBARDATA and MONITORINFO are plain structs sized as the API
    // requires, and both live for the duration of each call.
    unsafe {
        let mut data: APPBARDATA = std::mem::zeroed();
        data.cbSize = size_of::<APPBARDATA>() as u32;
        if SHAppBarMessage(ABM_GETSTATE, &mut data) as u32 & ABS_AUTOHIDE == 0 {
            return;
        }
        let mut monitor: MONITORINFO = std::mem::zeroed();
        monitor.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut monitor) == 0 {
            return;
        }
        for edge in [ABE_LEFT, ABE_TOP, ABE_RIGHT, ABE_BOTTOM] {
            data.uEdge = edge;
            data.rc = monitor.rcMonitor;
            if SHAppBarMessage(ABM_GETAUTOHIDEBAREX, &mut data) == 0 {
                continue;
            }
            match edge {
                ABE_LEFT => client.left += AUTOHIDE_TASKBAR_GAP,
                ABE_TOP => client.top += AUTOHIDE_TASKBAR_GAP,
                ABE_RIGHT => client.right -= AUTOHIDE_TASKBAR_GAP,
                _ => client.bottom -= AUTOHIDE_TASKBAR_GAP,
            }
        }
    }
}

fn track_non_client_leave(hwnd: HWND) {
    if STATE.with(|state| state.tracking.replace(true)) {
        return;
    }
    let mut track = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE | TME_NONCLIENT,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: the struct is initialized and outlives the call.
    unsafe { TrackMouseEvent(&mut track) };
}

fn perform(hwnd: HWND, control: Control) {
    let command = match control {
        Control::Minimize => SC_MINIMIZE,
        // SAFETY: querying a live window.
        Control::Maximize if unsafe { IsZoomed(hwnd) } != 0 => SC_RESTORE,
        Control::Maximize => SC_MAXIMIZE,
        // Delivered to winit as a close request, so tray behavior applies.
        Control::Close => SC_CLOSE,
    };
    // System commands keep the native animations and window-state handling.
    // SAFETY: posting a standard message to a live window.
    unsafe { PostMessageW(hwnd, WM_SYSCOMMAND, command as WPARAM, 0) };
}

fn set_hovered(control: Option<Control>) {
    if STATE.with(|state| state.hovered.replace(control)) != control {
        schedule_sync();
    }
}

fn set_pressed(control: Option<Control>) {
    if STATE.with(|state| state.pressed.replace(control)) != control {
        schedule_sync();
    }
}

/// Window messages can arrive while Slint is busy, for example during a
/// window-state change it initiated, so UI state is applied from the event
/// loop instead of from inside the window procedure.
fn schedule_sync() {
    if STATE.with(|state| state.sync_scheduled.replace(true)) {
        return;
    }
    let _ = slint::invoke_from_event_loop(|| {
        let (app, hovered, pressed) = STATE.with(|state| {
            state.sync_scheduled.set(false);
            (
                state.app.borrow().as_ref().and_then(slint::Weak::upgrade),
                state.hovered.get(),
                state.pressed.get(),
            )
        });
        if let Some(app) = app {
            let chrome = app.global::<WindowChrome>();
            chrome.set_hovered_control(hovered.map_or("", Control::key).into());
            chrome.set_pressed_control(pressed.map_or("", Control::key).into());
        }
    });
}
