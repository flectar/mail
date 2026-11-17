//! New-mail notifications on Linux.
//!
//! The Flatpak shows them through the notification portal, which the sandbox
//! reaches without any extra permission; Flathub requires the portal where one
//! exists. Other builds (deb, rpm, AppImage) talk to the desktop's notification
//! server directly, as GLib, libnotify and KNotification do outside a sandbox:
//! the portal tells host apps apart reliably only since xdg-desktop-portal
//! 1.20, and GNOME's portal backend plays no sound.
//!
//! Both use the session bus connection ashpd keeps for the other portals, so
//! notifications add no connection and no thread where a portal runs. So does
//! the count on the app's icon.

use super::{
    Alert, AlertId, Availability, Badge, Controls, Failure, FixedWords, HostFuture,
    NotificationHost,
};
use ashpd::zbus;
use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

/// How long the notification service may take to answer a call.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// The desktop entry, and the application id the desktop knows the app by.
const APP_ID: &str = "com.flectar.mail";

pub(crate) fn host() -> Option<Arc<dyn NotificationHost>> {
    Some(Arc::new(Host(Arc::default())))
}

/// Clicks reach a running app over D-Bus; nothing starts it for one.
pub(crate) fn launched_for_click() -> bool {
    false
}

struct Host(Arc<Shared>);

/// Connects on first use, from the runtime, and again after a failure: a
/// notification server may start after the app does.
#[derive(Default)]
struct Shared {
    service: tokio::sync::Mutex<Option<Arc<imp::Service>>>,
    words: Arc<Mutex<FixedWords>>,
    launcher: badge::Launcher,
}

impl Shared {
    async fn service(&self) -> Option<Arc<imp::Service>> {
        let mut service = self.service.lock().await;
        if service.is_none() {
            *service = imp::Service::connect(Arc::clone(&self.words))
                .await
                .map(Arc::new);
        }
        service.clone()
    }
}

impl NotificationHost for Host {
    fn show(&self, alerts: Vec<Alert>) -> HostFuture<Vec<Result<(), Failure>>> {
        let shared = Arc::clone(&self.0);
        Box::pin(async move {
            let Some(service) = shared.service().await else {
                return vec![
                    Err(Failure::Unavailable("no notification service".into()));
                    alerts.len()
                ];
            };
            let mut shown = Vec::with_capacity(alerts.len());
            for alert in &alerts {
                shown.push(service.show(alert).await);
            }
            shown
        })
    }

    fn withdraw(&self, ids: Vec<AlertId>) -> HostFuture<()> {
        let shared = Arc::clone(&self.0);
        Box::pin(async move {
            if let Some(service) = shared.service().await {
                for id in ids {
                    service.withdraw(id).await;
                }
            }
        })
    }

    fn availability(&self) -> HostFuture<Availability> {
        let shared = Arc::clone(&self.0);
        Box::pin(async move {
            if shared.service().await.is_some() {
                Availability::Allowed
            } else {
                Availability::Unavailable
            }
        })
    }

    fn set_words(&self, words: &FixedWords) {
        *self.0.words.lock().unwrap_or_else(PoisonError::into_inner) = words.clone();
    }

    /// GNOME keeps portal notifications after the app exits, and a click on
    /// one then does nothing: the Flatpak isn't started over D-Bus. GNOME
    /// drops a notification server's own at exit.
    fn withdraw_at_exit(&self) -> bool {
        cfg!(feature = "flatpak")
    }

    /// Desktops keep no per-app notification page an app can open. The
    /// portal's backends decide on sound themselves: GNOME's and KDE's play
    /// none for portal notifications.
    fn controls(&self) -> Controls {
        Controls {
            settings: false,
            sound: !cfg!(feature = "flatpak"),
            badge: true,
        }
    }

    fn set_badge(&self, badge: Option<Badge>) -> HostFuture<()> {
        let shared = Arc::clone(&self.0);
        Box::pin(async move { shared.launcher.show(badge.map(|badge| badge.count)).await })
    }
}

