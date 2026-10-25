use i_slint_core::accessibility::AccessibleStringProperty;
use i_slint_core::item_tree::ItemRc;
use i_slint_core::items::AccessibleRole;
use i_slint_core::window::WindowInner;
use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    ops::ControlFlow,
    rc::Rc,
    time::Duration,
};

slint::slint! {
    import { EmailRow, ListAttachment, MailLabelRow, MailListEntry } from "../ui/models.slint";
    export { MailAttachments } from "../ui/models.slint";
    import { MailListPane } from "../ui/views/mail-list-pane.slint";
    import { MotionSettings } from "../ui/design-system.slint";

    export component DesktopListHarness inherits Window {
        preferred-width: 397px;
        preferred-height: 260px;
        in-out property <[EmailRow]> emails: [];
        in-out property <[MailListEntry]> entries: [];
        callback selected(int);
        init => { MotionSettings.enabled = false; }

        MailListPane {
            width: parent.width;
            height: parent.height;
            emails: root.emails;
            list_entries: root.entries;
            surface: #18191a;
            heading: white;
            body-text: white;
            secondary-text: #cccccc;
            muted-text: #999999;
            border: #3a3b3c;
            border-subtle: #303030;
            accent: #5ca3ff;
            select_email(id) => { root.selected(id); }
        }
    }

    export component MailListHarness inherits Window {
        preferred-width: 397px;
        preferred-height: 480px;
        in-out property <[EmailRow]> emails: [];
        in-out property <[MailListEntry]> entries: [];
        in-out property <bool> more: false;
        in-out property <bool> paging: false;
        in-out property <int> revision: 0;
        callback group-toggled();
        callback page-requested();
        init => { MotionSettings.enabled = false; }

        MailListPane {
            width: parent.width;
            height: parent.height;
            compact: true;
            emails: root.emails;
            list_entries: root.entries;
            can_load_more: root.more;
            loading_more: root.paging;
            content_revision: root.revision;
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
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
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

    // Finish collapsing all loaded mail, then commit multiple pages whose
    // messages belong to the same collapsed section. No scroll gesture or
    // change in visible height should be needed to reach the older mail.
    ui.set_more(false);
    ui.set_entries(ModelRc::new(VecModel::from(vec![MailListEntry {
        is_header: true,
        group_key: "this-week".into(),
        group_kind: "this-week".into(),
        group_count: 1,
        expanded: false,
        ..Default::default()
    }])));
    let settle = || {
        let mut pixels = vec![slint::Rgb8Pixel::default(); 397 * 480];
        for _ in 0..8 {
            slint::platform::update_timers_and_animations();
            window.draw_if_needed(|renderer| {
                renderer.render(&mut pixels, 397);
            });
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    };
    settle();
    for revision in 1..=3 {
        ui.set_paging(true);
        ui.set_emails(ModelRc::new(VecModel::from(
            (1..=revision + 1)
                .map(|id| EmailRow {
                    id,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        ui.set_revision(revision);
        ui.set_paging(false);
        ui.set_more(true);
        settle();
        assert_eq!(
            pages.get(),
            revision + 1,
            "pages hidden in a collapsed section must not stall pagination"
        );
    }
    ui.set_more(false);
    ui.set_revision(4);
    settle();
    assert_eq!(pages.get(), 4, "stop when the mailbox is exhausted");
}

thread_local! {
    // The platform clock, advanced by `settle`.
    static CLOCK: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

/// A headless desktop that keeps what is copied.
struct Desktop(Rc<MinimalSoftwareWindow>, Rc<RefCell<String>>);

impl Platform for Desktop {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn duration_since_start(&self) -> Duration {
        CLOCK.get()
    }
    fn set_clipboard_text(&self, text: &str, _: slint::platform::Clipboard) {
        *self.1.borrow_mut() = text.to_owned();
    }
}

/// A desktop list showing one row, and what the row copies, opens and asks
/// of its files.
struct DesktopList {
    ui: DesktopListHarness,
    window: Rc<MinimalSoftwareWindow>,
    clipboard: Rc<RefCell<String>>,
    opened: Rc<Cell<i32>>,
    file_commands: Rc<RefCell<Vec<(String, String)>>>,
}

fn desktop_list(email: EmailRow) -> DesktopList {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    let clipboard = Rc::new(RefCell::new(String::new()));
    slint::platform::set_platform(Box::new(Desktop(window.clone(), clipboard.clone()))).unwrap();
    let ui = DesktopListHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(397, 260));
    ui.set_emails(ModelRc::new(VecModel::from(vec![email.clone()])));
    ui.set_entries(ModelRc::new(VecModel::from(vec![MailListEntry {
        show_row: true,
        email_index: 0,
        email,
        ..Default::default()
    }])));
    let opened = Rc::new(Cell::new(-1));
    let target = opened.clone();
    ui.on_selected(move |id| target.set(id));
    let file_commands = Rc::new(RefCell::new(Vec::new()));
    let log = file_commands.clone();
    ui.global::<MailAttachments>()
        .on_command(move |command, id| {
            log.borrow_mut().push((command.to_string(), id.to_string()));
        });
    ui.show().unwrap();
    settle(&window, Duration::ZERO);
    DesktopList {
        ui,
        window,
        clipboard,
        opened,
        file_commands,
    }
}

fn settle(window: &MinimalSoftwareWindow, advance: Duration) {
    CLOCK.set(CLOCK.get() + advance);
    let mut pixels = vec![slint::Rgb8Pixel::default(); 397 * 260];
    for _ in 0..3 {
        slint::platform::update_timers_and_animations();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 397);
        });
    }
}

fn label(id: i32, name: &str) -> MailLabelRow {
    MailLabelRow {
        id,
        name: name.into(),
        color: slint::Color::from_rgb_u8(0x4a, 0x86, 0xe8),
        applied: true,
        ..Default::default()
    }
}

/// The shown element with this accessible label, of `role` if given.
fn labelled(window: &slint::Window, label: &str, role: Option<AccessibleRole>) -> Option<ItemRc> {
    let inner = WindowInner::from_pub(window);
    ItemRc::new_root(inner.component()).visit_descendants(|item| {
        if item.is_visible()
            && role.is_none_or(|role| item.accessible_role() == role)
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

/// Left edge, top, width and height in the window.
fn bounds(item: &ItemRc) -> (f32, f32, f32, f32) {
    let geometry = item.geometry();
    let origin = item.map_to_window(geometry.origin);
    (origin.x, origin.y, geometry.width(), geometry.height())
}

fn click(window: &slint::Window, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
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

#[test]
fn verification_code_chip_copies_instead_of_opening_the_message() {
    let DesktopList {
        ui,
        window,
        clipboard,
        opened,
        ..
    } = desktop_list(EmailRow {
        id: 7,
        sender: "GitHub".into(),
        subject: "Please verify your device".into(),
        preview: "Verification code: 482913".into(),
        labels: ModelRc::new(VecModel::from(vec![label(1, "Security"), label(2, "Work")])),
        verification_code: "482913".into(),
        verification_code_spelled: "4 8 2 9 1 3".into(),
        ..Default::default()
    });
    let copy = "Copy verification code 4 8 2 9 1 3";
    let copied = "Copied verification code 4 8 2 9 1 3";
    let chip = labelled(ui.window(), copy, Some(AccessibleRole::Button)).expect("the code chip");
    let (x, y, width, height) = bounds(&chip);

    // The code leads the detail line; one label still fits beside it.
    let security = labelled(ui.window(), "Security", None).expect("the first label");
    assert!(bounds(&security).0 > x + width, "labels follow the code");
    assert!(labelled(ui.window(), "Work", None).is_none());
    assert!(labelled(ui.window(), "+1", None).is_some());

    click(ui.window(), x + width / 2.0, y + height / 2.0);
    settle(&window, Duration::ZERO);
    assert_eq!(*clipboard.borrow(), "482913");
    assert_eq!(
        opened.get(),
        -1,
        "copying the code must not open the message"
    );
    assert!(labelled(ui.window(), copied, Some(AccessibleRole::Button)).is_some());
    assert!(
        labelled(ui.window(), copied, Some(AccessibleRole::Text)).is_some(),
        "screen readers hear about the copy"
    );

    // The confirmation lasts a moment.
    settle(&window, Duration::from_secs(2));
    assert!(labelled(ui.window(), copy, Some(AccessibleRole::Button)).is_some());
    assert!(labelled(ui.window(), copied, None).is_none());

    // The rest of the row still opens the message.
    click(ui.window(), x + width / 2.0, y - 24.0);
    assert_eq!(opened.get(), 7);
}

#[test]
fn labels_past_the_limit_take_no_room() {
    let DesktopList { ui, .. } = desktop_list(EmailRow {
        id: 1,
        sender: "Cloudflare".into(),
        subject: "Your weekly traffic report".into(),
        preview: "Requests were up 18% across flectar.com".into(),
        labels: ModelRc::new(VecModel::from(vec![
            label(1, "Reports"),
            label(2, "Marketing"),
            label(3, "Travel"),
        ])),
        ..Default::default()
    });
    let (marketing_x, _, marketing_width, _) =
        bounds(&labelled(ui.window(), "Marketing", None).expect("the second label"));
    let (more_x, ..) = bounds(&labelled(ui.window(), "+1", None).expect("the label count"));
    assert!(labelled(ui.window(), "Travel", None).is_none());
    // The count follows the second chip's end padding and one gap; a hidden
    // third chip would push it further right.
    let gap = more_x - (marketing_x + marketing_width);
    assert!(gap < 10.0, "hidden labels took {gap}px");
}

#[test]
fn labels_without_room_for_their_names_are_counted() {
    let DesktopList { ui, window, .. } = desktop_list(EmailRow {
        id: 1,
        sender: "Cloudflare".into(),
        subject: "Your weekly traffic report".into(),
        preview: "Requests were up 18% across flectar.com".into(),
        labels: ModelRc::new(VecModel::from(vec![
            label(1, "Reports"),
            label(2, "Marketing"),
            label(3, "Travel"),
        ])),
        ..Default::default()
    });

    // A narrower list keeps one label readable and counts the rest,
    ui.window().set_size(slint::PhysicalSize::new(320, 260));
    settle(&window, Duration::ZERO);
    let reports = labelled(ui.window(), "Reports", None).expect("the first label");
    let width = bounds(&reports).2;
    assert!(width >= 28.0, "the label was cut to {width}px");
    assert!(labelled(ui.window(), "Marketing", None).is_none());
    assert!(labelled(ui.window(), "+2", None).is_some());

    // and a narrower one only counts them.
    ui.window().set_size(slint::PhysicalSize::new(260, 260));
    settle(&window, Duration::ZERO);
    assert!(labelled(ui.window(), "Reports", None).is_none());
    assert!(labelled(ui.window(), "+3", None).is_some());
}

#[test]
fn a_preview_too_narrow_for_a_word_is_left_out() {
    let preview = "Requests were up 18% across flectar.com";
    let row = |labels: Vec<MailLabelRow>| EmailRow {
        id: 1,
        sender: "Cloudflare".into(),
        subject: "Your weekly traffic report".into(),
        preview: preview.into(),
        labels: ModelRc::new(VecModel::from(labels)),
        ..Default::default()
    };
    let DesktopList { ui, window, .. } = desktop_list(row(vec![label(1, "VIP")]));
    assert!(labelled(ui.window(), preview, None).is_some());

    // A second label leaves the preview a sliver, which would show only "…".
    let email = row(vec![label(1, "Quarterly reports"), label(2, "VIP")]);
    ui.set_emails(ModelRc::new(VecModel::from(vec![email.clone()])));
    ui.set_entries(ModelRc::new(VecModel::from(vec![MailListEntry {
        show_row: true,
        email_index: 0,
        email,
        ..Default::default()
    }])));
    settle(&window, Duration::ZERO);
    assert!(labelled(ui.window(), "VIP", None).is_some());
    assert!(labelled(ui.window(), preview, None).is_none());
}

/// A desktop list showing a receipt that carries `attachment`.
fn receipt_list(
    attachment: ListAttachment,
    attachment_total: i32,
    labels: Vec<MailLabelRow>,
) -> DesktopList {
    desktop_list(EmailRow {
        id: 9,
        sender: "Stripe".into(),
        subject: "Your August receipt is ready".into(),
        preview: "Payment received for your monthly plan".into(),
        labels: ModelRc::new(VecModel::from(labels)),
        attachment,
        attachment_total,
        ..Default::default()
    })
}

fn receipt(previewable: bool) -> ListAttachment {
    ListAttachment {
        id: "101".into(),
        stem: "Receipt-0818".into(),
        extension: ".pdf".into(),
        kind: "pdf".into(),
        previewable,
    }
}

#[test]
fn file_chip_previews_instead_of_opening_the_message() {
    let DesktopList {
        ui,
        window,
        opened,
        file_commands,
        ..
    } = receipt_list(receipt(true), 2, Vec::new());
    let chip = labelled(
        ui.window(),
        "Preview Receipt-0818.pdf, 1 more file",
        Some(AccessibleRole::Button),
    )
    .expect("the file chip");
    let (x, y, width, height) = bounds(&chip);

    click(ui.window(), x + width / 2.0, y + height / 2.0);
    settle(&window, Duration::ZERO);
    assert_eq!(
        *file_commands.borrow(),
        [("preview-listed".to_owned(), "101".to_owned())]
    );
    assert_eq!(
        opened.get(),
        -1,
        "previewing the file must not open the message"
    );

    // The rest of the row still opens the message.
    click(ui.window(), x + width / 2.0, y - 24.0);
    assert_eq!(opened.get(), 9);
}

#[test]
fn file_chip_downloads_files_without_a_preview() {
    let DesktopList {
        ui,
        window,
        opened,
        file_commands,
        ..
    } = receipt_list(receipt(false), 1, Vec::new());
    let chip = labelled(
        ui.window(),
        "Download Receipt-0818.pdf",
        Some(AccessibleRole::Button),
    )
    .expect("the file chip");
    let (x, y, width, height) = bounds(&chip);

    click(ui.window(), x + width / 2.0, y + height / 2.0);
    settle(&window, Duration::ZERO);
    assert_eq!(
        *file_commands.borrow(),
        [("download-listed".to_owned(), "101".to_owned())]
    );
    assert_eq!(opened.get(), -1);
}

fn two_labels() -> Vec<MailLabelRow> {
    vec![label(1, "Work"), label(2, "Marketing")]
}

#[test]
fn a_label_shows_beside_a_short_file_name() {
    let short = ListAttachment {
        stem: "a".into(),
        ..receipt(true)
    };
    let DesktopList { ui, .. } = receipt_list(short, 1, two_labels());
    let file = labelled(ui.window(), "Preview a.pdf", None).expect("the file chip");
    let work = labelled(ui.window(), "Work", None).expect("the first label");
    let (file_x, _, file_width, _) = bounds(&file);
    assert!(
        bounds(&work).0 > file_x + file_width,
        "labels follow the file"
    );
    assert!(labelled(ui.window(), "Marketing", None).is_none());
    assert!(labelled(ui.window(), "+1", None).is_some());
}

#[test]
fn labels_beside_a_long_file_name_are_only_counted() {
    let long = ListAttachment {
        stem: "Traffic report for every flectar.com zone, week 43".into(),
        ..receipt(true)
    };
    let DesktopList { ui, .. } = receipt_list(long, 1, two_labels());
    // The file keeps its room rather than sharing it with a label cut down
    // to its icon.
    let file = labelled(
        ui.window(),
        "Preview Traffic report for every flectar.com zone, week 43.pdf",
        None,
    )
    .expect("the file chip");
    assert!(labelled(ui.window(), "Work", None).is_none());
    assert!(labelled(ui.window(), "+2", None).is_some());
    let width = bounds(&file).2;
    assert!(width >= 149.0, "the file chip gave up its room: {width}px");
}
