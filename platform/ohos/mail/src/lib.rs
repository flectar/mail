//! Flectar Mail shell for OpenHarmony.
//!
//! Mirrors `platform/android/src/lib.rs`: a thin cdylib that builds the
//! `PlatformContext` (app-private paths, the Asset Store credential store, the
//! host document provider and the OAuth broker) and hands it to
//! `flectar_mail::run`, while the UI runs on the `slint-ohos-backend` platform.
//!
//! The ArkTS page owns the services native code cannot reach: opening the
//! system browser with a Want, the document pickers and the soft keyboard. See
//! `platform/ohos/README.md` for build, packaging and device notes.

#![allow(non_camel_case_types)]

use core::ffi::c_void;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use flectar_mail::flectar_mail_core::accounts::credentials::{CredentialStore, Slot};
use flectar_mail::flectar_mail_core::error::{CoreError, Result as CoreResult};
use slint_ohos_backend::ffi::{self, napi_env, napi_value};

mod bridge;
mod documents;

/// NAPI module name; must match the XComponent `libraryname` in the ArkTS page
/// (the loader resolves it to `libflectar_mail_ohos.so`).
const MODULE_NAME: &[u8] = b"flectar_mail_ohos\0";

/// App-private roots as seen from inside the app process (sandbox paths).
const APP_DATA_ROOT: &str = "/data/storage/el2/base/files";
const APP_CACHE_ROOT: &str = "/data/storage/el2/base/cache";

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Routes `tracing` (used throughout the app and its core) to hilog — mirroring
/// the Android shell's observability setup. Application stderr is not visible
/// on device, so this is the only way to see the app's own logs.
mod observability {
    use std::io::{self, Write};
    use std::sync::Once;

    #[derive(Default)]
    struct HilogWriter(Vec<u8>);

    impl Write for HilogWriter {
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
            let message = String::from_utf8_lossy(&self.0).replace('\0', "\u{FFFD}");
            write_log(message.trim_end());
            self.0.clear();
            Ok(())
        }
    }

    impl Drop for HilogWriter {
        fn drop(&mut self) {
            let _ = self.flush();
        }
    }

    fn write_log(message: &str) {
        let mut buffer = String::with_capacity(message.len() + 1);
        buffer.push_str(message);
        buffer.push('\0');
        // SAFETY: static NUL-terminated format string + matching `%{public}s`
        // argument (LOG_APP, LOG_INFO, domain 0xD00D).
        unsafe {
            slint_ohos_backend::ffi::OH_LOG_Print(
                0,
                4,
                0xD00D,
                c"flectar-mail".as_ptr(),
                c"%{public}s".as_ptr(),
                buffer.as_ptr().cast::<core::ffi::c_char>(),
            );
        }
    }

    pub(super) fn install() {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .with_writer(HilogWriter::default)
                .try_init();
            // Panics in the cdylib would otherwise kill the process silently
            // (application stderr is not visible on device): route them to
            // hilog so a crash leaves a trace we can read afterwards.
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                write_log(&format!("PANIC: {info}"));
                previous(info);
            }));
        });
    }
}

unsafe extern "C" fn module_init(env: napi_env, exports: napi_value) -> napi_value {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return exports;
    }
    observability::install();
    slint_ohos_backend::ohos_info!("mail: NAPI module init (XComponent load)");

    // SAFETY: `env`/`exports` are the NAPI module-init arguments, valid for the
    // duration of this call.
    unsafe { register_module_functions(env, exports) };

    if let Err(err) = slint_ohos_backend::start(run_mail) {
        slint_ohos_backend::ohos_error!("mail: failed to start Slint thread: {err}");
        return exports;
    }

    // SAFETY: `env`/`exports` are the NAPI module-init arguments; the property
    // lookup + `napi_unwrap` follow the OHOS XComponent module contract.
    unsafe {
        let mut component_obj: napi_value = core::ptr::null_mut();
        let rc = ffi::napi_get_named_property(
            env,
            exports,
            ffi::OH_NATIVE_XCOMPONENT_OBJ.as_ptr().cast(),
            &mut component_obj,
        );
        if rc != ffi::NAPI_OK {
            slint_ohos_backend::ohos_error!(
                "mail: XComponent object missing on module init ({rc})"
            );
            return exports;
        }
        let mut component: *mut c_void = core::ptr::null_mut();
        let rc = ffi::napi_unwrap(env, component_obj, &mut component);
        if rc != ffi::NAPI_OK || component.is_null() {
            slint_ohos_backend::ohos_error!("mail: XComponent pointer unavailable ({rc})");
            return exports;
        }
        // SAFETY: `component` came from unwrapping the XComponent instance
        // handed to this module at load time; the loader keeps it valid for
        // the process lifetime.
        let rc = slint_ohos_backend::register_xcomponent(component);
        if rc != 0 {
            slint_ohos_backend::ohos_error!("mail: RegisterCallback failed: {rc}");
        }
    }
    exports
}

