//! The read receipt bar says who asked and where the receipt would go, and
//! each answer reaches the app.

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
    import { ReadReceipts, ReceiptRequestBar } from "../ui/read-receipts.slint";
    export { ReadReceipts }

    export component ReceiptBarHarness inherits Window {
        width: 640px;
        height: 200px;
        ReceiptRequestBar {
            width: parent.width;
            text-color: black;
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
fn the_bar_names_who_asked_and_passes_each_answer_on() {
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
}
