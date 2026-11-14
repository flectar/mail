//! An account's sidebar heading shows what stops its checks, and its panel
//! offers the action that fits: trying again after a failure, or signing in
//! again after the server turned the sign-in down.

use i_slint_core::accessibility::{AccessibilityAction, AccessibleStringProperty};
use i_slint_core::item_tree::ItemRc;
use i_slint_core::window::WindowInner;
use slint::ComponentHandle;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{ops::ControlFlow, rc::Rc};

slint::slint! {
    import { SidebarSectionHeader } from "../ui/components/sidebar-controls.slint";
    import { AccountProblem } from "../ui/models.slint";

    export component AccountStatusHarness inherits Window {
        width: 300px;
        height: 400px;
        in property <AccountProblem> problem: AccountProblem.failed;
        in property <bool> marked: true;
        in-out property <int> checks;
        in-out property <int> sign-ins;
        in-out property <int> settings;
        SidebarSectionHeader {
            width: parent.width;
            label: "Flectar Support";
            detail: "support@flectar.com";
            account-menu: true;
            status: {
                problem: root.problem,
                marked: root.marked,
                reason: root.problem == AccountProblem.failed ? "network error: Connection refused" : "",
                checked_day: 0,
                checked_at: "09:58",
            };
            check-mail => { root.checks += 1; }
            sign-in => { root.sign-ins += 1; }
            account-settings => { root.settings += 1; }
        }
    }
}

struct Headless;

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

/// The shown element labelled `label` in the window, or in its open popup.
fn find(window: &slint::Window, label: &str) -> Option<ItemRc> {
    let inner = WindowInner::from_pub(window);
    let mut roots = vec![ItemRc::new_root(inner.component())];
    roots.extend(
        inner
            .active_popups()
            .iter()
            .map(|popup| ItemRc::new_root(popup.component.clone())),
    );
    roots.into_iter().find_map(|root| {
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
    })
}

fn press(window: &slint::Window, label: &str) {
    find(window, label)
        .unwrap_or_else(|| panic!("no shown {label:?}"))
        .accessible_action(&AccessibilityAction::Default);
}

fn popup_open(window: &slint::Window) -> bool {
    !WindowInner::from_pub(window).active_popups().is_empty()
}

#[test]
fn an_account_problem_opens_its_reason_and_the_fitting_action() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let ui = AccountStatusHarness::new().unwrap();
    ui.show().unwrap();

    // A failure shows its marker and, from it, the reason and Try again.
    press(ui.window(), "Couldn't check mail");
    assert!(popup_open(ui.window()));
    assert!(find(ui.window(), "network error: Connection refused").is_some());
    assert!(find(ui.window(), "Last checked 09:58 · Retrying automatically").is_some());
    press(ui.window(), "Try again");
    assert_eq!(ui.get_checks(), 1, "Try again checks the account");
    assert!(!popup_open(ui.window()), "and closes the panel");

    press(ui.window(), "Couldn't check mail");
    press(ui.window(), "Account settings");
    assert_eq!(ui.get_settings(), 1);
    assert!(!popup_open(ui.window()));

    // A refused sign-in offers signing in again instead.
    ui.set_problem(AccountProblem::SignIn);
    press(ui.window(), "Sign-in required");
    assert!(find(ui.window(), "Try again").is_none());
    assert!(find(ui.window(), "Last checked 09:58").is_some());
    press(ui.window(), "Sign in again");
    assert_eq!(ui.get_sign_ins(), 1);
    assert_eq!(ui.get_checks(), 1);

    // A failure that hasn't lasted shows no marker yet.
    ui.set_problem(AccountProblem::Failed);
    ui.set_marked(false);
    assert!(find(ui.window(), "Couldn't check mail").is_none());
}