/// The session bus connection ashpd shares with the other portals, or a new
/// one where no portal runs.
async fn session_bus() -> Option<zbus::Connection> {
    let portal =
        tokio::time::timeout(CALL_TIMEOUT, ashpd::desktop::settings::Settings::new()).await;
    match portal {
        Ok(Ok(settings)) => Some(settings.connection().clone()),
        _ => zbus::Connection::session().await.ok(),
    }
}

/// The count on the app's icon, for docks that show one: KDE Plasma's task
/// manager, Ubuntu Dock and Dash to Dock, elementary's dock and Plank. They
/// listen for the launcher entry broadcast Unity introduced, which needs no
/// permission, even in the Flatpak. Telling a dock that starts later, as
/// docks restart with the shell, takes watching the name it owns, which the
/// Flatpak may with `--talk-name=com.canonical.Unity`.
mod badge {
    use super::{APP_ID, session_bus, zbus};
    use ashpd::zvariant::Value;
    use futures_util::StreamExt;
    use std::{
        collections::HashMap,
        ffi::OsStr,
        path::Path,
        sync::{Arc, Mutex, PoisonError},
    };

    const INTERFACE: &str = "com.canonical.Unity.LauncherEntry";
    const PATH: &str = "/com/flectar/mail/LauncherEntry";
    /// The name a dock that shows counts owns.
    const DOCK: &str = "com.canonical.Unity";

    #[derive(Default)]
    pub(super) struct Launcher {
        /// Connects on first use; `None` without a session bus.
        connection: tokio::sync::OnceCell<Option<zbus::Connection>>,
        /// The count shown, for a dock that starts later.
        count: Arc<Mutex<Option<u32>>>,
    }

    impl Launcher {
        /// Show `count` on the icon, or none. Returns once it is sent, so
        /// the one sent as the app quits goes out before it does.
        pub(super) async fn show(&self, count: Option<u32>) {
            *self.count.lock().unwrap_or_else(PoisonError::into_inner) = count;
            let connection = self
                .connection
                .get_or_init(|| async {
                    let connection = session_bus().await?;
                    tokio::spawn(tell_new_docks(connection.clone(), Arc::clone(&self.count)));
                    Some(connection)
                })
                .await;
            if let Some(connection) = connection
                && let Err(error) = update(connection, count).await
            {
                tracing::debug!(%error, "notifications: could not update the count on the icon");
            }
        }
    }

    /// Tell each dock that takes the name the count shown now.
    async fn tell_new_docks(connection: zbus::Connection, count: Arc<Mutex<Option<u32>>>) {
        let Ok(bus) = zbus::fdo::DBusProxy::new(&connection).await else {
            return;
        };
        let Ok(mut owners) = bus.receive_name_owner_changed_with_args(&[(0, DOCK)]).await else {
            return;
        };
        while let Some(change) = owners.next().await {
            let started = change
                .args()
                .is_ok_and(|args| args.new_owner().as_ref().is_some());
            if !started {
                continue;
            }
            let shown = *count.lock().unwrap_or_else(PoisonError::into_inner);
            if let Err(error) = update(&connection, shown).await {
                tracing::debug!(%error, "notifications: could not tell a dock the count");
            }
        }
    }

    async fn update(connection: &zbus::Connection, count: Option<u32>) -> zbus::Result<()> {
        let properties = HashMap::from([
            ("count", Value::from(i64::from(count.unwrap_or(0)))),
            ("count-visible", Value::from(count.is_some())),
        ]);
        connection
            .emit_signal(
                None::<&str>,
                PATH,
                INTERFACE,
                "Update",
                &(app_uri(), properties),
            )
            .await
    }

    /// The desktop entry docks know the app by.
    fn app_uri() -> String {
        let launched = std::env::var_os("GIO_LAUNCHED_DESKTOP_FILE");
        let launched_pid = std::env::var("GIO_LAUNCHED_DESKTOP_FILE_PID").ok();
        let entry = desktop_entry(
            std::env::var("FLATPAK_ID").ok().as_deref(),
            launched.as_deref(),
            launched_pid.as_deref(),
            std::process::id(),
        );
        format!("application://{entry}")
    }

