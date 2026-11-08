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
//! notifications add no connection and no thread where a portal runs.

use super::{Alert, AlertId, Availability, Failure, FixedWords, HostFuture, NotificationHost};
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
    use super::{APP_ID, Alert, AlertId, CALL_TIMEOUT, Failure, FixedWords, body_text};
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

    /// The session bus connection ashpd shares with the other portals, or a
    /// new one where no portal runs.
    async fn session_bus() -> Option<zbus::Connection> {
        let portal =
            tokio::time::timeout(CALL_TIMEOUT, ashpd::desktop::settings::Settings::new()).await;
        match portal {
            Ok(Ok(settings)) => Some(settings.connection().clone()),
            _ => zbus::Connection::session().await.ok(),
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
    use crate::notifications::{Act, Response, parse_payload, payload, raise, respond};
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
    /// a click on one of its notifications.
    struct Application;

    #[zbus::interface(name = "org.freedesktop.Application")]
    impl Application {
        fn activate(&self, platform_data: HashMap<String, zvariant::OwnedValue>) {
            raise(activation_token(&platform_data));
        }

        fn open(&self, _uris: Vec<String>, platform_data: HashMap<String, zvariant::OwnedValue>) {
            raise(activation_token(&platform_data));
        }

        fn activate_action(
            &self,
            action_name: String,
            parameter: Vec<zvariant::OwnedValue>,
            platform_data: HashMap<String, zvariant::OwnedValue>,
        ) {
            if action_name == ACTION {
                deliver(
                    parameter.first().and_then(|value| text(value)),
                    &platform_data,
                );
            }
        }
    }
}
