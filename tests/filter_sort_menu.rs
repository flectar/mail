//! The filter menu of a list that sorts by date offers Newest first and
//! Oldest first after its filters, checks the filter and the order in use,
//! and keeps the order while a search shows results newest first.
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
    import { WorkspaceSearchToolbar } from "../ui/components/workspace-layout.slint";

    export component SortHarness inherits Window {
        width: 420px;
        height: 420px;
        in property <bool> oldest;
        in property <bool> searching;
        in-out property <string> sorted;
        WorkspaceSearchToolbar {
            x: 10px;
            y: 10px;
            width: 400px;
            filters: [
                { title: "All mail", value: "All mail" },
                { title: "Unread", value: "Unread" },
            ];
            selected-filter: "All mail";
            show-sort: true;
            sort-enabled: !root.searching;
            oldest-first: root.oldest;
            filter-tooltip: "Filter and sort mail";
            sort-selected(oldest) => {
                root.sorted = oldest ? "oldest" : "newest";
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

/// The roots of the window and of its open popups.
fn roots(window: &slint::Window) -> Vec<ItemRc> {
    let inner = WindowInner::from_pub(window);
    let mut roots = vec![ItemRc::new_root(inner.component())];
    roots.extend(
        inner
            .active_popups()
            .iter()
            .map(|popup| ItemRc::new_root(popup.component.clone())),
    );
    roots
}

/// The shown elements labelled `label` in the window and its open popups.
fn find_all(window: &slint::Window, label: &str) -> Vec<ItemRc> {
    let mut found = Vec::new();
    for root in roots(window) {
        let _ = root.visit_descendants(|item| {
            if item.is_visible()
                && item
                    .accessible_string_property(AccessibleStringProperty::Label)
                    .as_deref()
                    == Some(label)
            {
                found.push(item.clone());
            }
            ControlFlow::<()>::Continue(())
        });
    }
    found
}

fn find(window: &slint::Window, label: &str) -> Option<ItemRc> {
    find_all(window, label).into_iter().next()
}

/// The window's vertical span of `item`.
fn span(item: &ItemRc) -> (f32, f32) {
    let geometry = item.geometry();
    let top = item.map_to_window(geometry.origin).y;
    (top, top + geometry.size.height)
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

fn open_menu(window: &slint::Window) {
    let button = find(window, "Filter and sort mail").expect("the toolbar has a filter button");
    click(window, &button);
}

/// The items of the open menu that show a check mark, top to bottom.
fn checked(window: &slint::Window) -> Vec<&'static str> {
    let marks = find_all(window, "✓")
        .iter()
        .map(|mark| {
            let (top, bottom) = span(mark);
            (top + bottom) / 2.0
        })
        .collect::<Vec<_>>();
    ["All mail", "Unread", "Newest first", "Oldest first"]
        .into_iter()
        .filter(|label| {
            let item = find(window, label).expect("the menu lists every item");
            let (top, bottom) = span(&item);
            marks.iter().any(|middle| (top..bottom).contains(middle))
        })
        .collect()
}

#[test]
fn the_filter_menu_checks_the_order_and_turns_the_list_around() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let ui = SortHarness::new().unwrap();
    ui.show().unwrap();

    open_menu(ui.window());
    assert_eq!(checked(ui.window()), ["All mail", "Newest first"]);
    let oldest = find(ui.window(), "Oldest first").expect("the menu offers the order");
    click(ui.window(), &oldest);
    assert_eq!(ui.get_sorted(), "oldest");

    ui.set_oldest(true);
    ui.set_sorted("".into());
    open_menu(ui.window());
    assert_eq!(checked(ui.window()), ["All mail", "Oldest first"]);
    let newest = find(ui.window(), "Newest first").unwrap();
    click(ui.window(), &newest);
    assert_eq!(ui.get_sorted(), "newest");

    // Search results are newest first whatever the list's order, so the
    // items show that and can't be chosen.
    ui.set_oldest(false);
    ui.set_searching(true);
    ui.set_sorted("".into());
    open_menu(ui.window());
    assert_eq!(checked(ui.window()), ["All mail", "Newest first"]);
    let oldest = find(ui.window(), "Oldest first").unwrap();
    click(ui.window(), &oldest);
    assert_eq!(ui.get_sorted(), "");
}
