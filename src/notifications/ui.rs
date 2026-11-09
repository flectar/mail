//! The window's side of new-mail notifications: what it shows, the words in
//! its language, and opening conversations. Everything here runs on the
//! window's thread.

use super::{Act, Availability, FixedWords, NotificationHost, Request, Response, Route, Words};
use crate::{
    AppWindow, I18n, InboxState, SystemNotifications, WindowChrome,
    mail_scope::{Scope, StandardMailbox},
};
use flectar_mail_core::models::MailGrouping;
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
    runtime: tokio::runtime::Handle,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
    /// Clicks that came before the mail store opened, such as the one that
    /// started the app: [`core_ready`] answers them.
    static WAITING: RefCell<Vec<Response>> = const { RefCell::new(Vec::new()) };
}

/// Give the dispatcher the window, and handle clicks and buttons on
/// notifications in it from now on.
pub(crate) fn attach(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    host: Option<Arc<dyn NotificationHost>>,
) {
    let system = app.global::<SystemNotifications>();
    match &host {
        // No notifications here: Settings leaves them out.
        None => system.set_availability("".into()),
        Some(host) => {
            host.set_words(&fixed_words(app));
            let controls = host.controls();
            system.set_settings_openable(controls.settings);
            system.set_sound_fixed(!controls.sound);
            system.set_badge_supported(controls.badge);
            // Until the system answers.
            system.set_availability("allowed".into());
            let host = Arc::clone(host);
            system.on_open_settings(move || host.open_settings());
            #[cfg(background_checks)]
            system.on_open_app_settings(crate::platform::open_app_settings);
        }
    }
    WINDOW.with(|window| {
        *window.borrow_mut() = Some(Window {
            app: app.as_weak(),
            state: Rc::downgrade(state),
            host,
            runtime: runtime.clone(),
        });
    });
    refresh_availability();
    let app = app.as_weak();
    super::serve_requests(move |request| {
        let _ = app.upgrade_in_event_loop(move |app| match request {
            Request::Respond(response) => respond(&app, response),
            Request::Raise(activation_token) => {
                crate::window_controller::activate(&app, activation_token);
            }
            Request::Quit => crate::window_controller::quit(&app),
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

    async fn badge_description(&self, count: u32) -> Option<String> {
        with_window(move |app, _| {
            app.global::<I18n>()
                .invoke_notification_unread_count(i32::try_from(count).unwrap_or(i32::MAX))
                .to_string()
        })
        .await
    }
}

/// Which of these accounts' new mail the user sees arrive: the focused
/// window shows that account's Inbox, or the unified Inbox with the account
/// in it (a mail profile can leave it out), with no search.
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
        // A phone's window stays focused while the app is in the background.
        #[cfg(background_checks)]
        let looking = looking && crate::platform::app_is_active();
        let state = state.borrow();
        if !looking || !state.query.trim().is_empty() {
            return BTreeSet::new();
        }
        accounts
            .into_iter()
            .filter(|&account_id| {
                let inbox = Scope::Account {
                    account_id,
                    mailbox: StandardMailbox::Inbox,
                };
                state.scope == inbox
                    || (state.scope == Scope::UNIFIED_INBOX && state.shows_account(account_id))
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

/// Show in Settings whether the system lets the app's notifications through.
/// Asked again whenever the user may have changed it: when the window gets
/// the focus back, as after the system settings, and when the notification
/// settings change.
pub(crate) fn refresh_availability() {
    let Some((app, host, runtime)) = WINDOW.with(|window| {
        let window = window.borrow();
        let window = window.as_ref()?;
        Some((
            window.app.clone(),
            window.host.clone()?,
            window.runtime.clone(),
        ))
    }) else {
        return;
    };
    #[cfg(background_checks)]
    if let Some(app) = app.upgrade() {
        use crate::platform::BackgroundRefresh;
        let checks = match crate::platform::background_refresh() {
            BackgroundRefresh::On => "on",
            BackgroundRefresh::Off => "off",
            BackgroundRefresh::LowPower => "low-power",
        };
        app.global::<SystemNotifications>()
            .set_background_checks(checks.into());
    }
    runtime.spawn(async move {
        let availability = match host.availability().await {
            Availability::Allowed => "allowed",
            Availability::NotAsked => "not-asked",
            Availability::Denied => "denied",
            Availability::Unavailable => "unavailable",
            Availability::Refused => "refused",
        };
        let _ = app.upgrade_in_event_loop(move |app| {
            app.global::<SystemNotifications>()
                .set_availability(availability.into());
        });
    });
}

/// The app came to the front on a phone. The user may have changed the
/// system's settings meanwhile, and with an account set up it is the moment
/// to ask for permission.
#[cfg(background_checks)]
pub(crate) fn came_to_front() {
    refresh_availability();
    if let Some(app) = WINDOW.with(|window| window.borrow().as_ref()?.app.upgrade()) {
        ask_for_permission(&app);
    }
}

/// On a phone, ask the user to allow notifications while the app is in
/// front with an account to tell of, rather than once mail arrives: a check
/// in the background can't ask. Called as the app comes to the front and
/// when the accounts change.
#[cfg(background_checks)]
pub(crate) fn ask_for_permission(app: &AppWindow) {
    use slint::Model;
    if !app.get_notifications_enabled()
        || app.get_connected_accounts().row_count() == 0
        || !crate::platform::app_is_active()
    {
        return;
    }
    let Some((host, runtime)) = WINDOW.with(|window| {
        let window = window.borrow();
        let window = window.as_ref()?;
        Some((window.host.clone()?, window.runtime.clone()))
    }) else {
        return;
    };
    runtime.spawn(async move {
        if host.availability().await == Availability::NotAsked {
            host.request_permission().await;
            let _ = slint::invoke_from_event_loop(refresh_availability);
        }
    });
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
        new_mail_channel: i18n.invoke_notification_new_mail_channel().into(),
        quiet_mail_channel: i18n.invoke_notification_quiet_mail_channel().into(),
    }
}

/// A click on a notification: bring the window up and open what it is
/// about. Buttons don't come here; their actions need only the mail store.
fn respond(app: &AppWindow, response: Response) {
    let Some(state) = WINDOW.with(|window| window.borrow().as_ref()?.state.upgrade()) else {
        return;
    };
    let Response {
        route,
        act,
        activation_token,
    } = response;
    if act != Act::Open {
        return;
    }
    crate::window_controller::activate(app, activation_token);
    if state.borrow().core.is_none() {
        // Opening it needs the mail store, which a click that started the
        // app comes before.
        WAITING.with(|waiting| {
            waiting.borrow_mut().push(Response {
                route,
                act,
                activation_token: None,
            });
        });
        return;
    }
    reveal(app, &state, route);
}

/// The mail store opened: answer the clicks that came before it. They run
/// once the caller is done with the window's state.
pub(crate) fn core_ready() {
    let _ = slint::invoke_from_event_loop(|| {
        let Some(app) = WINDOW.with(|window| window.borrow().as_ref()?.app.upgrade()) else {
            return;
        };
        for response in WAITING.with(RefCell::take) {
            respond(&app, response);
        }
    });
}

/// Show the conversation a notification is about, in the list that has
/// it: the one showing, when that is the unified Inbox or the account's,
/// and otherwise the account's Inbox, leaving a mail profile that leaves the
/// account out. It opens once its page arrives, the way a restored tab
/// opens its conversation.
fn reveal(app: &AppWindow, state: &Rc<RefCell<InboxState>>, route: Route) {
    app.set_settings_open(false);
    app.set_active_view("mail".into());
    // The active mail profile can leave the account out; every account's
    // mail shows without one.
    let shown = state.borrow().shows_account(route.account_id);
    if !shown {
        app.invoke_select_mail_profile("".into());
    }
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
