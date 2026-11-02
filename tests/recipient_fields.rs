//! A long recipient list stays reachable: the field scrolls instead of
//! clipping, so every address can be read before the message is sent.

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
    import { RecipientRow } from "../ui/components/composer-controls.slint";

    export component RecipientHarness inherits Window {
        width: 420px;
        height: 120px;
        in-out property <string> to;

        RecipientRow {
            x: 0px;
            y: 0px;
            width: 420px;
            label: "To";
            placeholder-text: "name@example.com";
            text <=> root.to;
        }
    }
}

struct Headless;

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

/// The shown text input whose placeholder reads `placeholder`. The
/// placeholder is its own element, just ahead of the input in the tree.
fn field(ui: &RecipientHarness, placeholder: &str) -> ItemRc {
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
fn type_into(ui: &RecipientHarness, placeholder: &str, text: &str) {
    field(ui, placeholder).accessible_action(&AccessibilityAction::SetValue(text.into()));
}

fn shows(ui: &RecipientHarness, placeholder: &str) -> String {
    field(ui, placeholder)
        .accessible_string_property(AccessibleStringProperty::Value)
        .unwrap_or_default()
        .to_string()
}

/// The input's own width. A field that scrolls is wider than the viewport it
/// shows through; a clipped field can never exceed it.
fn input_width(ui: &RecipientHarness) -> f32 {
    field(ui, "name@example.com").geometry().width()
}

#[test]
fn a_long_recipient_list_widens_the_input_instead_of_clipping_it() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let ui = RecipientHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(420, 120));
    ui.show().unwrap();

    type_into(&ui, "name@example.com", "alice@example.com");
    let fitted = input_width(&ui);

    let long = "alice@example.com, bob@example.com, carol@example.com, dave@example.com, erin@example.com, frank@example.com";
    type_into(&ui, "name@example.com", long);

    // No address is lost from the value the composer sends.
    assert_eq!(shows(&ui, "name@example.com"), long);
    // The input grows past its viewport, so the caret can travel the text.
    assert!(
        input_width(&ui) > fitted,
        "a long list must widen the input beyond its viewport: {} is not greater than {fitted}",
        input_width(&ui)
    );
}
