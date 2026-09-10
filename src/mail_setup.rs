//! Certificate import for manual mail accounts. Certificates are public trust
//! material; private keys must never enter account settings or backups.
use super::*;

pub(super) fn register(app: &AppWindow, _runtime: &Rc<tokio::runtime::Runtime>) {
    let weak = app.as_weak();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let runtime = Rc::clone(_runtime);
    app.on_import_mail_certificate(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_account_setup_in_progress() {
            return;
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            let weak = app.as_weak();
            runtime.spawn(async move {
                let Some(file) = rfd::AsyncFileDialog::new()
                    .set_title("Import server certificate")
                    .add_filter("PEM certificate", &["pem", "crt", "cer"])
                    .pick_file()
                    .await
                else {
                    return;
                };
                let result = read_certificate(file.path()).await;
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if app.get_account_setup_in_progress() {
                        return;
                    }
                    match result {
                        Ok(pem) => {
                            app.set_trusted_certificate_pem(pem.into());
                            app.set_sync_status(UiMessage::plain(
                                "Certificate imported. Connect to verify the server.",
                            ));
                        }
                        Err(error) => app.set_sync_status(UiMessage::detail(
                            "Certificate import failed: {}",
                            error,
                        )),
                    }
                });
            });
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        app.set_sync_status(UiMessage::plain(
            "Paste the PEM certificate into the certificate field.",
        ));
    });
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
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
    flectar_mail_core::imap::trusted_certificates(&pem).map_err(|e| e.to_string())?;
    Ok(pem)
}
