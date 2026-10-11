//! Optional desktop formats; the Slint text clipboard remains the fallback.
#[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
thread_local! {
    static CLIPBOARD: std::cell::RefCell<Option<arboard::Clipboard>> = const { std::cell::RefCell::new(None) };
}

pub fn set_html(html: &str, text: &str) -> bool {
    #[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
    return CLIPBOARD.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = arboard::Clipboard::new().ok();
        }
        slot.as_mut()
            .is_some_and(|clipboard| clipboard.set_html(html, Some(text)).is_ok())
    });
    #[cfg(target_env = "ohos")]
    return crate::platform_services::current()
        .is_some_and(|service| service.clipboard_write(text, Some(html)).is_ok());
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = (html, text);
        false
    }
}

pub fn set_primary(text: &str) {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
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
    #[cfg(any(not(target_os = "linux"), target_env = "ohos"))]
    let _ = text;
}
