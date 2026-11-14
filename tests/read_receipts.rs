//! The read receipt bar says who asked and where the receipt would go, and
//! each answer reaches the app. The receipts that came back for a sent
//! message say who did what and when.

use i_slint_core::accessibility::{AccessibilityAction, AccessibleStringProperty};
use i_slint_core::item_tree::ItemRc;
use i_slint_core::window::WindowInner;
use slint::ComponentHandle;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::Cell, ops::ControlFlow, rc::Rc};

slint::slint! {
    import { ReadReceipts, ReceiptRequestBar, ReturnedReceipt, ReturnedReceiptList } from "../ui/read-receipts.slint";
    export { ReadReceipts, ReturnedReceipt }

    export component ReceiptBarHarness inherits Window {
        width: 640px;
        height: 400px;
        VerticalLayout {
            alignment: start;
            ReceiptRequestBar {
                text-color: black;
            }
            ReturnedReceiptList {
                text-color: black;
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

fn find(window: &slint::Window, label: &str) -> Option<ItemRc> {
    let inner = WindowInner::from_pub(window);
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

#[test]
fn receipts_say_who_asked_and_who_did_what() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let ui = ReceiptBarHarness::new().unwrap();
    ui.show().unwrap();
    let receipts = ui.global::<ReadReceipts>();
    receipts.set_sender("Ana Ruiz".into());
    let sent = Rc::new(Cell::new(0));
    let ignored = Rc::new(Cell::new(0));
    let count = Rc::clone(&sent);
    receipts.on_send(move || count.set(count.get() + 1));
    let count = Rc::clone(&ignored);
    receipts.on_ignore(move || count.set(count.get() + 1));

    assert!(
        find(
            ui.window(),
            "Ana Ruiz asked to be notified when you read this message."
        )
        .is_some()
    );
    find(ui.window(), "Send receipt")
        .unwrap()
        .accessible_action(&AccessibilityAction::Default);
    find(ui.window(), "Ignore")
        .unwrap()
        .accessible_action(&AccessibilityAction::Default);
    assert_eq!((sent.get(), ignored.get()), (1, 1));

    // A receipt that would go elsewhere says where.
    receipts.set_address("receipts@example.com".into());
    assert!(
        find(
            ui.window(),
            "Ana Ruiz asked to be notified at receipts@example.com when you read this message."
        )
        .is_some()
    );

    // On a message the user sent: the receipts that came back.
    receipts.set_available(false);
    let row = |kind: &str, name: &str, time: &str| ReturnedReceipt {
        kind: kind.into(),
        name: name.into(),
        time: time.into(),
    };
    receipts.set_returned(slint::ModelRc::new(slint::VecModel::from(vec![
        row("read", "Ana Ruiz", "10:42"),
        row("deleted", "Ben Ortiz", "Yesterday"),
        row("received", "Support team", "Nov 3"),
    ])));
    for label in [
        "Read by Ana Ruiz, 10:42",
        "Deleted by Ben Ortiz, Yesterday",
        "Received by Support team, Nov 3",
    ] {
        assert!(find(ui.window(), label).is_some(), "{label}");
    }
    assert!(find(ui.window(), "Show 3 more receipts").is_none());

    // A longer list holds the rest behind a button.
    receipts.set_more_returned(3);
    let asked = Rc::new(Cell::new(0));
    let count = Rc::clone(&asked);
    receipts.on_show_all_returned(move || count.set(count.get() + 1));
    find(ui.window(), "Show 3 more receipts")
        .unwrap()
        .accessible_action(&AccessibilityAction::Default);
    assert_eq!(asked.get(), 1);
}
