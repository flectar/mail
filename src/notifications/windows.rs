//! New-mail notifications on Windows: toasts, kept in the notification
//! center.
//!
//! A toast belongs to an app id (AUMID) the system knows, with a COM class
//! Windows starts the app through when the toast or one of its buttons is
//! clicked, also after the app has quit. The Store package declares both in
//! its manifest. The installer puts them on the installed copy's Start menu
//! shortcut and registers the class, the way Microsoft documents it for
//! desktop apps. A portable copy has no shortcut; it registers its app id and
//! class for the current user, as Windows App SDK registers unpackaged apps.
//! Each kind of copy has a class of its own: a packaged class would take over
//! an unpackaged one's clicks.
//!
//! COM calls the class on a thread of the multithreaded apartment, which this
//! module owns; winit keeps the window's thread in the single-threaded one.
//! The toasts are shown from that thread too.

use super::{
    Act, Alert, AlertId, Availability, Controls, Failure, HostFuture, NotificationHost, Response,
    parse_payload, payload, respond,
};
use crate::platform::windows_install::{command_executable, package_family_name, same_file};
use std::{
    ffi::c_void,
    sync::{Arc, mpsc},
};
use tokio::sync::oneshot;
use windows::{
    Data::Xml::Dom::XmlDocument,
    UI::Notifications::{
        NotificationSetting, ToastNotification, ToastNotificationHistory, ToastNotificationManager,
        ToastNotifier,
    },
    Win32::{
        Foundation::{CLASS_E_NOAGGREGATION, E_POINTER},
        System::Com::{
            CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoInitializeEx, CoRegisterClassObject,
            IClassFactory, IClassFactory_Impl, REGCLS_MULTIPLEUSE,
        },
        UI::{
            Notifications::{
                INotificationActivationCallback, INotificationActivationCallback_Impl,
                NOTIFICATION_USER_INPUT_DATA,
            },
            Shell::SetCurrentProcessExplicitAppUserModelID,
        },
    },
    core::{BOOL, GUID, HSTRING, IUnknown, Interface, PCWSTR, Ref, implement},
};

/// The installed copy's app id, on its process, its Start menu shortcut and
/// its toasts (platform/windows/installer.iss).
const INSTALLED_AUMID: &str = "Flectar.FlectarMail";
/// A portable copy's app id, apart from the installed copy's.
const PORTABLE_AUMID: &str = "Flectar.FlectarMail.Portable";
/// The `Application Id` in platform/windows/msix/AppxManifest.xml.in.
const PACKAGE_APPLICATION_ID: &str = "FlectarMail";
/// The activator class the installer registers.
const INSTALLED_ACTIVATOR: GUID = GUID::from_u128(0xED445996_CA23_4190_9817_4AABA2693036);
/// The activator class a portable copy registers for itself.
const PORTABLE_ACTIVATOR: GUID = GUID::from_u128(0x41EF731B_ECFA_4F1F_8DB4_9B7B00AD4804);
/// The activator class the Store package declares.
const PACKAGED_ACTIVATOR: GUID = GUID::from_u128(0x6D5AB7B0_F5FC_4A63_B767_DB4CE788E699);
/// What COM starts the app with for a click while it isn't running, before
/// its own `-Embedding`.
const ACTIVATION_ARGUMENT: &str = "--toast-activated";
/// The new-mail sound of the user's sound scheme.
const MAIL_SOUND: &str = "ms-winsoundevent:Notification.Mail";

/// How this copy of Flectar Mail is installed, which decides who registers
/// its app id and activator.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Install {
    /// The Microsoft Store package; its manifest registers both.
    Packaged { aumid: String },
    /// Installed by the setup program, which registered both.
    Installed,
    /// Run from the portable ZIP; it registers both itself.
    Portable,
}

impl Install {
    fn of_this_process() -> Self {
        if let Some(family) = package_family_name() {
            return Self::Packaged {
                aumid: format!("{family}!{PACKAGE_APPLICATION_ID}"),
            };
        }
        let installed = std::env::current_exe().ok().is_some_and(|executable| {
            registered_server(INSTALLED_ACTIVATOR)
                .is_some_and(|registered| same_file(&registered, &executable))
        });
        if installed {
            Self::Installed
        } else {
            Self::Portable
        }
    }

