//! Surface lifetime gate and software-renderer presentation to `OHNativeWindow`.
//!
//! Presentation protocol (single surface, one producer thread):
//!
//! 1. `OnSurfaceCreated/Changed` → [`SurfaceGate::surface_up`] + `Surface` event.
//! 2. The Slint thread draws: the renderer repaints only the dirty regions into
//!    a persistent CPU frame ([`Presenter::frame`], `ReusedBuffer` semantics),
//!    then the frame is copied into a buffer dequeued from the compositor
//!    queue, `mmap`ed, and flushed. Rendering directly into the queue buffer
//!    would force a full ~2.8 MP repaint per frame, because every dequeued
//!    buffer is new.
//! 3. `OnSurfaceDestroyed` → [`SurfaceGate::begin_destroy`], sends
//!    `SurfaceDestroyed`, then waits (bounded) for [`SurfaceGate::acked`] so the
//!    system never pulls the surface out from under an in-flight frame.

use core::ffi::c_void;
use core::ptr::{NonNull, null_mut};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{PremultipliedRgbaColor, SoftwareRenderer};

use crate::ffi;

/// Bytes per frame pixel (RGBA8888).
const PIXEL_BYTES: usize = core::mem::size_of::<PremultipliedRgbaColor>();

/// Upper bound for the damage rectangles passed to `FlushBuffer`.
const MAX_FLUSH_RECTS: usize = 64;

/// Gate state. `Down` is also the "acked" state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Gate {
    Down,
    Up,
    Destroying,
}

/// Serializes surface destruction against in-flight frames.
pub struct SurfaceGate {
    state: Mutex<Gate>,
    cv: Condvar,
}

