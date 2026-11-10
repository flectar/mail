//! New-mail notifications on Android. Rust shows them through
//! Notifications.java, takes the activity's taps and permission answers, and
//! serves what Android asks of the app without its window: checks for new
//! mail (MailCheckJob) and buttons on notifications
//! (NotificationActionReceiver).

use crate::{credentials::AndroidCredentialStore, oauth::AndroidOAuthBroker};
use flectar_mail::{
    Act, Alert, AlertId, Availability, Controls, EngineServices, Failure, FixedWords, HostFuture,
    NotificationHost, Response, parse_payload, payload,
};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JClass, JObject, JObjectArray, JString, JValue},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jint},
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, PoisonError},
};
use tokio::sync::oneshot;

/// The notification service, one for the process: the window's store and
/// one opened in the background use the same.
pub(super) struct AndroidNotifications {
    vm: JavaVM,
    /// The Application, which outlives any activity.
    context: GlobalRef,
    /// The activity in front, which alone can ask for permission.
    activity: Mutex<Option<GlobalRef>>,
    /// The words of what the lock screen shows instead.
    words: Mutex<FixedWords>,
    /// Permission requests waiting for the user's answer.
    asking: Mutex<Vec<oneshot::Sender<()>>>,
}

static HOST: OnceLock<Arc<AndroidNotifications>> = OnceLock::new();

const NOTIFICATIONS: &str = "com.flectar.mail.Notifications";
const MAIL_CHECKS: &str = "com.flectar.mail.MailChecks";

impl AndroidNotifications {
    /// The process's service, made from `context`, any Context of the app,
    /// the first time.
    pub(super) fn global(env: &mut JNIEnv, context: &JObject) -> Result<Arc<Self>, String> {
        if let Some(host) = HOST.get() {
            return Ok(host.clone());
        }
        let vm = env.get_java_vm().map_err(|error| error.to_string())?;
        let application = env
            .call_method(
                context,
                "getApplicationContext",
                "()Landroid/content/Context;",
                &[],
            )
            .and_then(|value| value.l())
            .map_err(|error| format!("Android application context: {error}"))?;
        let context = env
            .new_global_ref(application)
            .map_err(|error| error.to_string())?;
        let _ = HOST.set(Arc::new(Self {
            vm,
            context,
            activity: Mutex::new(None),
            words: Mutex::new(FixedWords::default()),
            asking: Mutex::new(Vec::new()),
        }));
        HOST.get()
            .cloned()
            .ok_or_else(|| "Android notifications are unavailable".to_owned())
    }

    /// The activity in front, or `None` once it has ended.
    pub(super) fn set_activity(&self, activity: Option<GlobalRef>) {
        *self.activity.lock().unwrap_or_else(PoisonError::into_inner) = activity;
    }

