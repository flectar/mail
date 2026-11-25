//! The composer's From row picks the account and its address from one menu,
//! and the signature from a menu at the row's end.
use i_slint_core::accessibility::{AccessibilityAction, AccessibleStringProperty};
use i_slint_core::item_tree::ItemRc;
use i_slint_core::window::WindowInner;
use slint::ComponentHandle;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::Cell, ops::ControlFlow, rc::Rc, time::Duration};

slint::slint! {
    #[edition = "1.19"]
    import { ComposerFromRow } from "../ui/components/composer-fields.slint";
    import { AppTheme } from "../ui/design-system.slint";
    export { MotionSettings } from "../ui/design-system.slint";

    export component FromRowHarness inherits Window {
        preferred-width: 720px;
        preferred-height: 360px;
        background: AppTheme.surface;
        in-out property <int> account-id: 1;
        in-out property <int> chosen-sender: -1;
        in-out property <int> chosen-signature: -1;
        in-out property <bool> asked-for-more: false;
        in property <[string]> senders: ["Alex Rivera <alex@flectar.com>", "Alex Rivera <billing@flectar.com>"];
        in property <string> security: "";

        ComposerFromRow {
            y: 0px;
            width: parent.width;
            accounts: [
                { id: 1, name: "Alex Rivera", email: "alex@flectar.com", initials: "AR" },
                { id: 2, name: "Flectar Support", email: "support@flectar.com", initials: "FS" },
            ];
            selected-account-id <=> root.account-id;
            from-label: "Alex Rivera  <alex@flectar.com>";
            sender-names: root.senders;
            sender-index: 0;
            signature-names: ["No signature", "Work", "Short"];
            signature-index: 1;
            security: root.security;
            background-color: AppTheme.surface;
            frame-border-color: AppTheme.border;
            hover-color: AppTheme.muted-control-bg;
            muted-color: AppTheme.muted;
            secondary-color: AppTheme.secondary;
            selected-background: AppTheme.accent-soft;
            title-color: AppTheme.heading;
            sender-chosen(index) => { root.chosen-sender = index; }
            more-senders => { root.asked-for-more = true; }
            signature-chosen(index) => { root.chosen-signature = index; }
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
    ui: FromRowHarness,
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
        let ui = FromRowHarness::new().unwrap();
        ui.window().set_size(slint::PhysicalSize::new(720, 360));
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

    /// Activates the shown element labelled `label`, as a click or a screen
    /// reader does.
    fn press(&self, label: &str) {
        find(self.ui.window(), label)
            .unwrap_or_else(|| panic!("no shown {label:?}"))
            .accessible_action(&AccessibilityAction::Default);
        self.settle();
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

/// Whether the open popup shows an element labelled `label`.
fn popup_shows(window: &slint::Window, label: &str) -> bool {
    WindowInner::from_pub(window)
        .active_popups()
        .iter()
        .any(|popup| {
            ItemRc::new_root(popup.component.clone())
                .visit_descendants(|item| {
                    if item.is_visible()
                        && item
                            .accessible_string_property(AccessibleStringProperty::Label)
                            .as_deref()
                            == Some(label)
                    {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                })
                .is_some()
        })
}

fn popup_open(window: &slint::Window) -> bool {
    !WindowInner::from_pub(window).active_popups().is_empty()
}

#[test]
fn the_from_menu_picks_an_account_or_one_of_its_addresses() {
    let harness = Harness::new();
    harness.press("From");
    assert!(popup_open(harness.ui.window()));
    harness.press("Alex Rivera <billing@flectar.com>");
    assert_eq!(harness.ui.get_chosen_sender(), 1);
    assert_eq!(harness.ui.get_account_id(), 1, "the account stayed");
    assert!(!popup_open(harness.ui.window()));

    harness.press("From");
    harness.press("Flectar Support, support@flectar.com");
    assert_eq!(harness.ui.get_account_id(), 2);
}

#[test]
fn many_addresses_are_searched_for_instead_of_listed() {
    let harness = Harness::new();
    let many = (1..=11)
        .map(|n| slint::SharedString::from(format!("Alex <alex{n}@flectar.com>")))
        .collect::<Vec<_>>();
    harness
        .ui
        .set_senders(slint::ModelRc::new(slint::VecModel::from(many)));
    harness.settle();
    harness.press("From");
    assert!(
        !popup_shows(harness.ui.window(), "Alex <alex2@flectar.com>"),
        "eleven addresses were listed"
    );
    harness.press("Choose another address…");
    assert!(harness.ui.get_asked_for_more());
}

#[test]
fn the_signature_menu_picks_a_signature_or_none() {
    let harness = Harness::new();
    harness.press("Signature");
    harness.press("Short");
    assert_eq!(harness.ui.get_chosen_signature(), 2);
    harness.press("Signature");
    harness.press("No signature");
    assert_eq!(harness.ui.get_chosen_signature(), 0);
}

#[test]
fn an_account_with_one_address_lists_only_the_accounts() {
    let harness = Harness::new();
    harness
        .ui
        .set_senders(slint::ModelRc::new(slint::VecModel::from(vec![
            slint::SharedString::from("Alex Rivera <alex@flectar.com>"),
        ])));
    harness.settle();
    harness.press("From");
    assert!(popup_shows(
        harness.ui.window(),
        "Alex Rivera, alex@flectar.com"
    ));
    assert!(!popup_shows(
        harness.ui.window(),
        "Alex Rivera <alex@flectar.com>"
    ));
}
