//! Host integration layer. This is the only application module that names a
//! mobile operating system; shared code selects behaviour with the `mobile`,
//! `desktop` and `linux_desktop` aliases from build.rs and calls the items
//! re-exported here. Every mobile module provides the same crate-internal
//! items, so a new platform adds one module instead of editing call sites.
//! See platform/README.md for the full host contract.

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::*;

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::*;

#[cfg(target_env = "ohos")]
mod ohos;
#[cfg(target_env = "ohos")]
pub use ohos::*;

/// Desktop reach system fonts through the fontique system backend.
#[cfg(desktop)]
pub(crate) fn email_font_dir() -> Option<&'static std::path::Path> {
    None
}

/// Build the platform context for the process launcher in src/desktop.rs.
#[cfg(desktop)]
pub fn launch_context() -> Result<crate::PlatformContext, flectar_mail_core::error::CoreError> {
    crate::PlatformContext::desktop()
}
