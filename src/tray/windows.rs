//! The count on the Windows tray icon. The notification area shows icons at
//! the small icon size for the taskbar's scale, which Microsoft asks tray
//! icons to come in, so the picture is drawn at that size rather than left
//! to Windows to resize. The count copies Windows 11's own badges, as the
//! taskbar's does, and changes colour with the taskbar's mode, accent and
//! high contrast. Windows announces those, and changes of scale, to every
//! top-level window, so a hidden one of the app's listens: the app's window
//! is gone while it sits in the tray, and the tray's own window is
//! message-only, which hears no announcements.

use crate::badge_pixels::{self, Badge, Palette};
use std::cell::Cell;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, FindWindowW, MSGFLT_ALLOW,
    RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, WM_CLOSE, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_SETTINGCHANGE, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

thread_local! {
    /// The tray size the picture was last drawn for.
    static DRAWN_FOR: Cell<usize> = const { Cell::new(0) };
    /// A redraw waits for the event loop.
    static REDRAW_DUE: Cell<bool> = const { Cell::new(false) };
}

/// `icon` at the tray's size, with `unread` on it unless that is none.
pub(super) fn picture(icon: &slint::Image, unread: u32) -> Option<slint::Image> {
    let source = icon.to_rgba8()?;
    let from = usize::try_from(source.width()).ok()?;
    if source.height() != source.width() {
        return None;
    }
    let tray = tray_size()?;
    // Beyond 400 % scale the picture would be larger than its source;
    // Windows enlarges it then.
    let size = tray.min(from);
    let badge = (unread > 0).then(|| (unread, badge_pixels::windows::palette()));
    let pixels = draw(source.as_bytes(), from, size, badge)?;
    DRAWN_FOR.set(tray);
    let size = u32::try_from(size).ok()?;
    let pixels =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&pixels, size, size);
    Some(slint::Image::from_rgba8(pixels))
}

/// `source`, RGBA pixels `from` square, drawn `size` pixels square with the
/// count in its colours on it, if there is one.
pub(super) fn draw(
    source: &[u8],
    from: usize,
    size: usize,
    badge: Option<(u32, Palette)>,
) -> Option<Vec<u8>> {
    let pixels = badge_pixels::scale_down(source, from, size);
    let Some((count, palette)) = badge else {
        return Some(pixels);
    };
    let height = badge_pixels::tray_badge_height(size);
    let (width, text) =
        badge_pixels::windows::text_mask(&badge_pixels::tray_label(count, height), height, size)?;
    let badge = Badge {
        width,
        height,
        text,
        palette,
        // Windows 11 lays its badges straight on the icon.
        gap: 0.0,
    };
    Some(badge_pixels::draw(&pixels, size, &badge))
}

/// The size the notification area shows icons at: the small icon size at
/// the taskbar's DPI. Explorer follows a change of scale at once, while this
/// process's system metrics keep the scale it started with.
fn tray_size() -> Option<usize> {
    let class: Vec<u16> = "Shell_TrayWnd\0".encode_utf16().collect();
    // SAFETY: the class name is NUL-terminated and outlives the call; asking
    // a window's DPI and a system metric has no other preconditions.
    let size = unsafe {
        let taskbar = FindWindowW(class.as_ptr(), null());
        let dpi = if taskbar.is_null() {
            0
        } else {
            GetDpiForWindow(taskbar)
        };
        let dpi = if dpi == 0 { GetDpiForSystem() } else { dpi };
        GetSystemMetricsForDpi(SM_CXSMICON, dpi)
    };
    usize::try_from(size).ok().filter(|&size| size > 0)
}

/// Redraw the tray when Windows changes the badges' colours or the tray's
/// scale, from a hidden top-level window created once on this thread.
pub(super) fn follow_changes() {
    thread_local! {
        static LISTENER: Cell<HWND> = const { Cell::new(null_mut()) };
    }
    if !LISTENER.get().is_null() {
        return;
    }
    let class: Vec<u16> = "FlectarMailTrayChanges\0".encode_utf16().collect();
    // SAFETY: the class name is NUL-terminated and outlives both calls; the
    // window procedure lives for the whole process.
    let window = unsafe {
        let instance = GetModuleHandleW(null());
        let info = WNDCLASSW {
            lpfnWndProc: Some(listen),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            // SAFETY: an all-zero class is valid for the remaining fields.
            ..std::mem::zeroed()
        };
        // Fails only when the class exists, which creating the window
        // reports instead.
        RegisterClassW(&info);
        // A pop-up without a parent is top level, and without WS_VISIBLE
        // never shows; the tool window style keeps it off the taskbar.
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        )
    };
    if window.is_null() {
        tracing::debug!("notifications: the tray's count can't follow colour and scale changes");
        return;
    }
    // Explorer, which isn't elevated, may announce a new taskbar to an app
    // run as administrator.
    // SAFETY: a window of this thread and a registered message.
    unsafe { ChangeWindowMessageFilterEx(window, taskbar_created(), MSGFLT_ALLOW, null_mut()) };
    LISTENER.set(window);
}

unsafe extern "system" fn listen(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // The window lasts as long as the app. Tools that close an app's windows
    // would otherwise end the following for good.
    if message == WM_CLOSE {
        return 0;
    }
    // SAFETY: the arguments of the message being processed.
    let recoloured = unsafe { badge_pixels::windows::recoloured(message, wparam, lparam) };
    // A new scale may reach the taskbar by any of these; the size tells.
    let rescaled = (matches!(message, WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE)
        || message == taskbar_created())
        && tray_size().is_some_and(|size| size != DRAWN_FOR.get());
    if recoloured || rescaled {
        redraw_soon();
    }
    // SAFETY: forwarding the message as received.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Redraw once the event loop runs: Windows sends announcements while Slint
/// may be busy, and several for one change of theme, which share a redraw.
fn redraw_soon() {
    if REDRAW_DUE.replace(true) {
        return;
    }
    let posted = slint::invoke_from_event_loop(|| {
        REDRAW_DUE.set(false);
        super::refresh();
    });
    if posted.is_err() {
        REDRAW_DUE.set(false);
    }
}

/// The message Explorer sends every top-level window once it has made a new
/// taskbar, as after a restart.
fn taskbar_created() -> u32 {
    thread_local! {
        static MESSAGE: u32 = {
            let name: Vec<u16> = "TaskbarCreated\0".encode_utf16().collect();
            // SAFETY: a NUL-terminated name that outlives the call.
            unsafe { RegisterWindowMessageW(name.as_ptr()) }
        };
    }
    MESSAGE.with(|message| *message)
}
