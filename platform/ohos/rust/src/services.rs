use flectar_mail::{
    PlatformContext,
    documents::{DocumentFuture, DocumentProvider, ImportedDocument},
    flectar_mail_core::{self as core, Core, error::CoreError, events::CoreEvent},
    platform_services::PlatformServices,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;
#[path = "asset.rs"]
mod asset;
#[path = "notifications.rs"]
mod notifications;

struct Pending {
    kind: &'static str,
    reply: oneshot::Sender<Result<Value, String>>,
}
static REQUESTS: OnceLock<Mutex<HashMap<u64, Pending>>> = OnceLock::new();
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
static IMPORT_ROOT: OnceLock<PathBuf> = OnceLock::new();
static APPLICATION_FOREGROUND: AtomicBool = AtomicBool::new(false);
static CANCEL_SYNC: AtomicBool = AtomicBool::new(false);
static SYNC_RUNNING: AtomicBool = AtomicBool::new(false);
static NOTIFICATIONS_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();
fn wake_notifications() {
    NOTIFICATIONS_WAKE.notify_waiters();
    // Retain a permit if lifecycle changes before a receiver registers.
    NOTIFICATIONS_WAKE.notify_one();
}
static ATTACHED: OnceLock<Mutex<Option<(Arc<Core>, tokio::runtime::Handle)>>> = OnceLock::new();
static OPENPGP: OnceLock<Mutex<bool>> = OnceLock::new();
// Keep workers alive after WorkScheduler's budget expires, so an already
// accepted SMTP transaction can finish while new work waits at the active gate.
static DEFERRED_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
struct DeferredCore {
    files_dir: PathBuf,
    cache_dir: PathBuf,
    core: Arc<Core>,
}
static DEFERRED_CORE: OnceLock<Mutex<Option<DeferredCore>>> = OnceLock::new();
static EXPORTS_CLEANED: AtomicBool = AtomicBool::new(false);
static SYSTEM_LOCALE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
#[cfg(debug_assertions)]
static UI_FIXTURE: AtomicBool = AtomicBool::new(false);
#[cfg(debug_assertions)]
static FIXTURE_PROFILE: OnceLock<bool> = OnceLock::new();

fn ui_fixture_enabled() -> bool {
    #[cfg(debug_assertions)]
    {
        UI_FIXTURE.load(Ordering::Acquire)
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

#[cfg(debug_assertions)]
pub(crate) fn configure_ui_fixture(enabled: bool) {
    UI_FIXTURE.store(enabled, Ordering::Release);
    if enabled {
        CANCEL_SYNC.store(true, Ordering::Release);
    }
}

fn deferred_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    if DEFERRED_RUNTIME.get().is_none() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(2)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let _ = DEFERRED_RUNTIME.set(runtime);
    }
    DEFERRED_RUNTIME
        .get()
        .ok_or_else(|| "Deferred runtime unavailable".into())
}

pub(crate) fn configure_system_locale(locale: String) {
    if !locale.is_empty() && locale.len() <= 128 {
        *SYSTEM_LOCALE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(locale);
    }
}

fn clean_stale_exports(files_dir: &Path) {
    if EXPORTS_CLEANED.swap(true, Ordering::AcqRel) {
        return;
    }
    let root = files_dir.join("exports");
    if root.is_symlink() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() || uuid::Uuid::parse_str(&entry.file_name().to_string_lossy()).is_err()
        {
            continue;
        }
        let expired = entry.metadata().ok().is_some_and(|m| {
            m.is_dir()
                && m.modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(24 * 60 * 60))
        });
        if expired {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[cfg(target_env = "ohos")]
unsafe extern "C" {
    fn feihong_host_request(id: u64, kind: *const c_char, payload: *const c_char);
}
#[cfg(not(target_env = "ohos"))]
unsafe fn feihong_host_request(_: u64, _: *const c_char, _: *const c_char) {}

fn requests() -> &'static Mutex<HashMap<u64, Pending>> {
    REQUESTS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn remove_import(value: &Value) {
    if let (Some(root), Some(path)) = (IMPORT_ROOT.get(), value.get("path").and_then(Value::as_str))
    {
        let path = Path::new(path);
        if path.parent() == Some(root.as_path()) && !path.is_symlink() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn feihong_complete_service_request(
    id: u64,
    ok: bool,
    payload: *const c_char,
) {
    if payload.is_null() {
        return;
    }
    let value = serde_json::from_slice::<Value>(unsafe { CStr::from_ptr(payload) }.to_bytes());
    let pending = requests()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    let Some(pending) = pending else {
        if let Ok(value) = value {
            remove_import(&value);
        }
        return;
    };
    let result = value
        .map_err(|_| "Native service returned invalid JSON".to_string())
        .and_then(|value| {
            if ok {
                Ok(value)
            } else {
                Err(value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("System operation failed")
                    .into())
            }
        });
    if let Err(Ok(value)) = pending.reply.send(result) {
        if pending.kind == "pick_import" {
            remove_import(&value);
        }
    }
}

struct CancelRequest(u64);
impl Drop for CancelRequest {
    fn drop(&mut self) {
        requests()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}
async fn request(kind: &'static str, payload: Value) -> Result<Value, String> {
    let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
    let (reply, receiver) = oneshot::channel();
    let kind_text = CString::new(kind).map_err(|e| e.to_string())?;
    let payload = CString::new(payload.to_string()).map_err(|e| e.to_string())?;
    requests()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, Pending { kind, reply });
    let _guard = CancelRequest(id);
    unsafe { feihong_host_request(id, kind_text.as_ptr(), payload.as_ptr()) };
    tokio::time::timeout(Duration::from_secs(15 * 60), receiver)
        .await
        .map_err(|_| "System operation timed out".to_string())?
        .map_err(|_| "System operation was cancelled".to_string())?
}
fn request_blocking(kind: &'static str, payload: Value) -> Result<Value, String> {
    let _ime = crate::backend::suspend_input_method();
    // ArkTS's UI thread is separate from Slint's owner; no nested Slint loop.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(request(kind, payload))
}

pub(crate) fn clipboard_read() -> Result<String, String> {
    request_blocking("clipboard_read", json!({})).map(|v| {
        v.get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    })
}

#[derive(Clone)]
struct OhosServices {
    files_dir: PathBuf,
    cache_dir: PathBuf,
    foreground: Arc<AtomicBool>,
}
impl OhosServices {
    fn imported(&self, value: Value) -> Result<Option<ImportedDocument>, String> {
        let Some(path) = value.get("path").and_then(Value::as_str) else {
            return Ok(None);
        };
        let root = self.cache_dir.join("feihong-imports");
        let path = PathBuf::from(path);
        if path.parent() != Some(root.as_path()) || path.is_symlink() || !path.is_file() {
            return Err("Imported file is outside private staging".into());
        }
        let document = ImportedDocument::temporary(path, |path| {
            let _ = std::fs::remove_file(path);
        });
        let document = match value.get("name").and_then(Value::as_str) {
            Some(name) => document.with_display_name(name)?,
            None => document,
        };
        Ok(Some(document))
    }
}
impl DocumentProvider for OhosServices {
    fn import(&self) -> DocumentFuture<Option<ImportedDocument>> {
        let host = self.clone();
        Box::pin(async move { host.imported(request("pick_import", json!({})).await?) })
    }
    fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool> {
        Box::pin(async move {
            Ok(request("pick_export", json!({"path":path,"name":name}))
                .await?
                .get("saved")
                .and_then(Value::as_bool)
                .unwrap_or(false))
        })
    }
}
impl PlatformServices for OhosServices {
    fn system_locale(&self) -> Option<String> {
        SYSTEM_LOCALE
            .get()
            .and_then(|locale| locale.lock().ok())
            .and_then(|locale| locale.clone())
    }
    fn open_url(&self, url: &str) -> Result<(), String> {
        request_blocking("open_url", json!({"url":url})).map(|_| ())
    }
    fn clipboard_write(&self, text: &str, html: Option<&str>) -> Result<(), String> {
        request_blocking("clipboard_write", json!({"text":text,"html":html})).map(|_| ())
    }
    fn import_file(&self) -> Result<Option<ImportedDocument>, String> {
        self.imported(request_blocking("pick_import", json!({}))?)
    }
    fn export_file(&self, path: &Path, name: &str) -> Result<bool, String> {
        Ok(
            request_blocking("pick_export", json!({"path":path,"name":name}))?
                .get("saved")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )
    }
    fn export_path(&self, name: &str) -> Result<PathBuf, String> {
        if Path::new(name).components().count() != 1 || name == "." || name == ".." {
            return Err("Invalid export filename".into());
        }
        let dir = self
            .files_dir
            .join("exports")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        Ok(dir.join(name))
    }
    fn print_file(&self, path: &Path, name: &str) -> Result<(), String> {
        request_blocking("print", json!({"path":path,"name":name})).map(|_| ())
    }
    fn core_started(&self, core: Arc<Core>, runtime: tokio::runtime::Handle) {
        let mut attached = ATTACHED
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if attached
            .as_ref()
            .is_some_and(|(existing, _)| Arc::ptr_eq(existing, &core))
        {
            return;
        }
        *attached = Some((core.clone(), runtime.clone()));
        if let Some(previous) = DEFERRED_CORE.get().and_then(|slot| {
            slot.lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .filter(|previous| !Arc::ptr_eq(&previous.core, &core))
                .map(|previous| previous.core.clone())
        }) {
            runtime.spawn(async move {
                previous.set_platform_active(false).await;
            });
        }
        APPLICATION_FOREGROUND.store(self.foreground.load(Ordering::Acquire), Ordering::Release);
        let gate_core = core.clone();
        let active = self.foreground.load(Ordering::Acquire) && !ui_fixture_enabled();
        runtime.spawn(async move {
            gate_core.set_platform_active(active).await;
        });
        let already_observed = DEFERRED_CORE.get().is_some_and(|slot| {
            slot.lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|existing| Arc::ptr_eq(&existing.core, &core))
        });
        if !already_observed && !ui_fixture_enabled() {
            runtime.spawn(notifications(core));
        }
    }
    fn foreground_changed(&self, foreground: bool) {
        APPLICATION_FOREGROUND.store(foreground, Ordering::Release);
        wake_notifications();
        let was_foreground = self.foreground.swap(foreground, Ordering::AcqRel);
        let active = foreground && !ui_fixture_enabled();
        if let Some((core, runtime)) = ATTACHED
            .get()
            .and_then(|a| a.lock().ok())
            .and_then(|a| a.clone())
        {
            runtime.spawn(async move {
                core.set_platform_active(active).await;
            });
        }
        if foreground && !was_foreground && !ui_fixture_enabled() {
            if let Some((core, runtime)) = ATTACHED
                .get()
                .and_then(|a| a.lock().ok())
                .and_then(|a| a.clone())
            {
                runtime.spawn(async move {
                    let _ = core.wait_for_mail_workers(Duration::from_secs(15)).await;
                    let _ = core.sync_now(None).await;
                    core.calendar_sync_now(None).await;
                });
            }
        }
        #[cfg(target_env = "ohos")]
        if !foreground {
            flectar_mail::suspend_file_preview();
        }
    }
}

fn notification_core_current(core: &Arc<Core>) -> bool {
    if let Some((current, _)) = ATTACHED
        .get()
        .and_then(|slot| slot.lock().ok())
        .and_then(|slot| slot.clone())
    {
        return Arc::ptr_eq(&current, core);
    }
    DEFERRED_CORE
        .get()
        .and_then(|slot| slot.lock().ok())
        .and_then(|slot| {
            slot.as_ref()
                .map(|current| Arc::ptr_eq(&current.core, core))
        })
        .unwrap_or(true)
}

fn notification_can_dispatch(core: &Arc<Core>) -> bool {
    !ui_fixture_enabled()
        && notification_core_current(core)
        && (APPLICATION_FOREGROUND.load(Ordering::Acquire)
            || (SYNC_RUNNING.load(Ordering::Acquire) && !CANCEL_SYNC.load(Ordering::Acquire)))
}

async fn notification_tick(core: &Arc<Core>) {
    if notification_can_dispatch(core) {
        tokio::time::sleep(Duration::from_secs(5)).await;
    } else {
        // No periodic database polling outside a foreground or system job window.
        std::future::pending::<()>().await;
    }
}

async fn notifications(core: Arc<Core>) {
    let mut events = core.bus.subscribe();
    if !notification_core_current(&core) {
        return;
    }
    if core.recover_notification_deliveries().await.is_err() {
        crate::native::log("Notification outbox recovery failed");
    }
    let mut first = true;
    loop {
        if !notification_core_current(&core) {
            break;
        }
        let event = if first {
            first = false;
            None
        } else {
            tokio::select! {
                _ = NOTIFICATIONS_WAKE.notified() => None,
                _ = notification_tick(&core) => None,
                event = events.recv() => match event {
                    Ok(event) => Some(event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
                    Err(_) => break,
                },
            }
        };
        if !notification_can_dispatch(&core) {
            continue;
        }
        if let Some(CoreEvent::EventReminder {
            event,
            occurrence_start,
        }) = event
        {
            if core
                .get_settings()
                .await
                .is_ok_and(|settings| settings.notifications_enabled)
            {
                let payload = json!({"title":"日程提醒",
                    "body":event.summary.as_deref().unwrap_or("即将开始的日程"),
                    "messageId":format!("calendar:{}:{occurrence_start}",event.id)});
                let _ =
                    tokio::time::timeout(Duration::from_secs(30), request("notify", payload)).await;
            }
        }
        // MailNew only represents the human-mail chime. The durable outbox
        // also includes automated mail and survives lost events/process death.
        if notifications::drain_due(&core).await.is_err() {
            crate::native::log("Notification outbox dispatch deferred");
        }
    }
}

pub fn platform_context(files_dir: PathBuf, cache_dir: PathBuf) -> Result<PlatformContext, String> {
    if !files_dir.is_absolute() || !cache_dir.is_absolute() {
        return Err("HarmonyOS must provide absolute private directories".into());
    }
    #[cfg(debug_assertions)]
    {
        let fixture = ui_fixture_enabled();
        if *FIXTURE_PROFILE.get_or_init(|| fixture) != fixture {
            return Err("Offline fixture requires a fresh application process".into());
        }
    }
    clean_stale_exports(&files_dir);
    let _ = IMPORT_ROOT.set(cache_dir.join("feihong-imports"));
    #[cfg(debug_assertions)]
    let fixture_store =
        ui_fixture_enabled().then(|| Arc::new(crate::offline_fixture::OfflineStore));
    #[cfg(debug_assertions)]
    let credentials: core::accounts::credentials::CredentialStoreHandle = match &fixture_store {
        Some(store) => store.clone(),
        None => Arc::new(asset::AssetStore),
    };
    #[cfg(not(debug_assertions))]
    let credentials = Arc::new(asset::AssetStore);
    #[cfg(debug_assertions)]
    let fixture_paths = if ui_fixture_enabled() {
        Some(private_fixture_paths(&files_dir, &cache_dir)?)
    } else {
        None
    };
    let host = Arc::new(OhosServices {
        files_dir: files_dir.clone(),
        cache_dir: cache_dir.clone(),
        foreground: Arc::new(AtomicBool::new(true)),
    });
    let mut installed = OPENPGP
        .get_or_init(|| Mutex::new(false))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !*installed {
        #[cfg(debug_assertions)]
        let keys: Arc<dyn core::mail_security::OpenPgpKeyStore> = match fixture_store {
            Some(store) => store,
            None => Arc::new(asset::AssetStore),
        };
        #[cfg(not(debug_assertions))]
        let keys = credentials.clone();
        #[cfg(debug_assertions)]
        let public_dir = fixture_paths
            .as_ref()
            .map(|paths| paths.data_dir.join("openpgp-public"))
            .unwrap_or_else(|| files_dir.join("openpgp-public"));
        #[cfg(not(debug_assertions))]
        let public_dir = files_dir.join("openpgp-public");
        let pgp = core::mail_security::EmbeddedOpenPgpBackend::new(public_dir, keys)
            .map_err(|e| e.to_string())?
            .with_passphrase_provider(Arc::new(|fingerprint| {
                request_blocking("openpgp_passphrase", json!({"fingerprint":fingerprint}))
                    .map_err(CoreError::Other)?
                    .get("passphrase")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| CoreError::Other("Private-key unlock cancelled".into()))
            }));
        core::mail_security::install_platform_backend(Arc::new(pgp)).map_err(|e| e.to_string())?;
        *installed = true;
    }
    drop(installed);
    let existing_core = DEFERRED_CORE.get().and_then(|slot| {
        slot.lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|existing| existing.files_dir == files_dir && existing.cache_dir == cache_dir)
            .map(|existing| existing.core.clone())
    });
    let mut platform = PlatformContext::app_private(
        files_dir,
        cache_dir,
        credentials,
        Arc::new(core::oauth::redirect::LoopbackRedirectBroker::default()),
    );
    platform.documents = host.clone();
    platform.services = Some(host);
    platform.existing_core = existing_core;
    #[cfg(debug_assertions)]
    if let Some(paths) = fixture_paths {
        platform.paths = paths;
        platform.existing_core = None;
    }
    Ok(platform)
}

#[cfg(debug_assertions)]
fn private_fixture_paths(
    files_dir: &Path,
    cache_dir: &Path,
) -> Result<core::config::Paths, String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    const MARKER: &[u8] = b"feihong-offline-ui-fixture-v1\n";
    let root = files_dir.join("debug-ui-fixture");
    let cache = cache_dir.join("debug-ui-fixture");
    if root.is_symlink() || cache.is_symlink() {
        return Err("Offline fixture directory is a symbolic link".into());
    }
    if root.exists() {
        if root.join(".fixture-profile").is_symlink()
            || std::fs::read(root.join(".fixture-profile")).ok().as_deref() != Some(MARKER)
        {
            return Err("Refusing to use an unmarked offline fixture directory".into());
        }
    } else {
        std::fs::create_dir(&root).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        let mut marker = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(".fixture-profile"))
            .map_err(|e| e.to_string())?;
        marker.write_all(MARKER).map_err(|e| e.to_string())?;
    }
    if root.join("data").is_symlink() {
        return Err("Offline fixture data is a symbolic link".into());
    }
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    Ok(core::config::Paths::new(root.join("data"), cache))
}

