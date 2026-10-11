//! System capabilities supplied by a native application host.
//! Mail, calendar and document state continue to live in Core.
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, RwLock},
};

pub trait PlatformServices: Send + Sync {
    fn system_locale(&self) -> Option<String> {
        None
    }
    fn open_url(&self, url: &str) -> Result<(), String>;
    fn clipboard_write(&self, text: &str, html: Option<&str>) -> Result<(), String>;
    fn import_file(&self) -> Result<Option<crate::documents::ImportedDocument>, String>;
    fn export_file(&self, path: &Path, name: &str) -> Result<bool, String>;
    /// Private staging outside the live database and cache trees.
    fn export_path(&self, name: &str) -> Result<PathBuf, String>;
    fn print_file(&self, path: &Path, name: &str) -> Result<(), String>;
    fn core_started(&self, core: Arc<flectar_mail_core::Core>, runtime: tokio::runtime::Handle);
    fn foreground_changed(&self, foreground: bool);
}

static SERVICES: OnceLock<RwLock<Option<Arc<dyn PlatformServices>>>> = OnceLock::new();

pub(crate) fn install(services: Option<Arc<dyn PlatformServices>>) {
    *SERVICES
        .get_or_init(|| RwLock::new(None))
        .write()
        .unwrap_or_else(|e| e.into_inner()) = services;
}

pub fn current() -> Option<Arc<dyn PlatformServices>> {
    SERVICES.get()?.read().ok()?.clone()
}

#[cfg(target_env = "ohos")]
static FONT_PATHS: OnceLock<Vec<String>> = OnceLock::new();

#[cfg(target_env = "ohos")]
pub fn configure_fonts(paths: &[String]) {
    let _ = FONT_PATHS.set(
        paths
            .iter()
            .filter(|p| Path::new(p).is_absolute())
            .cloned()
            .collect(),
    );
}

#[cfg(target_env = "ohos")]
pub(crate) fn font_paths() -> &'static [String] {
    FONT_PATHS.get().map(Vec::as_slice).unwrap_or(&[])
}
