use flectar_mail::documents::{
    DocumentFuture, DocumentProvider, DocumentRequestGuard, ImportedDocument,
};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JClass, JObject, JString, JValue},
    sys::{jboolean, jlong},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
};
use tokio::sync::oneshot;
type PendingReply = (
    oneshot::Sender<Result<Option<ImportedDocument>, String>>,
    bool,
);
pub struct AndroidDocuments {
    vm: JavaVM,
    activity: Mutex<GlobalRef>,
    pending: Mutex<HashMap<i64, PendingReply>>,
    next: AtomicI64,
}
static PROVIDER: OnceLock<Arc<AndroidDocuments>> = OnceLock::new();
impl AndroidDocuments {
    pub fn new(app: &slint::android::AndroidApp) -> Result<Arc<Self>, String> {
        // Android owns both references; GlobalRef retains the Activity.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
        let env = vm.attach_current_thread().map_err(|e| e.to_string())?;
        let activity = env
            .new_global_ref(unsafe { JObject::from_raw(app.activity_as_ptr().cast()) })
            .map_err(|e| e.to_string())?;
        drop(env);
        if let Some(provider) = PROVIDER.get() {
            *provider.activity.lock().unwrap() = activity;
            return Ok(provider.clone());
        }
        let provider = Arc::new(Self {
            vm,
            activity: Mutex::new(activity),
            pending: Mutex::new(HashMap::new()),
            next: AtomicI64::new(1),
        });
        PROVIDER
            .set(provider.clone())
            .map_err(|_| "Document provider already initialized")?;
        Ok(provider)
    }
    fn request(&self, path: String, name: String) -> DocumentFuture<Option<ImportedDocument>> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(id, (sender, path.is_empty()));
        let guard = DocumentRequestGuard(Box::new(move || {
            if let Some(provider) = PROVIDER.get() {
                let pending = provider.pending.lock().unwrap().remove(&id);
                if pending.is_some() {
                    let _ = provider.cancel(id);
                }
            }
        }));
        let result = (|| -> Result<(), String> {
            let mut env = self.vm.attach_current_thread().map_err(|e| e.to_string())?;
            let path = env.new_string(path).map_err(|e| e.to_string())?;
            let name = env.new_string(name).map_err(|e| e.to_string())?;
            env.call_method(
                self.activity.lock().unwrap().as_obj(),
                "chooseDocument",
                "(JLjava/lang/String;Ljava/lang/String;)V",
                &[
                    JValue::Long(id),
                    JValue::Object(&path),
                    JValue::Object(&name),
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })();
        Box::pin(async move {
            let _guard = guard;
            if let Err(e) = result {
                if let Some(p) = PROVIDER.get() {
                    p.pending.lock().unwrap().remove(&id);
                }
                return Err(e);
            }
            tokio::time::timeout(std::time::Duration::from_secs(600), receiver)
                .await
                .map_err(|_| "Document selection timed out.".to_string())
                .and_then(|v| v.map_err(|_| "Document selection ended.".to_string()))
                .and_then(|v| v)
        })
    }
}
impl AndroidDocuments {
    fn cancel(&self, id: i64) -> Result<(), String> {
        let mut env = self.vm.attach_current_thread().map_err(|e| e.to_string())?;
        let result = env.call_method(
            self.activity.lock().unwrap().as_obj(),
            "cancelDocumentRequest",
            "(J)V",
            &[JValue::Long(id)],
        );
        if result.is_err() {
            let _ = env.exception_clear();
        }
        result.map(|_| ()).map_err(|e| e.to_string())
    }
    fn release(path: PathBuf) {
        let Some(provider) = PROVIDER.get() else {
            return;
        };
        let Ok(mut env) = provider.vm.attach_current_thread() else {
            return;
        };
        let Ok(path) = env.new_string(path.to_string_lossy()) else {
            return;
        };
        if env
            .call_method(
                provider.activity.lock().unwrap().as_obj(),
                "releaseImportedDocument",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&path)],
            )
            .is_err()
        {
            let _ = env.exception_clear();
        }
    }
}
impl DocumentProvider for AndroidDocuments {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>> {
        self.request(String::new(), String::new())
    }
    fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool> {
        let request = self.request(path.to_string_lossy().into_owned(), name);
        Box::pin(async move { Ok(request.await?.is_some()) })
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeDocumentResult(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
    path: JString,
    error: JString,
) {
    let path = env
        .get_string(&path)
        .map(|s| s.to_string_lossy().into_owned());
    let error = env
        .get_string(&error)
        .map(|s| s.to_string_lossy().into_owned());
    if let Some(provider) = PROVIDER.get() {
        let pending = provider.pending.lock().unwrap().remove(&id);
        let importing = pending.as_ref().is_none_or(|(_, importing)| *importing);
        let result = match (path, error) {
            (Ok(p), Ok(e)) if e.is_empty() => Ok((!p.is_empty()).then(|| {
                if importing {
                    ImportedDocument::temporary(PathBuf::from(p), AndroidDocuments::release)
                } else {
                    ImportedDocument::source(PathBuf::from(p))
                }
            })),
            (Ok(_), Ok(e)) => Err(e),
            _ => Err("Invalid document picker result.".into()),
        };
        if let Some((sender, _)) = pending {
            // A dropped receiver drops its owned import, including the race
            // between native completion and cancellation of the Rust task.
            let _ = sender.send(result);
        }
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeDocumentRequestActive(
    _env: JNIEnv,
    _class: JClass,
    id: jlong,
) -> jboolean {
    u8::from(PROVIDER.get().is_some_and(|p| {
        p.pending
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|(sender, _)| !sender.is_closed())
    }))
}
