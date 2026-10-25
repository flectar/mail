//! The Slint `WindowAdapter` for the single OHOS XComponent window.
//!
//! Structure mirrors Slint's own `MinimalSoftwareWindow`: a `Window` bound to
//! this adapter through `Rc::new_cyclic`, plus a `SoftwareRenderer`. What it
//! adds over the minimal adapter is the wake-up channel: `request_redraw` must
//! be able to wake the Slint thread's event loop across threads.

use core::cell::Cell;
use std::rc::{Rc, Weak};
use std::sync::mpsc::Sender;

use slint::platform::software_renderer::{RepaintBufferType, SoftwareRenderer};
use slint::platform::{PointerEventButton, Renderer, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, Window, WindowSize};

// `Renderer` is sealed; the association method used below lives on the sealed
// supertrait (same reason the backend links `i-slint-core` directly for the
// internal `WindowAdapterInternal` hook).
use i_slint_core::renderer::RendererSealed;

use crate::event::{BackendEvent, TouchPhase};
use crate::gpu::GpuRenderer;
use crate::present::PresentOutcome;

/// Slint window backed by the XComponent surface.
pub struct OhosWindow {
    window: Window,
    /// Weak self-reference, needed to associate the experimental GPU renderer
    /// with this window adapter once the surface exists (`Rc::new_cyclic`
    /// hands it out during construction).
    self_weak: Weak<Self>,
    renderer: SoftwareRenderer,
    /// Experimental femtovg/wgpu renderer; when present and attached it
    /// replaces the software renderer entirely.
    gpu: Option<GpuRenderer>,
    /// True once the GPU surface is attached. Until then (and permanently
    /// after a failed attach) Slint talks to the software renderer, so text
    /// metrics never touch a renderer without a live canvas.
    gpu_active: Cell<bool>,
    /// The native window pointer and physical size the GPU surface is
    /// currently attached to (`None` before the first attach and after
    /// `SurfaceDestroyed`). An identical pointer + size means the surface is
    /// still configured; the same pointer with a new size is an in-place
    /// window resize.
    gpu_surface: Cell<Option<(usize, PhysicalSize)>>,
    needs_redraw: Cell<bool>,
    size: Cell<PhysicalSize>,
    event_tx: Sender<BackendEvent>,
}

impl OhosWindow {
    pub fn new(event_tx: Sender<BackendEvent>) -> Rc<Self> {
        if crate::gpu::requested() {
            crate::log::info(crate::gpu::describe());
        }
        // The closure parameter is annotated so inference does not unify `Self`
        // with `dyn WindowAdapter` at the `Window::new` coercion site.
        Rc::new_cyclic(|weak: &Weak<Self>| Self {
            window: Window::new(weak.clone()),
            self_weak: weak.clone(),
            // ReusedBuffer: the presenter paints partial updates into its
            // persistent frame, so key repeats and cursor blinks only rasterize
            // the changed regions (a NewBuffer renderer must repaint the whole
            // ~2.8 MP window for every frame).
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(
                RepaintBufferType::ReusedBuffer,
            ),
            gpu: crate::gpu::requested().then(crate::gpu::new_suspended),
            gpu_active: Cell::new(false),
            gpu_surface: Cell::new(None),
            needs_redraw: Cell::new(true),
            size: Cell::new(PhysicalSize::default()),
            event_tx,
        })
    }

