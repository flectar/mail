use jni::{objects::JClass, sys::jint};

/// `FlectarActivity.onTrimMemory`, with the device's API level.
#[unsafe(no_mangle)]
extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeTrimMemory(
    _env: jni::JNIEnv,
    _class: JClass,
    level: jint,
    sdk: jint,
) {
    flectar_mail::platform::trim_memory(level, sdk);
}
