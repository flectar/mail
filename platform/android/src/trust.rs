//! Server certificates checked by Android itself, as for the platform's own
//! HTTPS connections. SystemTrust.java hands each chain to the default
//! TrustManager, which follows the app's network security config and fetches
//! nothing online. The core checks the server name and the handshake
//! signatures, and asks this for every TLS connection, mail and HTTP alike.

use flectar_mail::flectar_mail_core::net::tls::{
    PlatformTrust, PlatformTrustError, use_platform_trust,
};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JObject, JString, JValue},
};
use std::sync::Arc;

const SYSTEM_TRUST: &str = "com.flectar.mail.SystemTrust";

/// SystemTrust.java, from any thread.
struct AndroidTrust {
    vm: JavaVM,
    /// The class, loaded once through the app's class loader: `FindClass`
    /// can't see the app's classes from threads Java didn't start, such as
    /// the async runtime's.
    class: GlobalRef,
}

impl std::fmt::Debug for AndroidTrust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AndroidTrust")
    }
}

impl AndroidTrust {
    /// SystemTrust.java's verdict on `chain` for `host`: `None` when Android
    /// trusts it, or else its refusal.
    fn verdict(
        &self,
        chain: &[&[u8]],
        host: &str,
        ocsp: Option<&[u8]>,
    ) -> Result<Option<String>, String> {
        let length = i32::try_from(chain.len()).map_err(|_| "too many certificates".to_owned())?;
        let mut env = self
            .vm
            .attach_current_thread()
            .map_err(|error| format!("attaching to Android: {error}"))?;
        let verdict = env.with_local_frame(8, |env| -> jni::errors::Result<_> {
            let certificates = env.new_object_array(length, "[B", JObject::null())?;
            for (index, certificate) in (0..).zip(chain) {
                let certificate = env.byte_array_from_slice(certificate)?;
                env.set_object_array_element(&certificates, index, &certificate)?;
                // Keep one certificate's reference at a time, however long
                // the chain a server sends.
                env.delete_local_ref(certificate)?;
            }
            let host = env.new_string(host)?;
            let ocsp = match ocsp {
                Some(response) => JObject::from(env.byte_array_from_slice(response)?),
                None => JObject::null(),
            };
            let verdict = env
                .call_static_method(
                    &self.class,
                    "check",
                    "([[BLjava/lang/String;[B)Ljava/lang/String;",
                    &[
                        JValue::Object(&certificates),
                        JValue::Object(&host),
                        JValue::Object(&ocsp),
                    ],
                )?
                .l()?;
            if verdict.is_null() {
                return Ok(None);
            }
            let text: String = env.get_string(&JString::from(verdict))?.into();
            Ok(Some(text))
        });
        verdict.map_err(|error| {
            if matches!(error, jni::errors::Error::JavaException) {
                // SystemTrust.check catches exceptions, so this is an error
                // such as running out of memory. Log it, then clear it.
                let _ = env.exception_describe();
                let _ = env.exception_clear();
            }
            error.to_string()
        })
    }
}

impl PlatformTrust for AndroidTrust {
    fn check(
        &self,
        chain: &[&[u8]],
        host: &str,
        ocsp: Option<&[u8]>,
    ) -> Result<(), PlatformTrustError> {
        match self.verdict(chain, host, ocsp) {
            Ok(None) => Ok(()),
            Ok(Some(refusal)) => Err(parse_refusal(&refusal)),
            Err(error) => Err(PlatformTrustError::Unavailable(error)),
        }
    }
}

/// SystemTrust.java's refusal, `kind: Android's reason`, as the core's.
fn parse_refusal(refusal: &str) -> PlatformTrustError {
    let (kind, reason) = refusal.split_once(':').unwrap_or(("untrusted", refusal));
    let reason = reason.trim().to_owned();
    match kind {
        "expired" => PlatformTrustError::Expired,
        "not-yet-valid" => PlatformTrustError::NotValidYet,
        "revoked" => PlatformTrustError::Revoked,
        "encoding" => PlatformTrustError::BadEncoding,
        "unavailable" => PlatformTrustError::Unavailable(reason),
        _ => PlatformTrustError::Untrusted(reason),
    }
}

/// Check every server certificate with Android from now on. `context` is a
/// live Context of the app, whose class loader has SystemTrust.java. The
/// first call wins; later ones change nothing.
pub(super) fn install(env: &mut JNIEnv, context: &JObject) -> Result<(), String> {
    let vm = env.get_java_vm().map_err(|error| error.to_string())?;
    let class = env
        .with_local_frame(8, |env| {
            let class = crate::java::app_class(env, context, SYSTEM_TRUST)?;
            env.new_global_ref(class)
        })
        .map_err(|error| {
            let _ = env.exception_clear();
            format!("loading {SYSTEM_TRUST}: {error}")
        })?;
    use_platform_trust(Arc::new(AndroidTrust { vm, class }));
    Ok(())
}

/// [`install`] with the window's activity.
pub(super) fn install_for_window(app: &slint::android::AndroidApp) -> Result<(), String> {
    // SAFETY: AndroidApp owns the live process VM.
    let vm =
        unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|error| error.to_string())?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|error| error.to_string())?;
    // SAFETY: AndroidApp owns the Activity reference; it is only read here.
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    install(&mut env, &activity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_name_their_kind() {
        assert_eq!(
            parse_refusal("expired: Chain validation failed: timestamp check failed"),
            PlatformTrustError::Expired
        );
        assert_eq!(
            parse_refusal("not-yet-valid: Chain validation failed"),
            PlatformTrustError::NotValidYet
        );
        assert_eq!(
            parse_refusal("revoked: Certificate has been revoked"),
            PlatformTrustError::Revoked
        );
        assert_eq!(
            parse_refusal("encoding: Unable to parse"),
            PlatformTrustError::BadEncoding
        );
        assert_eq!(
            parse_refusal("untrusted: Trust anchor for certification path not found."),
            PlatformTrustError::Untrusted("Trust anchor for certification path not found.".into())
        );
        assert_eq!(
            parse_refusal("unavailable: Android offers no X.509 TrustManager"),
            PlatformTrustError::Unavailable("Android offers no X.509 TrustManager".into())
        );
        // A refusal without a kind is an untrusted chain, in Android's words.
        assert_eq!(
            parse_refusal("something else"),
            PlatformTrustError::Untrusted("something else".into())
        );
    }
}
