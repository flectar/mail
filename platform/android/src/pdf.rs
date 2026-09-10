use jni::{
    JavaVM,
    objects::{JClass, JObject, JString},
};

pub fn configure(app: &slint::android::AndroidApp) -> Result<(), String> {
    // Android owns the VM and Activity. Both remain valid throughout android_main.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let info = env
        .call_method(
            activity,
            "getApplicationInfo",
            "()Landroid/content/pm/ApplicationInfo;",
            &[],
        )
        .and_then(|value| value.l())
        .map_err(|e| e.to_string())?;
    let directory = env
        .get_field(info, "nativeLibraryDir", "Ljava/lang/String;")
        .and_then(|value| value.l())
        .map_err(|e| e.to_string())?;
    let directory: String = env
        .get_string(&JString::from(directory))
        .map_err(|e| e.to_string())?
        .into();
    flectar_mail::pdf_preview::configure_android(directory.into())
}

#[unsafe(no_mangle)]
extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeSuspendPdfPreview(
    _env: jni::JNIEnv,
    _class: JClass,
) {
    flectar_mail::suspend_file_preview();
}
