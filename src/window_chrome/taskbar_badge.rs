//! The unread count on the taskbar button of a copy of Flectar Mail without
//! package identity, which Windows' own badges need: an overlay icon the app
//! draws, as Thunderbird and Telegram do. It copies Windows 11's own badge,
//! as Qt's does, in the colours of the taskbar's light or dark mode. Windows
//! shows one overlay for a taskbar group, so the main window's stands for the
//! app. The taskbar takes it only once it shows the button, and forgets it
//! when Explorer restarts; it says so both times with `TaskbarButtonCreated`.

use crate::badge_pixels;
use std::cell::RefCell;
use std::ptr::null_mut;
use windows::Win32::Foundation::HWND as ComHwnd;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::HICON as ComHicon;
use windows::core::{HSTRING, PCWSTR};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BI_BITFIELDS, BITMAPINFO, BITMAPV5HEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, GetDC, HBITMAP, ReleaseDC,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, HICON, ICONINFO, SM_CXSMICON,
};

thread_local! {
    /// The count and what a screen reader says for it, while there is one.
    static BADGE: RefCell<Option<(u32, String)>> = const { RefCell::new(None) };
    static TASKBAR: RefCell<Option<ITaskbarList3>> = const { RefCell::new(None) };
}

/// Show `badge` on the button of `window`, the main window when there is
/// one, now and whenever the taskbar shows the button again.
pub(super) fn set(window: HWND, badge: Option<(u32, String)>) {
    BADGE.with(|current| *current.borrow_mut() = badge);
    apply(window);
}

/// Put the current badge on the button of `window`: after the taskbar has
/// created it, at another DPI, or in other colours.
pub(super) fn apply(window: HWND) {
    if window.is_null() {
        return;
    }
    let Some(taskbar) = taskbar() else {
        return;
    };
    let result = BADGE.with(|badge| match badge.borrow().as_ref() {
        // SAFETY: a live window of this thread; no icon takes the overlay off.
        None => unsafe {
            taskbar.SetOverlayIcon(ComHwnd(window), ComHicon::default(), PCWSTR::null())
        },
        Some((count, description)) => {
            // SAFETY: a live window of this thread.
            let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(window)) };
            let Some(icon) = icon(&badge_pixels::label(*count), size) else {
                return Ok(());
            };
            let description = HSTRING::from(description.as_str());
            // SAFETY: the taskbar copies the icon, which is destroyed after.
            let result =
                unsafe { taskbar.SetOverlayIcon(ComHwnd(window), ComHicon(icon), &description) };
            // SAFETY: an icon this function created.
            unsafe { DestroyIcon(icon) };
            result
        }
    });
    if let Err(error) = result {
        tracing::debug!(%error, "notifications: could not show the count on the taskbar");
    }
}

/// The taskbar's interface, created once on the window's thread, where winit
/// has initialized COM.
fn taskbar() -> Option<ITaskbarList3> {
    TASKBAR.with(|taskbar| {
        if taskbar.borrow().is_none() {
            // SAFETY: COM is initialized on this thread, and HrInit is called
            // once before any other method, as the interface requires.
            let created = unsafe {
                CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
            }
            .and_then(|list| unsafe { list.HrInit() }.map(|()| list));
            match created {
                Ok(list) => *taskbar.borrow_mut() = Some(list),
                Err(error) => {
                    tracing::debug!(%error, "notifications: no taskbar to show the count on");
                    return None;
                }
            }
        }
        taskbar.borrow().clone()
    })
}

/// A `size` pixel square icon of `text` on the badge's disc.
fn icon(text: &str, size: i32) -> Option<HICON> {
    let width = usize::try_from(size).ok().filter(|&width| width > 0)?;
    let (_, text) = badge_pixels::windows::text_mask(text, width, width)?;
    let badge = badge_pixels::Badge {
        width,
        height: width,
        text,
        palette: badge_pixels::windows::palette(),
        gap: 0.0,
    };
    // The disc alone, filling the square.
    let pixels = badge_pixels::draw(&vec![0; width * width * 4], width, &badge);
    // SAFETY: `pixels` holds `size` rows of `size` RGBA pixels.
    unsafe { create_icon(size, &pixels) }
}

/// An icon from top-down RGBA pixels with straight alpha. Windows reads the
/// alpha and ignores the empty mask.
///
/// # Safety
/// `pixels` holds `size` rows of `size` pixels.
unsafe fn create_icon(size: i32, pixels: &[u8]) -> Option<HICON> {
    let header = BITMAPV5HEADER {
        bV5Size: size_of::<BITMAPV5HEADER>() as u32,
        bV5Width: size,
        bV5Height: -size,
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_BITFIELDS,
        bV5RedMask: 0x00ff_0000,
        bV5GreenMask: 0x0000_ff00,
        bV5BlueMask: 0x0000_00ff,
        bV5AlphaMask: 0xff00_0000,
        // SAFETY: an all-zero header is valid for the remaining fields.
        ..unsafe { std::mem::zeroed() }
    };
    // Monochrome rows are padded to 16 bits.
    let mask_bits = vec![0_u8; (size as usize).div_ceil(16) * 2 * size as usize];
    // SAFETY: the bitmaps are created, used for the icon and deleted here;
    // the copy writes exactly the colour bitmap's pixels, blue first.
    unsafe {
        let screen = GetDC(null_mut());
        let mut bits = null_mut();
        let colour: HBITMAP = CreateDIBSection(
            screen,
            (&raw const header).cast::<BITMAPINFO>(),
            DIB_RGB_COLORS,
            &mut bits,
            null_mut(),
            0,
        );
        ReleaseDC(null_mut(), screen);
        if colour.is_null() || bits.is_null() {
            return None;
        }
        let bitmap = std::slice::from_raw_parts_mut(bits.cast::<u8>(), pixels.len());
        for (to, from) in bitmap
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(pixels.as_chunks::<4>().0)
        {
            *to = [from[2], from[1], from[0], from[3]];
        }
        let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr().cast());
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: colour,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(mask);
        DeleteObject(colour);
        (!icon.is_null()).then_some(icon)
    }
}

/// The message the taskbar sends once a window's button exists.
pub(super) fn button_created_message() -> u32 {
    thread_local! {
        static MESSAGE: u32 = {
            let name: Vec<u16> = "TaskbarButtonCreated\0".encode_utf16().collect();
            // SAFETY: a NUL-terminated name that outlives the call.
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(name.as_ptr())
            }
        };
    }
    MESSAGE.with(|message| *message)
}
