//! A shown tab scrolls the real mail list back to where it was left, on a
//! deterministic clock and without a display server.
use slint::platform::{
    Platform, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

slint::slint! {
    import { EmailRow, MailListEntry } from "../ui/models.slint";
    import { MailListPane } from "../ui/views/mail-list-pane.slint";
    import { MotionSettings } from "../ui/design-system.slint";
    export { TabsUi } from "../ui/components/workspace-navigation.slint";

    export component TabScrollHarness inherits Window {
        preferred-width: 400px;
        preferred-height: 480px;
        in-out property <[MailListEntry]> entries: [];
        in-out property <string> scope: "Unified Inbox";
        out property <length> offset: -pane.scroll-y;
        init => { MotionSettings.enabled = false; }

        pane := MailListPane {
            width: parent.width;
            height: parent.height;
            list_entries: root.entries;
            selected_scope: root.scope;
            surface: #18191a;
            heading: white;
            body-text: white;
            secondary-text: #cccccc;
            muted-text: #999999;
            border-subtle: #303030;
            accent: #5ca3ff;
        }
    }
}

struct Clock {
    window: Rc<MinimalSoftwareWindow>,
    now: Rc<Cell<Duration>>,
}

impl Platform for Clock {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.now.get()
    }
}

fn entry(id: i32) -> MailListEntry {
    MailListEntry {
        show_row: true,
        email_index: id - 1,
        email: EmailRow {
            id,
            sender: format!("Sender {id}").into(),
            subject: format!("Subject {id}").into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Grow or shrink the list in place, as the app reconciles its rows.
fn resize(entries: &VecModel<MailListEntry>, count: i32) {
    let count = count as usize;
    while entries.row_count() > count {
        entries.remove(entries.row_count() - 1);
    }
    while entries.row_count() < count {
        entries.push(entry(entries.row_count() as i32 + 1));
    }
}

#[test]
fn shown_tabs_scroll_the_mail_list_back() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    let now = Rc::new(Cell::new(Duration::ZERO));
    slint::platform::set_platform(Box::new(Clock {
        window: window.clone(),
        now: now.clone(),
    }))
    .unwrap();
    let ui = TabScrollHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(400, 480));
    ui.show().unwrap();
    let tabs = ui.global::<TabsUi>();
    let advance = |by: Duration| {
        let mut pixels = vec![slint::Rgb8Pixel::default(); 400 * 480];
        for _ in 0..4 {
            now.set(now.get() + by / 4);
            slint::platform::update_timers_and_animations();
            window.draw_if_needed(|renderer| {
                renderer.render(&mut pixels, 400);
            });
        }
    };
    let entries = Rc::new(VecModel::<MailListEntry>::default());
    ui.set_entries(ModelRc::from(entries.clone()));
    // A restore belongs to the mailbox it was saved for; the list has no
    // search and shows all mail.
    let restore_for = |scope: &str, offset: f32| {
        tabs.set_list_restore(offset);
        tabs.set_list_restore_view(format!("{scope}\n\nAll mail").into());
        tabs.set_list_restore_request(tabs.get_list_restore_request() + 1);
    };
    let restore = |offset: f32| restore_for(ui.get_scope().as_str(), offset);

    // Message rows are 96px, so 50 of them reach 4800px.
    resize(&entries, 50);
    advance(Duration::from_millis(8));
    restore(1500.0);
    advance(Duration::from_millis(8));
    assert_eq!(ui.get_offset(), 1500.0);

    // Showing a tab changes the mailbox, which scrolls the list to the top;
    // the requested position still wins.
    ui.set_scope("Folder:2".into());
    restore(700.0);
    advance(Duration::from_millis(8));
    assert_eq!(ui.get_offset(), 700.0);

    // Going to another mailbox drops a position meant for the last one.
    restore(1200.0);
    ui.set_scope("Folder:4".into());
    advance(Duration::from_millis(8));
    assert_eq!(ui.get_offset(), 0.0);
    assert!(tabs.get_list_restore() < 0.0);
    ui.set_scope("Folder:2".into());
    advance(Duration::from_millis(8));
    assert_eq!(ui.get_offset(), 0.0, "and coming back does not revive it");

    // Once the list settles, ordinary navigation starts at the top again.
    advance(Duration::from_secs(1));
    assert!(tabs.get_list_restore() < 0.0);
    ui.set_scope("Folder:3".into());
    advance(Duration::from_millis(8));
    assert_eq!(ui.get_offset(), 0.0);

    // A position past the loaded rows is reached as more rows arrive.
    resize(&entries, 20);
    advance(Duration::from_millis(8));
    restore(3000.0);
    advance(Duration::from_millis(8));
    assert!(ui.get_offset() < 3000.0);
    resize(&entries, 50);
    advance(Duration::from_millis(8));
    assert_eq!(ui.get_offset(), 3000.0);

    // Scrolling by hand ends the restore: rows that arrive later leave the
    // list where the user put it.
    advance(Duration::from_secs(1));
    resize(&entries, 20);
    advance(Duration::from_millis(8));
    restore(3000.0);
    advance(Duration::from_millis(8));
    ui.window().dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(200.0, 240.0),
        delta_x: 0.0,
        delta_y: 200.0,
    });
    advance(Duration::from_millis(8));
    let scrolled = ui.get_offset();
    assert!(tabs.get_list_restore() < 0.0);
    resize(&entries, 50);
    advance(Duration::from_millis(8));
    assert_eq!(ui.get_offset(), scrolled);
}
