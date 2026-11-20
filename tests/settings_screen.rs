//! Settings is a screen of its own, like Mail or Calendar: it replaces the
//! screen it is opened from, which takes no input while Settings shows, and
//! closing Settings shows that screen again.

use flectar_mail::{
    AccountRow, AppWindow, EmailRow, MailListEntry, MailListNavigation, MailNavigationTarget,
    MotionSettings, TabRow, TabsUi,
};
use i_slint_core::accessibility::{AccessibilityAction, AccessibleStringProperty};
use i_slint_core::item_tree::ItemRc;
use i_slint_core::items::AccessibleRole;
use i_slint_core::window::WindowInner;
use slint::platform::{
    Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, LogicalPosition, ModelRc, Rgb8Pixel, VecModel};
use std::{
    cell::{Cell, RefCell},
    ops::ControlFlow,
    rc::Rc,
};

// The production resolver the list's keys find their row with.
#[path = "../src/mail_navigation.rs"]
mod mail_navigation;

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

/// Shows the window as a frame, which also creates the list rows in view.
fn show(window: &Rc<MinimalSoftwareWindow>, app: &AppWindow) -> impl Fn() {
    app.show().unwrap();
    let (window, size) = (window.clone(), app.window().size());
    let draw = move || {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            let width = size.width as usize;
            let mut pixels = vec![Rgb8Pixel::default(); width * size.height as usize];
            renderer.render(&mut pixels, width);
        });
    };
    draw();
    draw
}

/// The first element whose accessible label starts with `label`.
fn find(app: &AppWindow, label: &str) -> Option<ItemRc> {
    let inner = WindowInner::from_pub(app.window());
    ItemRc::new_root(inner.component()).visit_descendants(|item| {
        if item
            .accessible_string_property(AccessibleStringProperty::Label)
            .is_some_and(|text| text.starts_with(label))
        {
            ControlFlow::Break(item.clone())
        } else {
            ControlFlow::Continue(())
        }
    })
}

fn labelled(app: &AppWindow, label: &str) -> ItemRc {
    find(app, label).unwrap_or_else(|| panic!("no element labelled {label:?}"))
}