    /// Call into the app's class `class` with the Application, freeing the
    /// local references the call makes and any Java exception it leaves.
    fn call<T>(
        &self,
        class: &str,
        call: impl for<'local> FnOnce(
            &mut JNIEnv<'local>,
            &JClass<'local>,
            &JObject,
        ) -> jni::errors::Result<T>,
    ) -> Result<T, String> {
        let mut env = self
            .vm
            .attach_current_thread()
            .map_err(|error| format!("attaching to Android: {error}"))?;
        let result = env.with_local_frame(16, |env| {
            let class = crate::java::app_class(env, self.context.as_obj(), class)?;
            call(env, &class, self.context.as_obj())
        });
        if result.is_err() {
            let _ = env.exception_clear();
        }
        result.map_err(|error| error.to_string())
    }

    fn availability_now(&self) -> Availability {
        let answer = self.call(NOTIFICATIONS, |env, class, context| {
            env.call_static_method(
                class,
                "availability",
                "(Landroid/content/Context;)I",
                &[JValue::Object(context)],
            )?
            .i()
        });
        match answer {
            Ok(0) => Availability::Allowed,
            Ok(1) => Availability::NotAsked,
            Ok(_) => Availability::Denied,
            Err(error) => {
                tracing::warn!(%error, "notifications: could not ask Android about them");
                Availability::Unavailable
            }
        }
    }

    fn show_one(&self, alert: &Alert, new_message: &str) -> Result<(), Failure> {
        // The lock screen leaves out who wrote and about what. A summary's
        // title only counts the messages.
        let hidden = match alert.id {
            AlertId::Message { .. } => new_message,
            AlertId::Summary { .. } => alert.title.as_str(),
        };
        // A message's subject leads its expanded text, which adds the start
        // of the message.
        let details = match (alert.id, alert.preview.as_deref()) {
            (AlertId::Message { .. }, Some(preview)) if !preview.is_empty() => {
                format!("{}\n{preview}", alert.body)
            }
            _ => String::new(),
        };
        let (labels, requests): (Vec<&str>, Vec<String>) = alert
            .actions
            .iter()
            .map(|(act, label)| (label.as_str(), payload(alert.open, *act)))
            .unzip();
        let shown = self.call(NOTIFICATIONS, |env, class, context| {
            let tag = env.new_string(alert.id.key())?;
            let title = env.new_string(&alert.title)?;
            let text = env.new_string(&alert.body)?;
            let details = env.new_string(&details)?;
            let account = env.new_string(alert.account.as_deref().unwrap_or_default())?;
            let open = env.new_string(payload(alert.open, Act::Open))?;
            let labels = strings(env, &labels)?;
            let requests = strings(env, &requests)?;
            let hidden = env.new_string(hidden)?;
            env.call_static_method(
                class,
                "show",
                "(Landroid/content/Context;Ljava/lang/String;ZLjava/lang/String;\
                 Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;\
                 JILjava/lang/String;[Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Z",
                &[
                    JValue::Object(context),
                    JValue::Object(&tag),
                    JValue::Bool(if alert.sound { JNI_TRUE } else { JNI_FALSE }),
                    JValue::Object(&title),
                    JValue::Object(&text),
                    JValue::Object(&details),
                    JValue::Object(&account),
                    JValue::Long(alert.received_at),
                    JValue::Int(i32::try_from(alert.count).unwrap_or(i32::MAX)),
                    JValue::Object(&open),
                    JValue::Object(&labels),
                    JValue::Object(&requests),
                    JValue::Object(&hidden),
                ],
            )?
            .z()
        });
        match shown {
            Ok(true) => Ok(()),
            Ok(false) => Err(Failure::Unavailable(
                "notifications are off for Flectar Mail".into(),
            )),
            Err(error) => Err(Failure::Retry(error)),
        }
    }
}

/// A Java `String[]` of `values`.
fn strings<'local>(
    env: &mut JNIEnv<'local>,
    values: &[impl AsRef<str>],
) -> jni::errors::Result<JObjectArray<'local>> {
    let array = env.new_object_array(
        i32::try_from(values.len()).unwrap_or(i32::MAX),
        "java/lang/String",
        JObject::null(),
    )?;
    for (index, value) in values.iter().enumerate() {
        let value = env.new_string(value.as_ref())?;
        env.set_object_array_element(&array, i32::try_from(index).unwrap_or(i32::MAX), value)?;
    }
    Ok(array)
}

impl NotificationHost for AndroidNotifications {
    fn show(&self, alerts: Vec<Alert>) -> HostFuture<Vec<Result<(), Failure>>> {
        let new_message = self
            .words
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .new_message
            .clone();
        let shown = alerts
            .iter()
            .map(|alert| self.show_one(alert, &new_message))
            .collect();
        Box::pin(std::future::ready(shown))
    }

    fn withdraw(&self, ids: Vec<AlertId>) -> HostFuture<()> {
        let tags = ids.into_iter().map(AlertId::key).collect::<Vec<_>>();
        let withdrawn = self.call(NOTIFICATIONS, |env, class, context| {
            let tags = strings(env, &tags)?;
            env.call_static_method(
                class,
                "withdraw",
                "(Landroid/content/Context;[Ljava/lang/String;)V",
                &[JValue::Object(context), JValue::Object(&tags)],
            )?;
            Ok(())
        });
        if let Err(error) = withdrawn {
            tracing::warn!(%error, "notifications: could not take them back");
        }
        Box::pin(std::future::ready(()))
    }

    fn availability(&self) -> HostFuture<Availability> {
        Box::pin(std::future::ready(self.availability_now()))
    }

