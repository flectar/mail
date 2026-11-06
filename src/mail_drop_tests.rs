//! Drag-and-drop destinations resolved from typed sidebar scopes.
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

fn mailbox(scope: Scope, folder_id: i64, label: &str) -> MailboxEntry {
    MailboxEntry {
        account_id: 1,
        folder_id,
        parent_folder_id: -1,
        depth: 0,
        has_children: false,
        is_standard: matches!(scope, Scope::Account { .. }),
        is_selectable: true,
        can_create_children: false,
        can_rename: false,
        can_delete: false,
        label: label.into(),
        scope: Some(scope),
        context: "Person".into(),
        detail: String::new(),
        avatar: String::new(),
        is_account: false,
        count: 0,
    }
}

#[test]
fn drops_resolve_by_scope_not_by_mailbox_name() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).unwrap();
    let app = AppWindow::new().unwrap();
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
    state.core = Some(core);
    state.using_core = true;
    let account = |mailbox| Scope::Account {
        account_id: 1,
        mailbox,
    };
    state.mailboxes = vec![
        mailbox(account(StandardMailbox::Inbox), 10, "Inbox"),
        mailbox(account(StandardMailbox::Trash), 11, "Trash"),
        mailbox(account(StandardMailbox::Sent), 13, "Sent"),
        // A custom folder that merely shares the Trash mailbox's name.
        mailbox(Scope::Folder(12), 12, "Trash"),
    ];
    let mut message = mail::fixture_messages().remove(0);
    message.account_id = 1;
    message.thread_id = Some(77);
    message.folder = "Inbox".into();
    message.starred = false;
    state.messages = vec![message.clone()];
    state.set_scope(account(StandardMailbox::Inbox));
    let item = MailDragItem {
        message_id: message.id,
        account_id: 1,
    };
    let drop = |scope, account_id, folder_id| {
        resolve_single_mail_drop(&state, &item, scope, account_id, folder_id)
            .map(|(_, destination)| destination)
    };

    assert_eq!(
        drop(Scope::Folder(12), 1, 12),
        Ok(MailDropDestination::Folder(12))
    );
    assert_eq!(
        drop(account(StandardMailbox::Trash), 1, 11),
        Ok(MailDropDestination::Action("trash"))
    );
    assert_eq!(
        drop(Scope::Unified(StandardMailbox::Trash), -1, -1),
        Ok(MailDropDestination::Action("trash"))
    );
    assert_eq!(
        drop(Scope::Unified(StandardMailbox::Starred), -1, -1),
        Ok(MailDropDestination::Action("star"))
    );
    assert!(drop(account(StandardMailbox::Sent), 1, 13).is_err());
    assert!(drop(Scope::Unified(StandardMailbox::Sent), -1, -1).is_err());
    assert!(drop(account(StandardMailbox::Inbox), 1, 10).is_err());
    assert!(drop(Scope::Folder(12), 2, 12).is_err());
    assert!(
        drop(
            Scope::Label {
                kind: mail_scope::LabelKind::Global,
                id: 99,
            },
            -1,
            -1,
        )
        .is_err()
    );

    // Shown in the custom folder, the mail can still go to the Trash
    // mailbox of the same name.
    state.set_scope(Scope::Folder(12));
    state.messages[0].folder = "Trash".into();
    let drop = |scope, account_id, folder_id| {
        resolve_single_mail_drop(&state, &item, scope, account_id, folder_id)
            .map(|(_, destination)| destination)
    };
    assert_eq!(
        drop(account(StandardMailbox::Trash), 1, 11),
        Ok(MailDropDestination::Action("trash"))
    );
    assert!(drop(Scope::Folder(12), 1, 12).is_err());
}
