use crate::documents::{DocumentFuture, DocumentProvider, DocumentRequestGuard, ImportedDocument};
use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char},
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
};
use tokio::sync::oneshot;
type Reply = oneshot::Sender<Result<Option<ImportedDocument>, String>>;
static PENDING: OnceLock<Mutex<HashMap<i64, (Reply, bool)>>> = OnceLock::new();
static NEXT: AtomicI64 = AtomicI64::new(1);
unsafe extern "C" {
    fn flectar_cancel_document(id: i64);
    fn flectar_choose_document(
        id: i64,
        path: *const c_char,
        callback: extern "C" fn(i64, *const c_char, *const c_char),
    );
}
extern "C" fn result(id: i64, path: *const c_char, error: *const c_char) {
    // The native bridge keeps these UTF-8 strings alive for this callback.
    let string = |ptr: *const c_char| {
        if ptr.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        }
    };
    let path = string(path);
    let error = string(error);
    let pending = PENDING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(&id);
    let importing = pending.as_ref().is_none_or(|(_, importing)| *importing);
    let response = if error.is_empty() {
        Ok((!path.is_empty()).then(|| {
            if importing {
                ImportedDocument::temporary(PathBuf::from(path), release_import)
            } else {
                ImportedDocument::source(PathBuf::from(path))
            }
        }))
    } else {
        Err(error)
    };
    if let Some((sender, _)) = pending {
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
    if file.parent().and_then(|p| p.parent()) == Some(root.as_path()) && file.is_file() {
        // Remove only this copy and its now-empty unique directory.
        if std::fs::remove_file(&file).is_ok() {
            let _ = std::fs::remove_dir(file.parent().unwrap());
        }
    }
}

pub struct IosDocuments;
fn request(path: PathBuf) -> DocumentFuture<Option<ImportedDocument>> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = oneshot::channel();
    let value = CString::new(path.to_string_lossy().as_bytes());
    if let Ok(value) = value {
        PENDING
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(id, (sender, path.as_os_str().is_empty()));
        unsafe {
            flectar_choose_document(id, value.as_ptr(), result);
        }
    }
    let guard = DocumentRequestGuard(Box::new(move || {
        if PENDING
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .remove(&id)
            .is_some()
        {
            unsafe {
                flectar_cancel_document(id);
            }
        }
    }));
    Box::pin(async move {
        let _guard = guard;
        tokio::time::timeout(std::time::Duration::from_secs(600), receiver)
            .await
            .map_err(|_| "Document selection timed out.".to_string())
            .and_then(|r| r.map_err(|_| "Document selection ended.".to_string()))
            .and_then(|r| r)
    })
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
}
