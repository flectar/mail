//! Document provider for the OHOS shell: system file picker through the ArkTS
//! bridge, mirroring `platform/android/src/documents.rs`.
//!
//! The picker itself is ArkTS-only (`DocumentViewPicker`); the page copies
//! picked files into the app cache and reports the sandbox path back, so this
//! side never touches content-provider URIs. Request cancellation and timeouts
//! are handled inside the bridge (`PendingGuard`).

use std::path::PathBuf;

use flectar_mail::documents::{DocumentFuture, DocumentProvider, ImportedDocument};

/// Routes import/export through `bridge::pick_open` / `bridge::pick_save`.
pub(super) struct OhosDocuments;

impl DocumentProvider for OhosDocuments {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>> {
        Box::pin(async {
            let Some(path) = super::bridge::pick_open().await? else {
                return Ok(None);
            };
            // The page staged a copy in the app cache; deleting it once the
            // core is done mirrors the Android provider.
            let staged = PathBuf::from(path);
            Ok(Some(ImportedDocument::temporary(staged, |path| {
                if let Err(error) = std::fs::remove_file(&path) {
                    slint_ohos_backend::ohos_info!(
                        "mail: could not remove the imported copy: {error}"
                    );
                }
            })))
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
