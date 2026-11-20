//! Where this copy came from. Google Play requires its own billing for
//! what an app sells, so a copy Play installed doesn't offer Flectar Mail
//! Sync's web checkout.
use jni::{
    JNIEnv, JavaVM,
    objects::{JObject, JString, JValue},
};

/// The name Google Play's store goes by as an installer.
const GOOGLE_PLAY: &str = "com.android.vending";

/// Whether Google Play installed this copy. One from GitHub, another store
/// or a file names another installer, or none.
pub fn installed_by_play(app: &slint::android::AndroidApp) -> Result<bool, String> {
    // SAFETY: AndroidApp owns these live VM and Activity references.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let installer = installer(&mut env, &activity);
    if installer.is_err() {
        let _ = env.exception_clear();
    }
    Ok(installer.map_err(|e| e.to_string())?.as_deref() == Some(GOOGLE_PLAY))
}

fn installer(env: &mut JNIEnv, activity: &JObject) -> jni::errors::Result<Option<String>> {
    let manager = env
        .call_method(
            activity,
            "getPackageManager",
            "()Landroid/content/pm/PackageManager;",
            &[],
        )?
        .l()?;
    let package = env
        .call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])?
        .l()?;
    // Replaced by getInstallSourceInfo from API level 30, but it answers on
    // every version the app runs on (26 and later).
    let installer = env
        .call_method(
            &manager,
            "getInstallerPackageName",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&package)],
        )?
        .l()?;
    if installer.is_null() {
        return Ok(None);
    }
    Ok(Some(env.get_string(&JString::from(installer))?.into()))
}
