//! Native document access boundary. Providers import into private staging and
//! export completed files; content-provider URIs never reach filesystem APIs.
use std::{future::Future, path::PathBuf, pin::Pin};
pub type DocumentFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send>>;
/// Owns a native import copy until Core has staged it. Dropping a cancelled
/// request or an unread channel result must release the copy as well.
pub struct ImportedDocument {
    path: PathBuf,
    cleanup: Option<Box<dyn FnOnce(PathBuf) + Send>>,
}
impl ImportedDocument {
    pub fn source(path: PathBuf) -> Self {
        Self {
            path,
            cleanup: None,
        }
    }
    pub fn temporary(path: PathBuf, cleanup: impl FnOnce(PathBuf) + Send + 'static) -> Self {
        Self {
            path,
            cleanup: Some(Box::new(cleanup)),
        }
    }
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}
impl Drop for ImportedDocument {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup(self.path.clone());
        }
    }
}
/// Runs cancellation even when a task is aborted while awaiting a picker.
pub struct DocumentRequestGuard(pub Box<dyn FnOnce() + Send>);
impl Drop for DocumentRequestGuard {
    fn drop(&mut self) {
        (std::mem::replace(&mut self.0, Box::new(|| {})))();
    }
}
pub trait DocumentProvider: Send + Sync + 'static {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>>;
    fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool>;
    /// Import several documents for one user action (compose attachments).
    /// Hosts whose picker selects multiple files at once override this; the
    /// default imports one document per request, so a single-document picker
    /// never opens twice for one action. Each returned document owns its copy,
    /// so the caller must keep them alive until Core has staged the files.
    fn import_many(
        self: std::sync::Arc<Self>,
        _limit: usize,
    ) -> DocumentFuture<Vec<ImportedDocument>> {
        Box::pin(async move { Ok(self.import().await?.into_iter().collect()) })
    }
}
/// Desktop file dialogs. Mobile hosts inject their own provider through
/// `PlatformContext::app_private`.
#[cfg(desktop)]
pub fn default_provider() -> std::sync::Arc<dyn DocumentProvider> {
    std::sync::Arc::new(DesktopDocuments)
}
pub(crate) fn safe_name(name: &str) -> String {
    if flectar_mail_core::files::validate_name(name).is_ok() {
        name.into()
    } else {
        "download".into()
    }
}

#[cfg(desktop)]
struct DesktopDocuments;
#[cfg(desktop)]
impl DocumentProvider for DesktopDocuments {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>> {
        Box::pin(async {
            Ok(rfd::AsyncFileDialog::new()
                .set_title("Upload file")
                .pick_file()
                .await
                .map(|f| ImportedDocument::source(f.path().to_owned())))
        })
    }
    fn import_many(
        self: std::sync::Arc<Self>,
        limit: usize,
    ) -> DocumentFuture<Vec<ImportedDocument>> {
        Box::pin(async move {
            Ok(rfd::AsyncFileDialog::new()
                .set_title("Upload files")
                .pick_files()
                .await
                .map(|files| {
                    files
                        .into_iter()
                        .take(limit)
                        .map(|file| ImportedDocument::source(file.path().to_owned()))
                        .collect()
                })
                .unwrap_or_default())
        })
    }
    fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool> {
        Box::pin(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Download file")
                .set_file_name(name)
                .save_file()
                .await
            else {
                return Ok(false);
            };
            flectar_mail_core::files::save_cached_file(&path, file.path())
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn imported(path: PathBuf, drops: Arc<AtomicUsize>) -> ImportedDocument {
        ImportedDocument::temporary(path, move |path| {
            std::fs::remove_file(path).unwrap();
            drops.fetch_add(1, Ordering::SeqCst);
        })
    }

    #[tokio::test]
    async fn cancellation_releases_delivered_but_unread_import() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy");
        std::fs::write(&path, "private attachment").unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = tokio::sync::oneshot::channel();
        assert!(sender.send(imported(path.clone(), drops.clone())).is_ok());
        drop(receiver);
        assert!(!path.exists());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn late_result_releases_import_after_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy");
        std::fs::write(&path, "private attachment").unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = tokio::sync::oneshot::channel();
        drop(receiver);
        drop(sender.send(imported(path.clone(), drops.clone())));
        assert!(!path.exists());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn aborting_task_cancels_request_and_releases_staging_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy");
        std::fs::write(&path, "private attachment").unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let document = imported(path.clone(), drops.clone());
        let cancelled = Arc::new(AtomicUsize::new(0));
        let cancellation = cancelled.clone();
        let guard = DocumentRequestGuard(Box::new(move || {
            cancellation.fetch_add(1, Ordering::SeqCst);
        }));
        let (ready, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _document = document;
            let _guard = guard;
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        started.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!path.exists());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(cancelled.load(Ordering::SeqCst), 1);
    }

    struct FakeDocuments {
        directory: PathBuf,
        imports: AtomicUsize,
        cancel_after: usize,
    }

    impl DocumentProvider for FakeDocuments {
        fn import(&self) -> DocumentFuture<Option<ImportedDocument>> {
            let seen = self.imports.fetch_add(1, Ordering::SeqCst);
            let cancel = seen >= self.cancel_after;
            let path = self.directory.join(format!("import-{seen}"));
            Box::pin(async move {
                if cancel {
                    return Ok(None);
                }
                std::fs::write(&path, b"attachment").unwrap();
                Ok(Some(ImportedDocument::temporary(path, |path| {
                    let _ = std::fs::remove_file(path);
                })))
            })
        }
        fn export(&self, _path: PathBuf, _name: String) -> DocumentFuture<bool> {
            Box::pin(async { Ok(false) })
        }
    }

    #[tokio::test]
    async fn import_many_uses_one_picker_request() {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(FakeDocuments {
            directory: directory.path().to_owned(),
            imports: AtomicUsize::new(0),
            cancel_after: usize::MAX,
        });
        let documents = provider.clone().import_many(20).await.unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(provider.imports.load(Ordering::SeqCst), 1);
        drop(documents);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn import_many_returns_empty_when_the_picker_is_cancelled() {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(FakeDocuments {
            directory: directory.path().to_owned(),
            imports: AtomicUsize::new(0),
            cancel_after: 0,
        });
        let documents = provider.clone().import_many(20).await.unwrap();
        assert!(documents.is_empty());
        assert_eq!(provider.imports.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn desktop_source_survives_document_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user-original");
        std::fs::write(&path, "original").unwrap();
        drop(ImportedDocument::source(path.clone()));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "original");
    }
}
