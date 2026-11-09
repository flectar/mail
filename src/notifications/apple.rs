//! New-mail notifications on macOS and iOS, through the User Notifications
//! framework.
//!
//! On macOS only that framework: once a bundle id has used it, macOS refuses
//! the older NSUserNotificationCenter for that id, so the two never mix. It
//! needs the app bundle, known to Launch Services and signed as a whole; the
//! release build's ad-hoc bundle signature is enough (Telegram measured
//! authorization and delivery for such bundles on macOS 27). Outside a
//! bundle the framework aborts the process, so a build run from the command
//! line shows none. An iOS app always runs from its bundle, whose id a
//! sideloading store rewrites.
//!
//! The delegate is installed before the app finishes launching, as Apple
//! requires, so the click that launched the app reaches it. The system asks
//! the user to allow notifications the first time the app asks, which it does
//! when there is mail to show or when they are turned on.

use super::{
    Act, Alert, AlertId, Availability, Badge, Controls, Failure, FixedWords, HostFuture,
    NotificationHost, Response, parse_payload, payload,
};
use block2::RcBlock;
#[cfg(target_os = "macos")]
use objc2::MainThreadMarker;
use objc2::{
    AnyThread, define_class, msg_send,
    rc::Retained,
    runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject},
};
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSWorkspace};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSSet, NSString};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSBundle, NSURL};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNErrorCode, UNErrorDomain,
    UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSettings, UNNotificationSound,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use std::{
    cell::OnceCell,
    future::Future,
    ptr::NonNull,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::oneshot;

/// Whether the system refused this copy of the app outright, as macOS does
/// for one outside a proper bundle. It then answers the permission request
/// with an error, while its settings still say nobody was asked.
static REFUSED: AtomicBool = AtomicBool::new(false);

/// Permission requests waiting for their answer. Apple doesn't say what a
/// request answers while an earlier one's prompt is still up, so only a
/// request sent while none was waiting can show a refusal.
static ASKING: AtomicUsize = AtomicUsize::new(0);

/// Note an answer saying that macOS refuses the app's notifications, and
/// show it in Settings right away rather than at the window's next focus.
fn note_refusal(error: Option<&NSError>) {
    let refused = error.is_some_and(|error| {
        error.code() == UNErrorCode::NotificationsNotAllowed.0
            // SAFETY: the framework's own constant, set when it loads.
            && unsafe { UNErrorDomain }.is_some_and(|domain| error.domain().isEqualToString(domain))
    });
    if refused && !REFUSED.swap(true, Ordering::Relaxed) {
        let _ = slint::invoke_from_event_loop(super::ui::refresh_availability);
    }
}

/// The bundle the framework knows the app by (platform/macos/Info.plist.in).
#[cfg(target_os = "macos")]
const BUNDLE_ID: &str = "com.flectar.mail";
/// Where a notification keeps what a click on it opens.
const ROUTE_KEY: &str = "route";
/// The button sets of mail notifications: the second button archives where
/// the account can, and deletes otherwise.
const ARCHIVE_CATEGORY: &str = "mail-archive";
const DELETE_CATEGORY: &str = "mail-delete";

thread_local! {
    /// The center keeps only a weak reference to its delegate.
    static DELEGATE: OnceCell<Retained<Delegate>> = const { OnceCell::new() };
}

pub(crate) fn host() -> Option<Arc<dyn NotificationHost>> {
    #[cfg(target_os = "macos")]
    if !in_app_bundle() {
        return None;
    }
    let delegate = DELEGATE.with(|delegate| delegate.get_or_init(Delegate::new).clone());
    UNUserNotificationCenter::currentNotificationCenter()
        .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    Some(Arc::new(Host))
}

/// macOS launches the app for a click like any other launch, and hands the
/// click to the delegate once it has finished launching.
#[cfg(target_os = "macos")]
pub(crate) fn launched_for_click() -> bool {
    false
}

/// Whether this process runs from the Flectar Mail app bundle.
#[cfg(target_os = "macos")]
fn in_app_bundle() -> bool {
    let bundle = NSBundle::mainBundle();
    bundle
        .bundleIdentifier()
        .is_some_and(|identifier| identifier.to_string() == BUNDLE_ID)
        && bundle.bundlePath().to_string().ends_with(".app")
}

struct Host;

/// Call `start` now with a completion that sends its answer, and wait for
/// that. The framework's objects stay in `start`: they may not cross threads,
/// and the future does.
fn answer<T: Send + 'static>(
    gone: T,
    start: impl FnOnce(Box<dyn Fn(T) + Send + Sync>),
) -> impl Future<Output = T> + Send {
    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    start(Box::new(move |value| {
        if let Some(sender) = sender.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = sender.send(value);
        }
    }));
    async move { receiver.await.unwrap_or(gone) }
}

