//! What the composer's Send button needs: someone to send to, not text.
use i_slint_core::accessibility::AccessibleStringProperty;
use i_slint_core::item_tree::ItemRc;
use i_slint_core::window::WindowInner;
use slint::ComponentHandle;
use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::Cell, ops::ControlFlow, rc::Rc, time::Duration};

slint::slint! {
    #[edition = "1.19"]
    import { EmailComposer } from "../ui/components/email-composer.slint";
    export { MotionSettings } from "../ui/design-system.slint";
    export { AccountMailPreferences } from "../ui/components/account-mail-preferences.slint";

    export component SendingHarness inherits Window {
        preferred-width: 900px;
        preferred-height: 640px;
        in-out property <string> to;
        in-out property <string> cc;
        in-out property <string> bcc;
        in-out property <string> subject: "Photos";
        in-out property <string> body;
        in-out property <int> sent;

        EmailComposer {
            width: parent.width;
            height: parent.height;
            open: true;
            selected-account-id: 1;
            to <=> root.to;
            cc <=> root.cc;
            bcc <=> root.bcc;
            subject <=> root.subject;
            body <=> root.body;
            submit(send-now) => {
                if send-now {
                    root.sent += 1;
                }
            }
        }
    }
}

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    clock: Rc<Cell<Duration>>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.clock.get()
    }
}

struct Harness {
    ui: SendingHarness,
    window: Rc<MinimalSoftwareWindow>,
    clock: Rc<Cell<Duration>>,
}

impl Harness {
    fn new() -> Self {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let clock = Rc::new(Cell::new(Duration::ZERO));
        slint::platform::set_platform(Box::new(Headless {
            window: window.clone(),
            clock: clock.clone(),
        }))
        .unwrap();
        let ui = SendingHarness::new().unwrap();
        ui.window().set_size(slint::PhysicalSize::new(900, 640));
        ui.global::<MotionSettings>().set_enabled(false);
        ui.global::<AccountMailPreferences>()
            .set_composer_preferences_ready(true);
        ui.show().unwrap();
        let harness = Self { ui, window, clock };
        harness.settle();
        harness
    }

    fn settle(&self) {
        self.clock.set(self.clock.get() + Duration::from_secs(1));
        slint::platform::update_timers_and_animations();
        self.ui.window().request_redraw();
        self.window.draw_if_needed(|_| {});
    }

    /// Clicks Send, and tells whether the message went.
    fn send(&self) -> bool {
        let before = self.ui.get_sent();
        let button = find(self.ui.window(), "Send").expect("the Send button");
        let geometry = button.geometry();
        let middle = button.map_to_window(geometry.origin) + geometry.size.to_vector() / 2.0;
        let position = slint::LogicalPosition::new(middle.x, middle.y);
        let window = self.ui.window();
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
        self.settle();
        self.ui.get_sent() > before
    }
}

/// The shown element labelled `label` in the window.
fn find(window: &slint::Window, label: &str) -> Option<ItemRc> {
    let root = ItemRc::new_root(WindowInner::from_pub(window).component());
    root.visit_descendants(|item| {
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
fn a_message_without_text_can_be_sent() {
    let harness = Harness::new();
    harness.ui.set_to("ada@example.com".into());
    harness.settle();
    assert!(
        harness.send(),
        "a subject alone, or files alone, are a message"
    );
}

#[test]
fn a_message_goes_to_cc_or_bcc_alone() {
    let harness = Harness::new();
    harness.ui.set_body("See you there".into());
    harness.settle();
    assert!(
        !harness.send(),
        "a message with no one to go to can't be sent"
    );
    harness.ui.set_bcc("ada@example.com".into());
    harness.settle();
    assert!(harness.send(), "Bcc alone");
    harness.ui.set_bcc("".into());
    harness.ui.set_cc("grace@example.com".into());
    harness.settle();
    assert!(harness.send(), "Cc alone");
}
