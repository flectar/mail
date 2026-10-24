//! Certificate import for manual mail accounts. Certificates are public trust
//! material; private keys must never enter account settings or backups.
use super::*;

pub(super) fn register(
    app: &AppWindow,
    runtime: &Rc<tokio::runtime::Runtime>,
    documents: &std::sync::Arc<dyn documents::DocumentProvider>,
) {
    let weak = app.as_weak();
    let runtime = Rc::clone(runtime);
    let provider = std::sync::Arc::clone(documents);
    app.on_import_mail_certificate(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_account_setup_in_progress() {
            return;
        }
        let import = provider.import_with(documents::DocumentDialog {
            title: "Import server certificate".into(),
            filter: Some(("PEM certificate".into(), &["pem", "crt", "cer"])),
        });
        let request: documents::DocumentFuture<Option<String>> = Box::pin(async move {
            // Read before the document is dropped: a host's staged copy lives
            // only as long as the ImportedDocument.
            let Some(document) = import.await? else {
                return Ok(None);
            };
            read_certificate(document.path()).await.map(Some)
        });
        let weak = app.as_weak();
        documents::run_request(&runtime, request, move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if app.get_account_setup_in_progress() {
                return;
            }
            match result {
                Ok(Some(pem)) => {
                    app.set_trusted_certificate_pem(pem.into());
                    app.set_sync_status(UiMessage::plain(
                        "Certificate imported. Connect to verify the server.",
                    ));
                }
                Ok(None) => {}
                Err(error) => {
                    app.set_sync_status(UiMessage::detail("Certificate import failed: {}", error))
                }
            }
        });
    });
}

async fn read_certificate(path: &std::path::Path) -> Result<String, String> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    let pem =
        String::from_utf8(bytes).map_err(|_| "Choose a PEM-encoded certificate".to_string())?;
    if pem.trim().is_empty() {
        return Err("The certificate file is empty".into());
    }
    flectar_mail_core::net::tls::trusted_certificates(&pem).map_err(|e| e.to_string())?;
    Ok(pem)
}
