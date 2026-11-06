//! Password fields can be shown while typing on desktop and hide again once
//! cleared. Phones get no show button: there a shown password would be plain
//! text to the keyboard, which learns what is typed.

use i_slint_core::accessibility::{AccessibilityAction, AccessibleStringProperty};
use i_slint_core::item_tree::ItemRc;
use i_slint_core::window::WindowInner;
use slint::ComponentHandle;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{ops::ControlFlow, rc::Rc};

slint::slint! {
    import { LineEdit } from "../ui/components/controls.slint";
    export { WindowChrome } from "../ui/window-chrome.slint";

    export component PasswordFieldHarness inherits Window {
        preferred-width: 400px;
        preferred-height: 60px;
        in-out property <string> password;

        LineEdit {
            text <=> root.password;
            input-type: InputType.password;
            placeholder-text: "Password";
        }
    }
}

struct Headless;

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

/// The shown element labelled `label`, if there is one.
fn labelled(ui: &PasswordFieldHarness, label: &str) -> Option<ItemRc> {
    let inner = WindowInner::from_pub(ui.window());
    ItemRc::new_root(inner.component()).visit_descendants(|item| {
        if item.is_visible()
            && item
                .accessible_string_property(AccessibleStringProperty::Label)
                .as_deref()
                == Some(label)
        {
            ControlFlow::Break(item.clone())
        } else {
            ControlFlow::Continue(())
        }
    })
}

#[test]
fn passwords_can_be_shown_on_desktop_only() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let ui = PasswordFieldHarness::new().unwrap();
    ui.set_password("Secret123".into());
    ui.show().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(400, 60));

    // Phones own the window frame, so the platform is empty there.
    ui.global::<WindowChrome>().set_platform("".into());
    slint::platform::update_timers_and_animations();
    assert!(labelled(&ui, "Show password").is_none());

    ui.global::<WindowChrome>().set_platform("linux".into());
    slint::platform::update_timers_and_animations();
    labelled(&ui, "Show password")
        .expect("desktop password fields have a show button")
        .accessible_action(&AccessibilityAction::Default);
    slint::platform::update_timers_and_animations();
    assert!(labelled(&ui, "Hide password").is_some());

    // A cleared field, as when the form resets, hides the next password.
    ui.set_password("".into());
    slint::platform::update_timers_and_animations();
    assert!(labelled(&ui, "Show password").is_some());
}
