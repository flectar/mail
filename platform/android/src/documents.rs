use flectar_mail::documents::{
    DocumentDialog, DocumentFuture, DocumentProvider, DocumentRequestGuard, ImportedDocument,
};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JClass, JObject, JObjectArray, JString, JValue},
    sys::{jboolean, jlong},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;
/// How long a picker may take to choose and copy one document.
const PICKER_TIMEOUT: Duration = Duration::from_secs(600);
/// Several documents can be gigabytes of mail to copy.
const SEVERAL_TIMEOUT: Duration = Duration::from_secs(3600);
/// Where a request's result goes: one document, imported or exported, or
/// several imported for one action.
enum PendingReply {
    One(
        oneshot::Sender<Result<Option<ImportedDocument>, String>>,
        bool,
    ),
    Many(oneshot::Sender<Result<Vec<ImportedDocument>, String>>),
}
impl PendingReply {
    fn is_closed(&self) -> bool {
        match self {
            Self::One(sender, _) => sender.is_closed(),
            Self::Many(sender) => sender.is_closed(),
        }
    }
}
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
        let importing = path.is_empty();
        self.start(
            |sender| PendingReply::One(sender, importing),
            PICKER_TIMEOUT,
            |env, activity, id| {
                let path = env.new_string(path)?;
                let name = env.new_string(name)?;
                env.call_method(
                    activity,
                    "chooseDocument",
                    "(JLjava/lang/String;Ljava/lang/String;)V",
                    &[
                        JValue::Long(id),
                        JValue::Object(&path),
                        JValue::Object(&name),
                    ],
                )?;
                Ok(())
            },
        )
    }
    /// Registers request `id`, has `call` open its picker, and waits for its
    /// result, cancelling the picker if the request is dropped first.
    fn start<T: Send + 'static>(
        &self,
        reply: impl FnOnce(oneshot::Sender<Result<T, String>>) -> PendingReply,
        timeout: Duration,
        call: impl FnOnce(&mut JNIEnv, &JObject, i64) -> jni::errors::Result<()>,
    ) -> DocumentFuture<T> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, reply(sender));
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
            let activity = self.activity.lock().unwrap();
            call(&mut env, activity.as_obj(), id).map_err(|error| {
                let _ = env.exception_clear();
                error.to_string()
            })
        })();
        Box::pin(async move {
            let _guard = guard;
            if let Err(e) = result {
                if let Some(p) = PROVIDER.get() {
                    p.pending.lock().unwrap().remove(&id);
                }
                return Err(e);
            }
            tokio::time::timeout(timeout, receiver)
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
    fn import_many(
        &self,
        limit: usize,
        _dialog: DocumentDialog,
    ) -> DocumentFuture<Vec<ImportedDocument>> {
        let limit = i32::try_from(limit).unwrap_or(i32::MAX);
        self.start(PendingReply::Many, SEVERAL_TIMEOUT, |env, activity, id| {
            env.call_method(
                activity,
                "chooseDocuments",
                "(JI)V",
                &[JValue::Long(id), JValue::Int(limit)],
            )?;
            Ok(())
        })
    }
}
/// The strings in a Java `String[]`.
fn strings(env: &mut JNIEnv, array: &JObjectArray) -> jni::errors::Result<Vec<String>> {
    let count = env.get_array_length(array)?;
    let mut values = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    for index in 0..count {
        let string = JString::from(env.get_object_array_element(array, index)?);
        values.push(env.get_string(&string)?.to_string_lossy().into_owned());
        // JNI guarantees room for only 16 local references.
        env.delete_local_ref(string)?;
    }
    Ok(values)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeDocumentResult(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
    path: JString,
    name: JString,
    error: JString,
) {
    let path = env
        .get_string(&path)
        .map(|s| s.to_string_lossy().into_owned());
    let name = env
        .get_string(&name)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let error = env
        .get_string(&error)
        .map(|s| s.to_string_lossy().into_owned());
    if let Some(provider) = PROVIDER.get() {
        let pending = provider.pending.lock().unwrap().remove(&id);
        let importing = !matches!(pending, Some(PendingReply::One(_, false)));
        let result = match (path, error) {
            (Ok(p), Ok(e)) if e.is_empty() => Ok((!p.is_empty()).then(|| {
                if importing {
                    ImportedDocument::temporary(PathBuf::from(p), AndroidDocuments::release)
                        .with_name(name)
                } else {
                    ImportedDocument::source(PathBuf::from(p))
                }
            })),
            (Ok(_), Ok(e)) => Err(e),
            _ => Err("Invalid document picker result.".into()),
        };
        if let Some(PendingReply::One(sender, _)) = pending {
            // A dropped receiver drops its owned import, including the race
            // between native completion and cancellation of the Rust task.
            let _ = sender.send(result);
        }
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_flectar_mail_FlectarActivity_nativeDocumentsResult(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
    paths: JObjectArray,
    names: JObjectArray,
    error: JString,
) {
    let paths = strings(&mut env, &paths);
    let mut names = strings(&mut env, &names).unwrap_or_default().into_iter();
    let error = env
        .get_string(&error)
        .map(|s| s.to_string_lossy().into_owned());
    // The copies are Rust's from here; dropping them releases them.
    let documents = paths.map(|paths| {
        paths
            .into_iter()
            .map(|path| {
                ImportedDocument::temporary(PathBuf::from(path), AndroidDocuments::release)
                    .with_name(names.next().unwrap_or_default())
            })
            .collect::<Vec<_>>()
    });
    let result = match (documents, error) {
        (Ok(documents), Ok(error)) if error.is_empty() => Ok(documents),
        (Ok(_), Ok(error)) => Err(error),
        _ => Err("Invalid document picker result.".into()),
    };
    let pending = PROVIDER
        .get()
        .and_then(|provider| provider.pending.lock().unwrap().remove(&id));
    if let Some(PendingReply::Many(sender)) = pending {
        let _ = sender.send(result);
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
            .is_some_and(|reply| !reply.is_closed())
    }))
}