/// Expose the bridge entry points on the module object. The ArkTS page calls
/// `setBridge({ ... })` from the XComponent `onLoad` callback with the closures
/// native code needs, and `submitBridgeResult(...)` when a callback-based flow
/// (the file picker) finishes.
///
/// SAFETY: `env` and `exports` must be the live NAPI module-init arguments.
unsafe fn register_module_functions(env: napi_env, exports: napi_value) {
    // SAFETY: NAPI calls on the module-init thread with module-owned handles.
    unsafe {
        for (name, callback) in [
            (
                &b"setBridge\0"[..],
                bridge::set_bridge as ffi::napi_callback,
            ),
            (
                &b"submitBridgeResult\0"[..],
                bridge::submit_bridge_result as ffi::napi_callback,
            ),
            (
                &b"imeCommit\0"[..],
                bridge::ime_commit as ffi::napi_callback,
            ),
            (
                &b"imeDelete\0"[..],
                bridge::ime_delete as ffi::napi_callback,
            ),
            (
                &b"imeFunction\0"[..],
                bridge::ime_function as ffi::napi_callback,
            ),
        ] {
            let mut function: napi_value = core::ptr::null_mut();
            let rc = ffi::napi_create_function(
                env,
                name.as_ptr().cast(),
                name.len() - 1,
                callback,
                core::ptr::null_mut(),
                &mut function,
            );
            if rc != ffi::NAPI_OK {
                slint_ohos_backend::ohos_error!(
                    "mail: napi_create_function({}) failed: {rc}",
                    String::from_utf8_lossy(&name[..name.len() - 1])
                );
                continue;
            }
            let rc = ffi::napi_set_named_property(env, exports, name.as_ptr().cast(), function);
            if rc != ffi::NAPI_OK {
                slint_ohos_backend::ohos_error!(
                    "mail: napi_set_named_property({}) failed: {rc}",
                    String::from_utf8_lossy(&name[..name.len() - 1])
                );
            }
        }
    }
}

/// Point the shared application at the native libraries installed beside this
/// cdylib (the hap's `libs/<abi>`), so PDF preview can load the bundled
/// `libpdfium.so` instead of reporting "not configured".
fn configure_native_libraries() -> std::result::Result<(), String> {
    // Resolve our own module path instead of trusting the page or a document.
    let mut info: libc::Dl_info = unsafe { core::mem::zeroed() };
    let symbol = register_module as *const libc::c_void;
    // SAFETY: `info` is a valid out-parameter and the symbol lives in this
    // module for the process lifetime.
    if unsafe { libc::dladdr(symbol, &mut info) } == 0 || info.dli_fname.is_null() {
        return Err("could not resolve the native library directory".into());
    }
    // SAFETY: dladdr filled dli_fname with a NUL-terminated path that stays
    // valid while the module is loaded.
    let raw = unsafe { core::ffi::CStr::from_ptr(info.dli_fname) };
    let path = raw
        .to_str()
        .map_err(|_| "the native library path is not UTF-8")?;
    let directory = std::path::Path::new(path)
        .parent()
        .ok_or("the native library path has no directory")?;
    slint_ohos_backend::ohos_info!("mail: native libraries in {}", directory.display());
    flectar_mail::platform::configure_pdfium(directory.to_path_buf())
}

