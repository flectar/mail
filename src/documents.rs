//! Native document access boundary. Providers import into private staging and
//! export completed files; content-provider URIs never reach filesystem APIs.
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};
pub type DocumentFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send>>;
/// Owns a native import copy until Core has staged it. Dropping a cancelled
/// request or an unread channel result must release the copy as well.
pub struct ImportedDocument {
    path: PathBuf,
    display_name: Option<String>,
    cleanup: Option<Box<dyn FnOnce(PathBuf) + Send>>,
}
impl std::fmt::Debug for ImportedDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedDocument")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}
impl ImportedDocument {
    pub fn source(path: PathBuf) -> Self {
        Self {
            path,
            display_name: None,
            cleanup: None,
        }
    }
    pub fn temporary(path: PathBuf, cleanup: impl FnOnce(PathBuf) + Send + 'static) -> Self {
        Self {
            path,
            display_name: None,
            cleanup: Some(Box::new(cleanup)),
        }
    }
    /// Keep the provider's original leaf name separate from its private copy.
    /// Names become attachment MIME metadata and may later be used for export.
    pub fn with_display_name(mut self, name: impl Into<String>) -> Result<Self, String> {
        let name = name.into();
        flectar_mail_core::files::validate_name(&name).map_err(|error| error.to_string())?;
        self.display_name = Some(name);
        Ok(self)
    }
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
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
pub trait DocumentProvider: Send + Sync {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>>;
    fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool>;
}
pub fn default_provider() -> Arc<dyn DocumentProvider> {
    #[cfg(target_os = "ios")]
    {
        Arc::new(crate::ios_documents::IosDocuments)
    }
    #[cfg(not(target_os = "ios"))]
    {
        Arc::new(DesktopDocuments)
    }
}
pub(crate) fn safe_name(name: &str) -> String {
    if flectar_mail_core::files::validate_name(name).is_ok() {
        name.into()
    } else {
        "download".into()
    }
}

struct DesktopDocuments;
impl DocumentProvider for DesktopDocuments {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>> {
        Box::pin(async {
            #[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
            {
                Ok(rfd::AsyncFileDialog::new()
                    .set_title("Upload file")
                    .pick_file()
                    .await
                    .map(|f| ImportedDocument::source(f.path().to_owned())))
            }
            #[cfg(any(target_os = "android", target_os = "ios", target_env = "ohos"))]
            {
                Err("The native document provider is not initialized.".into())
            }
        })
    }
    fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool> {
        Box::pin(async move {
            #[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
            {
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
            }
            #[cfg(any(target_os = "android", target_os = "ios", target_env = "ohos"))]
            {
                let _ = (path, name);
                Err("The native document provider is not initialized.".into())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    #[test]
    fn desktop_source_survives_document_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user-original");
        std::fs::write(&path, "original").unwrap();
        drop(ImportedDocument::source(path.clone()));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "original");
    }

    #[test]
    fn original_name_survives_private_staging_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("random-private-copy.bin");
        std::fs::write(&path, "attachment bytes").unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let name = "旅行预算 2026 📨.pdf";
        let document = imported(path.clone(), drops.clone())
            .with_display_name(name)
            .unwrap();
        assert_eq!(document.display_name(), Some(name));
        assert_eq!(document.path(), path);
        assert_eq!(std::fs::read(document.path()).unwrap(), b"attachment bytes");
        let message = flectar_mail_core::mime::OutgoingMessage {
            from: flectar_mail_core::models::Address {
                name: None,
                email: "sender@example.test".into(),
            },
            to: &[flectar_mail_core::models::Address {
                name: None,
                email: "receiver@example.test".into(),
            }],
            cc: &[],
            bcc: &[],
            subject: "attachment contract",
            body_text: "mail body",
            body_html: None,
            in_reply_to: None,
            references: &[],
            message_id: None,
            message_id_domain: "example.test",
            attachments: vec![flectar_mail_core::mime::OutgoingAttachment {
                filename: document.display_name().unwrap().to_owned(),
                mime_type: "application/pdf".into(),
                bytes: std::fs::read(document.path()).unwrap(),
            }],
        };
        let (_, raw) = flectar_mail_core::mime::build_message(&message).unwrap();
        let parsed = flectar_mail_core::mime::parse_message(&raw).unwrap();
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename.as_deref(), Some(name));
        let (bytes, delivered_name) =
            flectar_mail_core::mime::extract_attachment(&raw, &parsed.attachments[0].part_id)
                .unwrap();
        assert_eq!(bytes, b"attachment bytes");
        assert_eq!(delivered_name.as_deref(), Some(name));
        drop(document);
        assert!(!path.exists());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn invalid_original_names_release_private_staging() {
        let dir = tempfile::tempdir().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let oversized = "中".repeat(86);
        let invalid = [
            "",
            ".",
            "..",
            "../mail.pdf",
            "dir\\mail.pdf",
            "mail\n.pdf",
            &oversized,
        ];
        for (index, name) in invalid.into_iter().enumerate() {
            let path = dir.path().join(index.to_string());
            std::fs::write(&path, "private attachment").unwrap();
            assert!(
                imported(path.clone(), drops.clone())
                    .with_display_name(name)
                    .is_err()
            );
            assert!(!path.exists());
        }
        assert_eq!(drops.load(Ordering::SeqCst), invalid.len());
    }
}