pub(crate) fn prepare_ui_fixture(platform: PlatformContext) -> Result<PlatformContext, String> {
    #[cfg(debug_assertions)]
    if ui_fixture_enabled() {
        let mut platform = platform;
        let runtime = deferred_runtime()?;
        let core = runtime.block_on(async {
            let core = Arc::new(
                Core::start_mail_ui_with_platform(
                    platform.paths.clone(),
                    platform.credentials.clone(),
                    platform.oauth_redirects.clone(),
                )
                .await
                .map_err(|e| e.to_string())?,
            );
            core.set_platform_active(false).await;
            crate::offline_fixture::seed(&core, &platform.paths).await?;
            Ok::<_, String>(core)
        })?;
        platform.existing_core = Some(core);
        crate::native::log("offline UI fixture ready; protocol workers paused");
        return Ok(platform);
    }
    Ok(platform)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncOptions {
    files_dir: PathBuf,
    cache_dir: PathBuf,
    #[serde(default)]
    system_language: Option<String>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn feihong_run_deferred_sync(options: *const c_char) -> i32 {
    if ui_fixture_enabled() {
        return 0;
    }
    if options.is_null() || SYNC_RUNNING.swap(true, Ordering::AcqRel) {
        return -1;
    }
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            SYNC_RUNNING.store(false, Ordering::Release);
            wake_notifications();
        }
    }
    let _guard = Guard;
    CANCEL_SYNC.store(false, Ordering::Release);
    wake_notifications();
    let run = std::panic::catch_unwind(|| -> Result<(), String> {
        let options: SyncOptions =
            serde_json::from_slice(unsafe { CStr::from_ptr(options) }.to_bytes())
                .map_err(|_| "Invalid sync options".to_string())?;
        if let Some(locale) = options.system_language.clone() {
            configure_system_locale(locale);
        }
        let attached = ATTACHED
            .get()
            .and_then(|a| a.lock().ok())
            .and_then(|a| a.clone());
        let runtime = deferred_runtime()?;
        runtime.block_on(async move {
            let deferred = DEFERRED_CORE.get_or_init(|| Mutex::new(None))
                .lock().unwrap_or_else(|e| e.into_inner()).as_ref()
                .filter(|existing| existing.files_dir == options.files_dir && existing.cache_dir == options.cache_dir)
                .map(|existing| existing.core.clone());
            let core = if let Some((core,_))=attached { core } else if let Some(core)=deferred { core } else {
                let platform=platform_context(options.files_dir.clone(),options.cache_dir.clone())?;
                let core=Arc::new(Core::start_mail_ui_with_platform(platform.paths,platform.credentials,platform.oauth_redirects).await.map_err(|e|e.to_string())?);
                core.notify_ui_ready(); tokio::spawn(notifications(core.clone()));
                *DEFERRED_CORE.get().unwrap().lock().unwrap_or_else(|e| e.into_inner())=Some(DeferredCore {
                    files_dir: options.files_dir, cache_dir: options.cache_dir, core: core.clone(),
                });
                core
            };
            let current = ATTACHED.get().and_then(|slot| slot.lock().ok())
                .and_then(|slot| slot.clone()).map(|(core, _)| core);
            let core = if let Some(current) = current {
                if !Arc::ptr_eq(&core, &current) { core.set_platform_active(false).await; }
                current
            } else { core };
            if ui_fixture_enabled() { core.set_platform_active(false).await; return Ok(()); }
            core.set_platform_active(true).await;
            let sync=async { core.wait_for_mail_workers(Duration::from_secs(15)).await.map_err(|e|e.to_string())?; core.sync_now(None).await.map_err(|e|e.to_string())?; core.calendar_sync_now(None).await; Ok::<_,String>(()) };
            let cancel=async { loop { if CANCEL_SYNC.load(Ordering::Acquire) { break }; tokio::time::sleep(Duration::from_millis(100)).await; } };
            let result=tokio::select! { result=tokio::time::timeout(Duration::from_secs(90),sync) => result.map_err(|_|"Deferred sync budget exhausted".to_string()).and_then(|r|r), _=cancel => Err("Deferred sync stopped by system".into()) };
            let current = ATTACHED.get().and_then(|slot| slot.lock().ok())
                .and_then(|slot| slot.clone()).map(|(core, _)| core);
            let active = APPLICATION_FOREGROUND.load(Ordering::Acquire)
                && !ui_fixture_enabled()
                && current.as_ref().is_none_or(|current| Arc::ptr_eq(current, &core));
            core.set_platform_active(active).await;
            result
        })
    });
    match run {
        Ok(Ok(())) => 0,
        _ => -2,
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_cancel_deferred_sync() {
    CANCEL_SYNC.store(true, Ordering::Release);
    wake_notifications();
}
