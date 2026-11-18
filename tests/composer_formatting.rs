//! The composer's formatting buttons act on the text selected in the message.
use i_slint_core::accessibility::AccessibleStringProperty;
use i_slint_core::item_tree::ItemRc;
use i_slint_core::items::TextInput;
use i_slint_core::window::WindowInner;
use slint::ComponentHandle;
use slint::platform::{
    Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::Cell, ops::ControlFlow, rc::Rc, time::Duration};

slint::slint! {
    #[edition = "1.19"]
    import { EmailComposer } from "../ui/components/email-composer.slint";
    export { MotionSettings } from "../ui/design-system.slint";

    export component ComposerHarness inherits Window {
        preferred-width: 900px;
        preferred-height: 640px;
        in-out property <string> body: "Hello world";
        in-out property <int> start: 0;
        in-out property <int> end: 0;
        in-out property <string> formatted-kind;
        in-out property <int> formatted-start: -1;
        in-out property <int> formatted-end: -1;

        EmailComposer {
            width: parent.width;
            height: parent.height;
            body <=> root.body;
            selection-start: root.start;
            selection-end: root.end;
            // As the app does for a drag over the first word: it selects "Hello".
            editor-pointer-event(x, y, kind, control, shift, alt, meta) => {
                if (kind == "down") {
                    root.start = 0;
                    root.end = 5;
                }
            }
            selection-changed(text, anchor, cursor) => {
                root.start = min(anchor, cursor);
                root.end = max(anchor, cursor);
            }
            format-requested(kind, text, anchor, cursor) => {
                root.formatted-kind = kind;
                root.formatted-start = min(anchor, cursor);
                root.formatted-end = max(anchor, cursor);
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
    ui: ComposerHarness,
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
        let ui = ComposerHarness::new().unwrap();
        ui.window().set_size(slint::PhysicalSize::new(900, 640));
        ui.global::<MotionSettings>().set_enabled(false);
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

    /// Presses in the message, where the app selects its first word.
    fn select_first_word(&self) {
        let input = message_input(self.ui.window()).expect("the message's text input");
        let origin = input.map_to_window(input.geometry().origin);
        self.press_at(origin.x + 8.0, origin.y + 8.0);
        assert_eq!((self.ui.get_start(), self.ui.get_end()), (0, 5));
    }

    fn press_at(&self, x: f32, y: f32) {
        let position = slint::LogicalPosition::new(x, y);
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
    }

    fn click(&self, label: &str) {
        let item = find(self.ui.window(), label).unwrap_or_else(|| panic!("no {label} shown"));
        let geometry = item.geometry();
        let middle = item.map_to_window(geometry.origin) + geometry.size.to_vector() / 2.0;
        self.press_at(middle.x, middle.y);
    }

    fn key(&self, text: impl Into<slint::SharedString>) {
        let text = text.into();
        let window = self.ui.window();
        window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        window.dispatch_event(WindowEvent::KeyReleased { text });
        self.settle();
    }

    fn control(&self, text: &str) {
        let window = self.ui.window();
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        self.key(text);
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        self.settle();
    }

    /// The formatting the composer asked for last, and the text it asked for it on.
    fn take_format(&self) -> (String, i32, i32) {
        let format = (
            self.ui.get_formatted_kind().to_string(),
            self.ui.get_formatted_start(),
            self.ui.get_formatted_end(),
        );
        self.ui.set_formatted_kind("".into());
        self.ui.set_formatted_start(-1);
        self.ui.set_formatted_end(-1);
        format
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

/// The text input that holds the message, under the composer's own drawing of it.
fn message_input(window: &slint::Window) -> Option<ItemRc> {
    let root = ItemRc::new_root(WindowInner::from_pub(window).component());
    root.visit_descendants(|item| match item.downcast::<TextInput>() {
        Some(input) if input.as_pin_ref().text() == "Hello world" => {
            ControlFlow::Break(item.clone())
        }
        _ => ControlFlow::Continue(()),
    })
}

#[test]
fn toolbar_buttons_format_the_selected_text() {
    let harness = Harness::new();
    harness.select_first_word();
    // A button takes the focus from the message, which takes its selection along.
    for (label, kind) in [
        ("Bold · Ctrl+B", "bold"),
        ("Italic · Ctrl+I", "italic"),
        ("Underline · Ctrl+U", "underline"),
        ("Strikethrough", "strike"),
    ] {
        harness.click(label);
        assert_eq!(harness.take_format(), (kind.to_owned(), 0, 5), "{label}");
    }
}

#[test]
fn the_link_button_links_the_selected_text() {
    let harness = Harness::new();
    harness.select_first_word();
    harness.click("Add or edit link");
    assert!(
        find(harness.ui.window(), "Add link").is_some(),
        "the link editor opens for the selected text"
    );
}

#[test]
fn shortcuts_format_what_the_message_has_selected() {
    let harness = Harness::new();
    harness.select_first_word();
    harness.control("b");
    assert_eq!(harness.take_format(), ("bold".to_owned(), 0, 5));
    // And on a selection made with the keyboard.
    harness.key(Key::End);
    assert_eq!((harness.ui.get_start(), harness.ui.get_end()), (11, 11));
    harness.control("a");
    harness.control("i");
    assert_eq!(harness.take_format(), ("italic".to_owned(), 0, 11));
}

#[test]
fn a_toolbar_menu_closed_unused_leaves_typing_in_the_message() {
    let harness = Harness::new();
    harness.select_first_word();
    // The toolbar leaves the focus in the message, so closing a menu gives it back there.
    harness.click("Text size");
    harness.key(Key::Escape);
    harness.key("x");
    assert_eq!(harness.ui.get_body(), "x world");
}
