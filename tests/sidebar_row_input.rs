//! Taps, right-clicks and long presses on sidebar rows, which also have menus.
use i_slint_core::input::TouchPhase;
use i_slint_core::lengths::LogicalPoint;
use i_slint_core::platform::InternalEvent;
use slint::ComponentHandle;
use slint::platform::{
    Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::Cell, rc::Rc, time::Duration};

slint::slint! {
    import { SidebarItem, SidebarSectionHeader } from "../ui/components/sidebar-controls.slint";

    export component SidebarRowHarness inherits Window {
        width: 260px;
        height: 300px;
        in-out property <string> selected-scope;
        in-out property <int> folder-toggles;
        in-out property <int> new-subfolders;
        in-out property <int> account-toggles;
        SidebarItem {
            y: 0px;
            width: parent.width;
            label: "Projects";
            scope: "Folder:5";
            account-id: 1;
            folder-id: 5;
            has-children: true;
            folder-menu-enabled: true;
            can-create-child: true;
            clicked(scope) => { root.selected-scope = scope; }
            toggled(id, open) => { root.folder-toggles += 1; }
            new-subfolder(account, folder, name) => { root.new-subfolders += 1; }
        }
        SidebarSectionHeader {
            y: 40px;
            width: parent.width;
            label: "Flectar";
            detail: "alex@flectar.com";
            toggled(open) => { root.account-toggles += 1; }
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

fn click(window: &slint::Window, x: f32, y: f32, button: PointerEventButton) {
    let position = slint::LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button });
    window.dispatch_event(WindowEvent::PointerReleased { position, button });
}

fn touch(window: &slint::Window, x: f32, y: f32, phase: TouchPhase) {
    window.dispatch_event(WindowEvent::internal(InternalEvent::Touch {
        id: 0,
        position: LogicalPoint::new(x, y),
        phase,
    }));
}

/// Runs the open menu's first item, if a menu is open.
fn run_first_menu_item(window: &slint::Window) {
    for key in [Key::DownArrow, Key::Return] {
        window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    }
}

// Slint's Android build turns a press on a menu's own area into a wait for a
// long press, so rows keep their touch area inside it: taps reach the row on
// every platform, and the row opens the menu itself.
#[test]
fn rows_take_taps_and_open_their_menus_on_right_click_or_hold() {
    let clock = Rc::new(Cell::new(Duration::ZERO));
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
        clock: clock.clone(),
    }))
    .unwrap();
    let ui = SidebarRowHarness::new().unwrap();
    ui.show().unwrap();
    let advance = |ms: u64| {
        clock.set(clock.get() + Duration::from_millis(ms));
        slint::platform::update_timers_and_animations();
    };

    click(ui.window(), 10.0, 14.0, PointerEventButton::Left);
    assert_eq!(
        ui.get_folder_toggles(),
        1,
        "the disclosure folds the folder"
    );
    click(ui.window(), 150.0, 14.0, PointerEventButton::Left);
    assert_eq!(ui.get_selected_scope(), "Folder:5", "a click selects it");
    click(ui.window(), 150.0, 60.0, PointerEventButton::Left);
    assert_eq!(ui.get_account_toggles(), 1, "a click folds the account");

    ui.set_selected_scope("".into());
    touch(ui.window(), 150.0, 14.0, TouchPhase::Started);
    advance(100);
    touch(ui.window(), 150.0, 14.0, TouchPhase::Ended);
    advance(600);
    assert_eq!(ui.get_selected_scope(), "Folder:5", "a tap selects it");
    run_first_menu_item(ui.window());
    assert_eq!(ui.get_new_subfolders(), 0, "a tap opens no menu");

    // Holding a finger still for half a second opens the menu, as on Android
    // and iOS, and lifting it afterwards is not a tap.
    ui.set_selected_scope("".into());
    touch(ui.window(), 150.0, 14.0, TouchPhase::Started);
    advance(600);
    touch(ui.window(), 150.0, 14.0, TouchPhase::Ended);
    assert_eq!(ui.get_selected_scope(), "");
    run_first_menu_item(ui.window());
    assert_eq!(ui.get_new_subfolders(), 1, "the hold opened the menu");

    // A finger that moves is scrolling the sidebar.
    touch(ui.window(), 150.0, 14.0, TouchPhase::Started);
    advance(100);
    touch(ui.window(), 150.0, 30.0, TouchPhase::Moved);
    advance(600);
    touch(ui.window(), 150.0, 30.0, TouchPhase::Ended);
    run_first_menu_item(ui.window());
    assert_eq!(ui.get_new_subfolders(), 1, "scrolling opens no menu");
}
