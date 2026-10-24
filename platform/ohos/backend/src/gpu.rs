//! Experimental GPU renderer (femtovg on wgpu via EGL) for the XComponent
//! surface.
//!
//! Attempted by default; `FLECTAR_OHOS_SOFTWARE=1` opts out on local runs (the
//! device build has no environment), and a failed surface attach falls back to
//! the software renderer. Construction is "suspended" (no surface, no GL
//! context): the renderer attaches to the `OHNativeWindow` when the surface
//! callback fires, renders on the Slint thread, and the surface gate keeps the
//! compositor from pulling the window away while a frame is in flight.
//!
//! The raw-window-handle crate documents exactly this scenario (XComponent
//! `OnSurfaceCreated` hands over an `OHNativeWindow` for
//! `eglCreateWindowSurface`), and wgpu's `DisplayAndWindowHandle` is a
//! blanket trait over `HasWindowHandle + HasDisplayHandle`.

use core::ffi::c_void;
use core::ptr::NonNull;

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, OhosNdkWindowHandle,
    RawWindowHandle, WindowHandle,
};
use slint::PhysicalSize;

use crate::ffi;

/// The renderer type used for the surface path (femtovg over wgpu 29/30 —
/// whichever `i-slint-renderer-femtovg` resolved, matching `set_surface`).
pub type GpuRenderer =
    i_slint_renderer_femtovg::FemtoVGRenderer<i_slint_renderer_femtovg::wgpu::WGPUBackend>;

/// Borrowed `OHNativeWindow*` presented to wgpu as an OHOS NDK window handle.
struct SurfaceHandle {
    native_window: NonNull<c_void>,
}

// SAFETY: the handle only carries the pointer; every use (surface creation,
// rendering) happens on the Slint thread inside the surface lifetime protocol
// enforced by `present::SurfaceGate`.
unsafe impl Send for SurfaceHandle {}
unsafe impl Sync for SurfaceHandle {}

impl HasWindowHandle for SurfaceHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: `native_window` is the live `OHNativeWindow` handed over by
        // `OnSurfaceCreated`; the gate prevents use after `OnSurfaceDestroyed`.
        Ok(unsafe {
            WindowHandle::borrow_raw(RawWindowHandle::OhosNdk(OhosNdkWindowHandle::new(
                self.native_window,
            )))
        })
    }
}

impl HasDisplayHandle for SurfaceHandle {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // The OHOS display handle carries no data.
        Ok(DisplayHandle::ohos())
    }
}

/// Whether the experimental GPU path is attempted for this process.
///
/// OHOS apps do not inherit shell environment variables, so the env probe only
/// helps local runs; the device build attempts the GPU path by default and
/// falls back to the software renderer automatically when the surface cannot
/// be attached.
pub fn requested() -> bool {
    if std::env::var_os("FLECTAR_OHOS_SOFTWARE").is_some_and(|value| !value.is_empty()) {
        return false;
    }
    true
}

/// Create the suspended renderer (no surface yet).
pub fn new_suspended() -> GpuRenderer {
    use i_slint_renderer_femtovg::FemtoVGRendererExt;
    GpuRenderer::new_suspended()
}

/// Attach the renderer to the XComponent surface. Called from the Slint thread
/// for every `Surface` event (creation and resize).
pub fn set_surface(
    renderer: &GpuRenderer,
    window: *mut c_void,
    size: PhysicalSize,
) -> Result<(), String> {
    let Some(native_window) = NonNull::new(window) else {
        return Err("surface callback without an OHNativeWindow".into());
    };
    let target =
        i_slint_core::graphics::wgpu_30::SurfaceTarget::WindowHandle(Box::new(SurfaceHandle {
            native_window,
        }));
    renderer
        .set_surface(target, size, Some(requested_gl_api()), false)
        .map_err(|error| format!("{error}"))
}

/// Ask wgpu for the GL (EGL/GLES) backend explicitly.
///
/// Two Slint behaviors make this necessary: the femtovg backend passes
/// `wgpu::Backends::GL` as `backends_to_avoid` ("rendering artifacts :("), and
/// Slint's `mask_backends` computes `requested & !avoid` — keeping the request
/// unchanged only when that subtraction would leave nothing. Any default
/// request (`None` → `from_env().unwrap_or_default()` = all compiled backends)
/// therefore never reaches GL, leaving Vulkan as the only candidate — and
/// Vulkan's OHOS instance rejects the window handle ("not a Vulkan-compatible
/// handle"). Requesting *exactly* GL survives the mask, and wgpu-hal's EGL
/// backend has dedicated OHOS support: `(WindowKind::Unknown, Rwh::OhosNdk(h))`
/// feeds `h.native_window` straight into `eglCreateWindowSurface`.
fn requested_gl_api() -> i_slint_core::graphics::RequestedGraphicsAPI {
    use i_slint_core::graphics::wgpu_30::api::{WGPUConfiguration, WGPUSettings};
    use i_slint_core::graphics::wgpu_30::wgpu;

    let mut settings = WGPUSettings::default();
    settings.backends = wgpu::Backends::GL;
    // The email renderer (Vello) shares this device and needs the full default
    // limits (compute/storage buffers) — the `downlevel_webgl2_defaults` the
    // settings start with only suit WebGL2 deployments. This mirrors the
    // desktop GPU path (`require_wgpu_30` with the same limits).
    settings.device_required_limits = wgpu::Limits::default();
    i_slint_core::graphics::RequestedGraphicsAPI::WGPU30(WGPUConfiguration::Automatic(settings))
}

/// Render one frame; the femtovg wgpu surface presents it itself.
pub fn render(renderer: &GpuRenderer, size: PhysicalSize) -> Result<(), String> {
    use i_slint_renderer_femtovg::FemtoVGRendererExt;
    renderer
        .render_transformed_with_post_callback(0.0, (0.0, 0.0), size, None)
        .map(|_| ())
        .map_err(|error| format!("{error}"))
}

/// Release the surface early. Dropping the renderer would also work, but the
/// wgpu backend keeps the EGL surface alive inside `set_surface`'s storage;
/// the surface gate blocks further rendering after `SurfaceDestroyed` and the
/// next surface event replaces it.
pub fn release_surface(_window: *mut c_void) {}

/// Log-friendly descriptor of the request outcome.
pub fn describe() -> &'static str {
    "GPU renderer enabled (femtovg/wgpu; falls back to software on failure)"
}

// Keep the module explicitly aware of the ffi type so the surface pointer type
// stays the same as the backend's everywhere.
const _: () = {
    fn _assert_surface_ptr_type(pointer: *mut ffi::OHNativeWindow) -> *mut c_void {
        pointer
    }
};
