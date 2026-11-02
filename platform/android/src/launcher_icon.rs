//! Bridge to FlectarActivity.setLauncherIcon for the app icon chosen in
//! Settings.
use jni::{
    JavaVM,
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
        .map_err(|_| "Launcher icon bridge already initialized")?;
    flectar_mail::platform::configure_launcher_icon(|icon| {
        if let Err(error) = choose(icon) {
            tracing::warn!(%error, "could not choose the launcher icon");
        }
    });
    Ok(())
}

fn choose(icon: &str) -> Result<(), String> {
    let bridge = BRIDGE
        .get()
        .ok_or("Launcher icon bridge is not initialized")?;
    let mut env = bridge
        .vm
        .attach_current_thread()
        .map_err(|e| e.to_string())?;
    let icon = env.new_string(icon).map_err(|e| e.to_string())?;
    let result = env.call_method(
        bridge
            .activity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_obj(),
        "setLauncherIcon",
        "(Ljava/lang/String;)V",
        &[JValue::Object(&icon)],
    );
    if result.is_err() {
        let _ = env.exception_clear();
    }
    result.map(|_| ()).map_err(|e| e.to_string())
}
