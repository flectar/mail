//! The unread count on the taskbar button of a copy of Flectar Mail without
//! package identity, which Windows' own badges need: an overlay icon the app
//! draws, as Thunderbird and Telegram do. It copies Windows 11's own badge,
//! as Qt's does, in the colours of the taskbar's light or dark mode. Windows
//! shows one overlay for a taskbar group, so the main window's stands for the
//! app. The taskbar takes it only once it shows the button, and forgets it
//! when Explorer restarts; it says so both times with `TaskbarButtonCreated`.

use super::badge_pixels::{self, Palette};
use std::cell::RefCell;
use std::ptr::null_mut;
use windows::UI::ViewManagement::{AccessibilitySettings, UIColorType, UISettings};
use windows::Win32::Foundation::HWND as ComHwnd;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::HICON as ComHicon;
use windows::core::{HSTRING, PCWSTR};
use windows_sys::Win32::Foundation::{HWND, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_BITFIELDS, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BITMAPV5HEADER,
    CLIP_DEFAULT_PRECIS, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateBitmap, CreateCompatibleDC,
    CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_CENTER,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FW_BOLD, GdiFlush,
    GetDC, GetSysColor, GetTextExtentPoint32W, GetTextMetricsW, HBITMAP, HDC, HFONT, OUT_TT_PRECIS,
    ReleaseDC, SelectObject, SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT,
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
    let mask = badge_pixels::centre(width, &text_mask(text, size)?);
    let pixels = badge_pixels::compose(width, &mask, palette());
    // SAFETY: `pixels` holds `size` rows of `size` BGRA pixels.
    unsafe { create_icon(size, &pixels) }
}

/// The colours of Windows 11's own badges, which WinUI's InfoBadge shares:
/// the lighter accent with black text on a dark taskbar, the darker accent
/// with white text on a light one, and the window's colours in high
/// contrast.
fn palette() -> Palette {
    let high_contrast = AccessibilitySettings::new()
        .and_then(|settings| settings.HighContrast())
        .unwrap_or(false);
    if high_contrast {
        // SAFETY: reading system colours has no preconditions.
        let (window, text) = unsafe { (GetSysColor(COLOR_WINDOW), GetSysColor(COLOR_WINDOWTEXT)) };
        // COLORREF holds red in its lowest byte.
        let rgb = |colour: u32| {
            let [red, green, blue, _] = colour.to_le_bytes();
            [red, green, blue]
        };
        return Palette {
            disc: rgb(window),
            text: rgb(text),
        };
    }
    let light = taskbar_is_light();
    let (shade, fallback) = if light {
        // The default blue accent's shades, should the settings fail.
        (UIColorType::AccentDark1, [0x00, 0x67, 0xc0])
    } else {
        (UIColorType::AccentLight2, [0x4c, 0xc2, 0xff])
    };
    let disc = UISettings::new()
        .and_then(|settings| settings.GetColorValue(shade))
        .map_or(fallback, |colour| [colour.R, colour.G, colour.B]);
    Palette {
        disc,
        text: if light { [0xff; 3] } else { [0; 3] },
    }
}

/// Whether the taskbar is light. It follows the Windows mode, not the apps'
/// mode, which can differ; builds from before the light taskbar have no
/// value and were dark.
fn taskbar_is_light() -> bool {
    windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_u32("SystemUsesLightTheme"))
        .is_ok_and(|value| value != 0)
}

