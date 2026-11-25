//! Scanning a QR code with Google's code scanner, to pair devices in Flectar
//! Mail Sync. The scanner runs in Google Play services, so the app needs no
//! camera permission (QrScanner.java). Phones without Play services, such as
//! Huawei's, pair with a code instead.
use flectar_mail::{QrScannerHost, ScanError, ScanFuture, ScanResult, ScanTexts};
use jni::{
    JNIEnv, JavaVM,
    objects::{JClass, JObject, JString, JValue},
};
use std::sync::{Mutex, MutexGuard, PoisonError};
use tokio::sync::oneshot;

/// The scan waiting for QrScanner's answer.
static PENDING: Mutex<Option<oneshot::Sender<ScanResult>>> = Mutex::new(None);

pub struct AndroidQrScanner {
    app: slint::android::AndroidApp,
    available: bool,
}

impl AndroidQrScanner {
    pub fn new(app: &slint::android::AndroidApp) -> Self {
        let available = with_scanner(app, |env, class, activity| {
            env.call_static_method(
                class,
                "available",
                "(Landroid/app/Activity;)Z",
                &[JValue::Object(activity)],
            )?
            .z()
        })
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "could not tell whether the QR code scanner works here");
            false
        });
        Self {
            app: app.clone(),
            available,
        }
    }
}

impl QrScannerHost for AndroidQrScanner {
    fn available(&self) -> bool {
        self.available
    }

    /// Google's scanner has words of its own.
    fn scan(&self, _texts: ScanTexts) -> ScanFuture {
        let (sender, receiver) = oneshot::channel();
        // An earlier scan nobody answered ends as closed.
        pending().replace(sender);
        let started = with_scanner(&self.app, |env, class, activity| {
            env.call_static_method(
                class,
                "scan",
                "(Landroid/app/Activity;)V",
                &[JValue::Object(activity)],
            )?;
            Ok(())
        });
        if let Err(error) = started
            && let Some(sender) = pending().take()
        {
            let _ = sender.send(Err(ScanError::Unavailable(error)));
        }
        Box::pin(async move { receiver.await.unwrap_or(Ok(None)) })
    }
}

fn pending() -> MutexGuard<'static, Option<oneshot::Sender<ScanResult>>> {
    PENDING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run `call` with QrScanner's class and the activity.
fn with_scanner<T>(
    app: &slint::android::AndroidApp,
    call: impl FnOnce(&mut JNIEnv, &JClass, &JObject) -> jni::errors::Result<T>,
) -> Result<T, String> {
    // SAFETY: AndroidApp owns these live VM and Activity references.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let result = crate::java::app_class(&mut env, &activity, "com.flectar.mail.QrScanner")
        .and_then(|class| call(&mut env, &class, &activity));
    if result.is_err() {
        let _ = env.exception_clear();
    }
    result.map_err(|e| e.to_string())
}

/// QrScanner's answer: the code's text, the reason the scanner failed, or
/// neither when the person closed it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_QrScanner_nativeResult(
    mut env: JNIEnv,
    _class: JClass,
    text: JString,
    error: JString,
) {
    let mut read = |value: &JString| {
        (!value.is_null())
            .then(|| env.get_string(value).ok().map(String::from))
            .flatten()
    };
    let text = read(&text);
    let error = read(&error);
    let Some(sender) = pending().take() else {
        return;
    };
    let _ = sender.send(match (text, error) {
        (Some(text), _) => Ok(Some(text)),
        (None, Some(error)) => Err(ScanError::Unavailable(error)),
        (None, None) => Ok(None),
    });
}