    /// The Flatpak's entry is named by its id. An AppImage's is renamed by
    /// the launcher that installed it, which says, as GLib does, which entry
    /// it started this very process from; a process started by one that was
    /// launched so inherits a stale name and the parent's process id.
    fn desktop_entry(
        flatpak_id: Option<&str>,
        launched: Option<&OsStr>,
        launched_pid: Option<&str>,
        pid: u32,
    ) -> String {
        if let Some(id) = flatpak_id.filter(|id| !id.is_empty()) {
            return format!("{id}.desktop");
        }
        launched
            .filter(|_| launched_pid.and_then(|launched| launched.parse::<u32>().ok()) == Some(pid))
            .and_then(|path| Path::new(path).file_name()?.to_str())
            .filter(|name| name.ends_with(".desktop"))
            .map_or_else(|| format!("{APP_ID}.desktop"), str::to_owned)
    }

    #[cfg(test)]
    mod tests {
        use super::desktop_entry;
        use std::ffi::OsStr;

        #[test]
        fn docks_know_the_app_by_its_desktop_entry() {
            let renamed = OsStr::new(
                "/home/a/.local/share/applications/appimagekit_0f3-com.flectar.mail.desktop",
            );
            assert_eq!(
                desktop_entry(None, None, None, 7),
                "com.flectar.mail.desktop"
            );
            assert_eq!(
                desktop_entry(Some("com.flectar.mail"), Some(renamed), Some("7"), 7),
                "com.flectar.mail.desktop"
            );
            assert_eq!(
                desktop_entry(None, Some(renamed), Some("7"), 7),
                "appimagekit_0f3-com.flectar.mail.desktop"
            );
            // Inherited from a terminal started from its own entry.
            assert_eq!(
                desktop_entry(
                    None,
                    Some(OsStr::new(
                        "/usr/share/applications/org.gnome.Ptyxis.desktop"
                    )),
                    Some("3"),
                    7
                ),
                "com.flectar.mail.desktop"
            );
            assert_eq!(
                desktop_entry(None, Some(OsStr::new("/tmp/flectar")), Some("7"), 7),
                "com.flectar.mail.desktop"
            );
            assert_eq!(
                desktop_entry(Some(""), None, None, 7),
                "com.flectar.mail.desktop"
            );
        }
    }
}

/// The notification's body: the subject, then the start of the message, and
/// for a summary the account it sums up when there are several.
fn body_text(alert: &Alert) -> String {
    let mut body = alert.body.clone();
    if let Some(preview) = alert
        .preview
        .as_deref()
        .filter(|preview| !preview.is_empty())
    {
        body.push('\n');
        body.push_str(preview);
    }
    if let (AlertId::Summary { .. }, Some(account)) = (alert.id, alert.account.as_deref()) {
        body.push('\n');
        body.push_str(account);
    }
    body
}

