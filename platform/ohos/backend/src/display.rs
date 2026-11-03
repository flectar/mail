//! Display metrics via the OHOS native display manager.

use crate::ffi;

/// Default display density in physical pixels per logical pixel (the ArkUI
/// `vp` scale). `None` when the display manager is unavailable.
pub fn default_density() -> Option<f32> {
    let mut density: f32 = 0.0;
    // SAFETY: `density` is a valid f32 the callee fills; return code 0
    // (`DISPLAY_MANAGER_OK`) signals success.
    let rc = unsafe { ffi::OH_NativeDisplayManager_GetDefaultDisplayDensityPixels(&mut density) };
    if rc == 0 && density > 0.0 {
        Some(density)
    } else {
        None
    }
}