    /// The activity asks; without one nobody can be asked.
    fn request_permission(&self) -> HostFuture<Availability> {
        let activity = self
            .activity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (Some(activity), Some(host)) = (activity, HOST.get().cloned()) else {
            return self.availability();
        };
        let (sender, answered) = oneshot::channel();
        self.asking
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sender);
        let asked = self.vm.attach_current_thread().and_then(|mut env| {
            env.call_method(activity.as_obj(), "askNotificationPermission", "()V", &[])
                .map(drop)
        });
        if let Err(error) = asked {
            tracing::warn!(%error, "notifications: could not ask for permission");
            return self.availability();
        }
        Box::pin(async move {
            let _ = answered.await;
            host.availability_now()
        })
    }

    /// The button labels need no registering on Android; the channels take
    /// their names in the UI's language.
    fn set_words(&self, words: &FixedWords) {
        *self.words.lock().unwrap_or_else(PoisonError::into_inner) = words.clone();
        if words.new_mail_channel.is_empty() || words.quiet_mail_channel.is_empty() {
            return;
        }
        let named = self.call(NOTIFICATIONS, |env, class, context| {
            let new_mail = env.new_string(&words.new_mail_channel)?;
            let quiet_mail = env.new_string(&words.quiet_mail_channel)?;
            env.call_static_method(
                class,
                "nameChannels",
                "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)V",
                &[
                    JValue::Object(context),
                    JValue::Object(&new_mail),
                    JValue::Object(&quiet_mail),
                ],
            )?;
            Ok(())
        });
        if let Err(error) = named {
            tracing::warn!(%error, "notifications: could not name the channels");
        }
    }

    /// Android badges are dots on the launcher icon, which it draws from the
    /// notifications themselves.
    fn controls(&self) -> Controls {
        Controls {
            settings: true,
            sound: true,
            badge: false,
        }
    }

    fn open_settings(&self) {
        self.open(NOTIFICATIONS, "openSettings");
    }

    fn checks_wanted(&self, wanted: bool) {
        let scheduled = self.call(MAIL_CHECKS, |env, class, context| {
            env.call_static_method(
                class,
                "schedule",
                "(Landroid/content/Context;Z)V",
                &[
                    JValue::Object(context),
                    JValue::Bool(if wanted { JNI_TRUE } else { JNI_FALSE }),
                ],
            )?;
            Ok(())
        });
        if let Err(error) = scheduled {
            tracing::warn!(%error, "notifications: could not schedule checks for new mail");
        }
    }
}

impl AndroidNotifications {
    /// Open a settings page through the static `method` of `class`.
    fn open(&self, class: &str, method: &str) {
        let opened = self.call(class, |env, class, context| {
            env.call_static_method(
                class,
                method,
                "(Landroid/content/Context;)V",
                &[JValue::Object(context)],
            )?;
            Ok(())
        });
        if let Err(error) = opened {
            tracing::warn!(%error, "notifications: could not open the settings");
        }
    }

    /// Whether Android lets the app check for mail in the background.
    pub(super) fn background_checks(&self) -> flectar_mail::platform::BackgroundRefresh {
        use flectar_mail::platform::BackgroundRefresh;
        let state = self.call(NOTIFICATIONS, |env, class, context| {
            env.call_static_method(
                class,
                "backgroundChecks",
                "(Landroid/content/Context;)I",
                &[JValue::Object(context)],
            )?
            .i()
        });
        match state {
            Ok(1) => BackgroundRefresh::Off,
            Ok(2) => BackgroundRefresh::LowPower,
            _ => BackgroundRefresh::On,
        }
    }

    /// The app's page in Settings, where its background use is allowed.
    pub(super) fn open_app_settings(&self) {
        self.open(NOTIFICATIONS, "openAppSettings");
    }
}

/// The service for the window, which can ask for permission through the
/// activity, and the bridge to the app's background state.
pub(super) fn window_host(
    app: &slint::android::AndroidApp,
) -> Result<Arc<AndroidNotifications>, String> {
    // SAFETY: Android owns the VM for the process and the Activity reference
    // for this call; new_global_ref keeps the Activity past it.
    let vm =
        unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|error| error.to_string())?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|error| error.to_string())?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let host = AndroidNotifications::global(&mut env, &activity)?;
    host.set_activity(Some(
        env.new_global_ref(&activity)
            .map_err(|error| error.to_string())?,
    ));
    let (state, open) = (host.clone(), host.clone());
    flectar_mail::platform::configure_background(flectar_mail::platform::BackgroundBridge {
        state: Box::new(move || state.background_checks()),
        open_settings: Box::new(move || open.open_app_settings()),
    });
    Ok(host)
}

