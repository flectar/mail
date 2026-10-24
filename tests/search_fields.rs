//! Search boxes and multi-line fields show their current value: a query or
//! text reset or filled in after the user typed reaches the field.

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
    import { WorkspaceSearchToolbar } from "../ui/components/workspace-layout.slint";
    import { TextEdit } from "../ui/components/controls.slint";

    export component SearchFieldsHarness inherits Window {
        preferred-width: 900px;
        preferred-height: 400px;
        in-out property <string> query;
        in-out property <string> searched;
        in-out property <string> notes;

        VerticalLayout {
            WorkspaceSearchToolbar {
                height: 56px;
                text <=> root.query;
                placeholder-text: "Search mail";
                edited(value) => { root.searched = value; }
            }
            TextEdit {
                height: 120px;
                text <=> root.notes;
                placeholder-text: "Notes about this contact";
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

/// The shown text input whose placeholder reads `placeholder`. The
/// placeholder is its own element, just ahead of the input in the tree.
fn field(ui: &SearchFieldsHarness, placeholder: &str) -> ItemRc {
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
fn type_into(ui: &SearchFieldsHarness, placeholder: &str, text: &str) {
    field(ui, placeholder).accessible_action(&AccessibilityAction::SetValue(text.into()));
}

fn shows(ui: &SearchFieldsHarness, placeholder: &str) -> String {
    field(ui, placeholder)
        .accessible_string_property(AccessibleStringProperty::Value)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn fields_follow_values_set_after_typing() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let ui = SearchFieldsHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(900, 400));
    ui.show().unwrap();

    type_into(&ui, "Search mail", "invoice");
    assert_eq!(ui.get_query(), "invoice");
    assert_eq!(ui.get_searched(), "invoice");
    // Switching folder or account clears the query, and a saved search
    // fills it in.
    ui.set_query("".into());
    assert_eq!(shows(&ui, "Search mail"), "");
    ui.set_query("from:alice".into());
    assert_eq!(shows(&ui, "Search mail"), "from:alice");

    type_into(&ui, "Notes about this contact", "Met at the conference");
    assert_eq!(ui.get_notes(), "Met at the conference");
    // Opening another contact shows its notes.
    ui.set_notes("Prefers email".into());
    assert_eq!(shows(&ui, "Notes about this contact"), "Prefers email");
}
