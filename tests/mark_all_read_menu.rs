//! A sidebar row with unread mail offers "Mark all as read…" in its menu,
//! and choosing it hands on the row's scope and the name it shows.
use i_slint_core::accessibility::AccessibleStringProperty;
use i_slint_core::item_tree::ItemRc;
use i_slint_core::window::WindowInner;
use slint::ComponentHandle;
use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{ops::ControlFlow, rc::Rc};

slint::slint! {
    import { SidebarItem } from "../ui/components/sidebar-controls.slint";

    export component MarkAllHarness inherits Window {
        width: 300px;
        height: 300px;
        in property <bool> markable: true;
        in-out property <string> asked-scope;
        in-out property <string> asked-name;
        SidebarItem {
            y: 0px;
            width: parent.width;
            label: "Inbox";
            scope: "Unified Inbox";
            count: "14";
            can-mark-all-read: root.markable;
            mark-all-read(scope, name) => {
                root.asked-scope = scope;
                root.asked-name = name;
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

/// Right-click the row, as a mouse opens its menu.
fn open_menu(window: &slint::Window) {
    let position = slint::LogicalPosition::new(60.0, 12.0);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Right,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Right,
    });
    slint::platform::update_timers_and_animations();
}

/// Click the middle of `item`. Menu items in Slint 1.18 take clicks; they
/// have no default accessibility action.
fn click(window: &slint::Window, item: &ItemRc) {
    let geometry = item.geometry();
    let middle = item.map_to_window(geometry.origin) + geometry.size.to_vector() / 2.0;
    let position = slint::LogicalPosition::new(middle.x, middle.y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    slint::platform::update_timers_and_animations();
}

#[test]
fn a_row_with_unread_mail_offers_to_mark_it_all_read() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let ui = MarkAllHarness::new().unwrap();
    ui.show().unwrap();
    open_menu(ui.window());
    let item = find(ui.window(), "Mark all as read…").expect("the row's menu offers it");
    click(ui.window(), &item);
    assert_eq!(ui.get_asked_scope(), "Unified Inbox");
    assert_eq!(ui.get_asked_name(), "Inbox");

    // A row without unread mail has nothing to mark, and here no menu.
    ui.set_asked_scope("".into());
    ui.set_markable(false);
    open_menu(ui.window());
    assert!(find(ui.window(), "Mark all as read…").is_none());
    assert!(
        WindowInner::from_pub(ui.window())
            .active_popups()
            .is_empty()
    );
}