/// Runs on the Slint thread spawned by the backend.
fn run_mail() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Application stderr is not reliably visible on device; route panics to
    // hilog so first-boot crashes are diagnosable.
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".into());
        slint_ohos_backend::ohos_error!("mail: panic at {location}: {info}");
    }));

    configure_native_libraries()?;

    let internal = PathBuf::from(APP_DATA_ROOT);
    let cache = PathBuf::from(APP_CACHE_ROOT);
    fs::create_dir_all(&internal)?;
    fs::create_dir_all(&cache)?;

    // Credentials live in the system Asset Store. Debug builds may opt into a
    // plaintext file store on devices where the asset service is unavailable;
    // release builds never take that path, even with the variable set.
    let allow_file_store =
        cfg!(debug_assertions) && std::env::var_os("FLECTAR_ALLOW_FILE_CREDENTIALS").is_some();
    let credentials: Arc<dyn CredentialStore> = if allow_file_store {
        slint_ohos_backend::ohos_info!(
            "mail: FLECTAR_ALLOW_FILE_CREDENTIALS set; using the file credential store (debug only)"
        );
        Arc::new(OhosFileCredentialStore::new(
            internal.join("credentials-v1.json"),
        ))
    } else {
        slint_ohos_backend::ohos_info!("mail: using the OHOS Asset Store credential backend");
        Arc::new(OhosAssetCredentialStore)
    };
    let oauth = Arc::new(
        flectar_mail::flectar_mail_core::oauth::redirect::LoopbackRedirectBroker::default(),
    );

    // Import/export go through the system document picker; authorization and
    // external addresses go through the ArkTS bridge (OHOS reaches the system
    // browser with a Want, which the page exposes as `openUrl`).
    let documents: Arc<dyn flectar_mail::documents::DocumentProvider> =
        Arc::new(documents::OhosDocuments);
    // Soft-keyboard control for Slint's editable fields.
    slint_ohos_backend::ime::set_hooks(bridge::show_ime, bridge::hide_ime);
    // A denied paste asks for the pasteboard ACL permission once (see clipboard.rs).
    slint_ohos_backend::clipboard::set_permission_hook(bridge::request_pasteboard);
    let mut platform =
        flectar_mail::PlatformContext::app_private(internal, cache, documents, credentials, oauth);
    platform.external_urls = flectar_mail::ExternalUrlOpener::new(bridge::open_url);
    // System notifications for new mail (nothing is published when the user
    // turns notifications off in Settings).
    platform.notification_publisher = flectar_mail::NotificationPublisher::new(bridge::notify);
    slint_ohos_backend::ohos_info!("mail: entering flectar_mail::run");
    flectar_mail::run(platform).map_err(|err| err.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Debug-only fallback credential store
// ---------------------------------------------------------------------------

/// App-private JSON credential store for debug builds.
///
/// Release builds always use the system Asset Store (see `run_mail`). This
/// fallback keeps development on devices without a working asset service
/// usable; it mirrors the core's `DevelopmentFileCredentialStore` and never
/// logs secret material.
struct OhosFileCredentialStore {
    path: PathBuf,
}

impl OhosFileCredentialStore {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn file_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn key(account_id: i64, slot: Slot) -> String {
        format!("{}:{}", account_id, slot.as_str())
    }

    fn load_map(&self) -> CoreResult<BTreeMap<String, String>> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                CoreError::CredentialStoreUnavailable(format!("credential file corrupt: {error}"))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(CoreError::CredentialStoreUnavailable(error.to_string())),
        }
    }

    fn save_map(&self, map: &BTreeMap<String, String>) -> CoreResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| CoreError::CredentialStoreUnavailable(error.to_string()))?;
        }
        let encoded = serde_json::to_vec(map)
            .map_err(|error| CoreError::CredentialStoreUnavailable(error.to_string()))?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, &encoded)
            .map_err(|error| CoreError::CredentialStoreUnavailable(error.to_string()))?;
        // The trait requires durability across process death: sync before the
        // rename so a crash cannot leave a renamed-but-empty file.
        if let Ok(file) = fs::File::open(&temporary) {
            let _ = file.sync_all();
        }
        fs::rename(&temporary, &self.path)
            .map_err(|error| CoreError::CredentialStoreUnavailable(error.to_string()))?;
        Ok(())
    }
}