impl SurfaceGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Gate::Down),
            cv: Condvar::new(),
        })
    }

    /// Surface is ready (called from `OnSurfaceCreated/Changed`).
    pub fn surface_up(&self) {
        *self.state.lock().unwrap() = Gate::Up;
        self.cv.notify_all();
    }

    /// True while the surface is being torn down; the draw path must skip
    /// frames when this returns `true`.
    pub fn is_destroying(&self) -> bool {
        *self.state.lock().unwrap() == Gate::Destroying
    }

    /// Called from `OnSurfaceDestroyed` (OHOS main thread). Returns `true` if a
    /// live surface was being torn down (and the caller should wait), `false`
    /// if there was nothing to tear down.
    pub fn begin_destroy(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if *state == Gate::Up {
            *state = Gate::Destroying;
            true
        } else {
            *state = Gate::Down;
            false
        }
    }

    /// Bounded wait for the Slint thread to acknowledge destruction.
    pub fn wait_acked(&self, timeout: Duration) {
        let mut state = self.state.lock().unwrap();
        let deadline = Instant::now() + timeout;
        while *state != Gate::Down {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (guard, _) = self.cv.wait_timeout(state, deadline - now).unwrap();
            state = guard;
        }
    }

    /// Called by the Slint thread after it processed `SurfaceDestroyed`.
    pub fn acked(&self) {
        *self.state.lock().unwrap() = Gate::Down;
        self.cv.notify_all();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentOutcome {
    Presented,
    /// No surface (yet/again) or nothing to render.
    Skipped,
    /// The buffer queue refused or the flush failed; the frame is dropped.
    Failed(i32),
}

/// Buffer-queue presenter for one `OHNativeWindow`.
pub struct Presenter {
    window: *mut ffi::OHNativeWindow,
    configured: bool,
    /// Persistent CPU frame: the software renderer repaints only dirty regions
    /// into it (`ReusedBuffer` semantics), so key repeats and cursor blinks
    /// never pay for a full-window rasterization.
    frame: Vec<PremultipliedRgbaColor>,
    frame_size: (u32, u32),
}

impl Presenter {
    pub fn new() -> Self {
        Self {
            window: null_mut(),
            configured: false,
            frame: Vec::new(),
            frame_size: (0, 0),
        }
    }

    /// Attach (or re-attach) the surface pointer.
    pub fn set_surface(&mut self, window: *mut c_void) {
        if self.window != window {
            self.configured = false;
        }
        self.window = window.cast();
    }

    /// Detach the surface; draws become no-ops until the next `set_surface`.
    pub fn clear(&mut self) {
        self.window = null_mut();
        self.configured = false;
        // The next surface must be painted from scratch; drop the frame so the
        // renderer keeps nothing stale (the renderer is invalidated separately
        // through `OhosWindow::invalidate_frame`).
        self.frame.clear();
        self.frame_size = (0, 0);
    }

    pub fn has_surface(&self) -> bool {
        !self.window.is_null()
    }

    /// Repaint the dirty regions and hand the frame to the compositor.
    pub fn present(
        &mut self,
        renderer: &SoftwareRenderer,
        width: u32,
        height: u32,
    ) -> PresentOutcome {
        if self.window.is_null() || width == 0 || height == 0 {
            return PresentOutcome::Skipped;
        }

        let pixels = width as usize * height as usize;
        if self.frame_size != (width, height) {
            self.frame.clear();
            // Two rows of margin: the renderer derives its extents from the
            // window's logical size, which can round a pixel above the surface
            // size. Extra rows are never copied out.
            self.frame.resize(
                pixels + 2 * width as usize,
                PremultipliedRgbaColor::default(),
            );
            self.frame_size = (width, height);
        }

        // Partial repaint: only the regions that changed since the previous
        // frame are rasterized into the persistent frame.
        let dirty = renderer.render(&mut self.frame, width as usize);
        let mut rects = [ffi::Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        }; MAX_FLUSH_RECTS];
        let mut rect_count = 0usize;
        let mut overflowed = false;
        for (position, size) in dirty.iter() {
            if rect_count == MAX_FLUSH_RECTS {
                overflowed = true;
                break;
            }
            rects[rect_count] = ffi::Rect {
                x: position.x,
                y: position.y,
                w: size.width,
                h: size.height,
            };
            rect_count += 1;
        }
        if overflowed {
            // More damage than the fixed array holds: declare the whole frame
            // damaged instead of leaving regions unflushed (stale pixels).
            rects[0] = ffi::Rect {
                x: 0,
                y: 0,
                w: width,
                h: height,
            };
            rect_count = 1;
        }
        if rect_count == 0 {
            // Nothing changed: the compositor keeps showing the previous frame.
            return PresentOutcome::Presented;
        }

        // SAFETY: the pointer is valid between Surface and SurfaceDestroyed and
        // the destroy path waits for our ack, which this thread only sends from
        // its event loop (never while present() is running).
        unsafe {
            if !self.configured {
                ffi::OH_NativeWindow_NativeWindowHandleOpt(
                    self.window,
                    ffi::SET_BUFFER_GEOMETRY,
                    width as i32,
                    height as i32,
                );
                ffi::OH_NativeWindow_NativeWindowHandleOpt(
                    self.window,
                    ffi::SET_FORMAT,
                    ffi::NATIVEBUFFER_PIXEL_FMT_RGBA_8888,
                );
                self.configured = true;
            }

            let mut buffer: *mut ffi::OHNativeWindowBuffer = null_mut();
            let mut fence_fd: i32 = -1;
            let rc = ffi::OH_NativeWindow_NativeWindowRequestBuffer(
                self.window,
                &mut buffer,
                &mut fence_fd,
            );
            if rc != 0 || buffer.is_null() {
                return PresentOutcome::Failed(rc);
            }

            let handle = ffi::OH_NativeWindow_GetBufferHandleFromNative(buffer);
            let Some(handle) = NonNull::new(handle) else {
                let _ = ffi::OH_NativeWindow_NativeWindowAbortBuffer(self.window, buffer);
                return PresentOutcome::Failed(-1);
            };
            let handle = &*handle.as_ptr();
            if handle.size <= 0 || handle.fd < 0 || handle.stride <= 0 {
                let _ = ffi::OH_NativeWindow_NativeWindowAbortBuffer(self.window, buffer);
                return PresentOutcome::Failed(-1);
            }

            let length = handle.size as usize;
            let base = ffi::mmap(
                null_mut(),
                length,
                ffi::PROT_READ | ffi::PROT_WRITE,
                ffi::MAP_SHARED,
                handle.fd,
                0,
            );
            if base == ffi::MAP_FAILED {
                let _ = ffi::OH_NativeWindow_NativeWindowAbortBuffer(self.window, buffer);
                return PresentOutcome::Failed(-1);
            }

            // Copy the whole persistent frame: queue buffers rotate, so a
            // partially written buffer could expose stale rows from other
            // frames. The rasterization saved by partial repaint is what
            // matters; an 11 MB copy is a few milliseconds.
            let source = self.frame.as_ptr().cast::<u8>();
            let row_bytes = width as usize * PIXEL_BYTES;
            let stride = handle.stride as usize;
            let destination = base.cast::<u8>();
            if stride == row_bytes {
                core::ptr::copy_nonoverlapping(source, destination, row_bytes * height as usize);
            } else {
                for row in 0..height as usize {
                    core::ptr::copy_nonoverlapping(
                        source.add(row * row_bytes),
                        destination.add(row * stride),
                        row_bytes,
                    );
                }
            }

            ffi::munmap(base, length);

            let region = ffi::Region {
                rects: rects.as_mut_ptr(),
                rectNumber: rect_count as i32,
            };
            let rc = ffi::OH_NativeWindow_NativeWindowFlushBuffer(self.window, buffer, -1, region);
            if rc == 0 {
                PresentOutcome::Presented
            } else {
                PresentOutcome::Failed(rc)
            }
        }
    }
}

impl Default for Presenter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_destroy_waits_for_ack() {
        let gate = SurfaceGate::new();
        gate.surface_up();
        assert!(!gate.is_destroying());
        assert!(gate.begin_destroy());
        assert!(gate.is_destroying());
        gate.acked();
        assert!(!gate.is_destroying());
        // A second destroy without a live surface does not wait.
        assert!(!gate.begin_destroy());
    }

    #[test]
    fn clear_resets_the_frame() {
        let mut presenter = Presenter::new();
        presenter.frame = vec![PremultipliedRgbaColor::default(); 4];
        presenter.frame_size = (2, 2);
        presenter.clear();
        assert!(presenter.frame.is_empty());
        assert_eq!(presenter.frame_size, (0, 0));
    }
}
