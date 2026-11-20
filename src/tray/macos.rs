//! The menu bar item's name for VoiceOver. Slint names the item only through
//! its title, which macOS shows as text beside the icon, so the tray leaves
//! the title empty and names the item's button here. Slint makes the item
//! once its event loop runs, and a hidden tray's item may not be there until
//! the tray turns on, so this looks for it then, for a while.

use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, class, msg_send};
use objc2_app_kit::{NSApplication, NSView};
use objc2_foundation::NSString;
use std::time::Duration;

/// How long apart, and how many times, to look for the item.
const INTERVAL: Duration = Duration::from_millis(250);
const ATTEMPTS: u32 = 40;

/// Give the menu bar item the app's name, as soon as it exists.
pub(super) fn name_the_item() {
    look(ATTEMPTS);
}

fn look(attempts: u32) {
    if name() || attempts == 0 {
        return;
    }
    slint::Timer::single_shot(INTERVAL, move || look(attempts - 1));
}

/// Name the app's status bar button; whether there is one yet.
fn name() -> bool {
    let Some(main_thread) = MainThreadMarker::new() else {
        return false;
    };
    let name = NSString::from_str("Flectar Mail");
    NSApplication::sharedApplication(main_thread)
        .windows()
        .iter()
        .filter_map(|window| window.contentView())
        .any(|view| name_button_in(&view, &name))
}

/// Name the status bar button in `view` or under it.
fn name_button_in(view: &NSView, name: &NSString) -> bool {
    let object: &AnyObject = view;
    // SAFETY: NSStatusBarButton is an AppKit class, and asking a view's class
    // has no preconditions.
    let button: bool = unsafe { msg_send![object, isKindOfClass: class!(NSStatusBarButton)] };
    if button {
        // SAFETY: setting a view's accessibility label on the main thread.
        let _: () = unsafe { msg_send![object, setAccessibilityLabel: name] };
        return true;
    }
    view.subviews()
        .iter()
        .any(|child| name_button_in(&child, name))
}
