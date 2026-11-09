//! New-mail notifications.
//!
//! Core keeps a durable outbox of new mail and decides what to notify for: the
//! user's Important/All/tabs scope, mail that is still new and unread, and
//! threads whose AI route has resolved. This module shows what it hands over
//! through the platform's notification service, a [`NotificationHost`]: one
//! notification per message, or one summary per account when more arrive at
//! once. The words are translated here, so the shared core never emits
//! user-visible English text. A notification is taken back once its mail is
//! read, archived, moved or deleted, here or on another device, and clicks and
//! buttons open the conversation or act on the message.
//!
//! What every platform's guidelines and peer mail clients agree on shapes the
//! rest: no notification for mail that lands in the list the user is looking
//! at, one sound per burst and only for mail from a person, and the system's
//! own sound and Do Not Disturb rather than audio played here.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, LazyLock, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use flectar_mail_core::{
    Core,
    events::CoreEvent,
    models::{ActionKind, ActionTarget, PerformActionArgs, now_ms},
};
use tokio::sync::{
    Notify,
    broadcast::{self, error::RecvError},
    watch,
};

pub use flectar_mail_core::{NewMailNotification, NotificationBatch, NotificationOutcome};

#[cfg(target_os = "windows")]
#[path = "notifications/windows.rs"]
mod platform;
#[cfg(user_notifications)]
#[path = "notifications/apple.rs"]
mod platform;
#[cfg(linux_desktop)]
#[path = "notifications/linux.rs"]
mod platform;
/// Other desktops have no notification service the app knows.
#[cfg(all(
    desktop,
    not(any(target_os = "windows", target_os = "macos", linux_desktop))
))]
mod platform {
    pub(crate) fn host() -> Option<std::sync::Arc<dyn super::NotificationHost>> {
        None
    }

    pub(crate) fn launched_for_click() -> bool {
        false
    }
}

/// The platform's notification service, if this system offers one the app
/// can use. Called once, before the window is created.
#[cfg(any(desktop, user_notifications))]
pub(crate) use platform::host;
/// Whether the system started this process to deliver a click on one of its
/// notifications.
#[cfg(desktop)]
pub(crate) use platform::launched_for_click;

/// Whether the system started this process with `argument` to deliver a
/// click on a notification. Only Windows passes arguments of its own.
#[cfg(target_os = "windows")]
pub(crate) use platform::click_argument;
#[cfg(all(desktop, not(target_os = "windows")))]
pub(crate) fn click_argument(_argument: &std::ffi::OsStr) -> bool {
    false
}

/// A future a [`NotificationHost`] returns.
pub type HostFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// The platform's notification service.
pub trait NotificationHost: Send + Sync + 'static {
    /// Show these notifications, replacing any shown under the same
    /// [`AlertId`]. Answers for each, in order, once the system took it.
    fn show(&self, alerts: Vec<Alert>) -> HostFuture<Vec<Result<(), Failure>>>;
    /// Take these notifications back. Ids the system no longer shows are
    /// ignored.
    fn withdraw(&self, ids: Vec<AlertId>) -> HostFuture<()>;
    /// Whether the system lets the app show notifications now.
    fn availability(&self) -> HostFuture<Availability>;
    /// Ask the user to allow notifications where the system asks first, and
    /// say what they chose; elsewhere the same as [`Self::availability`].
    /// The request starts when this is called; the future only reports the
    /// answer, which may never come while the user ignores the prompt.
    fn request_permission(&self) -> HostFuture<Availability> {
        self.availability()
    }
    /// The button labels in the UI's language, for systems that register
    /// them ahead of the notifications (macOS). Called when the window is
    /// ready and whenever the language changes.
    fn set_words(&self, _words: &FixedWords) {}
    /// Whether to take every notification back before the app exits: the
    /// system keeps them, but nothing would start the app for a click.
    fn withdraw_at_exit(&self) -> bool {
        false
    }
    /// What Settings can offer about this system's notifications.
    fn controls(&self) -> Controls {
        Controls::default()
    }
    /// Open the system's settings for the app's notifications, where
    /// [`Controls::settings`] says there are such.
    fn open_settings(&self) {}
    /// Show `badge` on the app icon, or take it off, where
    /// [`Controls::badge`] says the system shows one. Called when the count
    /// changes, and with `None` as the app quits on the desktop.
    fn set_badge(&self, _badge: Option<Badge>) -> HostFuture<()> {
        Box::pin(std::future::ready(()))
    }
}

/// The count of unread mail on the app icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub count: u32,
    /// What a screen reader says for it, in the UI's language: "3 unread
    /// messages".
    pub description: String,
}

/// What the system leaves to the app's Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    /// The system has settings for the app's notifications that the app can
    /// open, where the user turns them back on.
    pub settings: bool,
    /// The app decides whether its notifications play a sound.
    pub sound: bool,
    /// The system shows a count the app sets on its icon.
    pub badge: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            settings: false,
            sound: true,
            badge: false,
        }
    }
}

/// One notification for the system to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub id: AlertId,
    /// The sender for new mail; "N new messages" for a summary.
    pub title: String,
    /// The subject for new mail; who wrote for a summary.
    pub body: String,
    /// The start of the message, when its body is already here.
    pub preview: Option<String>,
    /// The account's address, when there are several accounts.
    pub account: Option<String>,
    /// When the server received the message, or the newest one summed up,
    /// in milliseconds.
    pub received_at: i64,
    /// Play the system's new-mail sound with it.
    pub sound: bool,
    /// What a click on the notification opens.
    pub open: Route,
    /// Its buttons, in order, with their labels.
    pub actions: Vec<(Act, String)>,
}

/// What a shown notification is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AlertId {
    /// New mail in one message.
    Message { account_id: i64, message_id: i64 },
    /// An account's summary of new mail.
    Summary { account_id: i64 },
}

impl AlertId {
    pub fn account_id(self) -> i64 {
        match self {
            Self::Message { account_id, .. } | Self::Summary { account_id } => account_id,
        }
    }

    /// A stable name for the notification, for systems that identify
    /// notifications by string.
    pub fn key(self) -> String {
        match self {
            Self::Message {
                account_id,
                message_id,
            } => format!("message-{account_id}-{message_id}"),
            Self::Summary { account_id } => format!("summary-{account_id}"),
        }
    }
}

/// Where a notification leads: a conversation, or an account's Inbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub account_id: i64,
    pub thread_id: Option<i64>,
    pub message_id: Option<i64>,
}

/// What the user asked of a notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Clicked it: open what it is about.
    Open,
    MarkRead,
    Archive,
    Delete,
}

impl Act {
    fn key(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::MarkRead => "read",
            Self::Archive => "archive",
            Self::Delete => "delete",
        }
    }

    fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "open" => Self::Open,
            "read" => Self::MarkRead,
            "archive" => Self::Archive,
            "delete" => Self::Delete,
            _ => return None,
        })
    }
}

