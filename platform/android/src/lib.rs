#[cfg(target_os = "android")]
mod observability {
    use std::ffi::CString;
    use std::io::{self, Write};
    use std::sync::Once;

    const ANDROID_LOG_INFO: std::ffi::c_int = 4;
    const ANDROID_LOG_ERROR: std::ffi::c_int = 6;

    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(
            priority: std::ffi::c_int,
            tag: *const std::ffi::c_char,
            text: *const std::ffi::c_char,
        ) -> std::ffi::c_int;
    }

    #[derive(Default)]
    struct AndroidLogWriter(Vec<u8>);

    impl Write for AndroidLogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(bytes);
            if self.0.contains(&b'\n') {
                self.flush()?;
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.0.is_empty() {
                return Ok(());
            }
            let message = String::from_utf8_lossy(&self.0).replace('\0', "�");
            self.0.clear();
            write_log(ANDROID_LOG_INFO, message.trim_end());
            Ok(())
        }
    }

    impl Drop for AndroidLogWriter {
        fn drop(&mut self) {
            let _ = self.flush();
        }
    }

    fn write_log(priority: std::ffi::c_int, message: &str) {
        static TAG: &[u8] = b"flectar-mail\0";
        let Ok(message) = CString::new(message) else {
            return;
        };
        // SAFETY: both pointers refer to NUL-terminated strings for the
        // duration of this synchronous Android logging call.
        unsafe {
            __android_log_write(priority, TAG.as_ptr().cast(), message.as_ptr());
        }
    }

    pub(super) fn install() {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .with_writer(AndroidLogWriter::default)
                .try_init();

            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                write_log(ANDROID_LOG_ERROR, &format!("uncaught Rust panic: {info}"));
                previous(info);
            }));
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                abi = std::env::consts::ARCH,
                "Android host initialized"
            );
        });
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    observability::install();
    let internal = app
        .internal_data_path()
        .expect("Android did not provide an app-private data directory");
    tracing::info!(internal_data_path = %internal.display(), "resolved Android app-private root");
    let oauth_redirects = oauth::AndroidOAuthBroker::global(&app, &internal)
        .expect("failed to initialize Android OAuth services");
    if oauth::route_callback_activity(&app, &oauth_redirects) {
        return;
    }
    let credential_store = credentials::AndroidCredentialStore::new(&app, &internal)
        .expect("failed to initialize Android Keystore credential storage");
    pdf::configure(&app).expect("failed to configure bundled PDF preview");
    let documents =
        documents::AndroidDocuments::new(&app).expect("failed to initialize document access");
    let mut platform = flectar_mail::PlatformContext::app_private(
        internal.clone(),
        internal,
        std::sync::Arc::new(credential_store),
        oauth_redirects,
    );
    platform.documents = documents;
    slint::android::init(app).expect("failed to initialize Slint's Android backend");
    flectar_mail::run(platform).expect("Flectar Mail terminated with an error");
}
#[cfg(target_os = "android")]
mod credentials;
#[cfg(target_os = "android")]
mod oauth;

#[cfg(target_os = "android")]
mod documents;

#[cfg(target_os = "android")]
mod pdf;