    fn aumid(&self) -> &str {
        match self {
            Self::Packaged { aumid } => aumid,
            Self::Installed => INSTALLED_AUMID,
            Self::Portable => PORTABLE_AUMID,
        }
    }

    fn activator(&self) -> GUID {
        match self {
            Self::Packaged { .. } => PACKAGED_ACTIVATOR,
            Self::Installed => INSTALLED_ACTIVATOR,
            Self::Portable => PORTABLE_ACTIVATOR,
        }
    }
}

/// Whether COM started this process for a click on a toast. The click
/// itself arrives through the activator once it is registered; until then
/// the process starts like a sign-in launch, so a button needs no window.
pub(crate) fn launched_for_click() -> bool {
    std::env::args_os()
        .skip(1)
        .any(|argument| argument == ACTIVATION_ARGUMENT)
}

/// Whether COM passed `argument` to start the app for a click: ours, or its
/// own `-Embedding`.
pub(crate) fn click_argument(argument: &std::ffi::OsStr) -> bool {
    argument == ACTIVATION_ARGUMENT
        || argument.eq_ignore_ascii_case("-Embedding")
        || argument.eq_ignore_ascii_case("/Embedding")
}

pub(crate) fn host() -> Option<Arc<dyn NotificationHost>> {
    let install = Install::of_this_process();
    if !matches!(install, Install::Packaged { .. }) {
        // Before any window: its taskbar button and the toasts share the
        // app id of the Start menu shortcut.
        // SAFETY: the argument is a NUL-terminated string that outlives the
        // call.
        if let Err(error) =
            unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(install.aumid())) }
        {
            tracing::warn!(%error, "notifications: could not set the app id");
        }
    }
    if install == Install::Portable
        && let Err(error) = register_portable()
    {
        tracing::warn!(%error, "notifications: could not register the portable copy");
    }
    let (jobs, inbox) = mpsc::channel::<Job>();
    let thread_install = install.clone();
    let started = std::thread::Builder::new()
        .name("flectar-notifications".into())
        .spawn(move || serve(&thread_install, &inbox));
    if let Err(error) = started {
        tracing::warn!(%error, "notifications: could not start the toast thread");
        return None;
    }
    Some(Arc::new(Host { jobs }))
}

/// Work for the toast thread, with where its answer goes.
enum Job {
    Show(Vec<Alert>, oneshot::Sender<Vec<Result<(), Failure>>>),
    Withdraw(Vec<AlertId>, oneshot::Sender<()>),
    Availability(oneshot::Sender<Availability>),
}

struct Host {
    jobs: mpsc::Sender<Job>,
}

impl Host {
    fn ask<T: Send + 'static>(
        &self,
        job: impl FnOnce(oneshot::Sender<T>) -> Job,
        gone: T,
    ) -> HostFuture<T> {
        let (answer, answered) = oneshot::channel();
        let sent = self.jobs.send(job(answer)).is_ok();
        Box::pin(async move {
            if !sent {
                return gone;
            }
            answered.await.unwrap_or(gone)
        })
    }
}

impl NotificationHost for Host {
    fn show(&self, alerts: Vec<Alert>) -> HostFuture<Vec<Result<(), Failure>>> {
        let gone = vec![Err(Failure::Retry("the toast thread stopped".into())); alerts.len()];
        self.ask(|answer| Job::Show(alerts, answer), gone)
    }

    fn withdraw(&self, ids: Vec<AlertId>) -> HostFuture<()> {
        self.ask(|answer| Job::Withdraw(ids, answer), ())
    }

    fn availability(&self) -> HostFuture<Availability> {
        self.ask(Job::Availability, Availability::Unavailable)
    }

    fn controls(&self) -> Controls {
        Controls {
            settings: true,
            sound: true,
            badge: false,
        }
    }

