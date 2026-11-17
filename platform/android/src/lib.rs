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
fn app_cache_dir(app: &slint::android::AndroidApp) -> Result<std::path::PathBuf, String> {
    use jni::{
        JavaVM,
        objects::{JObject, JString},
    };

    // NativeActivity supplies an app-private data path. Use Android's
    // designated, purgeable cache directory for reconstructable data.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let directory = env
        .call_method(&activity, "getCacheDir", "()Ljava/io/File;", &[])
        .and_then(|value| value.l())
        .map_err(|e| e.to_string())?;
    let path = env
        .call_method(&directory, "getAbsolutePath", "()Ljava/lang/String;", &[])
        .and_then(|value| value.l())
        .map_err(|e| e.to_string())?;
    let path: String = env
        .get_string(&JString::from(path))
        .map_err(|e| e.to_string())?
        .into();
    Ok(path.into())
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    observability::install();
    // SAFETY: AndroidApp owns these live VM and Activity references.
    unsafe { tls::initialize(app.vm_as_ptr().cast(), app.activity_as_ptr().cast()) }
        .expect("failed to initialize Android TLS certificate verification");
    let internal = app
        .internal_data_path()
        .expect("Android did not provide an app-private data directory");
    let cache_dir =
        app_cache_dir(&app).expect("Android did not provide an app-private cache directory");
    tracing::info!(internal_data_path = %internal.display(), "resolved Android app-private root");
    let oauth_redirects = oauth::AndroidOAuthBroker::global(&app, &internal)
        .expect("failed to initialize Android OAuth services");
    if oauth::route_callback_activity(&app, &oauth_redirects) {
        return;
    }
    // A store opened for a background check closes before the window opens
    // its own: two would sync the same accounts.
    flectar_mail::window_starting();
    let notifications =
        notifications::window_host(&app).expect("failed to initialize Android notifications");
    let credential_store = credentials::AndroidCredentialStore::new(&app, &internal)
        .expect("failed to initialize Android Keystore credential storage");
    pdf::configure(&app).expect("failed to configure bundled PDF preview");
    if let Err(error) = launcher_icon::install(&app) {
        tracing::warn!(%error, "launcher icon choice unavailable");
    }
    if let Err(error) = motion::install(&app) {
        tracing::warn!(%error, "the system's reduce motion setting is unavailable");
    }
    let documents =
        documents::AndroidDocuments::new(&app).expect("failed to initialize document access");
    let mut platform = flectar_mail::PlatformContext::app_private(
        internal.clone(),
        cache_dir,
        documents,
        std::sync::Arc::new(credential_store),
        oauth_redirects,
    );
    let host: std::sync::Arc<dyn flectar_mail::NotificationHost> = notifications.clone();
    platform.notifications = Some(host);
    match spelling::AndroidSpelling::new(&app) {
        Ok(spelling) => platform.spelling = Some(std::sync::Arc::new(spelling)),
        Err(error) => tracing::warn!(%error, "spell checking unavailable"),
    }
    slint::android::init(app).expect("failed to initialize Slint's Android backend");
    flectar_mail::run(platform).expect("Flectar Mail terminated with an error");
    // Work that outlives the activity, such as a check for new mail, asks
    // for tokens through the Application from now on, and can't ask for
    // permission.
    oauth::AndroidOAuthBroker::activity_ended();
    notifications.set_activity(None);
    flectar_mail::window_ended();
}
#[cfg(target_os = "android")]
mod credentials;
#[cfg(target_os = "android")]
mod java;
#[cfg(target_os = "android")]
mod notifications;
#[cfg(target_os = "android")]
mod oauth;
#[cfg(target_os = "android")]
mod tls;

#[cfg(target_os = "android")]
mod documents;

#[cfg(target_os = "android")]
mod launcher_icon;
#[cfg(target_os = "android")]
mod memory;
#[cfg(target_os = "android")]
mod motion;
#[cfg(target_os = "android")]
mod pdf;
#[cfg(target_os = "android")]
mod spelling;
