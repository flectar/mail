//! The window's side of new-mail notifications: what it shows, the words in
//! its language, opening conversations and acting on messages. Everything
//! here runs on the window's thread.

use super::{Act, FixedWords, NotificationHost, Request, Response, Route, Words};
use crate::{
    AppWindow, I18n, InboxState, WindowChrome,
    mail_scope::{Scope, StandardMailbox},
};
use flectar_mail_core::models::{ActionKind, MailGrouping};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    collections::BTreeSet,
    rc::{Rc, Weak},
    sync::Arc,
};
use tokio::sync::oneshot;

struct Window {
    app: slint::Weak<AppWindow>,
    state: Weak<RefCell<InboxState>>,
    host: Option<Arc<dyn NotificationHost>>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// Give the dispatcher the window, and handle clicks and buttons on
/// notifications in it from now on.
pub(crate) fn attach(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    host: Option<Arc<dyn NotificationHost>>,
) {
    if let Some(host) = &host {
        host.set_words(&fixed_words(app));
    }
    WINDOW.with(|window| {
        *window.borrow_mut() = Some(Window {
            app: app.as_weak(),
            state: Rc::downgrade(state),
            host,
        });
    });
    let app = app.as_weak();
    let runtime = runtime.clone();
    super::serve_requests(move |request| {
        let runtime = runtime.clone();
        let _ = app.upgrade_in_event_loop(move |app| match request {
            Request::Respond(response) => respond(&app, &runtime, response),
            Request::Raise(activation_token) => {
                crate::window_controller::activate(&app, activation_token);
            }
        });
    });
}

/// Run `f` with the window on its thread, or `None` when there is no
/// window (yet).
async fn with_window<T: Send + 'static>(
    f: impl FnOnce(&AppWindow, &Rc<RefCell<InboxState>>) -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = oneshot::channel();
    slint::invoke_from_event_loop(move || {
        let result = WINDOW.with(|window| {
            let window = window.borrow();
            let window = window.as_ref()?;
            Some(f(&window.app.upgrade()?, &window.state.upgrade()?))
        });
        let _ = tx.send(result);
    })
    .ok()?;
    rx.await.ok().flatten()
}

/// The app's window, asked on its thread.
pub(crate) struct Live;

impl super::Window for Live {
    async fn on_screen(&self, accounts: BTreeSet<i64>) -> Option<BTreeSet<i64>> {
        on_screen(accounts).await
    }

    async fn words(&self, summaries: Vec<(u32, Vec<String>)>) -> Option<Words> {
        words(summaries).await
    }
}

/// Which of these accounts' new mail the user sees arrive: the focused
/// window shows the unified Inbox, or that account's, with no search.
/// Mail never lands in a list the user looks at, so Apple's guidelines
/// and Geary skip the notification there.
async fn on_screen(accounts: BTreeSet<i64>) -> Option<BTreeSet<i64>> {
    with_window(move |app, state| {
        let window = app.window();
        let looking = window.is_visible()
            && !window.is_minimized()
            && app.global::<WindowChrome>().get_active()
            && app.get_active_view() == "mail"
            && !app.get_settings_open();
        let state = state.borrow();
        if !looking || !state.query.trim().is_empty() {
            return BTreeSet::new();
        }
        accounts
            .into_iter()
            .filter(|&account_id| {
                state.scope == Scope::UNIFIED_INBOX
                    || state.scope
                        == Scope::Account {
                            account_id,
                            mailbox: StandardMailbox::Inbox,
                        }
            })
            .collect()
    })
    .await
}