    fn open_settings(&self) {
        use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
        let page: Vec<u16> = "ms-settings:notifications\0".encode_utf16().collect();
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        // SAFETY: both strings are NUL-terminated and outlive the call; the
        // other arguments may be null.
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                page.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}

/// The toast thread: joins the multithreaded apartment, takes clicks from
/// now on, and then shows and withdraws toasts as asked.
fn serve(install: &Install, jobs: &mpsc::Receiver<Job>) {
    // SAFETY: called once on this thread, before any other COM use on it.
    let joined = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if let Err(error) = joined.ok() {
        tracing::warn!(%error, "notifications: could not start COM");
        return;
    }
    let factory: IClassFactory = Factory.into();
    // SAFETY: the class id outlives the call, and COM keeps its own reference
    // to the factory for the life of the process.
    let registered = unsafe {
        CoRegisterClassObject(
            &install.activator(),
            &factory,
            CLSCTX_LOCAL_SERVER,
            REGCLS_MULTIPLEUSE,
        )
    };
    if let Err(error) = registered {
        tracing::warn!(%error, "notifications: could not take clicks on toasts");
    }
    let toasts = Toasts::new(install);
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Show(alerts, answer) => {
                let _ = answer.send(toasts.show(&alerts));
            }
            Job::Withdraw(ids, answer) => {
                for id in ids {
                    toasts.withdraw(id);
                }
                let _ = answer.send(());
            }
            Job::Availability(answer) => {
                let _ = answer.send(toasts.availability());
            }
        }
    }
}

/// Toasts under this copy's app id.
struct Toasts {
    /// `None` for the Store package, which uses the calls without an id.
    aumid: Option<HSTRING>,
}

impl Toasts {
    fn new(install: &Install) -> Self {
        Self {
            aumid: match install {
                Install::Packaged { .. } => None,
                install => Some(HSTRING::from(install.aumid())),
            },
        }
    }

    fn notifier(&self) -> windows::core::Result<ToastNotifier> {
        match &self.aumid {
            Some(aumid) => ToastNotificationManager::CreateToastNotifierWithId(aumid),
            None => ToastNotificationManager::CreateToastNotifier(),
        }
    }

    fn availability(&self) -> Availability {
        if elevated() {
            // Windows shows no toasts from a process run as administrator.
            return Availability::Unavailable;
        }
        match self.notifier().and_then(|notifier| notifier.Setting()) {
            Ok(NotificationSetting::Enabled) => Availability::Allowed,
            Ok(
                NotificationSetting::DisabledForApplication | NotificationSetting::DisabledForUser,
            ) => Availability::Denied,
            Ok(_) | Err(_) => Availability::Unavailable,
        }
    }

    /// Show a batch's toasts, reading the setting once for all of them.
    fn show(&self, alerts: &[Alert]) -> Vec<Result<(), Failure>> {
        // Microsoft asks apps to read the setting rather than wait for a
        // failure: Show doesn't fail while notifications are off.
        let refused = match self.availability() {
            Availability::Allowed => None,
            Availability::Denied => Some("notifications are turned off in Windows settings"),
            Availability::NotAsked | Availability::Unavailable | Availability::Refused => {
                Some("Windows shows no notifications for this app")
            }
        };
        if let Some(reason) = refused {
            return vec![Err(Failure::Unavailable(reason.into())); alerts.len()];
        }
        let notifier = self.notifier();
        alerts
            .iter()
            .map(|alert| {
                let shown = (|| -> windows::core::Result<()> {
                    let notifier = notifier.as_ref().map_err(Clone::clone)?;
                    let document = XmlDocument::new()?;
                    document.LoadXml(&HSTRING::from(toast_xml(alert)))?;
                    let toast = ToastNotification::CreateToastNotification(&document)?;
                    let (tag, group) = tag_and_group(alert.id);
                    toast.SetTag(&HSTRING::from(tag))?;
                    toast.SetGroup(&HSTRING::from(group))?;
                    notifier.Show(&toast)
                })();
                shown.map_err(|error| Failure::Retry(error.message()))
            })
            .collect()
    }

    fn withdraw(&self, id: AlertId) {
        let (tag, group) = tag_and_group(id);
        let (tag, group) = (HSTRING::from(tag), HSTRING::from(group));
        let removed =
            ToastNotificationManager::History().and_then(|history: ToastNotificationHistory| {
                match &self.aumid {
                    Some(aumid) => history.RemoveGroupedTagWithId(&tag, &group, aumid),
                    None => history.RemoveGroupedTag(&tag, &group),
                }
            });
        if let Err(error) = removed {
            tracing::debug!(%error, "notifications: could not withdraw a toast");
        }
    }
}

/// A toast's tag and group, each under the 64 characters Windows allows: the
/// group per account, as the notification center groups them.
fn tag_and_group(id: AlertId) -> (String, String) {
    let tag = match id {
        AlertId::Message { message_id, .. } => format!("message-{message_id}"),
        AlertId::Summary { .. } => "summary".to_owned(),
    };
    (tag, format!("account-{}", id.account_id()))
}