/// The desktop's notification server, org.freedesktop.Notifications.
#[cfg(not(feature = "flatpak"))]
mod imp {
    use super::{
        APP_ID, Alert, AlertId, CALL_TIMEOUT, Failure, FixedWords, body_text, session_bus,
    };
    use crate::notifications::{Act, Response, respond};
    use ashpd::{zbus, zvariant};
    use futures_util::StreamExt;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex, PoisonError},
    };

    const DESTINATION: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";
    /// The freedesktop sound theme's name for new email.
    const SOUND: &str = "message-new-email";

    pub(super) struct Service {
        proxy: zbus::Proxy<'static>,
        capabilities: Vec<String>,
        shown: Arc<Mutex<Shown>>,
        words: Arc<Mutex<FixedWords>>,
    }

    /// What this process showed, by the server's id for it. The server only
    /// reports clicks to the connection that showed the notification, and
    /// GNOME takes the notifications away when that connection closes.
    #[derive(Default)]
    struct Shown {
        ids: HashMap<AlertId, u32>,
        alerts: HashMap<u32, Alert>,
        /// Activation tokens the server sent ahead of the click they belong
        /// to.
        tokens: HashMap<u32, String>,
    }

    impl Shown {
        fn forget(&mut self, id: u32) {
            if let Some(alert) = self.alerts.remove(&id) {
                self.ids.remove(&alert.id);
            }
            self.tokens.remove(&id);
        }
    }

    impl Service {
        pub(super) async fn connect(words: Arc<Mutex<FixedWords>>) -> Option<Self> {
            let connection = session_bus().await?;
            let proxy = zbus::Proxy::new(&connection, DESTINATION, PATH, DESTINATION)
                .await
                .ok()?;
            let capabilities: Vec<String> =
                tokio::time::timeout(CALL_TIMEOUT, proxy.call("GetCapabilities", &()))
                    .await
                    .ok()?
                    .ok()?;
            let shown = Arc::new(Mutex::new(Shown::default()));
            listen(&connection, Arc::clone(&shown)).await?;
            Some(Self {
                proxy,
                capabilities,
                shown,
                words,
            })
        }

        fn supports(&self, capability: &str) -> bool {
            self.capabilities
                .iter()
                .any(|offered| offered == capability)
        }

        pub(super) async fn show(&self, alert: &Alert) -> Result<(), Failure> {
            let replaces = self
                .shown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .ids
                .get(&alert.id)
                .copied()
                .unwrap_or(0);
            let body = body_text(alert);
            // Markup the server parses must not be what the sender wrote.
            let body = if self.supports("body-markup") {
                escape_markup(&body)
            } else {
                body
            };
            let mut actions = Vec::new();
            if self.supports("actions") {
                let open = self
                    .words
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .open
                    .clone();
                actions.extend(["default".to_owned(), open]);
                for (act, label) in &alert.actions {
                    actions.extend([act.key().to_owned(), label.clone()]);
                }
            }
            let mut hints: HashMap<&str, zvariant::Value> = HashMap::from([
                ("desktop-entry", APP_ID.into()),
                ("category", "email.arrived".into()),
                ("urgency", zvariant::Value::U8(1)),
            ]);
            if alert.sound {
                hints.insert("sound-name", SOUND.into());
            } else {
                hints.insert("suppress-sound", true.into());
            }
            let arguments = (
                "Flectar Mail",
                replaces,
                APP_ID,
                alert.title.as_str(),
                body.as_str(),
                actions,
                hints,
                -1_i32,
            );
            let call = self.proxy.call::<_, _, u32>("Notify", &arguments);
            let id = match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(Ok(id)) => id,
                Ok(Err(error)) => return Err(Failure::Retry(error.to_string())),
                Err(_) => {
                    return Err(Failure::Retry(
                        "the notification server did not answer".into(),
                    ));
                }
            };
            let mut shown = self.shown.lock().unwrap_or_else(PoisonError::into_inner);
            if replaces != 0 && replaces != id {
                shown.forget(replaces);
            }
            shown.ids.insert(alert.id, id);
            shown.alerts.insert(id, alert.clone());
            Ok(())
        }

        pub(super) async fn withdraw(&self, id: AlertId) {
            let Some(server_id) = self
                .shown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .ids
                .get(&id)
                .copied()
            else {
                return;
            };
            let arguments = (server_id,);
            let call = self.proxy.call::<_, _, ()>("CloseNotification", &arguments);
            let _ = tokio::time::timeout(CALL_TIMEOUT, call).await;
            self.shown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .forget(server_id);
        }
    }

    /// Follow the server's signals in the order it sends them: an activation
    /// token arrives just before the click it is for.
    async fn listen(connection: &zbus::Connection, shown: Arc<Mutex<Shown>>) -> Option<()> {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(DESTINATION)
            .ok()?
            .path(PATH)
            .ok()?
            .build();
        let mut signals = zbus::MessageStream::for_match_rule(rule, connection, None)
            .await
            .ok()?;
        tokio::spawn(async move {
            while let Some(Ok(message)) = signals.next().await {
                let header = message.header();
                let body = message.body();
                match header.member().map(|member| member.as_str()) {
                    Some("ActivationToken") => {
                        if let Ok((id, token)) = body.deserialize::<(u32, String)>() {
                            shown
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .tokens
                                .insert(id, token);
                        }
                    }
                    Some("ActionInvoked") => {
                        let Ok((id, key)) = body.deserialize::<(u32, String)>() else {
                            continue;
                        };
                        let act = if key == "default" {
                            Some(Act::Open)
                        } else {
                            Act::parse(&key)
                        };
                        let clicked = {
                            let mut shown = shown.lock().unwrap_or_else(PoisonError::into_inner);
                            let token = shown.tokens.remove(&id);
                            shown.alerts.get(&id).map(|alert| (alert.open, token))
                        };
                        if let (Some(act), Some((route, activation_token))) = (act, clicked) {
                            respond(Response {
                                route,
                                act,
                                activation_token,
                            });
                        }
                    }
                    Some("NotificationClosed") => {
                        if let Ok((id, _reason)) = body.deserialize::<(u32, u32)>() {
                            shown
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .forget(id);
                        }
                    }
                    _ => {}
                }
            }
        });
        Some(())
    }

    /// Escape what the notification server would read as markup.
    fn escape_markup(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len());
        for character in text.chars() {
            match character {
                '&' => escaped.push_str("&amp;"),
                '<' => escaped.push_str("&lt;"),
                '>' => escaped.push_str("&gt;"),
                character => escaped.push(character),
            }
        }
        escaped
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn markup_from_the_sender_is_escaped() {
            assert_eq!(
                super::escape_markup("Tom & <jerry@example.com>"),
                "Tom &amp; &lt;jerry@example.com&gt;"
            );
        }
    }
}

