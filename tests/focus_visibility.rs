use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
};
use std::{cell::Cell, rc::Rc};

slint::slint! {
    import { Button } from "../ui/components/button.slint";
    import { IconButton } from "../ui/components/controls.slint";

    export component FocusHarness inherits Window {
        preferred-width: 240px;
        preferred-height: 160px;
        out property <bool> button-ring: labeled.focus-visible;
        out property <bool> icon-ring: icon.focus-visible;
        callback button-activated();
        callback icon-activated();

        labeled := Button {
            x: 20px; y: 20px; width: 140px;
            text: "Action";
            clicked => { root.button-activated(); }
        }
        icon := IconButton {
            x: 20px; y: 80px;
            tooltip-text: "More";
            clicked => { root.icon-activated(); }
        }
    }
}

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn pointer_activation_hides_rings_and_keyboard_use_restores_them() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).unwrap();
    let ui = FocusHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(240, 160));
    ui.show().unwrap();

    let button_clicks = Rc::new(Cell::new(0));
    let icon_clicks = Rc::new(Cell::new(0));
    let count = button_clicks.clone();
    ui.on_button_activated(move || count.set(count.get() + 1));
    let count = icon_clicks.clone();
    ui.on_icon_activated(move || count.set(count.get() + 1));

    let pointer = |x, y| {
        let position = slint::LogicalPosition::new(x, y);
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    };
    let key = |key: slint::SharedString| {
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: key.clone() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: key });
    };

    pointer(60.0, 36.0);
    assert_eq!(button_clicks.get(), 1);
    assert!(
        !ui.get_button_ring(),
        "a click must not leave a button outline"
    );

    key(slint::platform::Key::Return.into());
    assert_eq!(
        button_clicks.get(),
        2,
        "the clicked button retains keyboard operation"
    );
    assert!(ui.get_button_ring(), "keyboard use should reveal its focus");

    pointer(60.0, 36.0);
    assert!(
        !ui.get_button_ring(),
        "clicking an already focused button clears the ring"
    );

    key(slint::platform::Key::Tab.into());
    assert!(ui.get_icon_ring(), "Tab focus must stay visible");
    key(slint::platform::Key::Space.into());
    assert_eq!(icon_clicks.get(), 1);

    pointer(36.0, 96.0);
    assert_eq!(icon_clicks.get(), 2);
    assert!(
        !ui.get_icon_ring(),
        "an icon click must clear its keyboard ring"
    );
}
