//! A host that gates first use on a stored privacy consent lets it be
//! withdrawn from Settings > About, and only after a confirmation. Hosts
//! without such a consent offer no withdrawal at all.

use flectar_mail::{AccountRow, AppWindow, MotionSettings};
use i_slint_core::accessibility::{AccessibilityAction, AccessibleStringProperty};
use i_slint_core::item_tree::ItemRc;
use i_slint_core::window::WindowInner;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, ModelRc, Rgb8Pixel, VecModel};
use std::{cell::Cell, ops::ControlFlow, rc::Rc};

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

/// The shown element whose accessible label is exactly `label`.
fn shown(app: &AppWindow, label: &str) -> Option<ItemRc> {
    let inner = WindowInner::from_pub(app.window());
    ItemRc::new_root(inner.component()).visit_descendants(|item| {
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
}

fn press(app: &AppWindow, label: &str) {
    shown(app, label)
        .unwrap_or_else(|| panic!("no shown element labelled {label:?}"))
        .accessible_action(&AccessibilityAction::Default);
}

#[test]
fn only_hosts_with_a_consent_offer_withdrawal_and_it_needs_confirmation() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let app = AppWindow::new().unwrap();
    app.global::<MotionSettings>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(1320, 800));
    app.set_startup_ready(true);
    app.set_startup_hydrated(true);
    app.set_connected_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
        id: 1,
        email: "person@example.com".into(),
        ..Default::default()
    }])));
    app.show().unwrap();
    let size = app.window().size();
    let draw = || {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            let width = size.width as usize;
            let mut pixels = vec![Rgb8Pixel::default(); width * size.height as usize];
            renderer.render(&mut pixels, width);
        });
    };
    let withdrawals = Rc::new(Cell::new(0));
    let count = withdrawals.clone();
    app.on_withdraw_privacy_consent(move || count.set(count.get() + 1));

    app.invoke_open_settings();
    app.set_settings_tab("About".into());
    draw();
    assert!(shown(&app, "Privacy policy").is_some());
    assert!(
        shown(&app, "Withdraw consent…").is_none(),
        "a host without a stored consent offers no withdrawal"
    );

    app.set_privacy_consent_withdrawable(true);
    draw();
    press(&app, "Withdraw consent…");
    draw();
    assert!(shown(&app, "Withdraw privacy consent?").is_some());
    assert_eq!(withdrawals.get(), 0, "the button only asks first");

    press(&app, "Cancel");
    draw();
    assert!(shown(&app, "Withdraw privacy consent?").is_none());
    assert_eq!(withdrawals.get(), 0, "cancelling keeps the consent");

    press(&app, "Withdraw consent…");
    draw();
    press(&app, "Withdraw consent");
    draw();
    assert_eq!(withdrawals.get(), 1, "confirming withdraws once");
    assert!(shown(&app, "Withdraw privacy consent?").is_none());
}