/// The notification portal, from inside the Flatpak sandbox.
#[cfg(feature = "flatpak")]
mod imp {
    use super::{APP_ID, Alert, AlertId, CALL_TIMEOUT, Failure, FixedWords, body_text};
    use crate::notifications::{Act, Response, parse_payload, payload, quit, raise, respond};
    use ashpd::{
        desktop::notification::{Button, Notification, NotificationProxy, Priority},
        zbus, zvariant,
    };
    use futures_util::StreamExt;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    /// The action every notification and button sends, with its payload as
    /// the target. The `app.` prefix makes GNOME and KDE call the app's
    /// `org.freedesktop.Application.ActivateAction`, which carries the
    /// activation token the window needs to take focus.
    const ACTION: &str = "notification";
    /// The app's object for `org.freedesktop.Application`, from its id.
    const APP_PATH: &str = "/com/flectar/mail";

    pub(super) struct Service {
        proxy: NotificationProxy<'static>,
    }

    impl Service {
        pub(super) async fn connect(_words: Arc<Mutex<FixedWords>>) -> Option<Self> {
            let proxy = NotificationProxy::new().await.ok()?;
            let connection = proxy.connection().clone();
            // Another profile's process may own the name; its clicks then
            // still arrive as ActionInvoked below, without a token.
            let _ = connection.object_server().at(APP_PATH, Application).await;
            let _ = connection.request_name(APP_ID).await;
            if let Ok(mut invoked) = proxy.receive_action_invoked().await {
                tokio::spawn(async move {
                    while let Some(action) = invoked.next().await {
                        if action.name() != format!("app.{ACTION}") {
                            continue;
                        }
                        let parameter = action.parameter();
                        let platform_data = parameter
                            .get(1)
                            .and_then(|value| platform_data(value))
                            .unwrap_or_default();
                        deliver(
                            parameter.first().and_then(|value| text(value)),
                            &platform_data,
                        );
                    }
                });
            }
            Some(Self { proxy })
        }

        pub(super) async fn show(&self, alert: &Alert) -> Result<(), Failure> {
            let action = format!("app.{ACTION}");
            let target = payload(alert.open, Act::Open);
            let body = body_text(alert);
            let mut notification = Notification::new(&alert.title)
                .body(body.as_str())
                .priority(Priority::Normal)
                .default_action(action.as_str())
                .default_action_target(target.as_str());
            for (act, label) in &alert.actions {
                let target = payload(alert.open, *act);
                notification =
                    notification.button(Button::new(label, &action).target(target.as_str()));
            }
            let key = alert.id.key();
            let call = self.proxy.add_notification(&key, notification);
            match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(Failure::Retry(error.to_string())),
                Err(_) => Err(Failure::Retry(
                    "the notification portal did not answer".into(),
                )),
            }
        }