impl NotificationHost for Host {
    fn show(&self, alerts: Vec<Alert>) -> HostFuture<Vec<Result<(), Failure>>> {
        Box::pin(async move {
            let mut shown = Vec::with_capacity(alerts.len());
            for alert in &alerts {
                shown.push(show(alert).await);
            }
            shown
        })
    }

    fn withdraw(&self, ids: Vec<AlertId>) -> HostFuture<()> {
        let identifiers = ids
            .into_iter()
            .map(|id| NSString::from_str(&id.key()))
            .collect::<Vec<_>>();
        UNUserNotificationCenter::currentNotificationCenter()
            .removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(
                &identifiers,
            ));
        Box::pin(std::future::ready(()))
    }

    fn availability(&self) -> HostFuture<Availability> {
        Box::pin(answer(Availability::Unavailable, |done| {
            let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
                // SAFETY: the framework passes a valid settings object for
                // the block's call.
                let status = unsafe { settings.as_ref() }.authorizationStatus();
                done(match availability(status) {
                    // Nobody can be asked about an app macOS refuses.
                    Availability::NotAsked if REFUSED.load(Ordering::Relaxed) => {
                        Availability::Refused
                    }
                    availability => availability,
                });
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .getNotificationSettingsWithCompletionHandler(&block);
        }))
    }

    fn request_permission(&self) -> HostFuture<Availability> {
        let alone = ASKING.fetch_add(1, Ordering::Relaxed) == 0;
        let asked = answer(false, move |done| {
            let block = RcBlock::new(move |granted: Bool, error: *mut NSError| {
                ASKING.fetch_sub(1, Ordering::Relaxed);
                if alone {
                    // SAFETY: a non-null error is a valid NSError for the call.
                    note_refusal(unsafe { error.as_ref() });
                }
                done(granted.as_bool());
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .requestAuthorizationWithOptions_completionHandler(
                    // The system shows the app icon's count only for an app
                    // allowed badges; without asking for them, no switch
                    // appears in its settings.
                    UNAuthorizationOptions::Alert
                        | UNAuthorizationOptions::Sound
                        | UNAuthorizationOptions::Badge,
                    &block,
                );
        });
        Box::pin(async move {
            // The answer to the request tells only whether alerts are on; the
            // settings say the rest.
            asked.await;
            Host.availability().await
        })
    }

    fn controls(&self) -> Controls {
        Controls {
            settings: true,
            sound: true,
            badge: true,
        }
    }

    /// The Dock tile's badge, with the count's digits grouped as the UI
    /// groups them. AppKit wants its own thread: the window's while the app
    /// runs, and this one as it quits, once that loop has ended.
    #[cfg(target_os = "macos")]
    fn set_badge(&self, badge: Option<Badge>) -> HostFuture<()> {
        let apply = move || {
            let Some(main_thread) = MainThreadMarker::new() else {
                return;
            };
            let label = badge.map(|badge| {
                NSString::from_str(&crate::number_format::count(u64::from(badge.count)))
            });
            NSApplication::sharedApplication(main_thread)
                .dockTile()
                .setBadgeLabel(label.as_deref());
        };
        if MainThreadMarker::new().is_some() {
            apply();
        } else {
            let _ = slint::invoke_from_event_loop(apply);
        }
        Box::pin(std::future::ready(()))
    }

    /// The count on the home screen icon, which the system formats.
    #[cfg(not(target_os = "macos"))]
    fn set_badge(&self, badge: Option<Badge>) -> HostFuture<()> {
        let count = badge.map_or(0, |badge| badge.count);
        Box::pin(answer((), move |done| {
            let block = RcBlock::new(move |error: *mut NSError| {
                // SAFETY: a non-null error is a valid NSError for the call.
                if let Some(error) = unsafe { error.as_ref() } {
                    tracing::debug!(
                        error = %error.localizedDescription(),
                        "notifications: could not show the count on the app icon"
                    );
                }
                done(());
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .setBadgeCount_withCompletionHandler(
                    objc2_foundation::NSInteger::try_from(count)
                        .unwrap_or(objc2_foundation::NSInteger::MAX),
                    Some(&block),
                );
        }))
    }

    /// The app's page of Notifications in System Settings. macOS has no
    /// documented link; this is the one Chromium opens, tested by its authors
    /// on macOS 13 to 26.
    #[cfg(target_os = "macos")]
    fn open_settings(&self) {
        let link = NSString::from_str(&format!(
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={BUNDLE_ID}"
        ));
        if let Some(url) = NSURL::URLWithString(&link) {
            NSWorkspace::sharedWorkspace().openURL(&url);
        }
    }

    /// The app's notification settings, where iOS keeps them.
    #[cfg(not(target_os = "macos"))]
    fn open_settings(&self) {
        crate::platform::open_notification_settings();
    }

    fn set_words(&self, words: &FixedWords) {
        let action = |act: Act, title: &str, options: UNNotificationActionOptions| {
            UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str(act.key()),
                &NSString::from_str(title),
                options,
            )
        };
        let category = |identifier: &str, second: Retained<UNNotificationAction>| {
            let actions = NSArray::from_retained_slice(&[
                action(
                    Act::MarkRead,
                    &words.mark_read,
                    UNNotificationActionOptions::empty(),
                ),
                second,
            ]);
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(identifier),
                &actions,
                &NSArray::new(),
                UNNotificationCategoryOptions::empty(),
            )
        };
        // A phone's lock screen shows the buttons too: moving mail away
        // waits for whoever holds it to unlock it.
        #[cfg(target_os = "macos")]
        let moves = UNNotificationActionOptions::empty();
        #[cfg(not(target_os = "macos"))]
        let moves = UNNotificationActionOptions::AuthenticationRequired;
        let categories = NSSet::from_retained_slice(&[
            category(
                ARCHIVE_CATEGORY,
                action(Act::Archive, &words.archive, moves),
            ),
            category(
                DELETE_CATEGORY,
                action(
                    Act::Delete,
                    &words.delete,
                    UNNotificationActionOptions::Destructive | moves,
                ),
            ),
        ]);
        // Replaces what was registered: the categories of notifications from
        // an earlier run keep their buttons, in the new language.
        UNUserNotificationCenter::currentNotificationCenter()
            .setNotificationCategories(&categories);
    }
}

fn availability(status: UNAuthorizationStatus) -> Availability {
    match status {
        UNAuthorizationStatus::Authorized
        | UNAuthorizationStatus::Provisional
        | UNAuthorizationStatus::Ephemeral => Availability::Allowed,
        UNAuthorizationStatus::NotDetermined => Availability::NotAsked,
        UNAuthorizationStatus::Denied => Availability::Denied,
        _ => Availability::Unavailable,
    }
}

fn show(alert: &Alert) -> impl Future<Output = Result<(), Failure>> + Send {
    let added = answer(
        Some("the notification center did not answer".to_owned()),
        |done| {
            let block = RcBlock::new(move |error: *mut NSError| {
                // SAFETY: a non-null error is a valid NSError for the call.
                done(
                    unsafe { error.as_ref() }.map(|error| error.localizedDescription().to_string()),
                );
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(&request(alert), Some(&block));
        },
    );
    async move {
        match added.await {
            None => Ok(()),
            // Denied, or a prompt still waiting for its answer: settled as
            // not allowed, so mail doesn't pile up behind it.
            Some(error) => Err(Failure::Unavailable(error)),
        }
    }
}

/// A notification request for `alert`, under its stable identifier so it can
/// be replaced and taken back.
fn request(alert: &Alert) -> Retained<UNNotificationRequest> {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&alert.title));
    match alert
        .preview
        .as_deref()
        .filter(|preview| !preview.is_empty())
    {
        Some(preview) => {
            content.setSubtitle(&NSString::from_str(&alert.body));
            content.setBody(&NSString::from_str(preview));
        }
        None => content.setBody(&NSString::from_str(&summary_body(alert))),
    }
    // macOS groups a mail app's notifications by account.
    content.setThreadIdentifier(&NSString::from_str(&format!(
        "account-{}",
        alert.id.account_id()
    )));
    if alert.actions.iter().any(|(act, _)| *act == Act::Archive) {
        content.setCategoryIdentifier(&NSString::from_str(ARCHIVE_CATEGORY));
    } else if !alert.actions.is_empty() {
        content.setCategoryIdentifier(&NSString::from_str(DELETE_CATEGORY));
    }
    if alert.sound {
        content.setSound(Some(&UNNotificationSound::defaultSound()));
    }
    let route = NSDictionary::from_retained_objects(
        &[&*NSString::from_str(ROUTE_KEY)],
        &[NSString::from_str(&payload(alert.open, Act::Open))],
    );
    // SAFETY: the dictionary holds only strings, which the framework stores
    // as property-list values.
    unsafe {
        content.setUserInfo(&Retained::cast_unchecked::<NSDictionary>(route));
    }
    UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&alert.id.key()),
        &content,
        None,
    )
}

