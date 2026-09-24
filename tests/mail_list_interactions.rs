use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
};
use slint::{ModelRc, VecModel};
use std::{cell::Cell, rc::Rc};

slint::slint! {
    import { EmailRow, MailListEntry } from "../ui/models.slint";
    import { MailListPane } from "../ui/views/mail-list-pane.slint";

    export component MailListHarness inherits Window {
        preferred-width: 397px;
        preferred-height: 480px;
        in-out property <[EmailRow]> emails: [];
        in-out property <[MailListEntry]> entries: [];
        in-out property <bool> more: false;
        callback group-toggled();
        callback page-requested();

        MailListPane {
            width: parent.width;
            height: parent.height;
            compact: true;
            emails: root.emails;
            list_entries: root.entries;
            can_load_more: root.more;
            surface: #18191a;
            heading: white;
            body-text: white;
            secondary-text: #cccccc;
            muted-text: #999999;
            border-subtle: #303030;
            accent: #5ca3ff;
            toggle_mail_group(key) => { root.group-toggled(); }
            load_more => { root.page-requested(); }
        }
    }
}

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn toggling_a_date_group_keeps_infinite_pagination_active() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).unwrap();
    let ui = MailListHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(397, 480));
    ui.set_emails(ModelRc::new(VecModel::from(vec![EmailRow {
        id: 1,
        sender: "Sender".into(),
        subject: "Subject".into(),
        ..Default::default()
    }])));
    ui.set_entries(ModelRc::new(VecModel::from(vec![
        MailListEntry {
            is_header: true,
            group_key: "this-week".into(),
            group_kind: "this-week".into(),
            group_count: 1,
            expanded: true,
            show_row: true,
            ..Default::default()
        },
        MailListEntry {
            show_row: true,
            email_index: 0,
            email: EmailRow {
                id: 1,
                sender: "Sender".into(),
                subject: "Subject".into(),
                ..Default::default()
            },
            ..Default::default()
        },
    ])));
    let toggles = Rc::new(Cell::new(0));
    let pages = Rc::new(Cell::new(0));
    let count = toggles.clone();
    ui.on_group_toggled(move || count.set(count.get() + 1));
    let count = pages.clone();
    ui.on_page_requested(move || count.set(count.get() + 1));
    ui.show().unwrap();

    let position = slint::LogicalPosition::new(80.0, 82.0);
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    assert_eq!(
        toggles.get(),
        1,
        "the date group should respond to the click"
    );

    ui.set_more(true);
    for _ in 0..3 {
        slint::platform::update_timers_and_animations();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        pages.get(),
        1,
        "pagination should still load after a group click"
    );
}