        pub(super) async fn withdraw(&self, id: AlertId) {
            let key = id.key();
            let _ = tokio::time::timeout(CALL_TIMEOUT, self.proxy.remove_notification(&key)).await;
        }
    }

    /// The text value of a D-Bus variant, unwrapping nested variants.
    fn text(value: &zvariant::Value) -> Option<String> {
        match value {
            zvariant::Value::Str(text) => Some(text.to_string()),
            zvariant::Value::Value(inner) => text(inner),
            _ => None,
        }
    }

    /// A platform-data dictionary passed as a variant.
    fn platform_data(value: &zvariant::Value) -> Option<HashMap<String, zvariant::OwnedValue>> {
        match value {
            zvariant::Value::Value(inner) => platform_data(inner),
            value => HashMap::try_from(value.try_clone().ok()?).ok(),
        }
    }

    /// The activation token a desktop passes along with a click (Wayland), or
    /// the X11 startup id it sends under the older name.
    fn activation_token(platform_data: &HashMap<String, zvariant::OwnedValue>) -> Option<String> {
        ["activation-token", "desktop-startup-id"]
            .into_iter()
            .find_map(|key| platform_data.get(key).and_then(|value| text(value)))
            .filter(|token| !token.is_empty())
    }

    fn deliver(target: Option<String>, platform_data: &HashMap<String, zvariant::OwnedValue>) {
        let Some((route, act)) = target.as_deref().and_then(parse_payload) else {
            return;
        };
        respond(Response {
            route,
            act,
            activation_token: activation_token(platform_data),
        });
    }

    /// `org.freedesktop.Application`, through which the desktop hands the app
    /// a click on one of its notifications. GNOME's Background Apps also
    /// raises the window with it, and ends the app with its `quit` action
    /// before resorting to `flatpak kill`.
    struct Application;

    #[zbus::interface(name = "org.freedesktop.Application")]
    impl Application {
        fn activate(&self, platform_data: HashMap<String, zvariant::OwnedValue>) {
            raise(activation_token(&platform_data));
        }

        fn open(&self, _uris: Vec<String>, platform_data: HashMap<String, zvariant::OwnedValue>) {
            raise(activation_token(&platform_data));
        }

        /// An unknown action is an error, as GLib answers: a caller such as
        /// GNOME's Background Apps then falls back to ending the app itself.
        fn activate_action(
            &self,
            action_name: String,
            parameter: Vec<zvariant::OwnedValue>,
            platform_data: HashMap<String, zvariant::OwnedValue>,
        ) -> zbus::fdo::Result<()> {
            match action_name.as_str() {
                ACTION => deliver(
                    parameter.first().and_then(|value| text(value)),
                    &platform_data,
                ),
                "quit" => quit(),
                _ => {
                    return Err(zbus::fdo::Error::InvalidArgs(format!(
                        "Unknown action '{action_name}'"
                    )));
                }
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{Application, HashMap};
        use crate::notifications::{Request, serve_requests};
        use std::sync::{Mutex, mpsc};

        /// GNOME's Background Apps ends the app with `quit`, and resorts to
        /// `flatpak kill` only when the call fails.
        #[test]
        fn the_desktop_can_quit_the_app_and_hears_of_unknown_actions() {
            let (requests, received) = mpsc::channel();
            let requests = Mutex::new(requests);
            serve_requests(move |request| {
                let _ = requests.lock().unwrap().send(request);
            });

            let quit = Application.activate_action("quit".into(), Vec::new(), HashMap::new());
            assert!(quit.is_ok());
            assert_eq!(received.try_recv(), Ok(Request::Quit));

            let unknown =
                Application.activate_action("new-window".into(), Vec::new(), HashMap::new());
            assert!(unknown.is_err());
            assert!(received.try_recv().is_err());
        }
    }
}
