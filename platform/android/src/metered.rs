//! Whether the network in use is metered (#253), as Android's
//! ConnectivityManager tells: mobile data, or a network the user marked as
//! metered. The app reads it to hold back downloads on one.
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JObject, JValue},
};
use std::sync::{Mutex, OnceLock, PoisonError};

struct Bridge {
    vm: JavaVM,
    activity: Mutex<GlobalRef>,
}

static BRIDGE: OnceLock<Bridge> = OnceLock::new();

pub fn install(app: &slint::android::AndroidApp) -> Result<(), String> {
    // Android owns both references; GlobalRef retains the Activity.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
    let env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = env
        .new_global_ref(unsafe { JObject::from_raw(app.activity_as_ptr().cast()) })
        .map_err(|e| e.to_string())?;
    drop(env);
    if let Some(bridge) = BRIDGE.get() {
        *bridge
            .activity
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = activity;
        return Ok(());
    }
    BRIDGE
        .set(Bridge {
            vm,
            activity: Mutex::new(activity),
        })
        .map_err(|_| "Metered connection bridge already initialized")?;
    flectar_mail::platform::configure_metered(|| match metered() {
        Ok(metered) => metered,
        Err(error) => {
            tracing::debug!(%error, "could not ask whether the network is metered");
            false
        }
    });
    Ok(())
}

fn metered() -> Result<bool, String> {
    let bridge = BRIDGE
        .get()
        .ok_or("Metered connection bridge is not initialized")?;
    let mut env = bridge
        .vm
        .attach_current_thread()
        .map_err(|e| e.to_string())?;
    let activity = bridge
        .activity
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let result = env.with_local_frame(8, |env| ask(env, activity.as_obj()));
    if result.is_err() {
        let _ = env.exception_clear();
    }
    result.map_err(|e| e.to_string())
}

/// `getSystemService(CONNECTIVITY_SERVICE).isActiveNetworkMetered()`, which
/// counts no network as metered.
fn ask(env: &mut JNIEnv, activity: &JObject) -> jni::errors::Result<bool> {
    let name = env.new_string("connectivity")?;
    let manager = env
        .call_method(
            activity,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[JValue::Object(&name)],
        )?
        .l()?;
    if manager.is_null() {
        return Ok(false);
    }
    env.call_method(&manager, "isActiveNetworkMetered", "()Z", &[])?
        .z()
}
