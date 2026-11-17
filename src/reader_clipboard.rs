//! Optional desktop formats; the Slint text clipboard remains the fallback.
#[cfg(desktop)]
thread_local! {
    static CLIPBOARD: std::cell::RefCell<Option<arboard::Clipboard>> = const { std::cell::RefCell::new(None) };
}

pub fn set_html(html: &str, text: &str) -> bool {
    #[cfg(desktop)]
    return CLIPBOARD.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = arboard::Clipboard::new().ok();
        }
        slot.as_mut()
            .is_some_and(|clipboard| clipboard.set_html(html, Some(text)).is_ok())
    });
    #[cfg(mobile)]
    {
        let _ = (html, text);
        false
    }
}

/// The HTML on the clipboard, which apps put there with the text they copy.
pub fn html() -> Option<String> {
    #[cfg(desktop)]
    return CLIPBOARD.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = arboard::Clipboard::new().ok();
        }
        slot.as_mut()?.get().html().ok()
    });
    #[cfg(mobile)]
    None
}

/// A picture on the clipboard, such as a screenshot, as a PNG file.
pub fn image_png() -> Option<Vec<u8>> {
    #[cfg(desktop)]
    return CLIPBOARD.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = arboard::Clipboard::new().ok();
        }
        let picture = slot.as_mut()?.get().image().ok()?;
        let pixels = image::RgbaImage::from_raw(
            u32::try_from(picture.width).ok()?,
            u32::try_from(picture.height).ok()?,
            picture.bytes.into_owned(),
        )?;
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .ok()?;
        Some(png)
    });
    #[cfg(mobile)]
    None
}

pub fn set_primary(text: &str) {
    #[cfg(linux_desktop)]
    if !text.is_empty() {
        use arboard::SetExtLinux;
        CLIPBOARD.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = arboard::Clipboard::new().ok();
            }
            if let Some(clipboard) = slot.as_mut() {
                let _ = clipboard
                    .set()
                    .clipboard(arboard::LinuxClipboardKind::Primary)
                    .text(text);
            }
        });
    }
    #[cfg(not(linux_desktop))]
    let _ = text;
}