    /// Attach the GPU renderer to the XComponent surface (no-op in software
    /// mode). Called from the Slint thread for every `Surface` event:
    ///
    /// * same native window and size — nothing to do;
    /// * same native window, new size — reconfigure the existing surface
    ///   (a PC window resize must not re-create the wgpu device);
    /// * anything else (first attach, new window, or after destroy) — full
    ///   surface setup. A failed attach leaves the software renderer in
    ///   charge for good.
    pub fn attach_surface(&self, window: *mut core::ffi::c_void, size: PhysicalSize) {
        let Some(gpu) = &self.gpu else {
            return;
        };
        let key = (window as usize, size);
        if self.gpu_surface.get() == Some(key) {
            return;
        }
        if let Some((attached, _)) = self.gpu_surface.get()
            && attached == window as usize
        {
            match gpu.resize(size) {
                Ok(()) => {
                    self.gpu_surface.set(Some(key));
                    crate::log::info(&format!(
                        "gpu: surface resized ({}x{})",
                        size.width, size.height
                    ));
                    return;
                }
                Err(error) => {
                    crate::log::error(&format!(
                        "gpu: resize failed, re-creating the surface: {error}"
                    ));
                }
            }
        }
        match crate::gpu::set_surface(gpu, window, size) {
            Ok(()) => {
                self.gpu_active.set(true);
                self.gpu_surface.set(Some(key));
                // Slint associates the renderer with the window adapter exactly
                // once, when the component is set (`WindowInner::set_component`)
                // — before the surface exists here, so that association went to
                // the software renderer, which was active back then. The
                // femtovg renderer hard-requires the adapter while rendering
                // ("Renderer must be associated with component before use"), so
                // redo the association now that its canvas is live.
                if let Some(adapter) = self.self_weak.upgrade() {
                    let adapter: Rc<dyn WindowAdapter> = adapter;
                    gpu.set_window_adapter(&adapter);
                }
                crate::log::info(&format!(
                    "gpu: surface attached ({}x{})",
                    size.width, size.height
                ));
            }
            Err(error) => {
                self.gpu_active.set(false);
                self.gpu_surface.set(None);
                // The component may have been associated with the GPU
                // renderer before this attempt; re-associate the software
                // renderer so text metrics and rendering keep working after
                // the fallback.
                self.associate_software();
                crate::log::error(&format!(
                    "gpu: surface attach failed; using the software renderer: {error}"
                ));
            }
        }
    }

    /// The surface is going away (`OnSurfaceDestroyed`): the native window
    /// behind the GPU surface may become invalid, so the next `Surface` event
    /// always re-creates the surface instead of reusing it.
    pub fn detach_surface(&self) {
        self.gpu_surface.set(None);
    }

    /// Force the next frame to repaint every pixel. Called when the native
    /// surface appears (or changes): the compositor may hand out buffers whose
    /// previous contents are unknown, and the renderer's partial-repaint cache
    /// must not assume its old frame is still on screen.
    pub fn invalidate_frame(&self) {
        // The documented effect of a buffer-type change is a cache reset; the
        // second call restores partial repainting for subsequent frames.
        self.renderer
            .set_repaint_buffer_type(RepaintBufferType::NewBuffer);
        self.renderer
            .set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
    }

    /// Re-associate the software renderer with this window adapter. Slint
    /// performs the association once at component creation; a GPU attempt can
    /// replace it, so a later fallback has to restore it explicitly.
    fn associate_software(&self) {
        if let Some(adapter) = self.self_weak.upgrade() {
            let adapter: Rc<dyn WindowAdapter> = adapter;
            self.renderer.set_window_adapter(&adapter);
        }
    }

    /// Render if a redraw was requested since the last call. Software frames
    /// are handed to `present_software` (the buffer-queue presenter); GPU
    /// frames present themselves through the wgpu surface.
    ///
    /// Returns whether a frame was attempted and, for the software path, the
    /// presenter's outcome so the caller can retry a failed flush.
    pub fn draw_if_needed(
        &self,
        size: PhysicalSize,
        present_software: impl FnOnce(&SoftwareRenderer) -> PresentOutcome,
    ) -> (bool, Option<PresentOutcome>) {
        if !self.needs_redraw.replace(false) {
            return (false, None);
        }
        match (&self.gpu, self.gpu_active.get()) {
            (Some(gpu), true) => {
                if let Err(error) = crate::gpu::render(gpu, size) {
                    crate::log::error(&format!(
                        "gpu: render failed; falling back to software: {error}"
                    ));
                    self.gpu_active.set(false);
                    self.associate_software();
                    let outcome = present_software(&self.renderer);
                    return (true, Some(outcome));
                }
                (true, None)
            }
            _ => (true, Some(present_software(&self.renderer))),
        }
    }

    /// Dispatch the windowing-system scale factor (called once per surface
    /// before the first `adapt_size`).
    pub fn dispatch_scale_factor(&self, scale_factor: f32) {
        let _ = self
            .window
            .dispatch_event_with_result(WindowEvent::ScaleFactorChanged { scale_factor });
    }

    /// Set the physical surface size; converts to a logical `Resized` event.
    pub fn adapt_size(&self, size: PhysicalSize) {
        self.window.set_size(size);
    }

