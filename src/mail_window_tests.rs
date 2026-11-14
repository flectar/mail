//! A list shown oldest first, opened at a message further down than one read
//! goes: new mail a notification opens in a long inbox.
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

/// Conversations 1 to `count` in one account's inbox, each a message from
/// that time, so the oldest is 1.
fn seed_inbox(runtime: &tokio::runtime::Runtime, core: &CoreMailSource, count: i64) {
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
            for id in 1..=count {
                conn.execute(
                    "INSERT INTO threads (id,account_id,subject_norm,last_message_at,message_count)
                     VALUES (?1,1,'window',?1,1)",
                    [id],
                )?;
                conn.execute(
                    "INSERT INTO messages (id,account_id,thread_id,folder_id,uid,subject,from_addr,date,is_read,body_state)
                     VALUES (?1,1,?1,1,?1,'Window','sender@example.test',?1,1,'cached')",
                    [id],
                )?;
            }
            Ok(())
        }))
        .unwrap();
}

/// Let the page worker answer, and hand each page to the list as its wake-up
/// would, until no load is left.
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

fn row_ids(state: &Rc<RefCell<InboxState>>) -> Vec<i32> {
    state.borrow().messages.iter().map(|row| row.id).collect()
}

#[test]
fn new_mail_opens_a_long_oldest_first_list_where_it_is() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).unwrap();
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
    seed_inbox(&runtime, &core, 300);
    assert!(core.set_order(Scope::UNIFIED_INBOX, ListOrder::OldestFirst));
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

    // A notification opens conversation 300, the newest, which neither the
    // first page nor one read from the list's start reaches.
    {
        let mut state = state.borrow_mut();
        state.pending_selection = Some(300);
        state.pending_selection_opens = true;
    }
    mail_work::refresh(&app, &state, runtime.handle(), false, &[]).unwrap();
    settle(&app, &runtime, &state);
    assert_eq!(row_ids(&state), [300]);
    assert_eq!(state.borrow().selected_id, Some(300));
    assert!(state.borrow().window_start.is_some());
    assert_eq!(state.borrow().server_only_note(), 0);

    // A refresh, such as for more new mail, reads from where the rows start.
    mail_work::refresh(&app, &state, runtime.handle(), true, &[]).unwrap();
    settle(&app, &runtime, &state);
    assert_eq!(row_ids(&state), [300]);
    assert_eq!(state.borrow().selected_id, Some(300));

    // The rows before come on request, until the list's start.
    let load_earlier = || {
        let first = mail::cursor_of(&state.borrow().messages[0]);
        let page = runtime
            .block_on(core.load_earlier(Scope::UNIFIED_INBOX, first, 150))
            .unwrap();
        mail_view_model::prepend_mail_page(&app, &state, runtime.handle(), page);
    };
    load_earlier();
    assert_eq!(row_ids(&state), (150..=300).collect::<Vec<_>>());
    assert!(state.borrow().window_start.is_some());
    load_earlier();
    assert_eq!(row_ids(&state), (1..=300).collect::<Vec<_>>());
    assert!(state.borrow().window_start.is_none());

    // Mail that is gone by the time its notification is opened leaves an
    // empty reader, after one read from the start and one try at the row.
    {
        let mut state = state.borrow_mut();
        state.pending_selection = Some(999);
        state.pending_selection_opens = true;
    }
    mail_work::refresh(&app, &state, runtime.handle(), false, &[]).unwrap();
    settle(&app, &runtime, &state);
    assert_eq!(state.borrow().selected_id, None);
    assert!(state.borrow().preview_closed);
    assert!(state.borrow().window_start.is_none());
    assert_eq!(row_ids(&state)[0], 1);
}
