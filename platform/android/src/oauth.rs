use async_trait::async_trait;
use flectar_mail::flectar_mail_core::{
    error::{CoreError, Result},
    models::Provider,
    oauth::{
        loopback::AuthCode,
        redirect::{
            OAuthRedirectBroker, OAuthRedirectGuard, OAuthRedirectSession, PlatformAuthorization,
        },
    },
};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JClass, JObject, JString, JValue},
    sys::{JNI_FALSE, JNI_TRUE, jlong},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

const MICROSOFT_SCHEME: &str = "msauth";
const MICROSOFT_HOST: &str = "com.flectar.mail";

struct PendingRedirect {
    guard: OAuthRedirectGuard,
    redirect_uri: String,
    sender: oneshot::Sender<Result<AuthCode>>,
}

struct BrokerState {
    pending_redirect: Mutex<Option<PendingRedirect>>,
    pending_google: Mutex<HashMap<i64, oneshot::Sender<Result<PlatformAuthorization>>>>,
    transaction_path: PathBuf,
    activity: Mutex<GlobalRef>,
    vm: JavaVM,
    next_request: AtomicI64,
}

pub(super) struct AndroidOAuthBroker {
    state: Arc<BrokerState>,
}

struct AndroidRedirectSession {
    state: Arc<BrokerState>,
    redirect_uri: String,
    receiver: oneshot::Receiver<Result<AuthCode>>,
}

static BROKER: OnceLock<Arc<AndroidOAuthBroker>> = OnceLock::new();

impl AndroidOAuthBroker {
    pub(super) fn global(
        app: &slint::android::AndroidApp,
        private_root: &Path,
    ) -> Result<Arc<Self>> {
        let transient_microsoft_callback = activity_intent_data(app)
            .is_some_and(|uri| uri.starts_with("msauth://com.flectar.mail/"));
        // SAFETY: Android owns this process-wide VM pointer for the lifetime of
        // the process. JavaVM is a non-owning handle.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }
            .map_err(|error| CoreError::Other(format!("Android Java VM: {error}")))?;
        let env = vm
            .attach_current_thread()
            .map_err(|error| CoreError::Other(format!("attach Android UI thread: {error}")))?;
        // SAFETY: Android keeps the Activity reference valid during this call;
        // new_global_ref promotes it before the borrowed JObject is dropped.
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let activity = env
            .new_global_ref(activity)
            .map_err(|error| CoreError::Other(format!("retain Android Activity: {error}")))?;

        if let Some(broker) = BROKER.get() {
            // A redirect launches a short-lived second NativeActivity. Keep
            // the original visible Activity as the Google authorization host;
            // replacing it here would retain an Activity that is immediately
            // finished below.
            if !transient_microsoft_callback {
                *broker
                    .state
                    .activity
                    .lock()
                    .map_err(|_| CoreError::Other("OAuth Activity lock is unavailable".into()))? =
                    activity;
            }
            return Ok(broker.clone());
        }

        drop(env);

        let broker = Arc::new(Self {
            state: Arc::new(BrokerState {
                pending_redirect: Mutex::new(None),
                pending_google: Mutex::new(HashMap::new()),
                transaction_path: private_root.join("oauth/pending.json"),
                activity: Mutex::new(activity),
                vm,
                next_request: AtomicI64::new(1),
            }),
        });
        let _ = BROKER.set(broker.clone());
        BROKER
            .get()
            .cloned()
            .ok_or_else(|| CoreError::Other("Android OAuth broker initialization failed".into()))
    }

    fn publish_redirect(&self, uri: &str) -> Result<()> {
        let parsed = url::Url::parse(uri)
            .map_err(|_| CoreError::Auth("invalid Android OAuth callback".into()))?;
        if parsed.scheme() != MICROSOFT_SCHEME || parsed.host_str() != Some(MICROSOFT_HOST) {
            return Err(CoreError::Auth("unclaimed Android OAuth callback".into()));
        }
        let mut pending = self
            .state
            .pending_redirect
            .lock()
            .map_err(|_| CoreError::Other("OAuth redirect lock is unavailable".into()))?;
        let response = pending
            .as_mut()
            .ok_or_else(|| CoreError::Auth("OAuth callback is expired or already used".into()))?;
        if uri.split('?').next() != Some(response.redirect_uri.as_str()) {
            return Err(CoreError::Auth(
                "OAuth callback does not match this signed application".into(),
            ));
        }
        let response = response.guard.accept(uri)?;
        let pending = pending
            .take()
            .ok_or_else(|| CoreError::Auth("OAuth callback is already used".into()))?;
        let _ = std::fs::remove_file(&self.state.transaction_path);
        pending
            .sender
            .send(Ok(response))
            .map_err(|_| CoreError::Auth("OAuth request is no longer active".into()))
    }