/// How much bold system text covers each pixel of a `size` square, as
/// Windows renders text that small: hinted, without colour fringes.
fn text_mask(text: &str, size: i32) -> Option<Vec<u8>> {
    let text: Vec<u16> = text.encode_utf16().collect();
    let length = i32::try_from(text.len()).ok()?;
    // SAFETY: every object created here is selected out and deleted before
    // returning, and the pixels are read while the bitmap lives.
    unsafe {
        let screen = GetDC(null_mut());
        let dc = CreateCompatibleDC(screen);
        ReleaseDC(null_mut(), screen);
        if dc.is_null() {
            return None;
        }
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(
            dc,
            &top_down(size),
            DIB_RGB_COLORS,
            &mut bits,
            null_mut(),
            0,
        );
        if bitmap.is_null() || bits.is_null() {
            DeleteDC(dc);
            return None;
        }
        let old_bitmap = SelectObject(dc, bitmap);
        let font = fitting_font(dc, &text, length, size);
        let old_font = SelectObject(dc, font);
        SetTextColor(dc, 0x00ff_ffff);
        SetBkMode(dc, TRANSPARENT as i32);
        let mut area = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        DrawTextW(
            dc,
            text.as_ptr(),
            length,
            &mut area,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        GdiFlush();
        let pixels =
            std::slice::from_raw_parts(bits.cast::<u8>(), size as usize * size as usize * 4);
        let mask = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
            .collect();
        SelectObject(dc, old_font);
        DeleteObject(font);
        SelectObject(dc, old_bitmap);
        DeleteObject(bitmap);
        DeleteDC(dc);
        Some(mask)
    }
}

/// Bold Segoe UI at the size of Windows' own badge numbers, 11 pixels at
/// 96 DPI for one digit and 10 for more, as wide as fits three quarters of
/// the disc. 99+ is narrowed instead of shrunk, down to thirteen sixteenths
/// of the disc: at 16 pixels it would otherwise be 5 pixels tall.
///
/// # Safety
/// `dc` is a live device context; the caller deletes the font.
unsafe fn fitting_font(dc: HDC, text: &[u16], length: i32, size: i32) -> HFONT {
    let height = (if length > 1 { size * 10 } else { size * 11 } + 8) / 16;
    // SAFETY: the caller's live device context.
    let (font, extent, average) = unsafe { measured_font(dc, text, length, height, 0) };
    if extent * 4 <= size * 3 {
        return font;
    }
    // SAFETY: a font this function created and no DC holds.
    unsafe { DeleteObject(font) };
    for width in (1..average).rev() {
        // SAFETY: as above.
        let (font, extent, _) = unsafe { measured_font(dc, text, length, height, width) };
        if extent * 16 <= size * 13 || width == 1 {
            return font;
        }
        // SAFETY: as above.
        unsafe { DeleteObject(font) };
    }
    // SAFETY: as above.
    unsafe { measured_font(dc, text, length, height, 0) }.0
}

/// Bold Segoe UI `height` pixels tall, with characters `width` pixels wide
/// on average or its own width when 0, with how wide `text` and its
/// average character are in it.
///
/// # Safety
/// `dc` is a live device context; the caller deletes the font.
unsafe fn measured_font(
    dc: HDC,
    text: &[u16],
    length: i32,
    height: i32,
    width: i32,
) -> (HFONT, i32, i32) {
    let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
    // SAFETY: `face` is NUL-terminated and outlives the call.
    let font = unsafe {
        CreateFontW(
            -height,
            width,
            0,
            0,
            FW_BOLD as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_TT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            ANTIALIASED_QUALITY as u32,
            DEFAULT_PITCH as u32,
            face.as_ptr(),
        )
    };
    let mut extent = SIZE { cx: 0, cy: 0 };
    // SAFETY: an all-zero TEXTMETRICW is valid and filled in below.
    let mut metrics: TEXTMETRICW = unsafe { std::mem::zeroed() };
    // SAFETY: measuring with the font selected for the call only.
    unsafe {
        let previous = SelectObject(dc, font);
        GetTextExtentPoint32W(dc, text.as_ptr(), length, &mut extent);
        GetTextMetricsW(dc, &mut metrics);
        SelectObject(dc, previous);
    }
    (font, extent.cx, metrics.tmAveCharWidth)
}

/// A `size` pixel square, 32 bits per pixel, first row on top.
fn top_down(size: i32) -> BITMAPINFO {
    // SAFETY: an all-zero BITMAPINFO is valid; the header is filled in below.
    let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: size,
        biHeight: -size,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    info
}

/// An icon from top-down BGRA pixels with straight alpha. Windows reads the
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
    // the copy writes exactly the colour bitmap's pixels.
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
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
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
