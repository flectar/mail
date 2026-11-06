//! Native document access boundary. Providers import into private staging and
//! export completed files; content-provider URIs never reach filesystem APIs.
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
};
pub type DocumentFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send>>;
/// Owns a native import copy until Core has staged it. Dropping a cancelled
/// request or an unread channel result must release the copy as well.
pub struct ImportedDocument {
    path: PathBuf,
    /// The name the user picked the document by, when the host stores the
    /// copy under a generated name. The path stays the only filesystem input.
    picked_name: Option<String>,
    cleanup: Option<Box<dyn FnOnce(PathBuf) + Send>>,
}
impl ImportedDocument {
    pub fn source(path: PathBuf) -> Self {
        Self {
            path,
            picked_name: None,
            cleanup: None,
        }
    }
    pub fn temporary(path: PathBuf, cleanup: impl FnOnce(PathBuf) + Send + 'static) -> Self {
        Self {
            path,
            picked_name: None,
            cleanup: Some(Box::new(cleanup)),
        }
    }
    /// Records the name the user picked the document by. Hosts store picked
    /// copies under names they generate instead of trusting the provider's,
    /// and pass the picker's name here. Slashes, backslashes and control
    /// characters, which some cloud providers allow in names, become
    /// underscores; a name that is still not a valid file name is ignored.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        let name = name
            .into()
            .replace(|c: char| c.is_control() || matches!(c, '/' | '\\'), "_");
        if flectar_mail_core::files::validate_name(&name).is_ok() {
            self.picked_name = Some(name);
        }
        self
    }
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    /// The document's name for the user, attachments, uploads and imports:
    /// the picked name, or the stored file's own name.
    pub fn name(&self) -> Option<String> {
        self.picked_name.clone().or_else(|| {
            self.path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
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
/// How a desktop file dialog presents one request. Hosts whose system picker
/// has no title or file-type filter ignore it.
#[derive(Clone, Default)]
pub struct DocumentDialog {
    pub title: String,
    /// Filter name and the extensions it offers.
    pub filter: Option<(String, &'static [&'static str])>,
    /// Directory the picker opens in. `None` follows the platform default.
    pub directory: Option<PathBuf>,
}
impl DocumentDialog {
    pub fn titled(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            filter: None,
            directory: None,
        }
    }
}
pub trait DocumentProvider: Send + Sync {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>>;
    fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool>;
    fn import_with(&self, _dialog: DocumentDialog) -> DocumentFuture<Option<ImportedDocument>> {
        self.import()
    }
    fn export_with(
        &self,
        path: PathBuf,
        name: String,
        _dialog: DocumentDialog,
    ) -> DocumentFuture<bool> {
        self.export(path, name)
    }
    /// Imports the documents for one user action (compose attachments). The
    /// default opens a single-document picker once; hosts with a multi-select
    /// picker override it. `limit` bounds how many copies a host makes;
    /// hosts that return source paths need not apply it.
    fn import_many(
        &self,
        _limit: usize,
        dialog: DocumentDialog,
    ) -> DocumentFuture<Vec<ImportedDocument>> {
        let import = self.import_with(dialog);
        Box::pin(async move { Ok(import.await?.into_iter().collect()) })
    }
}
/// Runs one document request and hands its result to `on_ready` on the UI
/// thread. Providers rely on Tokio timers and file I/O, so the request runs on
/// the runtime while the event loop keeps going, which desktop dialogs need.
/// Every picker and export goes through here, so a host that must keep the
/// presentation thread idle while its picker covers the window changes only
/// this function.
pub(crate) fn run_request<T: Send + 'static>(
    runtime: &tokio::runtime::Handle,
    request: DocumentFuture<T>,
    on_ready: impl FnOnce(Result<T, String>) + 'static,
) {
    let request = runtime.spawn(request);
    let abort = request.abort_handle();
    let on_ready = std::rc::Rc::new(std::cell::Cell::new(Some(on_ready)));
    let pending = std::rc::Rc::clone(&on_ready);
    let spawned = slint::spawn_local(async move {
        let result = request.await.unwrap_or_else(|error| Err(error.to_string()));
        if let Some(on_ready) = pending.take() {
            on_ready(result);
        }
    });
    if let Err(error) = spawned {
        abort.abort();
        if let Some(on_ready) = on_ready.take() {
            on_ready(Err(error.to_string()));
        }
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

/// Builds the dialog for a download, starting in the directory the user
/// configured in Settings.
pub(crate) fn download_dialog(title: impl Into<String>, setting: &str) -> DocumentDialog {
    DocumentDialog {
        title: title.into(),
        filter: None,
        directory: resolve_download_directory(setting),
    }
}

/// Builds the download dialog from the stored settings. Every download goes
/// through here, so a new download site picks up the configured location
/// without repeating the lookup. An unreadable settings row falls back to the
/// platform default rather than blocking the download.
pub(crate) async fn download_dialog_for(core: &flectar_mail_core::Core) -> DocumentDialog {
    let setting = core
        .get_settings()
        .await
        .map(|settings| settings.download_directory)
        .unwrap_or_default();
    download_dialog("Download file", &setting)
}

/// Resolves the configured download directory for desktop save dialogs. An
/// empty setting, a path that no longer exists, or a path that is not a
/// directory all fall back to the platform default, so a download never fails
/// because a saved location went away, such as an unmounted network share.
pub(crate) fn resolve_download_directory(setting: &str) -> Option<PathBuf> {
    let trimmed = setting.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    path.is_dir().then_some(path)
}

/// How Settings shows the download folder. Inside Flatpak the picker returns a
/// document portal path such as `/run/user/1000/doc/1a2b3c/Downloads`, which
/// keeps working for downloads but means nothing to the user, so the folder's
/// host path is shown when the portal records it.
pub(crate) fn download_directory_label(setting: &str) -> String {
    let trimmed = setting.trim();
    document_portal_host_path(Path::new(trimmed))
        .map(|host| host.to_string_lossy().into_owned())
        .unwrap_or_else(|| trimmed.to_owned())
}

/// The host path the document portal records on a file or folder it exported
/// into the sandbox. `None` for ordinary paths and on portals that predate the
/// attribute.
#[cfg(linux_desktop)]
fn document_portal_host_path(path: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut value = vec![0_u8; libc::PATH_MAX as usize];
    // SAFETY: both names are NUL-terminated, and `value` is writable for the
    // length passed.
    let length = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"user.document-portal.host-path".as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    value.truncate(usize::try_from(length).ok()?);
    if value.last() == Some(&0) {
        value.pop();
    }
    (!value.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(value)))
}

#[cfg(not(linux_desktop))]
fn document_portal_host_path(_path: &Path) -> Option<PathBuf> {
    None
}

/// The starting folder handed to a picker. xdg-desktop-portal resolves a
/// document portal path to its host path and then drops the last component,
/// which suits a file but opens a folder's parent; a trailing separator leaves
/// it nothing to drop. Host paths mean the same folder either way.
#[cfg(linux_desktop)]
pub(crate) fn picker_directory(directory: &Path) -> PathBuf {
    let mut path = directory.as_os_str().to_owned();
    if !path.as_encoded_bytes().ends_with(b"/") {
        path.push("/");
    }
    PathBuf::from(path)
}

#[cfg(all(desktop, not(linux_desktop)))]
pub(crate) fn picker_directory(directory: &Path) -> PathBuf {
    directory.to_owned()
}

#[cfg(desktop)]
struct DesktopDocuments;
#[cfg(desktop)]
impl DocumentDialog {
    fn file_dialog(&self) -> rfd::AsyncFileDialog {
        let dialog = rfd::AsyncFileDialog::new().set_title(&self.title);
        let dialog = match &self.directory {
            Some(directory) => dialog.set_directory(picker_directory(directory)),
            None => dialog,
        };
        match &self.filter {
            Some((name, extensions)) => dialog.add_filter(name, extensions),
            None => dialog,
        }
    }
}
#[cfg(desktop)]
impl DocumentProvider for DesktopDocuments {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>> {
        self.import_with(DocumentDialog::titled("Upload file"))
    }
    fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool> {
        self.export_with(path, name, DocumentDialog::titled("Download file"))
    }
    fn import_with(&self, dialog: DocumentDialog) -> DocumentFuture<Option<ImportedDocument>> {
        Box::pin(async move {
            Ok(dialog
                .file_dialog()
                .pick_file()
                .await
                .map(|f| ImportedDocument::source(f.path().to_owned())))
        })
    }
    fn import_many(
        &self,
        _limit: usize,
        dialog: DocumentDialog,
    ) -> DocumentFuture<Vec<ImportedDocument>> {
        Box::pin(async move {
            Ok(dialog
                .file_dialog()
                .pick_files()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|file| ImportedDocument::source(file.path().to_owned()))
                .collect())
        })
    }
    fn export_with(
        &self,
        path: PathBuf,
        name: String,
        dialog: DocumentDialog,
    ) -> DocumentFuture<bool> {
        Box::pin(async move {
            let Some(file) = dialog.file_dialog().set_file_name(name).save_file().await else {
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

    #[test]
    fn download_dialog_starts_in_the_configured_directory() {
        let dir = tempfile::tempdir().unwrap();
        let dialog = download_dialog("Download file", &dir.path().to_string_lossy());
        assert_eq!(dialog.title, "Download file");
        assert_eq!(dialog.directory, Some(dir.path().to_owned()));
    }

    #[test]
    fn download_dialog_without_a_configured_directory_uses_the_platform_default() {
        assert_eq!(download_dialog("Download file", "").directory, None);
    }

    #[test]
    fn configured_directory_is_used_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_download_directory(&dir.path().to_string_lossy()),
            Some(dir.path().to_owned())
        );
    }

    #[test]
    fn empty_setting_uses_the_platform_default() {
        assert_eq!(resolve_download_directory(""), None);
        assert_eq!(resolve_download_directory("   "), None);
    }

    #[test]
    fn missing_directory_uses_the_platform_default() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone");
        assert_eq!(resolve_download_directory(&missing.to_string_lossy()), None);
    }

    #[test]
    fn file_path_instead_of_a_directory_uses_the_platform_default() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lieferschein.pdf");
        std::fs::write(&file, b"%PDF-1.7").unwrap();
        assert_eq!(resolve_download_directory(&file.to_string_lossy()), None);
    }

    #[cfg(linux_desktop)]
    #[test]
    fn picker_directory_ends_in_one_separator() {
        assert_eq!(
            picker_directory(Path::new("/run/user/1000/doc/1a2b3c/Downloads")).as_os_str(),
            "/run/user/1000/doc/1a2b3c/Downloads/"
        );
        assert_eq!(
            picker_directory(Path::new("/home/ana/Downloads/")).as_os_str(),
            "/home/ana/Downloads/"
        );
    }

    #[test]
    fn ordinary_download_directory_is_shown_as_stored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        assert_eq!(download_directory_label(&path), path);
        assert_eq!(download_directory_label(""), "");
    }

    /// Stands in for a folder the document portal exported into the sandbox.
    #[cfg(linux_desktop)]
    fn exported_folder(host_path: &[u8]) -> tempfile::TempDir {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let path = std::ffi::CString::new(dir.path().as_os_str().as_bytes()).unwrap();
        // SAFETY: both names are NUL-terminated, and `host_path` is readable
        // for the length passed.
        let set = unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"user.document-portal.host-path".as_ptr(),
                host_path.as_ptr().cast(),
                host_path.len(),
                0,
            )
        };
        assert_eq!(set, 0, "{}", std::io::Error::last_os_error());
        dir
    }

    #[cfg(linux_desktop)]
    #[test]
    fn exported_download_directory_is_shown_by_its_host_path() {
        for host_path in [&b"/home/ana/Downloads"[..], b"/home/ana/Downloads\0"] {
            let dir = exported_folder(host_path);
            assert_eq!(
                download_directory_label(&dir.path().to_string_lossy()),
                "/home/ana/Downloads"
            );
        }
    }

    fn imported(path: PathBuf, drops: Arc<AtomicUsize>) -> ImportedDocument {
        ImportedDocument::temporary(path, move |path| {
            std::fs::remove_file(path).unwrap();
            drops.fetch_add(1, Ordering::SeqCst);
        })
    }

    #[test]
    fn picked_names_replace_the_stored_name() {
        let path = PathBuf::from("/private/cache/import-random.pdf");
        let document = ImportedDocument::source(path.clone()).with_name("Report 2026.pdf");
        assert_eq!(document.path(), path);
        assert_eq!(document.name().as_deref(), Some("Report 2026.pdf"));
        assert_eq!(
            ImportedDocument::source(path.clone()).name().as_deref(),
            Some("import-random.pdf")
        );
        for (picked, kept) in [
            ("Invoice 10/2026.pdf", "Invoice 10_2026.pdf"),
            ("a\\b", "a_b"),
            ("line\nbreak", "line_break"),
        ] {
            let document = ImportedDocument::source(path.clone()).with_name(picked);
            assert_eq!(document.name().as_deref(), Some(kept), "{picked:?}");
        }
        let long = "x".repeat(256);
        for invalid in ["", ".", "..", long.as_str()] {
            let document = ImportedDocument::source(path.clone()).with_name(invalid);
            assert_eq!(
                document.name().as_deref(),
                Some("import-random.pdf"),
                "{invalid:?}"
            );
        }
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
        let documents = provider.import_many(20, DocumentDialog::default()).await.unwrap();
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
        let documents = provider.import_many(20, DocumentDialog::default()).await.unwrap();
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