/// A summary names the account it sums up when there are several.
fn summary_body(alert: &Alert) -> String {
    match (alert.id, &alert.account) {
        (AlertId::Summary { .. }, Some(account)) => format!("{}\n{account}", alert.body),
        _ => alert.body.clone(),
    }
}

/// What a click on a notification, or one of its buttons, asks for.
fn response(response: &UNNotificationResponse) -> Option<Response> {
    let action = response.actionIdentifier();
    // SAFETY: the framework exports the identifier as a constant string.
    let act = if &*action == unsafe { UNNotificationDefaultActionIdentifier } {
        Act::Open
    } else {
        Act::parse(&action.to_string())?
    };
    let info = response.notification().request().content().userInfo();
    let value = info.objectForKey(&NSString::from_str(ROUTE_KEY))?;
    let route = value.downcast::<NSString>().ok()?.to_string();
    let (route, _) = parse_payload(&route)?;
    Some(Response {
        route,
        act,
        activation_token: None,
    })
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class keeps
    // no state.
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    #[name = "FlectarMailNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// A notification while the app is frontmost: the dispatcher already
        /// left out mail the window shows, so show the rest as usual.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion_handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        /// A click or a button. Where the system suspends the app once told
        /// the response was handled, it is told only once a button's action
        /// is done.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &block2::DynBlock<dyn Fn()>,
        ) {
            let Some(response) = self::response(response) else {
                completion_handler.call(());
                return;
            };
            #[cfg(background_checks)]
            {
                let completion = Completion(completion_handler.copy());
                super::respond_then(response, move || completion.call());
            }
            #[cfg(not(background_checks))]
            {
                super::respond(response);
                completion_handler.call(());
            }
        }
    }
);

/// The system's completion handler for a response, called once the app has
/// handled it.
#[cfg(background_checks)]
struct Completion(RcBlock<dyn Fn()>);

// SAFETY: the block is the framework's completion handler, which it lets the
// app call from any thread; copying and releasing a block is thread-safe.
#[cfg(background_checks)]
unsafe impl Send for Completion {}

#[cfg(background_checks)]
impl Completion {
    fn call(self) {
        self.0.call(());
    }
}

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}
