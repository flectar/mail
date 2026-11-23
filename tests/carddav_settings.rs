use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::RefCell, rc::Rc};

slint::slint! {
    #[edition = "1.19"]
    import { SettingsDialog } from "../ui/dialogs/settings-dialog.slint";

    export component CarddavSettingsHarness inherits Window {
        preferred-width: 390px;
        preferred-height: 780px;
        in property <bool> compact: true;
        in property <string> status;
        in property <bool> connecting: false;
        in-out property <int> account: 1;
        in-out property <string> url: "https://contacts.example.com/";
        in-out property <string> username: "user@example.com";
        in-out property <string> password: "test-app-password";
        in-out property <bool> manage-existing: false;
        callback connect(int, string, string, string);
        callback disconnect(int);

        SettingsDialog {
            width: parent.width;
            height: parent.height;
            compact: root.compact;
            settings_tab: "Accounts";
            sync_status: root.status;
            carddav_account_id <=> root.account;
            carddav_url <=> root.url;
            carddav_username <=> root.username;
            carddav_password <=> root.password;
            carddav_manage_existing <=> root.manage-existing;
            carddav_connecting: root.connecting;
            connect_carddav(account, url, username, password) => {
                root.connect(account, url, username, password);
            }
            disconnect_carddav(account) => { root.disconnect(account); }
        }
    }
}

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

type Calls = Rc<RefCell<Vec<(i32, String, String, String)>>>;

fn click(ui: &CarddavSettingsHarness, x: f32, y: f32) {
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

/// Where Connect is: the dialog is as tall as its form, so walk down the
/// right end of its footer from the middle of the window until a click
/// submits. A click past the dialog would land on the backdrop and close it.
fn connect_point(
    ui: &CarddavSettingsHarness,
    calls: &Calls,
    width: f32,
    height: f32,
) -> (f32, f32) {
    let x = (width + 520.0_f32.min(width - 24.0)) / 2.0 - 60.0;
    let mut y = height / 2.0;
    while y < height {
        click(ui, x, y);
        if !calls.borrow().is_empty() {
            return (x, y);
        }
        assert_eq!(
            ui.get_account(),
            1,
            "nothing between the form and Connect closes the dialog"
        );
        y += 2.0;
    }
    panic!("Connect is out of view at {width}x{height}");
}

#[test]
fn connect_shows_over_the_page_and_stays_reachable() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).unwrap();
    let ui = CarddavSettingsHarness::new().unwrap();
    let calls: Calls = Rc::new(RefCell::new(Vec::new()));
    let recorded = calls.clone();
    ui.on_connect(move |account, url, username, password| {
        recorded
            .borrow_mut()
            .push((account, url.into(), username.into(), password.into()));
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
            "Could not connect CardDAV: HTTP 401 Unauthorized from \
             https://contacts.example.com/.well-known/carddav. Check the username and \
             app password before trying again."
                .into(),
        );
        ui.set_account(1);
        ui.set_url("https://contacts.example.com/".into());
        ui.set_password("test-app-password".into());
        calls.borrow_mut().clear();

        let (x, y) = connect_point(&ui, &calls, width as f32, height as f32);
        assert_eq!(
            calls.borrow().as_slice(),
            &[(
                1,
                "https://contacts.example.com/".into(),
                "user@example.com".into(),
                "test-app-password".into(),
            )],
            "Connect submits the form at {width}x{height}"
        );

        ui.set_connecting(true);
        click(&ui, x, y);
        assert_eq!(
            calls.borrow().len(),
            1,
            "a pending connection must not submit again"
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
        assert_eq!(calls.borrow().len(), 2, "the server needs the password");

        ui.set_password("test-app-password".into());
        ui.set_url("".into());
        click(&ui, x, y);
        assert_eq!(
            calls.borrow().len(),
            2,
            "every connection needs a server address"
        );
    }
}

#[test]
fn escape_and_the_backdrop_close_the_dialog() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).unwrap();
    let ui = CarddavSettingsHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(1320, 800));
    ui.set_compact(false);
    ui.show().unwrap();

    // The dialog opens with the server address focused, so Escape reaches it.
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Escape.into(),
    });
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Escape.into(),
    });
    assert_eq!(ui.get_account(), -1, "Escape closes the dialog");
    assert_eq!(ui.get_password(), "", "closing forgets the typed password");

    ui.set_account(1);
    ui.set_password("test-app-password".into());
    click(&ui, 10.0, 10.0);
    assert_eq!(ui.get_account(), -1, "a click beside the dialog closes it");
    assert_eq!(ui.get_password(), "");
}

#[test]
fn only_a_connected_address_book_offers_disconnect() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).unwrap();
    let ui = CarddavSettingsHarness::new().unwrap();
    let disconnected = Rc::new(RefCell::new(Vec::new()));
    let recorded = disconnected.clone();
    ui.on_disconnect(move |account| recorded.borrow_mut().push(account));
    ui.window().set_size(slint::PhysicalSize::new(1320, 800));
    ui.set_compact(false);
    ui.show().unwrap();

    // Disconnect is the footer's first button, at the dialog's left edge.
    let footer_left = (1320.0 - 520.0) / 2.0 + 22.0 + 20.0;
    let calls: Calls = Rc::new(RefCell::new(Vec::new()));
    let recorded_calls = calls.clone();
    ui.on_connect(move |account, url, username, password| {
        recorded_calls
            .borrow_mut()
            .push((account, url.into(), username.into(), password.into()));
    });
    let (_, y) = connect_point(&ui, &calls, 1320.0, 800.0);

    click(&ui, footer_left, y);
    assert!(
        disconnected.borrow().is_empty(),
        "a new connection has nothing to disconnect"
    );
    assert_eq!(ui.get_account(), 1);

    ui.set_manage_existing(true);
    click(&ui, footer_left, y);
    assert_eq!(disconnected.borrow().as_slice(), &[1]);
    assert_eq!(ui.get_account(), -1, "disconnecting closes the dialog");
}
