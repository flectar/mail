//! The tray's tooltip says how many unread messages there are, or
//! conversations where the mail list shows those, under the app's name, and
//! only the name while there are none. The name beside the icon can be left
//! out, as the macOS menu bar needs, and turning the tray on tells Rust,
//! which draws it then and names the macOS item.

use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::rc::Rc;

slint::slint! {
    #[edition = "1.19"]
    import { FlectarTray } from "../ui/tray.slint";

    export component TrayHarness inherits FlectarTray {
        in property <int> count;
        in property <bool> grouped;
        in property <bool> name-shown: true;
        in property <bool> on;
        out property <int> times-shown;
        out property <string> shown-tooltip: self.tooltip;
        out property <string> shown-title: self.title;
        // No picture, so no platform tray icon is made.
        image: @image-url("");
        unread: root.count;
        conversations: root.grouped;
        named: root.name-shown;
        enabled: root.on;
        shown => {
            root.times-shown += 1;
        }
    }
}

struct Headless;

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

#[test]
fn the_tooltip_says_the_count_under_the_name() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let tray = TrayHarness::new().unwrap();
    assert_eq!(tray.get_shown_tooltip(), "Flectar Mail");
    tray.set_count(3);
    assert_eq!(tray.get_shown_tooltip(), "Flectar Mail\n3 unread messages");
    tray.set_count(1);
    assert_eq!(tray.get_shown_tooltip(), "Flectar Mail\n1 unread message");
    tray.set_count(0);
    assert_eq!(tray.get_shown_tooltip(), "Flectar Mail");
    // The count is of conversations where the mail list shows those.
    tray.set_grouped(true);
    tray.set_count(4);
    assert_eq!(
        tray.get_shown_tooltip(),
        "Flectar Mail\n4 unread conversations"
    );
    tray.set_count(1);
    assert_eq!(
        tray.get_shown_tooltip(),
        "Flectar Mail\n1 unread conversation"
    );
}

#[test]
fn the_name_beside_the_icon_can_be_left_out() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let tray = TrayHarness::new().unwrap();
    assert_eq!(tray.get_shown_title(), "Flectar Mail");
    tray.set_name_shown(false);
    assert_eq!(tray.get_shown_title(), "");
    // The tooltip keeps the name.
    assert_eq!(tray.get_shown_tooltip(), "Flectar Mail");
}

#[test]
fn turning_the_tray_on_tells_rust() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let tray = TrayHarness::new().unwrap();
    slint::platform::update_timers_and_animations();
    assert_eq!(tray.get_times_shown(), 0);
    tray.set_on(true);
    slint::platform::update_timers_and_animations();
    assert_eq!(tray.get_times_shown(), 1);
    // Turning it off says nothing; on again says so again.
    tray.set_on(false);
    slint::platform::update_timers_and_animations();
    assert_eq!(tray.get_times_shown(), 1);
    tray.set_on(true);
    slint::platform::update_timers_and_animations();
    assert_eq!(tray.get_times_shown(), 2);
}