    fn persist_pending(&self, provider: Provider, state: &str, redirect_uri: &str) -> Result<()> {
        let path = &self.state.transaction_path;
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::Other("OAuth transaction path has no parent".into()))?;
        std::fs::create_dir_all(parent)?;
        let expires_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .saturating_add(5 * 60 * 1_000);
        let value = serde_json::json!({
            "provider": provider.as_str(),
            "state": state,
            "redirect_uri": redirect_uri,
            "expires_at_ms": expires_at_ms,
        });
        let temporary = parent.join(".pending.json.tmp");
        std::fs::write(&temporary, serde_json::to_vec(&value)?)?;
        std::fs::rename(temporary, path)?;
        Ok(())
    }

    fn microsoft_redirect_uri(&self) -> Result<String> {
        let mut env =
            self.state.vm.attach_current_thread().map_err(|error| {
                CoreError::Other(format!("attach Android OAuth thread: {error}"))
            })?;
        let activity = self
            .state
            .activity
            .lock()
            .map_err(|_| CoreError::Other("OAuth Activity lock is unavailable".into()))?;
        let value = env
            .call_method(
                activity.as_obj(),
                "microsoftRedirectUri",
                "()Ljava/lang/String;",
                &[],
            )
            .and_then(|value| value.l())
            .map_err(|error| CoreError::Other(format!("read Android signing identity: {error}")))?;
        if value.is_null() {
            return Err(CoreError::Auth(
                "Android could not derive the Microsoft signing redirect".into(),
            ));
        }
        let value = JString::from(value);
        env.get_string(&value)
            .map(String::from)
            .map_err(|error| CoreError::Other(format!("decode Microsoft redirect: {error}")))
    }

    async fn authorize_google(
        &self,
        scopes: &[&str],
        interactive: bool,
    ) -> Result<PlatformAuthorization> {
        let request_id = self.state.next_request.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.state
            .pending_google
            .lock()
            .map_err(|_| CoreError::Other("Google authorization lock is unavailable".into()))?
            .insert(request_id, sender);

        let launch = (|| -> std::result::Result<(), jni::errors::Error> {
            let mut env = self.state.vm.attach_current_thread()?;
            let activity = self
                .state
                .activity
                .lock()
                .map_err(|_| jni::errors::Error::NullPtr("Android Activity lock"))?;
            let scope_array =
                env.new_object_array(scopes.len() as i32, "java/lang/String", JObject::null())?;
            for (index, scope) in scopes.iter().enumerate() {
                let value = env.new_string(scope)?;
                env.set_object_array_element(&scope_array, index as i32, value)?;
            }
            env.call_method(
                activity.as_obj(),
                "authorizeGoogle",
                "([Ljava/lang/String;JZ)V",
                &[
                    JValue::Object(scope_array.as_ref()),
                    JValue::Long(request_id),
                    JValue::Bool(if interactive { JNI_TRUE } else { JNI_FALSE }),
                ],
            )?;
            Ok(())
        })();

        if let Err(error) = launch {
            self.state
                .pending_google
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(&request_id));
            return Err(CoreError::Other(format!(
                "could not start Google authorization: {error}"
            )));
        }

        match tokio::time::timeout(Duration::from_secs(300), receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CoreError::Auth("Google authorization was cancelled".into())),
            Err(_) => {
                self.state
                    .pending_google
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.remove(&request_id));
                Err(CoreError::Auth("Google authorization timed out".into()))
            }
        }
    }
}

#[async_trait]
impl OAuthRedirectBroker for AndroidOAuthBroker {
    async fn begin(
        &self,
        provider: Provider,
        expected_state: &str,
    ) -> Result<Box<dyn OAuthRedirectSession>> {
        if provider != Provider::Microsoft {
            return Err(CoreError::Auth(
                "this Android provider requires native authorization".into(),
            ));
        }
        // Derive this from the certificate that signed the installed package.
        // Play App Signing replaces the upload certificate, so a URI compiled
        // from the build machine's key would be wrong for Play-installed users.
        let redirect_uri = self.microsoft_redirect_uri()?;
        let parsed = url::Url::parse(&redirect_uri)
            .map_err(|_| CoreError::Auth("Microsoft Android redirect URI is invalid".into()))?;
        if parsed.scheme() != MICROSOFT_SCHEME
            || parsed.host_str() != Some(MICROSOFT_HOST)
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() == "/"
        {
            return Err(CoreError::Auth(
                "Microsoft Android redirect must be msauth://com.flectar.mail/<signature-hash>"
                    .into(),
            ));
        }

        let (sender, receiver) = oneshot::channel();
        let mut pending = self
            .state
            .pending_redirect
            .lock()
            .map_err(|_| CoreError::Other("OAuth redirect lock is unavailable".into()))?;
        if pending.is_some() {
            return Err(CoreError::Auth(
                "another browser authorization is already in progress".into(),
            ));
        }
        self.persist_pending(provider, expected_state, &redirect_uri)?;
        *pending = Some(PendingRedirect {
            guard: OAuthRedirectGuard::new(expected_state, Duration::from_secs(300)),
            redirect_uri: redirect_uri.clone(),
            sender,
        });
        Ok(Box::new(AndroidRedirectSession {
            state: self.state.clone(),
            redirect_uri,
            receiver,
        }))
    }

