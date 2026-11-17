//! Spelling with the spell checker the user chose in Android's settings, usually the
//! keyboard's, which checks in its own process and in the language set there. Spelling.java
//! talks to it; a session lasts while messages are being written.

use flectar_mail::{SpellingHost, SpellingSession};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JClass, JIntArray, JObject, JObjectArray, JString, JValue},
};
use std::{ops::Range, sync::Arc};

const SPELLING: &str = "com.flectar.mail.Spelling";

/// The Java side, from any thread.
struct Java {
    vm: JavaVM,
    /// The Application, which outlives any activity.
    context: GlobalRef,
}

impl Java {
    /// Run `work` attached to Java, freeing the local references it makes and any Java
    /// exception it leaves.
    fn with_env<T>(
        &self,
        work: impl for<'local> FnOnce(&mut JNIEnv<'local>) -> jni::errors::Result<T>,
    ) -> Result<T, String> {
        let mut env = self
            .vm
            .attach_current_thread()
            .map_err(|error| format!("attaching to Android: {error}"))?;
        let result = env.with_local_frame(16, |env| work(env));
        if result.is_err() {
            let _ = env.exception_clear();
        }
        result.map_err(|error| error.to_string())
    }

    /// Run `work` with Spelling.java's class and the Application.
    fn with_class<T>(
        &self,
        work: impl for<'local> FnOnce(
            &mut JNIEnv<'local>,
            &JClass<'local>,
            &JObject,
        ) -> jni::errors::Result<T>,
    ) -> Result<T, String> {
        self.with_env(|env| {
            let class = crate::java::app_class(env, self.context.as_obj(), SPELLING)?;
            work(env, &class, self.context.as_obj())
        })
    }
}

/// Android's spell checker, for the shared app.
pub(super) struct AndroidSpelling(Arc<Java>);

impl AndroidSpelling {
    pub(super) fn new(app: &slint::android::AndroidApp) -> Result<Self, String> {
        // SAFETY: AndroidApp owns these live VM and Activity references.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }
            .map_err(|error| error.to_string())?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|error| error.to_string())?;
        // SAFETY: as above; the reference is only read here.
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let application = env
            .call_method(
                &activity,
                "getApplicationContext",
                "()Landroid/content/Context;",
                &[],
            )
            .and_then(|value| value.l())
            .map_err(|error| format!("Android application context: {error}"))?;
        let context = env
            .new_global_ref(application)
            .map_err(|error| error.to_string())?;
        drop(env);
        Ok(Self(Arc::new(Java { vm, context })))
    }
}

impl SpellingHost for AndroidSpelling {
    fn available(&self) -> bool {
        self.0
            .with_class(|env, class, context| {
                env.call_static_method(
                    class,
                    "available",
                    "(Landroid/content/Context;)Z",
                    &[JValue::Object(context)],
                )?
                .z()
            })
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "spelling: could not ask Android");
                false
            })
    }

    fn open(&self) -> Option<Box<dyn SpellingSession>> {
        let spelling = self
            .0
            .with_class(|env, class, context| {
                let spelling = env.new_object(
                    class,
                    "(Landroid/content/Context;)V",
                    &[JValue::Object(context)],
                )?;
                env.new_global_ref(spelling)
            })
            .map_err(|error| tracing::warn!(%error, "spelling: Android's spell checker"))
            .ok()?;
        Some(Box::new(Session {
            java: self.0.clone(),
            spelling,
        }))
    }
}

/// A Spelling.java, which holds the session with the spell checker.
struct Session {
    java: Arc<Java>,
    spelling: GlobalRef,
}

impl SpellingSession for Session {
    fn misspelled(&self, text: &str) -> Result<Vec<Range<usize>>, String> {
        let offsets = self.java.with_env(|env| {
            let text = env.new_string(text)?;
            let found = env
                .call_method(
                    &self.spelling,
                    "check",
                    "(Ljava/lang/String;)[I",
                    &[JValue::Object(&text)],
                )?
                .l()?;
            // Null when Android checks no spelling now.
            if found.is_null() {
                return Ok(Vec::new());
            }
            let found = JIntArray::from(found);
            let mut offsets = vec![0; usize::try_from(env.get_array_length(&found)?).unwrap_or(0)];
            env.get_int_array_region(&found, 0, &mut offsets)?;
            Ok(offsets)
        })?;
        let (pairs, _) = offsets.as_chunks::<2>();
        Ok(pairs
            .iter()
            .filter_map(|[start, end]| {
                Some(usize::try_from(*start).ok()?..usize::try_from(*end).ok()?)
            })
            .collect())
    }

    fn suggest(&self, word: &str) -> Result<Vec<String>, String> {
        self.java.with_env(|env| {
            let word = env.new_string(word)?;
            let found = env
                .call_method(
                    &self.spelling,
                    "suggest",
                    "(Ljava/lang/String;)[Ljava/lang/String;",
                    &[JValue::Object(&word)],
                )?
                .l()?;
            let found = JObjectArray::from(found);
            let mut suggestions = Vec::new();
            for index in 0..env.get_array_length(&found)? {
                let suggestion = JString::from(env.get_object_array_element(&found, index)?);
                suggestions.push(env.get_string(&suggestion)?.into());
                env.delete_local_ref(suggestion)?;
            }
            Ok(suggestions)
        })
    }

    fn add_word(&self, word: &str) -> Result<(), String> {
        let opened = self.java.with_env(|env| {
            let word = env.new_string(word)?;
            env.call_method(
                &self.spelling,
                "addWord",
                "(Ljava/lang/String;)Z",
                &[JValue::Object(&word)],
            )?
            .z()
        })?;
        if opened {
            Ok(())
        } else {
            Err("Android has no dialog to add words to the dictionary".into())
        }
    }
}

impl Drop for Session {
    /// The last message closed: end the session with the spell checker.
    fn drop(&mut self) {
        if let Err(error) = self.java.with_env(|env| {
            env.call_method(&self.spelling, "close", "()V", &[])
                .map(|_| ())
        }) {
            tracing::warn!(%error, "spelling: could not end Android's session");
        }
    }
}
