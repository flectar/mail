use i_slint_core::accessibility::AccessibleStringProperty;
use i_slint_core::item_tree::ItemRc;
use i_slint_core::lengths::LogicalPoint;
use i_slint_core::window::WindowInner;
use slint::Rgb8Pixel;
use slint::platform::{
    Platform, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::ops::ControlFlow;
use std::rc::Rc;

slint::slint! {
    import { RecipientRow } from "../ui/components/composer-controls.slint";

    export component RecipientHarness inherits Window {
        preferred-width: 520px;
        preferred-height: 240px;
        in-out property <string> recipients;
        out property <length> row-height: row.height;

        public function focus-row() {
            row.focus();
        }

        row := RecipientRow {
            x: 0px;
            y: 0px;
            width: 520px;
            label: "To";
            text <=> root.recipients;
        }
    }
}

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn render(window: &MinimalSoftwareWindow) {
    slint::platform::update_timers_and_animations();
    let mut buffer = vec![Rgb8Pixel::default(); 520 * 240];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, 520);
    });
}

/// The vertical span of the editable text in window coordinates.
fn text_span(ui: &RecipientHarness, text: &str) -> (f32, f32) {
    let inner = WindowInner::from_pub(ui.window());
    let field = ItemRc::new_root(inner.component())
        .visit_descendants(|item| {
            if item
                .accessible_string_property(AccessibleStringProperty::Value)
                .as_deref()
                == Some(text)
            {
                ControlFlow::Break(item.clone())
            } else {
                ControlFlow::Continue(())
            }
        })
        .expect("the recipient field");
    let top = field.map_to_window(LogicalPoint::default()).y;
    (top, top + field.geometry().height())
}

#[test]
fn recipients_wrap_to_five_lines_then_follow_the_caret() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let ui = RecipientHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(520, 240));
    ui.show().unwrap();
    render(&window);
    assert_eq!(ui.get_row_height(), 40.0, "an empty row keeps its height");

    ui.invoke_focus_row();
    let key = |text: &str| {
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    };

    key("ada@example.com, grace@example.com");
    render(&window);
    assert_eq!(ui.get_row_height(), 40.0, "one line stays one row");

    let mut recipients = String::from("ada@example.com, grace@example.com");
    for index in 0..60 {
        let next = format!(", person{index}@example.com");
        key(&next);
        recipients.push_str(&next);
    }
    render(&window);
    let capped = ui.get_row_height();
    assert!(
        capped > 80.0 && capped < 160.0,
        "a long list grows the row to its cap: {capped}"
    );

    let (top, bottom) = text_span(&ui, &recipients);
    assert!(top < 0.0, "the start scrolled out of view: {top}");
    assert!(
        bottom <= capped,
        "the caret at the end stays inside the row: {bottom} > {capped}"
    );

    key(&slint::SharedString::from(slint::platform::Key::Return));
    assert_eq!(
        ui.get_recipients(),
        recipients,
        "Return must not insert a line break"
    );
}
