//! Bridge to FlectarActivity.animationsRemoved for the Reduce motion
//! setting.
use jni::{
    JavaVM,
    objects::{GlobalRef, JObject},
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
        .map_err(|_| "Reduce motion bridge already initialized")?;
    flectar_mail::platform::configure_reduce_motion(|| match animations_removed() {
        Ok(removed) => removed,
        Err(error) => {
            tracing::warn!(%error, "could not read the animation setting");
            false
        }
    });
    Ok(())
}

fn animations_removed() -> Result<bool, String> {
    let bridge = BRIDGE
        .get()
        .ok_or("Reduce motion bridge is not initialized")?;
    let mut env = bridge
        .vm
        .attach_current_thread()
        .map_err(|e| e.to_string())?;
    let result = env.call_method(
        bridge
            .activity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_obj(),
        "animationsRemoved",
        "()Z",
        &[],
    );
    if result.is_err() {
        let _ = env.exception_clear();
    }
    result
        .and_then(|value| value.z())
        .map_err(|e| e.to_string())
}