    async fn authorize_platform(
        &self,
        provider: Provider,
        scopes: &[&str],
        interactive: bool,
    ) -> Option<Result<PlatformAuthorization>> {
        if provider == Provider::Gmail {
            Some(self.authorize_google(scopes, interactive).await)
        } else {
            None
        }
    }
}

#[async_trait]
impl OAuthRedirectSession for AndroidRedirectSession {
    fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    async fn wait(self: Box<Self>, timeout: Duration) -> Result<AuthCode> {
        let AndroidRedirectSession {
            state, receiver, ..
        } = *self;
        let result = match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CoreError::Auth("sign-in callback was cancelled".into())),
            Err(_) => Err(CoreError::Auth("sign-in timed out".into())),
        };
        let _ = std::fs::remove_file(&state.transaction_path);
        if let Ok(mut pending) = state.pending_redirect.lock() {
            pending.take();
        }
        result
    }
}

/// Deliver an Entra callback from a transient NativeActivity and finish it so
/// the original Slint Activity becomes visible again.
pub(super) fn route_callback_activity(
    app: &slint::android::AndroidApp,
    broker: &AndroidOAuthBroker,
) -> bool {
    let Some(uri) = activity_intent_data(app) else {
        return false;
    };
    if !uri.starts_with("msauth://com.flectar.mail/") {
        return false;
    }
    match broker.publish_redirect(&uri) {
        Ok(()) => tracing::info!("delivered Android Microsoft OAuth callback"),
        Err(error) => tracing::warn!(%error, "rejected Android Microsoft OAuth callback"),
    }
    finish_activity(app);
    true
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeGoogleAuthorizationResult(
    mut env: JNIEnv,
    _class: JClass,
    request_id: jlong,
    access_token: JString,
    expires_in: jlong,
    error: JString,
) {
    let Some(broker) = BROKER.get() else {
        return;
    };
    let error = (!error.is_null())
        .then(|| env.get_string(&error).ok().map(String::from))
        .flatten()
        .unwrap_or_default();
    let result = if !error.is_empty() {
        if error.starts_with("needs_reauth:") {
            Err(CoreError::NeedsReauth)
        } else {
            Err(CoreError::Auth(format!(
                "Google authorization failed: {error}"
            )))
        }
    } else {
        let token = env
            .get_string(&access_token)
            .ok()
            .map(String::from)
            .filter(|value| !value.is_empty());
        match token {
            Some(access_token) => Ok(PlatformAuthorization {
                access_token,
                expires_in: (expires_in > 0).then_some(expires_in),
            }),
            None => Err(CoreError::Auth(
                "Google authorization returned no access token".into(),
            )),
        }
    };
    if let Ok(mut pending) = broker.state.pending_google.lock()
        && let Some(sender) = pending.remove(&request_id)
    {
        let _ = sender.send(result);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeMicrosoftRedirect(
    mut env: JNIEnv,
    _class: JClass,
    redirect_uri: JString,
) {
    let Some(broker) = BROKER.get() else {
        return;
    };
    let Some(uri) = env.get_string(&redirect_uri).ok().map(String::from) else {
        return;
    };
    if let Err(error) = broker.publish_redirect(&uri) {
        tracing::warn!(%error, "rejected Android Microsoft OAuth callback");
    }
}

fn activity_intent_data(app: &slint::android::AndroidApp) -> Option<String> {
    // SAFETY: Android owns this process-wide VM pointer.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    // SAFETY: this is an unowned Activity reference valid while `app` lives.
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let intent = env
        .call_method(activity, "getIntent", "()Landroid/content/Intent;", &[])
        .ok()?
        .l()
        .ok()?;
    let data = env
        .call_method(intent, "getDataString", "()Ljava/lang/String;", &[])
        .ok()?
        .l()
        .ok()?;
    if data.is_null() {
        return None;
    }
    let data = JString::from(data);
    env.get_string(&data).ok().map(|value| value.into())
}

fn finish_activity(app: &slint::android::AndroidApp) {
    // SAFETY: Android owns this process-wide VM pointer.
    let Ok(vm) = (unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }) else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    // SAFETY: see activity_intent_data.
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let _ = env.call_method(activity, "finish", "()V", &[]);
}