/// Why the system did not show a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Worth another try later, such as when the notification service did
    /// not answer.
    Retry(String),
    /// Trying again won't help until something changes: the user turned
    /// notifications off, or the system has no notification service.
    Unavailable(String),
}

/// Whether the system shows the app's notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Allowed,
    /// The system asks the user the first time (macOS).
    NotAsked,
    /// The user turned them off in the system settings.
    Denied,
    /// There is no notification service to show them: none running, or the
    /// platform has none the app can use.
    Unavailable,
    /// The system refuses them from this copy of the app outright (macOS).
    Refused,
}

/// A click or a button on a notification, as the platform reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub route: Route,
    pub act: Act,
    /// The token that lets the window take focus (Wayland and X11).
    pub activation_token: Option<String>,
}

const PAYLOAD_PREFIX: &str = "flectar-mail-notification:1";

/// The text a notification carries back to the app when clicked: what it
/// leads to and what was asked. Systems hand it to the app unchanged.
pub fn payload(route: Route, act: Act) -> String {
    let field = |value: Option<i64>| value.map_or_else(String::new, |value| value.to_string());
    format!(
        "{PAYLOAD_PREFIX};{};{};{};{}",
        act.key(),
        route.account_id,
        field(route.thread_id),
        field(route.message_id),
    )
}

/// The route and request a [`payload`] encodes; `None` for anything else.
pub fn parse_payload(payload: &str) -> Option<(Route, Act)> {
    let mut fields = payload
        .strip_prefix(PAYLOAD_PREFIX)?
        .strip_prefix(';')?
        .split(';');
    let act = Act::parse(fields.next()?)?;
    let account_id = fields.next()?.parse().ok()?;
    let optional = |field: Option<&str>| -> Option<Option<i64>> {
        match field? {
            "" => Some(None),
            value => value.parse().ok().map(Some),
        }
    };
    let thread_id = optional(fields.next())?;
    let message_id = optional(fields.next())?;
    if fields.next().is_some() {
        return None;
    }
    Some((
        Route {
            account_id,
            thread_id,
            message_id,
        },
        act,
    ))
}

/// What the platform asks of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    /// A click or button on a notification.
    Respond(Response),
    /// Bring the window up, as a desktop asks the app over D-Bus.
    #[cfg_attr(not(all(linux_desktop, feature = "flatpak")), allow(dead_code))]
    Raise(Option<String>),
    /// End the app, as GNOME's Background Apps asks over D-Bus.
    #[cfg_attr(not(all(linux_desktop, feature = "flatpak")), allow(dead_code))]
    Quit,
}

type RequestHandler = Arc<dyn Fn(Request) + Send + Sync>;

/// Where requests go, and the ones that came before the window could take
/// them, such as the click that launched the app.
struct Requests {
    handler: Option<RequestHandler>,
    queued: Vec<Request>,
}

static REQUESTS: Mutex<Requests> = Mutex::new(Requests {
    handler: None,
    queued: Vec::new(),
});

fn request(request: Request) {
    let handler = {
        let mut requests = REQUESTS.lock().unwrap_or_else(PoisonError::into_inner);
        match requests.handler.clone() {
            Some(handler) => handler,
            None => {
                requests.queued.push(request);
                return;
            }
        }
    };
    handler(request);
}

/// Report a click or button on a notification, from any thread. A click
/// goes to the window; a button's action needs only the mail store.
pub fn respond(response: Response) {
    match response.act {
        Act::Open => request(Request::Respond(response)),
        act => {
            if let Some(message_id) = response.route.message_id {
                with_core(move |runtime, core| {
                    let core = Arc::clone(core);
                    runtime.spawn(async move {
                        act_on(&core, act, message_id).await;
                    });
                });
            }
        }
    }
}

/// How long the app stays awake in the background for a button's action to
/// reach the server, within the half minute or so iOS allows. What doesn't
/// make it goes out once the app runs again.
#[cfg(background_checks)]
const ACT_LIMIT: Duration = Duration::from_secs(25);

/// Report a click or button on a notification, as [`respond`], where the
/// system may suspend the app soon after it hears the response was handled.
/// `done` says so once a click went to the window, or once a button's action
/// is saved; the app then stays awake for it to reach the server
/// ([`ACT_LIMIT`]).
#[cfg(background_checks)]
pub fn respond_then(response: Response, done: impl FnOnce() + Send + 'static) {
    match (response.act, response.route.message_id) {
        (Act::Open, _) | (_, None) => {
            respond(response);
            done();
        }
        (act, Some(message_id)) => {
            let awake = crate::platform::stay_awake();
            with_core(move |runtime, core| {
                let core = Arc::clone(core);
                runtime.spawn(async move {
                    let queued = act_on(&core, act, message_id).await;
                    done();
                    if let Some(action_ids) = queued {
                        // A launch for the button starts the accounts that
                        // carry it out; there may be no window to start them.
                        let sent = tokio::time::timeout(ACT_LIMIT, async {
                            core.start_accounts_now().await;
                            core.wait_for_actions(action_ids).await
                        })
                        .await;
                        if !matches!(sent, Ok(Ok(()))) {
                            tracing::info!("notifications: the action goes out on the next run");
                        }
                    }
                    drop(awake);
                });
            });
        }
    }
}

/// Carry out a button's action on the message; the actions it queued.
async fn act_on(core: &Core, act: Act, message_id: i64) -> Option<Vec<i64>> {
    let kind = match act {
        Act::Open => return None,
        Act::MarkRead => ActionKind::MarkRead,
        Act::Archive => ActionKind::Archive,
        Act::Delete => ActionKind::Trash,
    };
    match core
        .perform_action(PerformActionArgs {
            kind,
            target: ActionTarget::Messages(vec![message_id]),
            params: None,
        })
        .await
    {
        Ok(result) => Some(result.action_ids),
        Err(error) => {
            tracing::warn!(%error, "notifications: the action failed");
            None
        }
    }
}

/// What the system asks of the app outside its window, waiting for the mail
/// store: a button on a notification, a check for new mail in the
/// background.
type Errand = Box<dyn FnOnce(&tokio::runtime::Handle, &Arc<Core>) + Send>;

struct Errands {
    ready: Option<(tokio::runtime::Handle, Arc<Core>)>,
    waiting: Vec<Errand>,
}

static ERRANDS: Mutex<Errands> = Mutex::new(Errands {
    ready: None,
    waiting: Vec::new(),
});

