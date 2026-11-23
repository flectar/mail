use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::RefCell, rc::Rc};

slint::slint! {
    #[edition = "1.19"]
    import { SelectableText } from "../ui/components/selectable-text.slint";

    export component SelectableTextHarness inherits Window {
        preferred-width: 240px;
        preferred-height: 200px;
        in property <string> text;

        public function copy() { subject.copy(); }

        subject := SelectableText {
            x: 0px;
            y: 0px;
            width: 240px;
            height: self.preferred-height;
            text: root.text;
            color: black;
            font-size: 20px;
            max-lines: 3;
        }
    }
}

/// A headless desktop that keeps what is copied.
struct Desktop(Rc<MinimalSoftwareWindow>, Rc<RefCell<String>>);

impl Platform for Desktop {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn set_clipboard_text(&self, text: &str, _: slint::platform::Clipboard) {
        *self.1.borrow_mut() = text.to_owned();
    }
}

const SUBJECT: &str = "Your weekly traffic report for flectar.com and 14 more zones, \
                       with cache hit ratios, threats blocked and the top countries";

#[test]
fn copy_takes_the_selection_or_all_of_a_cut_subject() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    let clipboard = Rc::new(RefCell::new(String::new()));
    slint::platform::set_platform(Box::new(Desktop(window.clone(), clipboard.clone()))).unwrap();
    let ui = SelectableTextHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(240, 200));
    ui.set_text(SUBJECT.into());
    ui.show().unwrap();

    // Nothing selected: Copy takes the whole subject, the lines past the
    // third included.
    ui.invoke_copy();
    assert_eq!(clipboard.borrow().as_str(), SUBJECT);

    // A drag along the first line selects part of it, and Copy takes only
    // that part.
    clipboard.borrow_mut().clear();
    let at = |x: f32| slint::LogicalPosition::new(x, 10.0);
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position: at(1.0),
        button: PointerEventButton::Left,
    });
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position: at(60.0) });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position: at(60.0),
        button: PointerEventButton::Left,
    });
    ui.invoke_copy();
    let copied = clipboard.borrow().clone();
    assert!(
        !copied.is_empty() && copied.len() < 20 && SUBJECT.starts_with(&copied),
        "the selection is the start of the first line, got {copied:?}"
    );
}