/// What opens the mail store without the activity, from `context`.
fn background_services(env: &mut JNIEnv, context: &JObject) -> Result<EngineServices, String> {
    crate::observability::install();
    let vm = env.get_java_vm().map_err(|error| error.to_string())?;
    // SAFETY: the live process VM and a Context reference that Java keeps
    // valid for this native call; the verifier keeps the Application.
    unsafe { crate::tls::initialize(vm.get_java_vm_pointer().cast(), context.as_raw().cast()) }
        .map_err(|error| format!("Android TLS verification: {error}"))?;
    let directory = |env: &mut JNIEnv, method: &str| -> Result<PathBuf, String> {
        let file = env
            .call_method(context, method, "()Ljava/io/File;", &[])
            .and_then(|value| value.l())
            .map_err(|error| error.to_string())?;
        let path = env
            .call_method(&file, "getAbsolutePath", "()Ljava/lang/String;", &[])
            .and_then(|value| value.l())
            .map_err(|error| error.to_string())?;
        let path: String = env
            .get_string(&JString::from(path))
            .map_err(|error| error.to_string())?
            .into();
        Ok(path.into())
    };
    // The NativeActivity's internalDataPath is getFilesDir().
    let internal = directory(env, "getFilesDir")?;
    let cache = directory(env, "getCacheDir")?;
    let credentials = AndroidCredentialStore::with_vm(
        env.get_java_vm().map_err(|error| error.to_string())?,
        &internal,
    );
    let oauth_redirects = AndroidOAuthBroker::background(
        env.get_java_vm().map_err(|error| error.to_string())?,
        context,
        &internal,
    )
    .map_err(|error| error.to_string())?;
    let host = AndroidNotifications::global(env, context)?;
    Ok(EngineServices {
        paths: flectar_mail::PlatformContext::app_private_paths(internal.clone(), cache),
        credentials: Arc::new(credentials),
        oauth_redirects,
        host,
    })
}

/// The activity came to the front.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeResumed(
    _env: JNIEnv,
    _class: JClass,
) {
    flectar_mail::platform::set_app_active(true);
}

/// The activity left the front.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_FlectarActivity_nativePaused(
    _env: JNIEnv,
    _class: JClass,
) {
    flectar_mail::platform::set_app_active(false);
}

/// A tap on a notification: open what it is about once the mail store is
/// open.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeNotificationOpened(
    mut env: JNIEnv,
    _class: JClass,
    request: JString,
) {
    let Ok(request) = env.get_string(&request).map(String::from) else {
        return;
    };
    if let Some((route, _)) = parse_payload(&request) {
        flectar_mail::respond(Response {
            route,
            act: Act::Open,
            activation_token: None,
        });
    }
}

/// The user answered the permission dialog, or Android answered for them.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeNotificationPermissionAnswered(
    _env: JNIEnv,
    _class: JClass,
) {
    if let Some(host) = HOST.get() {
        for sender in
            std::mem::take(&mut *host.asking.lock().unwrap_or_else(PoisonError::into_inner))
        {
            let _ = sender.send(());
        }
    }
}

/// A button on a notification: returns once its action is saved.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_NotificationActionReceiver_nativeRespond(
    mut env: JNIEnv,
    _class: JClass,
    context: JObject,
    request: JString,
) {
    let Ok(request) = env.get_string(&request).map(String::from) else {
        return;
    };
    flectar_mail::respond_in_background(|| background_services(&mut env, &context), &request);
}

/// A check for new mail: returns once done.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_MailCheckJob_nativeCheck(
    mut env: JNIEnv,
    _class: JClass,
    context: JObject,
    job: jint,
) -> jboolean {
    let every = flectar_mail::check_for_mail(|| background_services(&mut env, &context), job);
    if every { JNI_TRUE } else { JNI_FALSE }
}

/// Android wants the check's time back.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_MailCheckJob_nativeStop(
    _env: JNIEnv,
    _class: JClass,
    job: jint,
) {
    flectar_mail::stop_check(job);
}
