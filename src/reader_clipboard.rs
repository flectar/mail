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

/// How long a copied secret stays on the clipboard, as password managers
/// clear theirs.
#[cfg(desktop)]
const SECRET_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);

/// Copy a secret, such as the recovery code, kept out of clipboard history,
/// clipboard managers and cloud clipboards where the system lets an app ask,
/// and take it off the clipboard after a minute unless something else was
/// copied meanwhile. False where only the Slint clipboard copies.
pub fn set_secret(text: &str) -> bool {
    #[cfg(desktop)]
    {
        let copied = CLIPBOARD.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = arboard::Clipboard::new().ok();
            }
            slot.as_mut().is_some_and(|clipboard| {
                let set = clipboard.set();
                #[cfg(target_os = "windows")]
                let set = {
                    use arboard::SetExtWindows;
                    set.exclude_from_monitoring()
                        .exclude_from_cloud()
                        .exclude_from_history()
                };
                #[cfg(target_os = "macos")]
                let set = {
                    use arboard::SetExtApple;
                    set.exclude_from_history()
                };
                #[cfg(linux_desktop)]
                let set = {
                    use arboard::SetExtLinux;
                    set.exclude_from_history()
                };
                set.text(text).is_ok()
            })
        });
        if copied {
            let text = text.to_owned();
            slint::Timer::single_shot(SECRET_LIFETIME, move || clear_secret(&text));
        }
        copied
    }
    #[cfg(mobile)]
    {
        let _ = text;
        false
    }
}

/// Take a secret off the clipboard, if it is still what the clipboard holds.
#[cfg(desktop)]
fn clear_secret(text: &str) {
    CLIPBOARD.with(|slot| {
        if let Some(clipboard) = slot.borrow_mut().as_mut()
            && clipboard.get_text().is_ok_and(|current| current == text)
        {
            let _ = clipboard.clear();
        }
    });
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