/// Run `errand` with the runtime and the mail store: now if it is open,
/// otherwise once it opens.
fn with_core(errand: impl FnOnce(&tokio::runtime::Handle, &Arc<Core>) + Send + 'static) {
    let (runtime, core) = {
        let mut errands = ERRANDS.lock().unwrap_or_else(PoisonError::into_inner);
        match &errands.ready {
            Some(ready) => ready.clone(),
            None => {
                errands.waiting.push(Box::new(errand));
                return;
            }
        }
    };
    errand(&runtime, &core);
}

/// The mail store opened: run the errands that waited for it, in order.
fn core_opened(runtime: &tokio::runtime::Handle, core: &Arc<Core>) {
    let waiting = {
        let mut errands = ERRANDS.lock().unwrap_or_else(PoisonError::into_inner);
        errands.ready = Some((runtime.clone(), Arc::clone(core)));
        std::mem::take(&mut errands.waiting)
    };
    for errand in waiting {
        errand(runtime, core);
    }
}

/// The user brought the app to the foreground, where the Inbox shows what
/// came while it wasn't running: that is no news for a check in the
/// background ([`flectar_mail_core::Core::forget_missed`]).
#[cfg(background_checks)]
pub(crate) fn app_became_active() {
    with_core(|_, core| core.forget_missed());
}

/// Stops a [`check_in_background`].
#[cfg_attr(not(background_checks), allow(dead_code))]
pub(crate) struct BackgroundCheck(tokio::sync::watch::Sender<bool>);

#[cfg_attr(not(background_checks), allow(dead_code))]
impl BackgroundCheck {
    /// The system wants its time back: stop, and say the check failed.
    pub(crate) fn cancel(&self) {
        self.0.send_replace(true);
    }
}

/// Check every account for new mail and tell of it, for a system that wakes
/// the app in the background to do so: start the accounts, and once each
/// one's Inbox is in, show what came while the app wasn't running and take
/// back what was read elsewhere, with the app icon's count brought up to
/// date. Calls `done` once, with whether every account was checked, `false`
/// when cancelled.
#[cfg_attr(not(background_checks), allow(dead_code))]
pub(crate) fn check_in_background(done: impl FnOnce(bool) + Send + 'static) -> BackgroundCheck {
    let (cancel, mut cancelled) = tokio::sync::watch::channel(false);
    with_core(move |runtime, core| {
        let core = Arc::clone(core);
        runtime.spawn(async move {
            let checked = tokio::select! {
                checked = check_accounts(core) => checked,
                _ = cancelled.wait_for(|&cancelled| cancelled) => false,
            };
            done(checked);
        });
    });
    BackgroundCheck(cancel)
}

#[cfg_attr(not(background_checks), allow(dead_code))]
async fn check_accounts(core: Arc<Core>) -> bool {
    core.start_accounts_now().await;
    let accounts = match core.list_accounts().await {
        Ok(accounts) => accounts,
        Err(error) => {
            tracing::warn!(%error, "notifications: could not list the accounts to check");
            return false;
        }
    };
    // Each account tells of its mail as soon as it is in: the system may end
    // the check before a slow server answers. Dropping the set stops them.
    let mut checks = tokio::task::JoinSet::new();
    for account in accounts {
        let core = Arc::clone(&core);
        checks.spawn(async move {
            let checked = core.sync_accounts(Some(&[account.id])).await;
            if let Err(error) = &checked {
                tracing::info!(account_id = account.id, %error, "notifications: background check failed");
            }
            if let Err(error) = core.announce_missed().await {
                tracing::warn!(%error, "notifications: could not queue missed mail");
            }
            flush().await;
            checked.is_ok()
        });
    }
    if checks.is_empty() {
        // Still send what is due and bring the count up to date.
        flush().await;
        return true;
    }
    let mut every = true;
    while let Some(checked) = checks.join_next().await {
        every &= checked.unwrap_or(false);
    }
    every
}

/// Bring the window up with the desktop's activation token, from any thread.
#[cfg_attr(not(all(linux_desktop, feature = "flatpak")), allow(dead_code))]
pub(crate) fn raise(activation_token: Option<String>) {
    request(Request::Raise(activation_token));
}

/// End the app, from any thread.
#[cfg_attr(not(all(linux_desktop, feature = "flatpak")), allow(dead_code))]
pub(crate) fn quit() {
    request(Request::Quit);
}

/// Take the requests from now on, starting with the ones that waited.
pub(crate) fn serve_requests(handler: impl Fn(Request) + Send + Sync + 'static) {
    let handler: RequestHandler = Arc::new(handler);
    let queued = {
        let mut requests = REQUESTS.lock().unwrap_or_else(PoisonError::into_inner);
        requests.handler = Some(Arc::clone(&handler));
        std::mem::take(&mut requests.queued)
    };
    for request in queued {
        handler(request);
    }
}

/// More new messages than this for one account at once are summed up in one
/// notification. Peers sum up from three (Proton) to five (Mailspring).
const SUMMARY_AFTER: usize = 3;
/// How many senders a summary names.
const SUMMARY_SENDERS: usize = 3;
/// How long a notification the host couldn't raise waits for another try.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// Mail changes come in bursts during a sync; withdrawals wait for it to
/// settle.
const WITHDRAW_SETTLE: Duration = Duration::from_secs(2);
/// How often week-old notifications are taken back and old records
/// forgotten.
const PRUNE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// How long taking notifications back may hold up the app's exit.
const EXIT_WITHDRAW_LIMIT: Duration = Duration::from_secs(2);

/// Wakes the dispatcher when the notification settings change.
static PREFERENCES: Notify = Notify::const_new();

/// The notification settings changed: take every notification back when
/// they were turned off, and ask for permission when they were turned on.
pub(crate) fn preferences_changed() {
    PREFERENCES.notify_one();
}

/// Whether the user wants anything a check in the background brings:
/// notifications, or the count on the app icon. Kept by the dispatcher.
static BACKGROUND_CHECKS_WANTED: AtomicBool = AtomicBool::new(false);

/// Whether to ask the system to wake the app now and then to check for new
/// mail.
#[cfg(background_checks)]
pub(crate) fn background_checks_wanted() -> bool {
    BACKGROUND_CHECKS_WANTED.load(Ordering::Relaxed)
}

/// Wakes the dispatcher for a [`flush`].
static FLUSH: Notify = Notify::const_new();
/// How many flushes were asked for, and up to which one the dispatcher's
/// passes have served: a pass serves those asked before it started.
static FLUSHES_ASKED: AtomicU64 = AtomicU64::new(0);
static FLUSHES_SERVED: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::Sender::new(0));

/// Have the dispatcher take back what is stale, show what is due and bring
/// the app icon's count up to date, and wait until it has.
#[cfg_attr(not(background_checks), allow(dead_code))]
async fn flush() {
    let ticket = FLUSHES_ASKED.fetch_add(1, Ordering::AcqRel) + 1;
    FLUSH.notify_one();
    let _ = FLUSHES_SERVED
        .subscribe()
        .wait_for(|&served| served >= ticket)
        .await;
}

