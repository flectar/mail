//! Exercise removal through the real list and reader projection.
use super::*;
use flectar_mail_core::accounts::credentials::DevelopmentFileCredentialStore;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[derive(Default)]
struct Case {
    name: &'static str,
    old: Vec<i32>,
    refreshed: Vec<i32>,
    opened: Option<i32>,
    expected: Option<i32>,
    dropped: Vec<i32>,
    collapsed: Option<i32>,
    starred_only: bool,
    /// Rows the refresh returns unstarred.
    unstarred: Vec<i32>,
    paginated: bool,
    /// The loaded rows afterwards, for a paginated case.
    listed: Vec<i32>,
    clear_selection: bool,
}

#[test]
fn background_removal_keeps_the_reader_and_visible_selection_in_sync() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let app = AppWindow::new().unwrap();
    app.set_startup_ready(true);
    app.set_startup_hydrated(true);
    app.set_active_view("mail".into());
    app.set_group_mail_by_date(true);
    app.set_connected_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
        id: 1,
        ..Default::default()
    }])));
    app.global::<MotionSettings>().set_enabled(false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let core = runtime
        .block_on(CoreMailSource::start(
            flectar_mail_core::config::Paths::for_tests(dir.path()),
            Arc::new(DevelopmentFileCredentialStore::new(
                dir.path().join("credentials.json"),
            )),
            Arc::new(flectar_mail_core::oauth::redirect::LoopbackRedirectBroker::default()),
        ))
        .unwrap();
    let now = Local::now();
    let make_message = |id| {
        let mut message = super::tests::message(id);
        message.folder = "Inbox".into();
        message.subject = format!("Message {id}");
        message.preview = format!("Body of message {id}");
        message.text = Some(message.preview.clone());
        message.html = Some(format!("<p>{}</p>", message.preview));
        message.body_pending = false;
        message.date_ms = if id <= 2 {
            now.timestamp_millis()
        } else {
            (now - chrono::Duration::days(1)).timestamp_millis()
        };
        message.starred = id != 3;
        message
    };
    let cursor = |id| {
        Some(MailCursor::Thread(ThreadCursor {
            last_message_at: make_message(id).date_ms,
            thread_id: i64::from(id),
        }))
    };
    let cases = [
        Case {
            name: "next message in a collapsed group",
            old: vec![1, 2, 3, 4],
            refreshed: vec![1, 3, 4],
            opened: Some(2),
            expected: Some(3),
            collapsed: Some(3),
            clear_selection: true,
            ..Default::default()
        },
        Case {
            name: "batch removal skips missing neighbors",
            old: vec![1, 2, 3, 4],
            refreshed: vec![1, 4],
            opened: Some(2),
            expected: Some(4),
            dropped: vec![2, 3],
            ..Default::default()
        },
        Case {
            name: "last message falls back into a collapsed group",
            old: vec![1, 2, 3],
            refreshed: vec![1, 2],
            opened: Some(3),
            expected: Some(2),
            collapsed: Some(2),
            ..Default::default()
        },
        Case {
            name: "empty mailbox clears the reader",
            old: vec![1],
            opened: Some(1),
            ..Default::default()
        },
        Case {
            name: "a list replaced by new mail closes the reader",
            old: vec![1, 2],
            refreshed: vec![98, 99],
            opened: Some(2),
            ..Default::default()
        },
        Case {
            name: "new arrivals do not steal the selection",
            old: vec![1, 2],
            refreshed: vec![99, 1],
            opened: Some(2),
            expected: Some(1),
            ..Default::default()
        },
        Case {
            name: "filtered neighbors are skipped",
            old: vec![1, 2, 3, 4],
            refreshed: vec![1, 3, 4],
            opened: Some(2),
            expected: Some(4),
            starred_only: true,
            ..Default::default()
        },
        Case {
            name: "removal from the retained pagination tail",
            old: (1..=50).collect(),
            refreshed: (1..=PAGE_SIZE as i32).collect(),
            opened: Some(30),
            expected: Some(31),
            dropped: vec![30],
            paginated: true,
            listed: (1..=50).filter(|id| *id != 30).collect(),
            ..Default::default()
        },
        Case {
            name: "the open message stays listed when it leaves the quick filter",
            old: vec![1, 2, 3, 4],
            refreshed: vec![1, 2, 3, 4],
            opened: Some(2),
            expected: Some(2),
            starred_only: true,
            unstarred: vec![2],
            ..Default::default()
        },
        Case {
            // The deeper read needs the mail workers this test runs without,
            // so the list and reader stay as they were.
            name: "new mail filling the refresh reads further for the open message",
            old: (1..=PAGE_SIZE as i32).collect(),
            refreshed: (101..=100 + PAGE_SIZE as i32).collect(),
            opened: Some(20),
            expected: Some(20),
            paginated: true,
            listed: (1..=PAGE_SIZE as i32).collect(),
            ..Default::default()
        },
        Case {
            name: "unrelated removal preserves the open message",
            old: vec![1, 2, 3],
            refreshed: vec![1, 2],
            opened: Some(2),
            expected: Some(2),
            ..Default::default()
        },
        Case {
            name: "a closed reader stays closed",
            old: vec![1, 2, 3],
            refreshed: vec![1, 3],
            ..Default::default()
        },
    ];
    for case in cases {
        let (favicons, _) = bounded_ui_channel();
        let (avatars, _) = bounded_ui_channel();
        let mut state = InboxState::empty(
            None,
            UiSender::new(favicons, UiWake::new(app.as_weak(), |_| {})),
            None,
            UiSender::new(avatars, UiWake::new(app.as_weak(), |_| {})),
            false,
            false,
            WarmStartCacheWriter::spawn(&runtime, dir.path().join("warm.json")),
        );
        state.core = Some(core.clone());
        state.using_core = true;
        state.messages = case.old.iter().copied().map(make_message).collect();
        state.selected_id = case.opened;
        state.preview_closed = case.opened.is_none();
        if case.starred_only {
            state.search_filter = "Starred".into();
        }
        if case.paginated {
            state.next_cursor = cursor(50);
        }
        app.set_emails(ModelRc::from(state.email_rows.clone()));
        app.set_mail_list_entries(ModelRc::from(state.mail_list_entries.clone()));
        let state = Rc::new(RefCell::new(state));
        render_current(&app, &state, &runtime).unwrap();
        if let Some(id) = case.collapsed {
            let key = mail_group_key(make_message(id).date_ms, now);
            let mut state_mut = state.borrow_mut();
            let (generation, expanded) = state_mut.mail_groups.toggle(&key);
            assert!(!expanded);
            state_mut.mail_groups.finish_transition(&key, generation);
            drop(state_mut);
            refresh_rows_only(&app, &state, &runtime);
            assert!(
                !state
                    .borrow()
                    .mail_list_entries
                    .iter()
                    .any(|entry| { !entry.is_header && entry.email.id == id && entry.show_row })
            );
        }
        if case.clear_selection {
            // The pane's rendered id remains authoritative during a refresh.
            state.borrow_mut().selected_id = None;
        }
        apply_background_mail_page(
            &app,
            &state,
            &runtime,
            mail::MailPage {
                account_revision: core.account_revision(),
                messages: case
                    .refreshed
                    .iter()
                    .copied()
                    .map(make_message)
                    .map(|mut message| {
                        message.starred &= !case.unstarred.contains(&message.id);
                        message
                    })
                    .collect(),
                labels: Vec::new(),
                mailboxes: Vec::new(),
                next_cursor: case.paginated.then(|| cursor(PAGE_SIZE as i32)).flatten(),
                account_count: 0,
                unified_mailboxes: Vec::new(),
            },
            &case.dropped,
        );
        let state = state.borrow();
        assert_eq!(state.selected_id, case.expected, "{}", case.name);
        assert_eq!(state.rendered_id, case.expected, "{}", case.name);
        assert_eq!(
            app.get_has_selected(),
            case.expected.is_some(),
            "{}",
            case.name
        );
        if let Some(id) = case.expected {
            assert_eq!(
                app.global::<EmailReader>().get_message_id(),
                id,
                "{}",
                case.name
            );
            assert!(
                state.mail_list_entries.iter().any(|entry| {
                    !entry.is_header
                        && entry.email.id == id
                        && entry.email.selected
                        && entry.show_row
                }),
                "selected message must be visible: {}",
                case.name
            );
        } else {
            assert!(
                !app.global::<EmailReader>().get_available(),
                "{}",
                case.name
            );
        }
        if case.paginated {
            assert_eq!(state.next_cursor, cursor(50), "{}", case.name);
            assert_eq!(
                state
                    .messages
                    .iter()
                    .map(|message| message.id)
                    .collect::<Vec<_>>(),
                case.listed,
                "{}",
                case.name
            );
        }
        if case.clear_selection
            && let Some(path) = std::env::var_os("FLECTAR_SELECTION_SCREENSHOT")
        {
            app.window().set_size(slint::PhysicalSize::new(1320, 800));
            app.show().unwrap();
            let mut pixels = vec![slint::Rgb8Pixel::default(); 1320 * 800];
            for _ in 0..3 {
                slint::platform::update_timers_and_animations();
                app.window().request_redraw();
                window.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, 1320);
                });
            }
            let bytes = pixels
                .iter()
                .flat_map(|pixel| [pixel.r, pixel.g, pixel.b])
                .collect();
            image::RgbImage::from_raw(1320, 800, bytes)
                .unwrap()
                .save(path)
                .unwrap();
        }
    }
}
