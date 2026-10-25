use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::RefCell, rc::Rc};

slint::slint! {
    import { SettingsDialog } from "../ui/dialogs/settings-dialog.slint";

    export component CaldavSettingsHarness inherits Window {
        preferred-width: 390px;
        preferred-height: 780px;
        in property <bool> compact: true;
        in property <string> status;
        in property <bool> connecting: false;
        in-out property <int> account: 1;
        in-out property <string> url: "https://calendar.example.com/";
        in-out property <string> username: "user@example.com";
        in-out property <string> password: "test-app-password";
        in-out property <int> connection: -1;
        callback connect(int, int, string, string, string);

        SettingsDialog {
            width: parent.width;
            height: parent.height;
            compact: root.compact;
            settings_open: true;
            settings_tab: "Accounts";
            sync_status: root.status;
            caldav_account_id <=> root.account;
            caldav_url <=> root.url;
            caldav_username <=> root.username;
            caldav_password <=> root.password;
            caldav_connection_id <=> root.connection;
            caldav_connecting: root.connecting;
            connect_caldav(account, connection, url, username, password) => {
                root.connect(account, connection, url, username, password);
            }
        }
    }
}

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn click(ui: &CaldavSettingsHarness, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

#[test]
fn connect_stays_reachable_with_a_keyboard_and_wrapped_status() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).unwrap();
    let ui = CaldavSettingsHarness::new().unwrap();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let recorded = calls.clone();
    ui.on_connect(move |account, connection, url, username, password| {
        recorded
            .borrow_mut()
            .push((account, connection, url, username, password));
    });
    ui.show().unwrap();

    for (width, height, compact) in [
        (390, 780, true),
        (390, 350, true),
        (320, 260, true),
        (1320, 800, false),
    ] {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        ui.set_compact(compact);
        ui.set_status(
            "Could not connect CalDAV: Network error: error sending request for URL \
             (https://calendar.example.com/remote.php/dav/calendars/user/). \
             Check the server address and your network connection before trying again."
                .into(),
        );
        ui.set_account(1);
        ui.set_connection(-1);
        ui.set_password("test-app-password".into());
        calls.borrow_mut().clear();

        let panel_height = (if compact { 500.0_f32 } else { 430.0 }).min(height as f32 - 24.0);
        let panel_width = 520.0_f32.min(width as f32 - 24.0);
        let button_height = if compact { 44.0 } else { 34.0 };
        let x = (width as f32 + panel_width) / 2.0 - 60.0;
        let y = (height as f32 + panel_height) / 2.0 - 20.0 - button_height / 2.0;
        click(&ui, x, y);

        let recorded = calls.borrow();
        assert_eq!(recorded.len(), 1, "Connect must work at {width}x{height}");
        assert_eq!(
            recorded[0],
            (
                1,
                -1,
                "https://calendar.example.com/".into(),
                "user@example.com".into(),
                "test-app-password".into(),
            )
        );
        assert_eq!(ui.get_account(), 1, "Connect must not hit the backdrop");
        let submitted = recorded[0].clone();
        drop(recorded);

        ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(
                width as f32 / 2.0,
                (height as f32 - panel_height) / 2.0 + 100.0,
            ),
            delta_x: 0.0,
            delta_y: -500.0,
        });
        calls.borrow_mut().clear();
        click(&ui, x, y);
        assert_eq!(
            calls.borrow().as_slice(),
            &[submitted],
            "scrolling the form must preserve Connect and its credentials"
        );

        ui.set_connecting(true);
        click(&ui, x, y);
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: slint::platform::Key::Return.into(),
        });
        ui.window().dispatch_event(WindowEvent::KeyReleased {
            text: slint::platform::Key::Return.into(),
        });
        assert_eq!(
            calls.borrow().len(),
            1,
            "pending discovery must not submit again"
        );

        ui.set_connecting(false);
        click(&ui, x, y);
        assert_eq!(
            calls.borrow().len(),
            2,
            "a failed attempt must allow retrying"
        );

        ui.set_password("".into());
        click(&ui, x, y);
        assert_eq!(
            calls.borrow().len(),
            2,
            "a new connection requires a password"
        );
        // Changing a saved connection submits its id and may keep the
        // saved password.
        ui.set_connection(7);
        click(&ui, x, y);
        assert_eq!(
            calls.borrow().len(),
            3,
            "reconnecting can keep the saved password"
        );
        assert_eq!(calls.borrow()[2].1, 7);
        assert_eq!(calls.borrow()[2].4, "");

        ui.set_url("".into());
        click(&ui, x, y);
        assert_eq!(
            calls.borrow().len(),
            3,
            "every connection requires a server address"
        );
        ui.set_url("https://calendar.example.com/".into());
    }
}
