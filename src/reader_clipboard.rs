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
