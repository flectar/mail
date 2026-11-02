use crate::documents::{
    DocumentDialog, DocumentFuture, DocumentProvider, DocumentRequestGuard, ImportedDocument,
};
use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_ulong},
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;
/// Where a request's result goes: one document, imported or exported, or
/// several imported for one action.
enum Pending {
    One(
        oneshot::Sender<Result<Option<ImportedDocument>, String>>,
        bool,
    ),
    Many(oneshot::Sender<Result<Vec<ImportedDocument>, String>>),
}
static PENDING: OnceLock<Mutex<HashMap<i64, Pending>>> = OnceLock::new();
static NEXT: AtomicI64 = AtomicI64::new(1);
/// How long a picker may take to choose and copy one document.
const PICKER_TIMEOUT: Duration = Duration::from_secs(600);
/// Several documents can be gigabytes of mail to copy.
const SEVERAL_TIMEOUT: Duration = Duration::from_secs(3600);
unsafe extern "C" {
    fn flectar_cancel_document(id: i64);
    fn flectar_choose_document(
        id: i64,
        path: *const c_char,
        callback: extern "C" fn(i64, *const c_char, *const c_char, *const c_char),
    );
    fn flectar_choose_documents(
        id: i64,
        limit: c_ulong,
        callback: extern "C" fn(
            i64,
            *const *const c_char,
            *const *const c_char,
            c_ulong,
            *const c_char,
        ),
    );
}
/// A UTF-8 string the native bridge keeps alive for the callback.
fn string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}
fn take(id: i64) -> Option<Pending> {
    PENDING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(&id)
}
/// `count` UTF-8 strings the native bridge keeps alive for the callback.
fn strings(array: *const *const c_char, count: c_ulong) -> Vec<String> {
    if array.is_null() {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(array, usize::try_from(count).unwrap_or(0)) }
        .iter()
        .map(|value| string(*value))
        .collect()
}
extern "C" fn result(id: i64, path: *const c_char, name: *const c_char, error: *const c_char) {
    let path = string(path);
    let name = string(name);
    let error = string(error);
    let pending = take(id);
    let importing = !matches!(pending, Some(Pending::One(_, false)));
    let response = if error.is_empty() {
        Ok((!path.is_empty()).then(|| {
            if importing {
                ImportedDocument::temporary(PathBuf::from(path), release_import).with_name(name)
            } else {
                ImportedDocument::source(PathBuf::from(path))
            }
        }))
    } else {
        Err(error)
    };
    if let Some(Pending::One(sender, _)) = pending {
        let _ = sender.send(response);
    }
}
extern "C" fn results(
    id: i64,
    paths: *const *const c_char,
    names: *const *const c_char,
    count: c_ulong,
    error: *const c_char,
) {
    let mut names = strings(names, count).into_iter();
    // The copies are Rust's from here; dropping them releases them.
    let documents = strings(paths, count)
        .into_iter()
        .map(|path| (path, names.next().unwrap_or_default()))
        .filter(|(path, _)| !path.is_empty())
        .map(|(path, name)| {
            ImportedDocument::temporary(PathBuf::from(path), release_import).with_name(name)
        })
        .collect::<Vec<_>>();
    let error = string(error);
    let response = if error.is_empty() {
        Ok(documents)
    } else {
        Err(error)
    };
    if let Some(Pending::Many(sender)) = take(id) {
        let _ = sender.send(response);
    }
}
fn release_import(path: PathBuf) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let Ok(root) = std::fs::canonicalize(PathBuf::from(home).join("Library/Caches/file-imports"))
    else {
        return;
    };
    let Ok(file) = std::fs::canonicalize(path) else {
        return;
    };
    if file.parent() == Some(root.as_path()) && file.is_file() {
        let _ = std::fs::remove_file(&file);
    }
}

pub struct IosDocuments;
/// Registers a request, opens its picker with `open`, and waits for its
/// result, cancelling the picker if the request is dropped first.
fn start<T: Send + 'static>(
    reply: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Pending,
    timeout: Duration,
    open: impl FnOnce(i64),
) -> DocumentFuture<T> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = oneshot::channel();
    PENDING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(id, reply(sender));
    open(id);
    let guard = DocumentRequestGuard(Box::new(move || {
        if take(id).is_some() {
            unsafe {
                flectar_cancel_document(id);
            }
        }
    }));
    Box::pin(async move {
        let _guard = guard;
        tokio::time::timeout(timeout, receiver)
            .await
            .map_err(|_| "Document selection timed out.".to_string())
            .and_then(|r| r.map_err(|_| "Document selection ended.".to_string()))
            .and_then(|r| r)
    })
}
fn request(path: PathBuf) -> DocumentFuture<Option<ImportedDocument>> {
    let Ok(value) = CString::new(path.to_string_lossy().as_bytes()) else {
        return Box::pin(async { Err("Invalid document path.".to_string()) });
    };
    let importing = path.as_os_str().is_empty();
    start(
        |sender| Pending::One(sender, importing),
        PICKER_TIMEOUT,
        |id| unsafe { flectar_choose_document(id, value.as_ptr(), result) },
    )
}
impl DocumentProvider for IosDocuments {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>> {
        request(PathBuf::new())
    }
    fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool> {
        Box::pin(async move {
            flectar_mail_core::files::validate_name(&name).map_err(|e| e.to_string())?;
            let folder = tempfile::tempdir_in(path.parent().ok_or("Invalid export directory.")?)
                .map_err(|e| e.to_string())?;
            let destination = folder.path().join(name);
            flectar_mail_core::files::save_cached_file(&path, &destination)
                .await
                .map_err(|e| e.to_string())?;
            Ok(request(destination).await?.is_some())
        })
    }
    fn import_many(
        &self,
        limit: usize,
        _dialog: DocumentDialog,
    ) -> DocumentFuture<Vec<ImportedDocument>> {
        let limit = c_ulong::try_from(limit).unwrap_or(c_ulong::MAX);
        start(Pending::Many, SEVERAL_TIMEOUT, |id| unsafe {
            flectar_choose_documents(id, limit, results)
        })
    }
}