/// Show new-mail notifications through `host`, take them back once their mail
/// is no longer new, and forget old records. Without a host, new mail is
/// settled as shown nowhere, so nothing piles up.
pub(crate) fn spawn_dispatcher(
    runtime: &tokio::runtime::Handle,
    core: Arc<Core>,
    host: Option<Arc<dyn NotificationHost>>,
) {
    dispatch_through(runtime, core, host, ui::Live);
}

/// The dispatcher, asking `window` what the app shows.
fn dispatch_through(
    runtime: &tokio::runtime::Handle,
    core: Arc<Core>,
    host: Option<Arc<dyn NotificationHost>>,
    window: impl Window + Send + Sync + 'static,
) {
    let mut events = core.bus.subscribe();
    core_opened(runtime, &core);
    runtime.spawn(async move {
        if let Err(error) = core.recover_notification_deliveries().await {
            tracing::warn!(%error, "notifications: could not recover interrupted deliveries");
        }
        let mut pruned_at = None;
        let mut enabled = notifications_enabled(&core).await;
        let mut badge = None;
        let mut flushed = 0;
        loop {
            let flushing = FLUSHES_ASKED.load(Ordering::Acquire);
            if flushing > flushed {
                // Mail read elsewhere while the app was away.
                withdraw(&core, host.as_deref(), Withdraw::Stale).await;
            }
            if pruned_at.is_none_or(|at: tokio::time::Instant| at.elapsed() >= PRUNE_EVERY) {
                // Taking back what is stale includes the week-old, which the
                // pruning would otherwise leave on screen for good.
                withdraw(&core, host.as_deref(), Withdraw::Stale).await;
                if let Err(error) = core.prune_notifications().await {
                    tracing::warn!(%error, "notifications: could not forget old records");
                }
                pruned_at = Some(tokio::time::Instant::now());
            }
            let wait = dispatch(&core, host.as_deref(), &window).await;
            let mut counting = false;
            if let Some(host) = host.as_deref()
                && host.controls().badge
            {
                match core.unread_badge().await {
                    Ok(count) => {
                        // None while the setting is off.
                        counting = count.is_some();
                        update_badge(host, &window, count, &mut badge).await;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "notifications: could not count unread mail")
                    }
                }
            }
            BACKGROUND_CHECKS_WANTED
                .store(host.is_some() && (enabled || counting), Ordering::Relaxed);
            flushed = flushing;
            FLUSHES_SERVED.send_replace(flushed);
            tokio::select! {
                event = next_event(&mut events) => match event {
                    Event::NewMail => {}
                    Event::MailChanged => {
                        tokio::time::sleep(WITHDRAW_SETTLE).await;
                        drain_changes(&mut events);
                        withdraw(&core, host.as_deref(), Withdraw::Stale).await;
                    }
                    Event::Closed => return,
                },
                () = PREFERENCES.notified() => {
                    let now_enabled = notifications_enabled(&core).await;
                    if enabled && !now_enabled {
                        withdraw(&core, host.as_deref(), Withdraw::All).await;
                    }
                    if now_enabled
                        && !enabled
                        && let Some(host) = &host
                    {
                        drop(host.request_permission());
                    }
                    enabled = now_enabled;
                }
                () = FLUSH.notified() => {}
                () = sleep(wait) => {}
                () = tokio::time::sleep(PRUNE_EVERY) => {}
            }
        }
    });
}

/// Take every notification back as the app exits, where the host asks for
/// it ([`NotificationHost::withdraw_at_exit`]). Called once the window's event
/// loop has ended.
pub(crate) fn withdraw_before_exit(
    runtime: &tokio::runtime::Handle,
    core: Option<Arc<Core>>,
    host: Option<Arc<dyn NotificationHost>>,
) {
    let (Some(core), Some(host)) = (core, host) else {
        return;
    };
    // The time limits are set inside the runtime, whose timer they need: the
    // window's loop has ended outside it.
    if host.withdraw_at_exit() {
        let _ = runtime.block_on(async {
            tokio::time::timeout(
                EXIT_WITHDRAW_LIMIT,
                withdraw(&core, Some(host.as_ref()), Withdraw::All),
            )
            .await
        });
    }
    // Nothing keeps a desktop badge current once the app is gone; a phone's
    // stays, as its own background checks update it.
    if cfg!(desktop) && host.controls().badge {
        let _ = runtime.block_on(async {
            tokio::time::timeout(EXIT_WITHDRAW_LIMIT, host.set_badge(None)).await
        });
    }
}

/// Put `count` on the app icon, or nothing for none or zero, unless it is
/// what `shown` says the icon already has.
async fn update_badge(
    host: &dyn NotificationHost,
    window: &impl Window,
    count: Option<i64>,
    shown: &mut Option<Option<u32>>,
) {
    let count = count
        .map(|count| u32::try_from(count).unwrap_or(if count < 0 { 0 } else { u32::MAX }))
        .filter(|&count| count > 0);
    if *shown == Some(count) {
        return;
    }
    let badge = match count {
        Some(count) => Some(Badge {
            count,
            description: window
                .badge_description(count)
                .await
                .unwrap_or_else(|| count.to_string()),
        }),
        None => None,
    };
    host.set_badge(badge).await;
    *shown = Some(count);
}

async fn notifications_enabled(core: &Core) -> bool {
    core.get_settings()
        .await
        .is_ok_and(|settings| settings.notifications_enabled)
}

/// Show what is due. Returns how long to wait before the next look.
async fn dispatch(
    core: &Core,
    host: Option<&dyn NotificationHost>,
    window: &impl Window,
) -> Option<Duration> {
    let result = core
        .dispatch_notifications(|batch| async move {
            match host {
                Some(host) => present(host, window, batch).await,
                None => vec![
                    NotificationOutcome::Unavailable(
                        "this platform shows no notifications".into()
                    );
                    batch.mail.len()
                ],
            }
        })
        .await;
    match result {
        Ok(next) => next.map(|at| Duration::from_millis(u64::try_from(at - now_ms()).unwrap_or(0))),
        Err(error) => {
            tracing::warn!(%error, "notifications: dispatch failed");
            Some(RETRY_AFTER)
        }
    }
}

enum Withdraw {
    /// The notifications whose mail is no longer new.
    Stale,
    /// Every notification shown.
    All,
}

