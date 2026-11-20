//! The badge as Windows 11 draws its own: the accent shades of the taskbar's
//! light or dark mode, and bold Segoe UI as the system renders small text.

use super::Palette;
use std::ptr::null_mut;
use windows::UI::ViewManagement::{AccessibilitySettings, UIColorType, UISettings};
use windows_sys::Win32::Foundation::{LPARAM, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, COLOR_WINDOW,
    COLOR_WINDOWTEXT, CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET,
    DEFAULT_PITCH, DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC,
    DeleteObject, DrawTextW, FW_BOLD, GdiFlush, GetDC, GetSysColor, GetTextExtentPoint32W,
    GetTextMetricsW, HDC, HFONT, OUT_TT_PRECIS, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    TEXTMETRICW, TRANSPARENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SPI_SETHIGHCONTRAST, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE,
};

/// The colours of Windows 11's own badges, which WinUI's InfoBadge shares:
/// the lighter accent with black text on a dark taskbar, the darker accent
/// with white text on a light one, and the window's colours in high
/// contrast.
pub(crate) fn palette() -> Palette {
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

/// Whether a window message tells of new colours for the badges: the Windows
/// mode or accent, which Windows announces as `ImmersiveColorSet`, high
/// contrast, or the system colours, which change with the high contrast
/// theme. Windows sends these to every top-level window.
///
/// # Safety
/// `lparam` is what the message passes: for `WM_SETTINGCHANGE`, null or a
/// NUL-terminated string.
pub(crate) unsafe fn recoloured(message: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
    match message {
        WM_SYSCOLORCHANGE => true,
        // SAFETY: forwarded from the caller.
        WM_SETTINGCHANGE => unsafe { colours_changed(wparam, lparam) },
        _ => false,
    }
}

/// Whether a `WM_SETTINGCHANGE` with these arguments changed the colours of
/// the badges.
///
/// # Safety
/// `lparam` is null or a NUL-terminated string, as the message passes.
unsafe fn colours_changed(wparam: WPARAM, lparam: LPARAM) -> bool {
    if wparam == SPI_SETHIGHCONTRAST as WPARAM {
        return true;
    }
    let area = lparam as *const u16;
    if area.is_null() {
        return false;
    }
    let expected = "ImmersiveColorSet\0".encode_utf16();
    for (index, wanted) in expected.enumerate() {
        // SAFETY: the string is read only up to its NUL, or the first
        // difference before it.
        if unsafe { *area.add(index) } != wanted {
            return false;
        }
    }
    true
}

/// How much bold system text covers each pixel of a badge `height` pixels
/// tall, as Windows renders text that small: hinted, without colour
/// fringes, with the ink in the middle. The badge is as wide as the text
/// needs, from `height` up to `max_width`; that width comes with the mask.
pub(crate) fn text_mask(text: &str, height: usize, max_width: usize) -> Option<(usize, Vec<u8>)> {
    let text: Vec<u16> = text.encode_utf16().collect();
    let length = i32::try_from(text.len()).ok()?;
    let height = i32::try_from(height).ok().filter(|&height| height > 0)?;
    let max_width = i32::try_from(max_width).ok()?.max(height);
    // SAFETY: every object created here is selected out and deleted before
    // returning, and the pixels are read while the bitmap lives.
    unsafe {
        let screen = GetDC(null_mut());
        let dc = CreateCompatibleDC(screen);
        ReleaseDC(null_mut(), screen);
        if dc.is_null() {
            return None;
        }
        let (font, width) = fitting_font(dc, &text, length, height, max_width);
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(
            dc,
            &top_down(width, height),
            DIB_RGB_COLORS,
            &mut bits,
            null_mut(),
            0,
        );
        if bitmap.is_null() || bits.is_null() {
            DeleteObject(font);
            DeleteDC(dc);
            return None;
        }
        let old_bitmap = SelectObject(dc, bitmap);
        let old_font = SelectObject(dc, font);
        SetTextColor(dc, 0x00ff_ffff);
        SetBkMode(dc, TRANSPARENT as i32);
        let mut area = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        DrawTextW(
            dc,
            text.as_ptr(),
            length,
            &mut area,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        GdiFlush();
        let (width, height) = (width as usize, height as usize);
        let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), width * height * 4);
        let mask: Vec<u8> = pixels
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
        Some((width, super::centre(width, height, &mask)))
    }
}

/// Bold Segoe UI at the size of Windows' own badge numbers, with the
/// badge's width for it, by the rules the module sets out: a disc widens up
/// to `max_width`, and text that still doesn't fit is narrowed.
///
/// # Safety
/// `dc` is a live device context; the caller deletes the font.
unsafe fn fitting_font(
    dc: HDC,
    text: &[u16],
    length: i32,
    height: i32,
    max_width: i32,
) -> (HFONT, i32) {
    let font_height = super::font_height(height as usize, text.len()) as i32;
    // SAFETY: the caller's live device context.
    let (font, extent, average) = unsafe { measured_font(dc, text, length, font_height, 0) };
    let width = super::badge_width(extent as f32, height as usize, max_width as usize);
    if super::fits(extent as f32, width) {
        return (font, width as i32);
    }
    // SAFETY: a font this function created and no DC holds.
    unsafe { DeleteObject(font) };
    for narrow in (1..average).rev() {
        // SAFETY: as above.
        let (font, extent, _) = unsafe { measured_font(dc, text, length, font_height, narrow) };
        if super::fits_narrowed(extent as f32, width) || narrow == 1 {
            return (font, width as i32);
        }
        // SAFETY: as above.
        unsafe { DeleteObject(font) };
    }
    // SAFETY: as above.
    (
        unsafe { measured_font(dc, text, length, font_height, 0) }.0,
        width as i32,
    )
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

/// A `width` by `height` pixel bitmap, 32 bits per pixel, first row on top.
fn top_down(width: i32, height: i32) -> BITMAPINFO {
    // SAFETY: an all-zero BITMAPINFO is valid; the header is filled in below.
    let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
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
