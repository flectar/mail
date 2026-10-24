//! The mail account form shows what it will submit: values reset or filled in
//! after the user typed reach the fields, and closing Settings closes the form.

use flectar_mail::AppWindow;
use i_slint_core::accessibility::{AccessibilityAction, AccessibleStringProperty};
use i_slint_core::item_tree::ItemRc;
use i_slint_core::window::WindowInner;
use slint::ComponentHandle;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::RefCell, ops::ControlFlow, rc::Rc};

slint::slint! {
    import { SettingsDialog } from "../ui/dialogs/settings-dialog.slint";

    export component AccountFormHarness inherits Window {
        preferred-width: 1320px;
        preferred-height: 800px;
        in-out property <bool> form-open: true;
        in-out property <string> email;
        in-out property <string> password;
        callback connect(string, string, string);

        SettingsDialog {
            width: parent.width;
            height: parent.height;
            settings_open: true;
            settings_tab: "Accounts";
            account_form_open <=> root.form-open;
            account_email <=> root.email;
            account_password <=> root.password;
            account_protocol: "jmap";
            add_password_account(protocol, email, username, password, jmap-url, incoming-host, incoming-port, smtp-host, smtp-port) => {
                root.connect(protocol, email, password);
            }
        }
    }
}

struct Headless;

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

/// The shown element whose accessible `property` is `value`.
fn shown(window: &slint::Window, property: AccessibleStringProperty, value: &str) -> ItemRc {
    let inner = WindowInner::from_pub(window);
    ItemRc::new_root(inner.component())
        .visit_descendants(|item| {
            if item.is_visible()
                && item.accessible_string_property(property).as_deref() == Some(value)
            {
                ControlFlow::Break(item.clone())
            } else {
                ControlFlow::Continue(())
            }
        })
        .unwrap_or_else(|| panic!("no shown element with {value:?}"))
}

/// The shown text input whose placeholder reads `placeholder`. The
/// placeholder is its own element, just ahead of the input in the tree.
fn field(ui: &AccountFormHarness, placeholder: &str) -> ItemRc {
    let inner = WindowInner::from_pub(ui.window());
    let mut after_placeholder = false;
    ItemRc::new_root(inner.component())
        .visit_descendants(|item| {
            let label = item.accessible_string_property(AccessibleStringProperty::Label);
            if label.as_deref() == Some(placeholder) {
                after_placeholder = true;
            } else if after_placeholder
                && item
                    .accessible_string_property(AccessibleStringProperty::Value)
                    .is_some()
            {
                if item.is_visible() {
                    return ControlFlow::Break(item.clone());
                }
                after_placeholder = false;
            }
            ControlFlow::Continue(())
        })
        .unwrap_or_else(|| panic!("no shown field with placeholder {placeholder:?}"))
}

/// Replaces the field's text the way typing does.
fn type_into(ui: &AccountFormHarness, placeholder: &str, text: &str) {
    field(ui, placeholder).accessible_action(&AccessibilityAction::SetValue(text.into()));
}

fn shows(ui: &AccountFormHarness, placeholder: &str) -> String {
    field(ui, placeholder)
        .accessible_string_property(AccessibleStringProperty::Value)
        .unwrap_or_default()
        .to_string()
}

fn press_connect(ui: &AccountFormHarness) {
    shown(
        ui.window(),
        AccessibleStringProperty::Label,
        "Connect account",
    )
    .accessible_action(&AccessibilityAction::Default);
}

const EMAIL: &str = "name@example.com";
const PASSWORD: &str = "Stored securely in your system keyring";

#[test]
fn account_form_fields_follow_their_values() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let ui = AccountFormHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(1320, 800));
    ui.show().unwrap();
    let submitted = Rc::new(RefCell::new(Vec::new()));
    let recorded = submitted.clone();
    ui.on_connect(move |protocol, email, password| {
        recorded.borrow_mut().push((
            protocol.to_string(),
            email.to_string(),
            password.to_string(),
        ));
    });

    type_into(&ui, EMAIL, "first@example.com");
    type_into(&ui, PASSWORD, "first-secret");
    assert_eq!(ui.get_email(), "first@example.com");
    assert_eq!(ui.get_password(), "first-secret");

    // A connected account clears the form, and opening it again starts
    // empty: fields the user typed in must show that too.
    ui.set_email("".into());
    ui.set_password("".into());
    assert_eq!(shows(&ui, EMAIL), "");
    assert_eq!(shows(&ui, PASSWORD), "");

    // With nothing shown, Connect stays off, as the form looks.
    type_into(&ui, EMAIL, "second@example.com");
    press_connect(&ui);
    assert!(submitted.borrow().is_empty());

    // Filled in again, it connects with exactly what is shown.
    type_into(&ui, PASSWORD, "second-secret");
    press_connect(&ui);
    assert_eq!(
        submitted.borrow().as_slice(),
        [(
            "jmap".to_owned(),
            "second@example.com".to_owned(),
            "second-secret".to_owned()
        )]
    );

    // Editing an account fills the form in, also over typed text.
    ui.set_email("admin@example.com".into());
    assert_eq!(shows(&ui, EMAIL), "admin@example.com");
    ui.hide().unwrap();

    // Hiding Settings destroys its dialog before the dialog's own handlers
    // run, so the window closes the forms and forgets typed passwords.
    let app = AppWindow::new().unwrap();
    app.show().unwrap();
    app.set_settings_open(true);
    slint::platform::update_timers_and_animations();
    app.set_account_form_open(true);
    app.set_account_password("typed".into());
    app.set_caldav_account_id(3);
    app.set_caldav_password("calendar".into());
    app.set_caldav_manage_existing(true);
    app.set_carddav_account_id(4);
    app.set_carddav_password("contacts".into());
    app.set_carddav_manage_existing(true);
    app.set_settings_open(false);
    slint::platform::update_timers_and_animations();
    assert!(!app.get_account_form_open());
    assert_eq!(app.get_account_password(), "");
    assert_eq!(app.get_caldav_account_id(), -1);
    assert_eq!(app.get_caldav_password(), "");
    assert!(!app.get_caldav_manage_existing());
    assert_eq!(app.get_carddav_account_id(), -1);
    assert_eq!(app.get_carddav_password(), "");
    assert!(!app.get_carddav_manage_existing());
}