impl CredentialStore for OhosFileCredentialStore {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> CoreResult<()> {
        let _guard = Self::file_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut map = self.load_map()?;
        map.insert(Self::key(account_id, slot), secret.to_string());
        self.save_map(&map)
    }

    fn load(&self, account_id: i64, slot: Slot) -> CoreResult<String> {
        let _guard = Self::file_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.load_map()?
            .remove(&Self::key(account_id, slot))
            .ok_or_else(|| CoreError::Auth("no stored credential".into()))
    }

    fn delete(&self, account_id: i64, slot: Slot) -> CoreResult<()> {
        let _guard = Self::file_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut map = self.load_map()?;
        map.remove(&Self::key(account_id, slot));
        self.save_map(&map)
    }

    fn delete_all(&self, account_id: i64) -> CoreResult<()> {
        let _guard = Self::file_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut map = self.load_map()?;
        let prefix = format!("{account_id}:");
        map.retain(|key, _| !key.starts_with(&prefix));
        self.save_map(&map)
    }
}

// ---------------------------------------------------------------------------
// Credential store (OHOS Asset Store)
// ---------------------------------------------------------------------------

/// Credentials in the system Asset Store, aliased as `flectar:{account}:{slot}`
/// (see `slint_ohos_backend::asset`). No per-app permission is required.
struct OhosAssetCredentialStore;

const ALL_SLOTS: [Slot; 9] = [
    Slot::Password,
    Slot::RefreshToken,
    Slot::AccessToken,
    Slot::OAuthClientId,
    Slot::OAuthClientSecret,
    Slot::OAuthBundle,
    Slot::AiApiKey,
    Slot::CaldavPassword,
    Slot::CarddavPassword,
];

impl OhosAssetCredentialStore {
    fn alias(account_id: i64, slot: Slot) -> String {
        format!("flectar:{account_id}:{}", slot.as_str())
    }
}

impl CredentialStore for OhosAssetCredentialStore {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> CoreResult<()> {
        slint_ohos_backend::asset::put(&Self::alias(account_id, slot), secret)
            .map_err(CoreError::CredentialStoreUnavailable)
    }

    fn load(&self, account_id: i64, slot: Slot) -> CoreResult<String> {
        match slint_ohos_backend::asset::get(&Self::alias(account_id, slot))
            .map_err(CoreError::CredentialStoreUnavailable)?
        {
            Some(secret) => Ok(secret),
            None => Err(CoreError::Auth("no stored credential".into())),
        }
    }

    fn delete(&self, account_id: i64, slot: Slot) -> CoreResult<()> {
        slint_ohos_backend::asset::remove(&Self::alias(account_id, slot))
            .map_err(CoreError::CredentialStoreUnavailable)
    }

    fn delete_all(&self, account_id: i64) -> CoreResult<()> {
        for slot in ALL_SLOTS {
            self.delete(account_id, slot)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NAPI module registration
// ---------------------------------------------------------------------------

// SAFETY: the NAPI runtime reads this once from the module registry; the
// `static mut` is only handed to `napi_module_register` by address and never
// mutated afterwards.
static mut MODULE: ffi::NapiModule = ffi::NapiModule {
    nm_version: 1,
    nm_flags: 0,
    nm_filename: core::ptr::null(),
    nm_register_func: Some(module_init),
    nm_modname: MODULE_NAME.as_ptr().cast(),
    nm_priv: core::ptr::null_mut(),
    reserved: [core::ptr::null_mut(); 4],
};

extern "C" fn register_module() {
    // SAFETY: `addr_of_mut!` avoids creating a reference to the mutable static.
    unsafe { ffi::napi_module_register(core::ptr::addr_of_mut!(MODULE)) };
}

#[used]
#[unsafe(link_section = ".init_array")]
static REGISTER_MODULE: extern "C" fn() = register_module;