/// The words a batch needs: the fixed ones, and the title and body of
/// each summary, given as its count and the senders it names.
async fn words(summaries: Vec<(u32, Vec<String>)>) -> Option<Words> {
    with_window(move |app, _| {
        let fixed = fixed_words(app);
        let i18n = app.global::<I18n>();
        let summaries = summaries
            .into_iter()
            .map(|(count, senders)| {
                let title = i18n
                    .invoke_notification_new_messages(i32::try_from(count).unwrap_or(i32::MAX))
                    .into();
                let named = senders
                    .iter()
                    .take(super::SUMMARY_SENDERS)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = senders.len().saturating_sub(super::SUMMARY_SENDERS);
                let body = if more == 0 {
                    named
                } else {
                    i18n.invoke_notification_more_senders(
                        named.into(),
                        i32::try_from(more).unwrap_or(i32::MAX),
                    )
                    .into()
                };
                (title, body)
            })
            .collect();
        Words { fixed, summaries }
    })
    .await
}

/// The UI's language changed: relabel what the system keeps registered.
pub(crate) fn language_changed(app: &AppWindow) {
    WINDOW.with(|window| {
        if let Some(host) = window
            .borrow()
            .as_ref()
            .and_then(|window| window.host.as_ref())
        {
            host.set_words(&fixed_words(app));
        }
    });
}

fn fixed_words(app: &AppWindow) -> FixedWords {
    let i18n = app.global::<I18n>();
    FixedWords {
        new_message: i18n.invoke_notification_new_message().into(),
        no_subject: i18n.invoke_notification_no_subject().into(),
        open: i18n.invoke_notification_open().into(),
        mark_read: i18n.invoke_notification_mark_read().into(),
        archive: i18n.invoke_notification_archive().into(),
        delete: i18n.invoke_notification_delete().into(),
    }
}

fn respond(app: &AppWindow, runtime: &tokio::runtime::Handle, response: Response) {
    let Some(state) = WINDOW.with(|window| window.borrow().as_ref()?.state.upgrade()) else {
        return;
    };
    match response.act {
        Act::Open => {
            crate::window_controller::activate(app, response.activation_token);
            reveal(app, &state, response.route);
        }
        act => {
            let Some(core) = state.borrow().core.as_ref().map(|core| core.file_core()) else {
                return;
            };
            let Some(message_id) = response.route.message_id else {
                return;
            };
            let kind = match act {
                Act::MarkRead => ActionKind::MarkRead,
                Act::Archive => ActionKind::Archive,
                Act::Delete | Act::Open => ActionKind::Trash,
            };
            runtime.spawn(async move {
                if let Err(error) = super::act(&core, kind, message_id).await {
                    tracing::warn!(%error, "notifications: the action failed");
                }
            });
        }
    }
}

/// Show the conversation a notification is about, in the list that has
/// it: the one showing, when that is the unified Inbox or the account's,
/// and otherwise the account's Inbox. It opens once its page arrives, the
/// way a restored tab opens its conversation.
fn reveal(app: &AppWindow, state: &Rc<RefCell<InboxState>>, route: Route) {
    app.set_settings_open(false);
    app.set_active_view("mail".into());
    let account_inbox = Scope::Account {
        account_id: route.account_id,
        mailbox: StandardMailbox::Inbox,
    };
    let (scope, row, searching, listed) = {
        let state = state.borrow();
        let row = match state.grouping {
            MailGrouping::Conversations => route.thread_id,
            MailGrouping::Messages => route.message_id,
        }
        .and_then(|id| i32::try_from(id).ok());
        let scope = if state.scope == Scope::UNIFIED_INBOX || state.scope == account_inbox {
            state.scope
        } else {
            account_inbox
        };
        let listed = scope == state.scope
            && row.is_some_and(|row| state.messages.iter().any(|email| email.id == row));
        (scope, row, !state.query.trim().is_empty(), listed)
    };
    if searching {
        app.set_search_query("".into());
        app.invoke_search_changed("".into());
    } else if listed && let Some(row) = row {
        app.invoke_select_email(row);
        return;
    }
    app.invoke_select_scope(scope.to_string().into());
    if let Some(row) = row {
        let mut state = state.borrow_mut();
        state.pending_selection = Some(row);
        state.pending_selection_opens = true;
    }
}