async fn withdraw(core: &Core, host: Option<&dyn NotificationHost>, which: Withdraw) {
    let withdrawal = match which {
        Withdraw::Stale => core.stale_notifications().await,
        Withdraw::All => core.shown_notifications().await,
    };
    let withdrawal = match withdrawal {
        Ok(withdrawal) if !withdrawal.is_empty() => withdrawal,
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(%error, "notifications: could not list what to withdraw");
            return;
        }
    };
    if let Some(host) = host {
        let ids = withdrawal
            .messages
            .iter()
            .map(|shown| AlertId::Message {
                account_id: shown.account_id,
                message_id: shown.message_id,
            })
            .chain(
                withdrawal
                    .summaries
                    .iter()
                    .map(|&account_id| AlertId::Summary { account_id }),
            )
            .collect();
        host.withdraw(ids).await;
    }
    if let Err(error) = core.notifications_withdrawn(&withdrawal).await {
        tracing::warn!(%error, "notifications: could not record withdrawals");
    }
}

enum Event {
    NewMail,
    MailChanged,
    Closed,
}

/// Wait for mail that may need a notification or the withdrawal of one. A
/// receiver that lagged may have missed both, so it counts as new mail and a
/// change.
async fn next_event(events: &mut broadcast::Receiver<CoreEvent>) -> Event {
    loop {
        match events.recv().await {
            Ok(CoreEvent::MailNew { .. }) => return Event::NewMail,
            Ok(CoreEvent::MailUpdated { .. }) | Err(RecvError::Lagged(_)) => {
                return Event::MailChanged;
            }
            Ok(_) => {}
            Err(RecvError::Closed) => return Event::Closed,
        }
    }
}

/// Drop the changes reported while waiting: one look at the stale
/// notifications covers them all. New mail stays handled by the dispatch
/// that follows.
fn drain_changes(events: &mut broadcast::Receiver<CoreEvent>) {
    while let Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) = events.try_recv() {}
}

async fn sleep(wait: Option<Duration>) {
    match wait {
        Some(wait) => tokio::time::sleep(wait).await,
        None => std::future::pending().await,
    }
}

/// Turn one batch of new mail into notifications and show them: one per
/// message, or one summary for an account with more than [`SUMMARY_AFTER`]
/// at once or a summary already showing. Mail the user is looking at in the
/// window is skipped, and only the first notification with mail from a
/// person may play the sound.
async fn present(
    host: &dyn NotificationHost,
    window: &impl Window,
    batch: NotificationBatch,
) -> Vec<NotificationOutcome> {
    let count = batch.mail.len();
    if host.availability().await == Availability::NotAsked {
        // Ask in context, as the mail arrives, and keep the mail until the
        // user answered: the system refuses it meanwhile. A phone asks once
        // the app is in front with an account (ui::ask_for_permission), as
        // this may run in the background.
        if cfg!(not(background_checks)) {
            drop(host.request_permission());
        }
        return vec![
            NotificationOutcome::Retry(
                "waiting for the user to allow notifications".into()
            );
            count
        ];
    }
    let accounts = batch
        .mail
        .iter()
        .map(|mail| mail.account_id)
        .collect::<BTreeSet<_>>();
    let Some(on_screen) = window.on_screen(accounts).await else {
        return vec![NotificationOutcome::Retry("the window is not ready".into()); count];
    };
    let mut outcomes = vec![None; count];
    let mut by_account: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (index, mail) in batch.mail.iter().enumerate() {
        if on_screen.contains(&mail.account_id) {
            outcomes[index] = Some(NotificationOutcome::OnScreen);
        } else {
            by_account.entry(mail.account_id).or_default().push(index);
        }
    }
    let plans = by_account
        .into_iter()
        .map(|(account_id, indices)| {
            let summarized = batch.summarized.get(&account_id).copied().unwrap_or(0);
            if indices.len() > SUMMARY_AFTER || summarized > 0 {
                Plan::Summary {
                    account_id,
                    count: summarized
                        .saturating_add(u32::try_from(indices.len()).unwrap_or(u32::MAX)),
                    senders: senders(indices.iter().map(|&index| &batch.mail[index])),
                    indices,
                }
            } else {
                Plan::Messages(indices)
            }
        })
        .collect::<Vec<_>>();
    let summaries = plans
        .iter()
        .filter_map(|plan| match plan {
            Plan::Summary { count, senders, .. } => Some((*count, senders.clone())),
            Plan::Messages(_) => None,
        })
        .collect();
    let Some(words) = window.words(summaries).await else {
        return vec![NotificationOutcome::Retry("the window is not ready".into()); count];
    };
    let mut alerts = Vec::new();
    let mut covered = Vec::new();
    let mut summary_words = words.summaries.into_iter();
    for plan in plans {
        match plan {
            Plan::Messages(indices) => {
                for index in indices {
                    alerts.push(message_alert(&batch.mail[index], &words.fixed));
                    covered.push((vec![index], false));
                }
            }
            Plan::Summary {
                account_id,
                indices,
                ..
            } => {
                let (title, body) = summary_words.next().unwrap_or_default();
                let newest = indices
                    .iter()
                    .map(|&index| &batch.mail[index])
                    .max_by_key(|mail| mail.received_at);
                alerts.push(Alert {
                    id: AlertId::Summary { account_id },
                    title,
                    body,
                    preview: None,
                    account: newest.and_then(|mail| mail.account.clone()),
                    received_at: newest.map_or_else(now_ms, |mail| mail.received_at),
                    sound: indices.iter().any(|&index| batch.mail[index].sound),
                    open: Route {
                        account_id,
                        thread_id: None,
                        message_id: None,
                    },
                    actions: Vec::new(),
                });
                covered.push((indices, true));
            }
        }
    }
    // One sound for the whole burst.
    let mut sounded = false;
    for alert in &mut alerts {
        alert.sound &= !sounded;
        sounded |= alert.sound;
    }
    let results = if alerts.is_empty() {
        Vec::new()
    } else {
        host.show(alerts).await
    };
    for (index, (indices, in_summary)) in covered.into_iter().enumerate() {
        let outcome = match results.get(index) {
            Some(Ok(())) if in_summary => NotificationOutcome::ShownInSummary,
            Some(Ok(())) => NotificationOutcome::Shown,
            Some(Err(Failure::Retry(error))) => NotificationOutcome::Retry(error.clone()),
            Some(Err(Failure::Unavailable(reason))) => {
                NotificationOutcome::Unavailable(reason.clone())
            }
            None => NotificationOutcome::Retry("the notification service gave no answer".into()),
        };
        for index in indices {
            outcomes[index] = Some(outcome.clone());
        }
    }
    outcomes
        .into_iter()
        .map(|outcome| {
            outcome.unwrap_or_else(|| NotificationOutcome::Retry("not presented".into()))
        })
        .collect()
}

enum Plan {
    Messages(Vec<usize>),
    Summary {
        account_id: i64,
        indices: Vec<usize>,
        count: u32,
        senders: Vec<String>,
    },
}

