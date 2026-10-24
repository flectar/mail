//! Account-removal transitions against the real Slint models.
use super::*;
use crate::mail_scope::LabelKind;
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

fn account(id: i64) -> Account {
    Account {
        id,
        email: format!("account-{id}@example.test"),
        display_name: Some(format!("Account {id}")),
        avatar_url: None,
        provider: Provider::Generic,
        auth_kind: flectar_mail_core::models::AuthKind::Password,
        mail_protocol: MailProtocol::Imap,
        sync_state: "idle".into(),
        sync_error: None,
        can_create_top_level_mailbox: true,
        server_keeps_mail_days: None,
    }
}
fn config(account: &Account) -> AccountConfig {
    AccountConfig {
        id: account.id,
        email: account.email.clone(),
        display_name: account.display_name.clone(),
        avatar_url: None,
        provider: account.provider,
        auth_kind: account.auth_kind,
        mail_protocol: account.mail_protocol,
        username: account.email.clone(),
        jmap_url: String::new(),
        jmap_account_id: None,
        incoming_host: "imap.example.test".into(),
        incoming_port: 993,
        smtp_host: "smtp.example.test".into(),
        smtp_port: 465,
        settings: flectar_mail_core::models::AccountSettings::new(MailConnectionSettings::new(
            ConnectionSecurity::Tls,
            ConnectionSecurity::Tls,
        )),
    }
}

#[test]
fn account_removal_clears_deleted_models_and_preserves_surviving_selection() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).unwrap();
    let app = AppWindow::new().unwrap();
    app.set_startup_ready(true);
    let runtime = Rc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let account_inbox = Scope::Account {
        account_id: 1,
        mailbox: StandardMailbox::Inbox,
    };
    let mut initial_scopes = vec![
        account_inbox,
        Scope::Important,
        Scope::Label {
            kind: LabelKind::Global,
            id: 42,
        },
    ];
    initial_scopes.extend(StandardMailbox::ALL.map(Scope::Unified));
    for (removed, initial_scope) in [2, 1].into_iter().flat_map(|removed| {
        initial_scopes
            .iter()
            .map(move |scope| (removed, *scope))
    }) {
        let (fav, _) = bounded_ui_channel();
        let (avatars, _) = bounded_ui_channel();
        let mut state = InboxState::empty(
            None,
            UiSender::new(fav, UiWake::new(app.as_weak(), |_| {})),
            None,
            UiSender::new(avatars, UiWake::new(app.as_weak(), |_| {})),
            false,
            false,
            WarmStartCacheWriter::spawn(&runtime, dir.path().join(format!("warm-{removed}.json"))),
        );
        state.using_core = true;
        state.connected_accounts = vec![account(1), account(2)];
        state.account_configs = state.connected_accounts.iter().map(config).collect();
        state.messages = fixture_messages().into_iter().take(2).collect();
        for (i, message) in state.messages.iter_mut().enumerate() {
            message.account_id = i as i64 + 1;
            message.account = format!("Account {}", i + 1);
            message.folder = "Inbox".into();
        }
        state.mailboxes = fixture_mailboxes(&state.messages);
        state.scope = initial_scope;
        state.selected_id = Some(state.messages[0].id);
        state.rendered_id = state.selected_id;
        state.preview_closed = false;
        state.checked_ids = state.messages.iter().map(|message| message.id).collect();
        state.next_cursor = Some(MailCursor::Thread(ThreadCursor {
            last_message_at: 1,
            thread_id: 1,
        }));
        state.page = 3;
        let first_id = state.messages[0].id;
        let state = Rc::new(RefCell::new(state));
        mail_work::register(&app, &state, &runtime);
        refresh_connected_accounts(&app, &state);
        assert_eq!(app.get_connected_accounts().row_count(), 2);
        let old_generation = mail_work::generation(&state.borrow());
        reconcile_removed_account(&mut state.borrow_mut(), removed);
        {
            let state = state.borrow();
            assert!(
                state
                    .messages
                    .iter()
                    .all(|message| message.account_id != removed)
            );
            assert!(
                state
                    .mailboxes
                    .iter()
                    .all(|mailbox| mailbox.account_id != removed)
            );
            assert!(
                state
                    .account_configs
                    .iter()
                    .all(|account| account.id != removed)
            );
            assert!(state.checked_ids.is_empty());
            assert!(state.next_cursor.is_none());
            assert_eq!(state.page, 1);
            assert!(!mail_work::accepts_background(&state, old_generation));
            // The survivor's own mailbox replaces both a unified selection
            // and one owned by the removed account.
            let expected_scope = match initial_scope {
                Scope::Unified(mailbox) => Scope::Account {
                    account_id: 3 - removed,
                    mailbox,
                },
                scope if scope == account_inbox && removed == 1 => Scope::Account {
                    account_id: 2,
                    mailbox: StandardMailbox::Inbox,
                },
                scope => scope,
            };
            assert_eq!(state.scope, expected_scope);
            if removed == 2 {
                assert_eq!(state.selected_id, Some(first_id));
            } else {
                assert_eq!(state.selected_id, None);
                assert!(state.preview_closed);
            }
        }
        refresh_connected_accounts(&app, &state);
        assert_eq!(app.get_connected_accounts().row_count(), 1);
        assert_eq!(app.get_compose_account_id(), (3 - removed) as i32);
        refresh_sidebar(&state);
        assert!(!state.borrow().sidebar_rows.iter().any(|row| matches!(
            row.kind,
            SidebarRowKind::UnifiedHeading
                | SidebarRowKind::UnifiedInbox
                | SidebarRowKind::UnifiedMailbox
        )));
        // Removing the final account must empty both the Rust state and the
        // models that drive onboarding, compose and the mail workspace.
        reconcile_removed_account(&mut state.borrow_mut(), 3 - removed);
        refresh_connected_accounts(&app, &state);
        render_current(&app, &state, &runtime).unwrap();
        assert_eq!(app.get_connected_accounts().row_count(), 0);
        assert_eq!(app.get_compose_account_id(), -1);
        assert_eq!(state.borrow().email_rows.row_count(), 0);
        assert!(state.borrow().mailboxes.is_empty());
        assert!(state.borrow().unified_mailboxes.is_empty());
        assert!(state.borrow().selected_id.is_none());
        assert!(state.borrow().rendered_id.is_none());
        assert!(!app.global::<EmailReader>().get_available());
    }
}