    /// Translate one touch sample into Slint pointer events.
    ///
    /// XComponent coordinates are physical pixels relative to the element;
    /// Slint works in logical pixels, so they are divided by the window's scale
    /// factor (the display density dispatched on surface creation).
    pub fn dispatch_touch(&self, x: f32, y: f32, phase: TouchPhase) {
        let scale = {
            let factor = self.window.scale_factor();
            if factor > 0.0 { factor } else { 1.0 }
        };
        let position = LogicalPosition::new(x / scale, y / scale);
        let event = match phase {
            TouchPhase::Down => WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            },
            TouchPhase::Move => WindowEvent::PointerMoved { position },
            TouchPhase::Up => WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            },
            // The system took the gesture over: end it without activating
            // whatever it started on.
            TouchPhase::Cancel => WindowEvent::PointerExited,
        };
        let _ = self.window.dispatch_event_with_result(event);
    }

    /// Translate one wheel/touchpad scroll into a Slint scroll event.
    ///
    /// The position is a physical XComponent coordinate and is scaled like a
    /// touch; the deltas are already logical pixels (the caller converts the
    /// platform's wheel unit).
    pub fn dispatch_scroll(&self, x: f32, y: f32, delta_x: f32, delta_y: f32) {
        let scale = {
            let factor = self.window.scale_factor();
            if factor > 0.0 { factor } else { 1.0 }
        };
        let position = LogicalPosition::new(x / scale, y / scale);
        let _ = self
            .window
            .dispatch_event_with_result(WindowEvent::PointerScrolled {
                position,
                delta_x,
                delta_y,
            });
    }

    /// Translate one keyboard event into a Slint key event.
    pub fn dispatch_key(&self, text: slint::SharedString, down: bool, repeat: bool) {
        let event = if down {
            if repeat {
                WindowEvent::KeyPressRepeated { text }
            } else {
                WindowEvent::KeyPressed { text }
            }
        } else {
            WindowEvent::KeyReleased { text }
        };
        let _ = self.window.dispatch_event_with_result(event);
    }
}

impl WindowAdapter for OhosWindow {
    fn window(&self) -> &Window {
        &self.window
    }

    fn renderer(&self) -> &dyn Renderer {
        // Only the renderer that is actually active may be visible to Slint.
        // Returning the suspended GPU renderer before its surface exists makes
        // `WindowInner::set_component` associate it, and a later software
        // fallback then has no adapter association.
        match (&self.gpu, self.gpu_active.get()) {
            (Some(gpu), true) => gpu,
            _ => &self.renderer,
        }
    }

    fn size(&self) -> PhysicalSize {
        self.size.get()
    }

    fn set_size(&self, size: WindowSize) {
        let scale_factor = self.window.scale_factor();
        self.size.set(size.to_physical(scale_factor));
        let logical_size = size.to_logical(scale_factor);
        let _ = self
            .window
            .dispatch_event_with_result(WindowEvent::Resized { size: logical_size });
    }

    fn request_redraw(&self) {
        self.needs_redraw.set(true);
        let _ = self.event_tx.send(BackendEvent::Wake);
    }

    /// Slint's internal adapter interface (IME and friends are not part of the
    /// public backend API; the built-in backends use the same handshake).
    fn internal(
        &self,
        _: i_slint_core::InternalToken,
    ) -> Option<&dyn i_slint_core::window::WindowAdapterInternal> {
        Some(self)
    }
}

impl i_slint_core::window::WindowAdapterInternal for OhosWindow {
    /// Editable fields ask for the platform input method here; the IME module
    /// forwards enable/disable to the ArkTS hooks.
    fn input_method_request(&self, request: i_slint_core::window::InputMethodRequest) {
        use i_slint_core::items::InputType;
        match request {
            i_slint_core::window::InputMethodRequest::Enable(properties) => {
                crate::ime::request(true, matches!(properties.input_type, InputType::Password))
            }
            i_slint_core::window::InputMethodRequest::Update(properties) => {
                crate::ime::update_cursor(
                    properties.cursor_rect_origin.x,
                    properties.cursor_rect_origin.y,
                    properties.cursor_rect_size.width,
                    properties.cursor_rect_size.height,
                )
            }
            i_slint_core::window::InputMethodRequest::Disable => crate::ime::request(false, false),
            _ => {}
        }
    }
}
