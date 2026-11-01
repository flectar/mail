//! Document provider for the OHOS shell: system file picker through the ArkTS
//! bridge, mirroring `platform/android/src/documents.rs`.
//!
//! The picker itself is ArkTS-only (`DocumentViewPicker`); the page copies
//! picked files into the app cache and reports the sandbox path back, so this
//! side never touches content-provider URIs. Request cancellation and timeouts
//! are handled inside the bridge (`PendingGuard`).

use std::path::PathBuf;

use flectar_mail::documents::{DocumentDialog, DocumentFuture, DocumentProvider, ImportedDocument};

/// Routes import/export through `bridge::pick_open` / `bridge::pick_save`.
pub(super) struct OhosDocuments;

impl OhosDocuments {
    fn imported(file: super::bridge::PickedFile) -> ImportedDocument {
        // The page staged a copy in the app cache; deleting it once the core
        // is done mirrors the Android provider. The display name is metadata
        // only; the staged path remains the filesystem input.
        let staged = PathBuf::from(file.path);
        ImportedDocument::temporary(staged, |path| {
            if let Err(error) = std::fs::remove_file(&path) {
                slint_ohos_backend::ohos_info!("mail: could not remove the imported copy: {error}");
            }
        })
        .with_name(file.name)
    }
}

impl DocumentProvider for OhosDocuments {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>> {
        Box::pin(async {
            let Some(file) = super::bridge::pick_open().await? else {
                return Ok(None);
            };
            Ok(Some(Self::imported(file)))
        })
    }

    fn import_many(
        &self,
        limit: usize,
        _dialog: DocumentDialog,
    ) -> DocumentFuture<Vec<ImportedDocument>> {
        Box::pin(async move {
            let files = super::bridge::pick_open_many(limit).await?;
            Ok(files.into_iter().map(Self::imported).collect())
        })
    }

    fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool> {
        Box::pin(async move {
            let source = path.to_string_lossy().into_owned();
            match super::bridge::pick_save(source, name).await? {
                // A cancelled picker is a normal outcome, not an error.
                Some(_destination) => Ok(true),
                None => Ok(false),
            }
        })
    }
}
