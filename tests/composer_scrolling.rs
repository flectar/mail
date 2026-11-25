//! The message scrolls with the wheel while the pointer rests on its text,
//! rather than jumping back to the caret (#488).
use slint::ComponentHandle;
use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::Cell, rc::Rc, time::Duration};

slint::slint! {
    #[edition = "1.19"]
    import { EmailComposer } from "../ui/components/email-composer.slint";
    export { MotionSettings } from "../ui/design-system.slint";

    export component ComposerHarness inherits Window {
        preferred-width: 900px;
        preferred-height: 640px;
        in-out property <string> body;
        in-out property <int> caret;
        in-out property <float> caret-y;
        // The pointer events the composer handed to the editor.
        in-out property <string> pointer-events;
        out property <length> scroll-y: composer.editor-scroll-y;

        composer := EmailComposer {
            width: parent.width;
            height: parent.height;
            body <=> root.body;
            selection-start: root.caret;
            selection-end: root.caret;
            // As the app lays out a long message: the caret sits on its last line.
            editor-content-height: 2000;
            editor-caret-y: root.caret-y;
            editor-caret-height: 20;
            editor-pointer-event(x, y, kind, control, shift, alt, meta) => {
                root.pointer-events += kind + " ";
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

    fn move_to(&self, x: f32, y: f32) {
        let position = slint::LogicalPosition::new(x, y);
        self.ui
            .window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        self.settle();
    }

    fn press(&self, x: f32, y: f32) {
        let position = slint::LogicalPosition::new(x, y);
        self.ui
            .window()
            .dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
        self.settle();
    }

    fn release(&self, x: f32, y: f32) {
        let position = slint::LogicalPosition::new(x, y);
        self.ui
            .window()
            .dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        self.settle();
    }

    fn wheel(&self, x: f32, y: f32, delta_y: f32) {
        let position = slint::LogicalPosition::new(x, y);
        self.ui
            .window()
            .dispatch_event(WindowEvent::PointerScrolled {
                position,
                delta_x: 0.0,
                delta_y,
            });
        self.settle();
    }
}

#[test]
fn the_wheel_scrolls_the_message_under_the_pointer_away_from_the_caret() {
    let harness = Harness::new();
    let body = (1..=90)
        .map(|line| format!("Line {line} of a long reply"))
        .collect::<Vec<_>>()
        .join("\n");
    harness.ui.set_caret(body.len() as i32);
    harness.ui.set_body(body.into());
    harness.settle();
    // Typing put the caret on the last line, so the message shows its end.
    harness.ui.set_caret_y(1960.0);
    harness.settle();
    let end = harness.ui.get_scroll_y();
    assert!(end > 0.0, "the message didn't scroll to the caret");

    // The pointer rests on the text while the wheel turns.
    let (x, y) = (450.0, 560.0);
    harness.move_to(x, y);
    assert_eq!(harness.ui.get_scroll_y(), end);
    harness.wheel(x, y, 300.0);
    let scrolled = harness.ui.get_scroll_y();
    assert!(
        scrolled < end - 100.0,
        "the wheel didn't scroll up: {scrolled} of {end}"
    );
    // A hand on the mouse moves it a little.
    harness.move_to(x + 4.0, y + 2.0);
    harness.move_to(x + 9.0, y + 5.0);
    assert_eq!(
        harness.ui.get_scroll_y(),
        scrolled,
        "moving the pointer over the text took the message back to the caret"
    );
}

#[test]
fn only_a_held_button_drags_in_the_message() {
    let harness = Harness::new();
    harness.ui.set_body("Hello world".into());
    harness.settle();
    let (x, y) = (300.0, 420.0);
    harness.move_to(x, y);
    harness.move_to(x + 20.0, y + 4.0);
    assert_eq!(
        harness.ui.get_pointer_events(),
        "",
        "a hover reached the editor"
    );
    harness.press(x, y);
    harness.move_to(x + 40.0, y);
    harness.release(x + 40.0, y);
    assert_eq!(harness.ui.get_pointer_events(), "down move up ");
}