/// The distinct senders of some new mail, newest first.
fn senders<'a>(mail: impl Iterator<Item = &'a NewMailNotification>) -> Vec<String> {
    let mut mail = mail.collect::<Vec<_>>();
    mail.sort_by_key(|mail| std::cmp::Reverse(mail.received_at));
    let mut names = Vec::new();
    for mail in mail {
        let name = mail.sender.trim();
        if !name.is_empty() && !names.iter().any(|known: &String| known == name) {
            names.push(name.to_owned());
        }
    }
    names
}

fn message_alert(mail: &NewMailNotification, words: &FixedWords) -> Alert {
    let sender = mail.sender.trim();
    let subject = mail.subject.trim();
    let delete = if mail.can_archive {
        (Act::Archive, words.archive.clone())
    } else {
        (Act::Delete, words.delete.clone())
    };
    Alert {
        id: AlertId::Message {
            account_id: mail.account_id,
            message_id: mail.message_id,
        },
        title: if sender.is_empty() {
            words.new_message.clone()
        } else {
            sender.to_owned()
        },
        body: if subject.is_empty() {
            words.no_subject.clone()
        } else {
            subject.to_owned()
        },
        preview: mail.preview.clone(),
        account: mail.account.clone(),
        received_at: mail.received_at,
        sound: mail.sound,
        open: Route {
            account_id: mail.account_id,
            thread_id: Some(mail.thread_id),
            message_id: Some(mail.message_id),
        },
        actions: vec![(Act::MarkRead, words.mark_read.clone()), delete],
    }
}

/// What the presentation needs to know from the window.
pub(crate) trait Window {
    /// Which of these accounts' new mail the user sees arrive in the window.
    /// `None` while there is no window to ask.
    fn on_screen(
        &self,
        accounts: BTreeSet<i64>,
    ) -> impl Future<Output = Option<BTreeSet<i64>>> + Send;
    /// The words for a batch: the fixed ones, and the title and body of each
    /// summary, given as its count and the senders it names.
    fn words(
        &self,
        summaries: Vec<(u32, Vec<String>)>,
    ) -> impl Future<Output = Option<Words>> + Send;
    /// What a screen reader says for the app icon's count: "3 unread
    /// messages".
    fn badge_description(&self, count: u32) -> impl Future<Output = Option<String>> + Send;
}

/// The translated words of a batch of notifications.
pub(crate) struct Words {
    fixed: FixedWords,
    /// Title and body of each summary, in the order asked for.
    summaries: Vec<(String, String)>,
}

/// The words every notification may use, in the UI's language.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FixedWords {
    /// The title of mail from a sender with no name or address.
    pub new_message: String,
    pub no_subject: String,
    /// The label of a click on the notification itself, where a system
    /// shows it (some Linux notification servers).
    pub open: String,
    pub mark_read: String,
    pub archive: String,
    pub delete: String,
    /// The names of Android's two notification channels: mail that may play
    /// the new-mail sound, and the rest.
    pub new_mail_channel: String,
    pub quiet_mail_channel: String,
}

/// The window's side: what it shows, the words in its language, and opening
/// conversations.
#[path = "notifications/ui.rs"]
pub(crate) mod ui;

