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

/// The iOS Keychain credential store, whose rules every platform tests.
#[cfg(any(test, target_os = "ios"))]
mod keychain;
/// The iOS sign-in session broker, whose rules every platform tests.
#[cfg(any(test, target_os = "ios"))]
mod web_authentication;

#[cfg(target_env = "ohos")]
mod ohos;
#[cfg(target_env = "ohos")]
pub use ohos::*;

/// Android and OpenHarmony memory levels, whose mapping every platform tests.
#[cfg(any(test, target_os = "android", target_env = "ohos"))]
mod memory_levels;

/// Fonts a host without Fontconfig hands to the email (Blitz/Parley) renderer
/// and the composer: the platform font directory, and the fallback policy the
/// host applies to its own UI, so mail content falls back the same way.
pub struct EmailFonts {
    /// The platform font directory.
    pub dir: &'static std::path::Path,
    /// Family-name fragments of the Han (CJK) faces, most preferred first.
    pub han_families: &'static [&'static str],
    /// Scripts, as ISO 15924 tags, that fall back to every system family.
    pub fallback_scripts: &'static [[u8; 4]],
}

/// Desktop reach system fonts through the fontique system backend.
#[cfg(desktop)]
pub(crate) fn email_fonts() -> Option<&'static EmailFonts> {
    None
}

/// Build the platform context for the process launcher in src/desktop.rs.
#[cfg(desktop)]
pub fn launch_context() -> Result<crate::PlatformContext, flectar_mail_core::error::CoreError> {
    crate::PlatformContext::desktop()
}
