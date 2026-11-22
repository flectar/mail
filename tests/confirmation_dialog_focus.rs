//! A confirmation dialog takes the keyboard focus when it opens, also when
//! `if` creates it already open, as the app's dialogs are. Escape and Return
//! then reach the dialog rather than whatever opened it behind the scrim.
use slint::ComponentHandle;
use slint::platform::{
    Platform, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::rc::Rc;

slint::slint! {
    #[edition = "1.19"]
    import { ConfirmationDialog } from "../ui/components/confirmation-dialog.slint";

    export component DialogHarness inherits Window {
        in-out property <bool> open;
        in-out property <int> cancelled;
        preferred-width: 400px;
        preferred-height: 300px;

        if root.open : ConfirmationDialog {
            width: parent.width;
            height: parent.height;
            open <=> root.open;
            title: "Delete the message?";
            cancelled => { root.cancelled += 1; }
        }
    }
}

const WIDTH: usize = 400;
const HEIGHT: usize = 300;

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn a_dialog_created_open_takes_the_focus() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let ui = DialogHarness::new().unwrap();
    ui.window()
        .set_size(slint::PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
    ui.show().unwrap();

    // The window creates the dialog, already open, when it next draws.
    ui.set_open(true);
    slint::platform::update_timers_and_animations();
    let mut pixels = vec![slint::Rgb8Pixel::default(); WIDTH * HEIGHT];
    ui.window().request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, WIDTH);
    });

    let escape: slint::SharedString = slint::platform::Key::Escape.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: escape.clone(),
    });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: escape });
    slint::platform::update_timers_and_animations();
    assert_eq!(ui.get_cancelled(), 1, "Escape reaches the dialog");
    assert!(!ui.get_open(), "and closes it");
}