/// The words without the window, for a process that runs without it.
#[cfg(any(test, background_engine))]
#[path = "notifications/words.rs"]
mod words;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_round_trip() {
        let route = Route {
            account_id: 3,
            thread_id: Some(40),
            message_id: Some(512),
        };
        for act in [Act::Open, Act::MarkRead, Act::Archive, Act::Delete] {
            assert_eq!(parse_payload(&payload(route, act)), Some((route, act)));
        }
        let inbox = Route {
            account_id: 3,
            thread_id: None,
            message_id: None,
        };
        assert_eq!(
            parse_payload(&payload(inbox, Act::Open)),
            Some((inbox, Act::Open))
        );
    }

    #[test]
    fn foreign_payloads_are_refused() {
        for text in [
            "",
            "mailto:someone@example.com",
            "flectar-mail-notification:2;open;1;;",
            "flectar-mail-notification:1;launch;1;;",
            "flectar-mail-notification:1;open;x;;",
            "flectar-mail-notification:1;open;1;2",
            "flectar-mail-notification:1;open;1;2;3;4",
        ] {
            assert_eq!(parse_payload(text), None, "{text}");
        }
    }

    #[test]
    fn alert_keys_name_the_message_or_the_account() {
        assert_eq!(
            AlertId::Message {
                account_id: 2,
                message_id: 9
            }
            .key(),
            "message-2-9"
        );
        assert_eq!(AlertId::Summary { account_id: 2 }.key(), "summary-2");
    }

    #[test]
    fn summaries_name_each_sender_once_newest_first() {
        let mail = |sender: &str, received_at| NewMailNotification {
            account_id: 1,
            message_id: received_at,
            thread_id: received_at,
            sender: sender.into(),
            subject: String::new(),
            preview: None,
            received_at,
            account: None,
            sound: false,
            can_archive: true,
        };
        let batch = [mail("Ann", 1), mail("Bo", 3), mail(" Ann ", 2), mail("", 4)];
        assert_eq!(senders(batch.iter()), vec!["Bo", "Ann"]);
    }

    /// A notification service that records what it was asked to show.
    struct FakeHost {
        shown: Mutex<Vec<Alert>>,
        answer: Result<(), Failure>,
        availability: Availability,
        asked: std::sync::atomic::AtomicBool,
        badges: Mutex<Vec<Option<Badge>>>,
    }

    impl FakeHost {
        fn new(answer: Result<(), Failure>) -> Self {
            Self {
                shown: Mutex::new(Vec::new()),
                answer,
                availability: Availability::Allowed,
                asked: std::sync::atomic::AtomicBool::new(false),
                badges: Mutex::new(Vec::new()),
            }
        }

        fn badges(&self) -> Vec<Option<Badge>> {
            self.badges.lock().unwrap().clone()
        }

        fn shown(&self) -> Vec<Alert> {
            self.shown.lock().unwrap().clone()
        }
    }

    impl NotificationHost for FakeHost {
        fn show(&self, alerts: Vec<Alert>) -> HostFuture<Vec<Result<(), Failure>>> {
            let answers = vec![self.answer.clone(); alerts.len()];
            self.shown.lock().unwrap().extend(alerts);
            Box::pin(std::future::ready(answers))
        }

        fn withdraw(&self, _ids: Vec<AlertId>) -> HostFuture<()> {
            Box::pin(std::future::ready(()))
        }

        fn availability(&self) -> HostFuture<Availability> {
            Box::pin(std::future::ready(self.availability))
        }

        fn request_permission(&self) -> HostFuture<Availability> {
            self.asked.store(true, std::sync::atomic::Ordering::SeqCst);
            // The user hasn't answered yet.
            Box::pin(std::future::pending())
        }

        fn set_badge(&self, badge: Option<Badge>) -> HostFuture<()> {
            self.badges.lock().unwrap().push(badge);
            Box::pin(std::future::ready(()))
        }
    }

    /// A desktop's notification service that takes everything back as the
    /// app leaves.
    #[derive(Default)]
    struct LeavingHost {
        badges: Mutex<Vec<Option<Badge>>>,
    }

    impl NotificationHost for LeavingHost {
        fn show(&self, alerts: Vec<Alert>) -> HostFuture<Vec<Result<(), Failure>>> {
            Box::pin(std::future::ready(vec![Ok(()); alerts.len()]))
        }

        fn withdraw(&self, _ids: Vec<AlertId>) -> HostFuture<()> {
            Box::pin(std::future::ready(()))
        }

        fn availability(&self) -> HostFuture<Availability> {
            Box::pin(std::future::ready(Availability::Allowed))
        }

        fn withdraw_at_exit(&self) -> bool {
            true
        }

        fn controls(&self) -> Controls {
            Controls {
                badge: true,
                ..Controls::default()
            }
        }

        fn set_badge(&self, badge: Option<Badge>) -> HostFuture<()> {
            self.badges.lock().unwrap().push(badge);
            Box::pin(std::future::ready(()))
        }
    }

    #[cfg(desktop)]
    #[test]
    fn leaving_takes_everything_back_from_outside_the_runtime() {
        // As main.rs calls it, once the window's loop has ended.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let profile = tempfile::tempdir().unwrap();
        let core = runtime
            .block_on(Core::start(flectar_mail_core::config::Paths::for_tests(
                profile.path(),
            )))
            .unwrap();
        let host = Arc::new(LeavingHost::default());
        let shared: Arc<dyn NotificationHost> = host.clone();
        withdraw_before_exit(runtime.handle(), Some(Arc::new(core)), Some(shared));
        assert_eq!(host.badges.lock().unwrap().as_slice(), [None]);
    }

    /// What a phone's check in the background does when no server answers:
    /// the mail that came in after the last run caught up is still told of.
    #[cfg(desktop)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_background_check_tells_of_mail_that_came_while_closed() {
        let profile = tempfile::tempdir().unwrap();
        let paths = flectar_mail_core::config::Paths::for_tests(profile.path());
        // A POP3 account whose server refuses the connection.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let minute = 60_000;
        let now = now_ms();
        let (caught_up_at, arrived) = (now - 60 * minute, now - 10 * minute);
        let message_id = {
            let db = flectar_mail_core::db::Db::open(&paths.db_file()).unwrap();
            db.write(move |conn| {
                conn.execute_batch(&format!(
                    "INSERT INTO accounts (id, email, provider, auth_kind, username,
                       incoming_host, incoming_port, smtp_host, smtp_port, created_at,
                       mail_protocol, settings_json, caught_up_at)
                     VALUES (1, 'me@test.dev', 'generic', 'password', 'me', '127.0.0.1', {port},
                       '127.0.0.1', {port}, 0, 'pop3',
                       '{{\"connection\":{{\"incomingSecurity\":\"none\",\"smtpSecurity\":\"none\"}}}}',
                       {caught_up_at});
                     INSERT INTO folders (id, account_id, path, role) VALUES (1, 1, 'INBOX', 'inbox');
                     INSERT INTO threads (id, account_id, subject_norm, unread_count, last_message_at)
                     VALUES (1, 1, 'lunch', 1, {arrived});
                     INSERT INTO messages (thread_id, account_id, folder_id, subject, from_name,
                       from_addr, date, internal_date, is_read, is_automated, is_draft, is_outgoing)
                     VALUES (1, 1, 1, 'Lunch?', 'Ana', 'ana@test.dev', {arrived}, {arrived}, 0, 0, 0, 0);"
                ))?;
                Ok(conn.last_insert_rowid())
            })
            .await
            .unwrap()
        };

        let core = Core::start_mail_ui_with_credentials(
            paths,
            Arc::new(
                flectar_mail_core::accounts::credentials::DevelopmentFileCredentialStore::new(
                    profile.path().join("credentials.json"),
                ),
            ),
        )
        .await
        .unwrap();
        let host = Arc::new(FakeHost::new(Ok(())));
        let shared: Arc<dyn NotificationHost> = host.clone();
        dispatch_through(
            &tokio::runtime::Handle::current(),
            Arc::new(core),
            Some(shared),
            FakeWindow(Some(BTreeSet::new())),
        );
        let (checked, done) = tokio::sync::oneshot::channel();
        let _check = check_in_background(move |every| {
            let _ = checked.send(every);
        });
        let every = tokio::time::timeout(Duration::from_secs(30), done)
            .await
            .expect("the check ended")
            .unwrap();

        assert!(!every, "the server refused the connection");
        let shown = host.shown();
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert_eq!(
            shown[0].id,
            AlertId::Message {
                account_id: 1,
                message_id,
            }
        );
        assert_eq!(shown[0].title, "Ana");
    }

    /// A window showing these accounts' Inbox, or none at all.
    struct FakeWindow(Option<BTreeSet<i64>>);

    impl Window for FakeWindow {
        async fn on_screen(&self, accounts: BTreeSet<i64>) -> Option<BTreeSet<i64>> {
            let shown = self.0.as_ref()?;
            Some(accounts.intersection(shown).copied().collect())
        }

        async fn words(&self, summaries: Vec<(u32, Vec<String>)>) -> Option<Words> {
            self.0.as_ref()?;
            Some(Words {
                fixed: FixedWords {
                    mark_read: "Mark as read".into(),
                    archive: "Archive".into(),
                    delete: "Delete".into(),
                    ..FixedWords::default()
                },
                summaries: summaries
                    .into_iter()
                    .map(|(count, senders)| (format!("{count} new"), senders.join(", ")))
                    .collect(),
            })
        }

        async fn badge_description(&self, count: u32) -> Option<String> {
            self.0.as_ref()?;
            Some(format!("{count} unread"))
        }
    }

    #[tokio::test]
    async fn the_app_icon_shows_the_count_only_when_it_changes() {
        let host = FakeHost::new(Ok(()));
        let window = FakeWindow(Some(BTreeSet::new()));
        let mut shown = None;
        let badge = |count| {
            Some(Badge {
                count,
                description: format!("{count} unread"),
            })
        };
        for count in [Some(3), Some(3), Some(0), None, Some(-1), Some(7)] {
            update_badge(&host, &window, count, &mut shown).await;
        }
        // Zero, a negative count and the badge turned off all show none.
        assert_eq!(host.badges(), vec![badge(3), None, badge(7)]);

        // Without a window yet, the number itself is what is said.
        let host = FakeHost::new(Ok(()));
        update_badge(&host, &FakeWindow(None), Some(12), &mut None).await;
        assert_eq!(
            host.badges(),
            vec![Some(Badge {
                count: 12,
                description: "12".into(),
            })]
        );
    }

    fn mail(account_id: i64, message_id: i64, sender: &str, sound: bool) -> NewMailNotification {
        NewMailNotification {
            account_id,
            message_id,
            thread_id: message_id * 10,
            sender: sender.into(),
            subject: format!("Subject {message_id}"),
            preview: None,
            received_at: message_id,
            account: None,
            sound,
            can_archive: true,
        }
    }

    fn batch(mail: Vec<NewMailNotification>) -> NotificationBatch {
        NotificationBatch {
            summarized: mail.iter().map(|mail| (mail.account_id, 0)).collect(),
            mail,
        }
    }

    const NO_SCREEN: FakeWindow = FakeWindow(Some(BTreeSet::new()));

    #[tokio::test]
    async fn new_mail_shows_on_its_own_with_one_sound() {
        let host = FakeHost::new(Ok(()));
        let outcomes = present(
            &host,
            &NO_SCREEN,
            batch(vec![mail(1, 1, "Ann", true), mail(1, 2, "Bo", true)]),
        )
        .await;
        assert_eq!(outcomes, vec![NotificationOutcome::Shown; 2]);
        let shown = host.shown();
        assert_eq!(shown.len(), 2);
        assert_eq!(
            shown.iter().map(|alert| alert.sound).collect::<Vec<_>>(),
            vec![true, false]
        );
        assert_eq!(shown[0].title, "Ann");
        assert_eq!(shown[0].body, "Subject 1");
        assert_eq!(
            shown[0].actions,
            vec![
                (Act::MarkRead, "Mark as read".into()),
                (Act::Archive, "Archive".into())
            ]
        );
    }

    #[tokio::test]
    async fn a_burst_is_summed_up_per_account() {
        let host = FakeHost::new(Ok(()));
        let mut mail = (1..=4)
            .map(|id| mail(1, id, if id % 2 == 0 { "Ann" } else { "Bo" }, false))
            .collect::<Vec<_>>();
        mail.push(self::mail(2, 9, "Cy", true));
        let outcomes = present(&host, &NO_SCREEN, batch(mail)).await;
        assert_eq!(
            outcomes,
            [
                vec![NotificationOutcome::ShownInSummary; 4],
                vec![NotificationOutcome::Shown]
            ]
            .concat()
        );
        let shown = host.shown();
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].id, AlertId::Summary { account_id: 1 });
        assert_eq!(shown[0].title, "4 new");
        // The newest sender first.
        assert_eq!(shown[0].body, "Ann, Bo");
        assert_eq!(
            shown[0].open,
            Route {
                account_id: 1,
                thread_id: None,
                message_id: None
            }
        );
        assert!(shown[0].actions.is_empty());
        // The only mail from a person rings, in the account's own notification.
        assert!(!shown[0].sound);
        assert!(shown[1].sound);
    }

    #[tokio::test]
    async fn a_summary_still_showing_takes_the_new_mail() {
        let host = FakeHost::new(Ok(()));
        let mut batch = batch(vec![mail(1, 5, "Ann", false)]);
        batch.summarized.insert(1, 2);
        let outcomes = present(&host, &NO_SCREEN, batch).await;
        assert_eq!(outcomes, vec![NotificationOutcome::ShownInSummary]);
        assert_eq!(host.shown()[0].title, "3 new");
    }

    #[tokio::test]
    async fn mail_on_screen_is_left_to_the_list() {
        let host = FakeHost::new(Ok(()));
        let window = FakeWindow(Some(BTreeSet::from([1])));
        let outcomes = present(
            &host,
            &window,
            batch(vec![mail(1, 1, "Ann", true), mail(2, 2, "Bo", true)]),
        )
        .await;
        assert_eq!(
            outcomes,
            vec![NotificationOutcome::OnScreen, NotificationOutcome::Shown]
        );
        let shown = host.shown();
        assert_eq!(shown.len(), 1);
        assert!(shown[0].sound);
    }

    #[tokio::test]
    async fn what_the_system_refuses_is_settled_or_tried_again() {
        let off = FakeHost::new(Err(Failure::Unavailable("turned off".into())));
        let outcomes = present(&off, &NO_SCREEN, batch(vec![mail(1, 1, "Ann", false)])).await;
        assert_eq!(
            outcomes,
            vec![NotificationOutcome::Unavailable("turned off".into())]
        );

        let busy = FakeHost::new(Err(Failure::Retry("no answer".into())));
        let outcomes = present(&busy, &NO_SCREEN, batch(vec![mail(1, 1, "Ann", false)])).await;
        assert_eq!(
            outcomes,
            vec![NotificationOutcome::Retry("no answer".into())]
        );

        // Without a window to ask, nothing goes out yet.
        let outcomes = present(
            &FakeHost::new(Ok(())),
            &FakeWindow(None),
            batch(vec![mail(1, 1, "Ann", false)]),
        )
        .await;
        assert!(matches!(outcomes[..], [NotificationOutcome::Retry(_)]));
    }

    #[tokio::test]
    async fn mail_waits_while_the_user_is_asked_to_allow_notifications() {
        let host = FakeHost {
            availability: Availability::NotAsked,
            ..FakeHost::new(Ok(()))
        };
        let outcomes = present(&host, &NO_SCREEN, batch(vec![mail(1, 1, "Ann", true)])).await;
        assert!(matches!(outcomes[..], [NotificationOutcome::Retry(_)]));
        assert!(host.asked.load(std::sync::atomic::Ordering::SeqCst));
        assert!(host.shown().is_empty());
    }

    #[test]
    fn a_message_alert_falls_back_to_generic_words() {
        let words = FixedWords {
            new_message: "New message".into(),
            no_subject: "(No subject)".into(),
            open: "Open".into(),
            mark_read: "Mark as read".into(),
            archive: "Archive".into(),
            delete: "Delete".into(),
            ..FixedWords::default()
        };
        let mail = NewMailNotification {
            account_id: 1,
            message_id: 7,
            thread_id: 5,
            sender: " ".into(),
            subject: String::new(),
            preview: Some("Hello".into()),
            received_at: 99,
            account: Some("me@example.com".into()),
            sound: true,
            can_archive: false,
        };
        let alert = message_alert(&mail, &words);
        assert_eq!(alert.title, "New message");
        assert_eq!(alert.body, "(No subject)");
        assert_eq!(
            alert.actions,
            vec![
                (Act::MarkRead, "Mark as read".into()),
                (Act::Delete, "Delete".into())
            ]
        );
        assert_eq!(
            alert.open,
            Route {
                account_id: 1,
                thread_id: Some(5),
                message_id: Some(7)
            }
        );
    }
}