/// The shown product rail button named `label`.
fn rail(app: &AppWindow, label: &str) -> ItemRc {
    let inner = WindowInner::from_pub(app.window());
    ItemRc::new_root(inner.component())
        .visit_descendants(|item| {
            if item.is_visible()
                && item.accessible_role() == AccessibleRole::Button
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
        .unwrap_or_else(|| panic!("no rail button {label:?}"))
}

fn checked(item: &ItemRc) -> bool {
    item.accessible_string_property(AccessibleStringProperty::Checked)
        .as_deref()
        == Some("true")
}

fn centre(item: &ItemRc) -> LogicalPosition {
    let geometry = item.geometry();
    let origin = item.map_to_window(geometry.origin);
    LogicalPosition::new(
        origin.x + geometry.width() / 2.0,
        origin.y + geometry.height() / 2.0,
    )
}

fn click(app: &AppWindow, position: LogicalPosition) {
    let window = app.window();
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

fn drag(app: &AppWindow, from: LogicalPosition, by: f32) {
    let window = app.window();
    let to = LogicalPosition::new(from.x + by, from.y);
    window.dispatch_event(WindowEvent::PointerMoved { position: from });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: from,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerMoved { position: to });
    window.dispatch_event(WindowEvent::PointerReleased {
        position: to,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerExited);
}

fn key(app: &AppWindow, key: impl Into<slint::SharedString>) {
    let text = key.into();
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}

fn account() -> ModelRc<AccountRow> {
    ModelRc::new(VecModel::from(vec![AccountRow {
        id: 1,
        email: "person@example.com".into(),
        ..Default::default()
    }]))
}

fn email(id: i32, sender: &str, subject: &str) -> EmailRow {
    EmailRow {
        id,
        account_id: 1,
        sender: sender.into(),
        subject: subject.into(),
        ..Default::default()
    }
}

/// A window with one account and two messages, at `width` by `height`.
fn window(width: u32, height: u32) -> (AppWindow, Rc<MinimalSoftwareWindow>) {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let app = AppWindow::new().unwrap();
    app.global::<MotionSettings>().set_enabled(false);
    app.global::<MailListNavigation>()
        .on_target(mail_navigation::target);
    app.window()
        .set_size(slint::PhysicalSize::new(width, height));
    app.set_startup_ready(true);
    app.set_startup_hydrated(true);
    app.set_connected_accounts(account());
    let emails = vec![
        email(1, "First sender", "First message"),
        email(2, "Second sender", "Second message"),
    ];
    let entries = emails
        .iter()
        .enumerate()
        .map(|(index, email)| MailListEntry {
            show_row: true,
            email_index: index as i32,
            email: email.clone(),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    app.set_emails(ModelRc::new(VecModel::from(emails)));
    app.set_mail_list_entries(ModelRc::new(VecModel::from(entries)));
    app.set_active_view("mail".into());
    (app, window)
}

#[test]
fn settings_replaces_the_screen_it_opens_from() {
    let (app, window) = window(1320, 800);
    let draw = show(&window, &app);
    let acted = Rc::new(RefCell::new(Vec::<(i32, String)>::new()));
    let log = acted.clone();
    app.on_message_action_for_email(move |id, action| {
        log.borrow_mut().push((id, action.to_string()));
    });
    let take = || std::mem::take(&mut *acted.borrow_mut());

    // Opening a row gives the list the keyboard, and Delete trashes the row.
    labelled(&app, "First sender, First message").accessible_action(&AccessibilityAction::Default);
    key(&app, Key::Delete);
    assert_eq!(take(), [(1, "trash".to_owned())]);
    let divider = labelled(&app, "Message list width");
    let divider_at = centre(&divider);
    let geometry = divider.geometry();
    let top = divider.map_to_window(geometry.origin).y;
    let width = app.get_workspace_list_pane_width();

    // The rail's Settings opens it with the list focused. Mail is gone, not
    // drawn over: none of it is left to take keys, clicks or a screen
    // reader's attention.
    click(&app, centre(&rail(&app, "Settings")));
    draw();
    assert_eq!(app.get_active_view(), "settings");
    assert_eq!(app.get_settings_return_view(), "mail");
    assert!(checked(&rail(&app, "Settings")));
    assert!(!checked(&rail(&app, "Mail")));
    assert!(find(&app, "Message list width").is_none());
    assert!(find(&app, "First sender, First message").is_none());
    for shortcut in [
        Key::Delete.into(),
        slint::SharedString::from("a"),
        "j".into(),
    ] {
        key(&app, shortcut);
    }
    assert!(take().is_empty(), "Settings passed keys to mail");
    let mut y = top + 4.0;
    while y < top + geometry.height() {
        drag(&app, LogicalPosition::new(divider_at.x, y), 60.0);
        assert_eq!(
            app.get_workspace_list_pane_width(),
            width,
            "a divider under Settings moved at y = {y}"
        );
        y += 8.0;
    }
    assert!(take().is_empty(), "Settings passed clicks to mail");

    // The rail's Settings again goes back to mail, which takes input again.
    click(&app, centre(&rail(&app, "Settings")));
    draw();
    assert_eq!(app.get_active_view(), "mail");
    assert!(checked(&rail(&app, "Mail")));
    drag(&app, centre(&labelled(&app, "Message list width")), 60.0);
    assert!(app.get_workspace_list_pane_width() > width);

    // Settings opened from the calendar goes back to the calendar, and
    // another rail button leaves Settings for its own screen.
    app.set_active_view("calendar".into());
    app.invoke_open_settings();
    app.invoke_close_settings();
    assert_eq!(app.get_active_view(), "calendar");
    app.invoke_open_settings();
    draw();
    click(&app, centre(&rail(&app, "Contacts")));
    assert_eq!(app.get_active_view(), "contacts");
    app.hide().unwrap();
}

/// The calendar's event editor shows over the calendar only. A tab showing
/// Settings meanwhile keeps the editor for when the calendar shows again.
#[test]
fn the_event_editor_stays_with_the_calendar() {
    let (app, window) = window(1320, 800);
    app.set_active_view("calendar".into());
    app.set_calendar_editor_open(true);
    let draw = show(&window, &app);
    assert!(app.get_calendar_editor_shown());
    assert!(find(&app, "New calendar event").is_some());

    app.invoke_open_settings();
    draw();
    assert!(!app.get_calendar_editor_shown());
    assert!(find(&app, "New calendar event").is_none());
    assert!(app.get_calendar_editor_open(), "the editor keeps its event");

    app.invoke_close_settings();
    draw();
    assert!(app.get_calendar_editor_shown());
    assert!(find(&app, "New calendar event").is_some());
    app.hide().unwrap();
}

/// On phones Settings has the tab switcher, and the overview it opens shows
/// over Settings. Back closes Settings.
#[test]
fn phones_show_the_tab_overview_over_settings() {
    let (app, window) = window(390, 844);
    let tabs = app.global::<TabsUi>();
    tabs.set_rows(ModelRc::new(VecModel::from(vec![
        TabRow {
            id: 1,
            workspace: "mail".into(),
            title: "Mail tab".into(),
            active: false,
            ..Default::default()
        },
        TabRow {
            id: 2,
            workspace: "settings".into(),
            active: true,
            ..Default::default()
        },
    ])));
    let selected = Rc::new(Cell::new(-1));
    let chosen = selected.clone();
    tabs.on_select(move |id| chosen.set(id));
    app.invoke_open_settings();
    let draw = show(&window, &app);
    assert!(tabs.get_enabled() && tabs.get_compact());

    tabs.set_overview_open(true);
    draw();
    click(&app, centre(&labelled(&app, "Mail tab")));
    assert_eq!(selected.get(), 1, "the overview's card took the click");
    tabs.set_overview_open(false);
    draw();

    assert!(app.invoke_system_back_requested());
    assert_eq!(app.get_active_view(), "mail");
    app.hide().unwrap();
}