/// The toast's XML. Text goes in escaped, so nothing a sender wrote is read
/// as markup.
fn toast_xml(alert: &Alert) -> String {
    let launch = payload(alert.open, Act::Open);
    let mut xml = format!(
        r#"<toast launch="{}" displayTimestamp="{}">"#,
        xml_escape(&launch),
        timestamp(alert.received_at),
    );
    if let Some(account) = &alert.account {
        let inbox = super::Route {
            account_id: alert.id.account_id(),
            thread_id: None,
            message_id: None,
        };
        xml.push_str(&format!(
            r#"<header id="account-{}" title="{}" arguments="{}"/>"#,
            alert.id.account_id(),
            xml_escape(account),
            xml_escape(&payload(inbox, Act::Open)),
        ));
    }
    xml.push_str(r#"<visual><binding template="ToastGeneric">"#);
    xml.push_str(&format!(
        r#"<text hint-maxLines="1">{}</text><text>{}</text>"#,
        xml_escape(&alert.title),
        xml_escape(&alert.body),
    ));
    if let Some(preview) = alert
        .preview
        .as_deref()
        .filter(|preview| !preview.is_empty())
    {
        xml.push_str(&format!("<text>{}</text>", xml_escape(preview)));
    }
    xml.push_str("</binding></visual>");
    if !alert.actions.is_empty() {
        xml.push_str("<actions>");
        for (act, label) in &alert.actions {
            xml.push_str(&format!(
                r#"<action content="{}" arguments="{}" activationType="background"/>"#,
                xml_escape(label),
                xml_escape(&payload(alert.open, *act)),
            ));
        }
        xml.push_str("</actions>");
    }
    if alert.sound {
        xml.push_str(&format!(r#"<audio src="{MAIL_SOUND}"/>"#));
    } else {
        xml.push_str(r#"<audio silent="true"/>"#);
    }
    xml.push_str("</toast>");
    xml
}

/// When the mail arrived, as the ISO 8601 time Windows shows on the toast.
fn timestamp(received_at: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(received_at)
        .unwrap_or_else(chrono::Utc::now)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Escape text for XML, dropping the characters XML 1.0 cannot hold.
fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\t' | '\n' | '\r' => escaped.push(character),
            character if character < ' ' || matches!(character, '\u{FFFE}' | '\u{FFFF}') => {}
            character => escaped.push(character),
        }
    }
    escaped
}

/// Hands a click on a toast, or one of its buttons, to the app: COM calls it
/// on the toast thread, in this process or in one COM started for it.
#[implement(INotificationActivationCallback)]
struct Activator;

impl INotificationActivationCallback_Impl for Activator_Impl {
    fn Activate(
        &self,
        _appusermodelid: &PCWSTR,
        invokedargs: &PCWSTR,
        _data: *const NOTIFICATION_USER_INPUT_DATA,
        _count: u32,
    ) -> windows::core::Result<()> {
        if invokedargs.is_null() {
            return Ok(());
        }
        // SAFETY: COM passes a NUL-terminated string valid for the call.
        let arguments = unsafe { invokedargs.to_string() }.unwrap_or_default();
        if let Some((route, act)) = parse_payload(&arguments) {
            respond(Response {
                route,
                act,
                activation_token: None,
            });
        }
        Ok(())
    }
}

/// Makes the activator for COM.
#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        interface: *const GUID,
        object: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if object.is_null() || interface.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: COM passes a writable pointer for the object.
        unsafe { *object = std::ptr::null_mut() };
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activator: INotificationActivationCallback = Activator.into();
        // SAFETY: both pointers were checked above and COM keeps them valid
        // for the call.
        unsafe { activator.query(interface, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

/// The app icon a portable copy's toasts show, written where the registry
/// can point at it.
const PORTABLE_ICON: &[u8] = include_bytes!("../../resources/app-icon/flectar-mail-window.png");

/// Register a portable copy's app id and activator for the current user,
/// pointing at this executable: what the installer does for an installed
/// copy, and Windows App SDK for unpackaged apps. Runs at every start, so a
/// copy that moved registers its new place.
fn register_portable() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let activator = guid_text(PORTABLE_ACTIVATOR);
    let app = windows_registry::CURRENT_USER
        .create(format!(r"Software\Classes\AppUserModelId\{PORTABLE_AUMID}"))
        .map_err(|error| error.to_string())?;
    app.set_expand_string("DisplayName", "Flectar Mail")
        .and_then(|()| app.set_string("CustomActivator", &activator))
        .map_err(|error| error.to_string())?;
    if let Some(icon) = portable_icon() {
        app.set_expand_string("IconUri", icon.to_string_lossy())
            .map_err(|error| error.to_string())?;
    }
    windows_registry::CURRENT_USER
        .create(format!(r"Software\Classes\CLSID\{activator}\LocalServer32"))
        .and_then(|key| {
            key.set_string(
                "",
                format!(r#""{}" {ACTIVATION_ARGUMENT}"#, executable.display()),
            )
        })
        .map_err(|error| error.to_string())
}

/// The icon file for a portable copy's toasts, in the user's local app data.
fn portable_icon() -> Option<std::path::PathBuf> {
    let directory =
        std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("flectar-mail");
    let icon = directory.join("notification-icon.png");
    let current = std::fs::read(&icon).is_ok_and(|written| written == PORTABLE_ICON);
    if !current {
        std::fs::create_dir_all(&directory).ok()?;
        std::fs::write(&icon, PORTABLE_ICON).ok()?;
    }
    Some(icon)
}

/// The executable a registered activator class starts, from the user's or
/// the machine's classes.
fn registered_server(class: GUID) -> Option<std::path::PathBuf> {
    let command = windows_registry::CLASSES_ROOT
        .open(format!(r"CLSID\{}\LocalServer32", guid_text(class)))
        .and_then(|key| key.get_string(""))
        .ok()?;
    command_executable(&command)
}

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`, as the registry writes classes.
fn guid_text(guid: GUID) -> String {
    format!("{{{guid:?}}}")
}

/// Whether the process runs as administrator.
fn elevated() -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle of the current process needs no closing, and
    // `token` receives a handle closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut length = 0_u32;
    // SAFETY: `elevation` is writable for the size passed.
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(0),
            &mut length,
        )
    };
    // SAFETY: `token` is the handle OpenProcessToken returned.
    unsafe { CloseHandle(token) };
    read != 0 && elevation.TokenIsElevated != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(account: Option<&str>, sound: bool) -> Alert {
        Alert {
            id: AlertId::Message {
                account_id: 2,
                message_id: 70,
            },
            title: "Ann & Bo".into(),
            body: "<Plans>".into(),
            preview: Some("See you \u{1}there".into()),
            account: account.map(str::to_owned),
            received_at: 1_762_590_900_000,
            sound,
            open: super::super::Route {
                account_id: 2,
                thread_id: Some(7),
                message_id: Some(70),
            },
            actions: vec![(Act::MarkRead, "Mark as read".into())],
        }
    }

    #[test]
    fn toasts_escape_what_senders_write() {
        let xml = toast_xml(&alert(None, true));
        assert!(xml.contains(r#"<text hint-maxLines="1">Ann &amp; Bo</text>"#));
        assert!(xml.contains("<text>&lt;Plans&gt;</text>"));
        // XML 1.0 has no place for control characters.
        assert!(xml.contains("<text>See you there</text>"));
        assert!(xml.contains(r#"displayTimestamp="2025-11-08T08:35:00Z""#));
        assert!(xml.contains(&format!(r#"<audio src="{MAIL_SOUND}"/>"#)));
        assert!(!xml.contains("<header"));
        assert!(xml.contains(r#"activationType="background""#));
    }

    #[test]
    fn several_accounts_get_headers_and_quiet_toasts_stay_silent() {
        let xml = toast_xml(&alert(Some("work@example.com"), false));
        assert!(xml.contains(r#"<header id="account-2" title="work@example.com""#));
        assert!(xml.contains(r#"<audio silent="true"/>"#));
    }

    #[test]
    fn tags_and_groups_fit_windows_limits() {
        let (tag, group) = tag_and_group(AlertId::Message {
            account_id: i64::MAX,
            message_id: i64::MAX,
        });
        assert!(tag.len() <= 64 && group.len() <= 64);
        assert_eq!(
            tag_and_group(AlertId::Summary { account_id: 3 }),
            ("summary".into(), "account-3".into())
        );
    }

    #[test]
    fn classes_are_written_as_the_registry_expects() {
        assert_eq!(
            guid_text(INSTALLED_ACTIVATOR),
            "{ED445996-CA23-4190-9817-4AABA2693036}"
        );
    }
}
