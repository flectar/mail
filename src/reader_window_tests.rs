//! Conversations in windows of their own (#254): one window for each
//! conversation, which shows it, acts on it alone, and closes once the
//! conversation leaves.
use super::*;
use flectar_mail_core::accounts::credentials::DevelopmentFileCredentialStore;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};

/// Each window, the main one and every conversation's, has its own.
struct Headless;
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

/// Conversations 1 and 2 in one account's inbox, a message each.
fn seed_inbox(runtime: &tokio::runtime::Runtime, core: &CoreMailSource) {
    runtime
        .block_on(core.file_core().db.write(move |conn| {
            conn.execute(
                "INSERT INTO accounts (id,email,provider,auth_kind,username,incoming_host,incoming_port,smtp_host,smtp_port,created_at,mail_protocol,settings_json)
                 VALUES (1,'fixture@example.test','generic','password','fixture','imap.example.test',993,'smtp.example.test',465,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
                [],
            )?;
            conn.execute(
                "INSERT INTO folders (id,account_id,path,role) VALUES (1,1,'INBOX','inbox')",
                [],
            )?;
            for (id, subject) in [(1, "Quarterly plan"), (2, "Lunch on Friday")] {
                conn.execute(
                    &format!(
                        "INSERT INTO threads (id,account_id,subject_norm,last_message_at,message_count)
                         VALUES ({id},1,'{subject}',{id},1)"
                    ),
                    [],
                )?;
                conn.execute(
                    &format!(
                        "INSERT INTO messages (id,account_id,thread_id,folder_id,uid,subject,from_addr,date,is_read,body_state)
                         VALUES ({id},1,{id},1,{id},'{subject}','sender@example.test',{id},1,'cached')"
                    ),
                    [],
                )?;
            }
            Ok(())
        }))
        .unwrap();
}

/// Let the page worker answer until no load is left.
fn settle(app: &AppWindow, runtime: &tokio::runtime::Runtime, state: &Rc<RefCell<InboxState>>) {
    for _ in 0..400 {
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(5)).await });
        app.invoke_drain_navigation_updates();
        if !mail_work::loading(&state.borrow()) {
            return;
        }
    }
    panic!("the list never finished loading");
}

/// The window of conversation window `id`.
fn window(id: u64) -> ReaderWindow {
    match reader_window::host(id).and_then(|host| host.upgrade()) {
        Some(ReaderRef::Window { window, .. }) => window,
        _ => panic!("window {id} is not open"),
    }
}

#[test]
fn conversations_open_in_windows_of_their_own_and_close_once_they_leave() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let app = AppWindow::new().unwrap();
    app.set_startup_ready(true);
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
    seed_inbox(&runtime, &core);
    let (favicons, _) = bounded_ui_channel();
    let (avatars, _) = bounded_ui_channel();
    let mut state = InboxState::empty(
        None,
        UiSender::new(favicons, UiWake::new(app.as_weak(), |_| {})),
        None,
        UiSender::new(avatars, UiWake::new(app.as_weak(), |_| {})),
        false,
        false,
        WarmStartCacheWriter::spawn(runtime.handle(), dir.path().join("warm.json")),
    );
    state.core = Some(core.clone());
    state.using_core = true;
    state.scope = Scope::UNIFIED_INBOX;
    let state = Rc::new(RefCell::new(state));
    mail_work::register(&app, &state, runtime.handle());
    mail_work::refresh(&app, &state, runtime.handle(), false, &[]).unwrap();
    settle(&app, &runtime, &state);
    let rows = state.borrow().messages.clone();
    assert_eq!(rows.len(), 2);

    reader_window::install(
        &app,
        &state,
        runtime.handle(),
        documents::default_provider(),
        ExternalUrlOpener::new(|_| Ok(())),
    );
    let windows = app.global::<ReaderWindows>();
    assert!(windows.get_available());

    // A conversation opens in a window that shows it, and opening it again
    // brings that window forward rather than opening another.
    windows.invoke_open(rows[0].id);
    assert_eq!(reader_window::open_windows(), 1);
    let first = window(1);
    assert_eq!(
        first.global::<ReaderUi>().get_selected_subject(),
        rows[0].subject.as_str()
    );
    assert!(!first.global::<ReaderWindows>().get_available());
    windows.invoke_open(rows[0].id);
    assert_eq!(reader_window::open_windows(), 1);

    // Another conversation has a window of its own, and each keeps its own
    // conversation.
    windows.invoke_open(rows[1].id);
    assert_eq!(reader_window::open_windows(), 2);
    let second = window(2);
    assert_eq!(
        second.global::<ReaderUi>().get_selected_subject(),
        rows[1].subject.as_str()
    );
    assert_eq!(reader_window::row(1).map(|row| row.id), Some(rows[0].id));
    assert_eq!(reader_window::row(2).map(|row| row.id), Some(rows[1].id));

    // Archiving its conversation closes the window, and acts on that
    // conversation alone.
    first.invoke_message_action("archive".into());
    assert_eq!(reader_window::open_windows(), 1);
    assert!(reader_window::host(1).is_none());
    assert!(mail_work::actions_pending(&state.borrow()));

    // The window's own close button closes it too.
    second.invoke_request_close();
    assert_eq!(reader_window::open_windows(), 0);
}
