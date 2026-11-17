//! Calls into the app's own Java classes from Rust threads.

use jni::{
    JNIEnv,
    objects::{JClass, JObject, JValue},
};

/// The app class `name` (dotted, as `com.flectar.mail.Notifications`), from
/// `context`'s class loader. `FindClass` uses the system class loader on
/// threads Java didn't start, which can't see the app's classes.
pub(super) fn app_class<'local>(
    env: &mut JNIEnv<'local>,
    context: &JObject,
    name: &str,
) -> jni::errors::Result<JClass<'local>> {
    let loader = env
        .call_method(context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
        .l()?;
    let name = env.new_string(name)?;
    let class = env
        .call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&name)],
        )?
        .l()?;
    Ok(JClass::from(class))
}
