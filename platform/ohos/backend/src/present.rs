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
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
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
    /// Identifies the current surface. A `Surface` event carries the epoch
    /// returned by [`Self::surface_up`]; `OnSurfaceDestroyed` invalidates it so
    /// an event that was queued behind the 500 ms destroy timeout cannot attach
    /// a window that the compositor has already released.
    epoch: AtomicU64,
}

impl SurfaceGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Gate::Down),
            cv: Condvar::new(),
            epoch: AtomicU64::new(0),
        })
    }

    /// Surface is ready (called from `OnSurfaceCreated/Changed`). Returns the
    /// epoch that the corresponding `Surface` event must carry.
    pub fn surface_up(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        *state = Gate::Up;
        self.cv.notify_all();
        epoch
    }

    /// Re-arm presentation when only the show callback arrives. This must not
    /// allocate a new epoch: a `Surface` event for the same surface may still
    /// be queued behind the show event.
    pub fn ensure_up(&self) {
        let mut state = self.state.lock().unwrap();
        if *state != Gate::Up {
            *state = Gate::Up;
            self.cv.notify_all();
        }
    }

    /// True while the surface is being torn down; the draw path must skip
    /// frames when this returns `true`.
    pub fn is_destroying(&self) -> bool {
        *self.state.lock().unwrap() == Gate::Destroying
    }

    /// Whether `epoch` identifies the live surface. Queued events from before
    /// a destroy are rejected by this check.
    pub fn is_surface_epoch(&self, epoch: u64) -> bool {
        let state = self.state.lock().unwrap();
        *state == Gate::Up && self.epoch.load(Ordering::Acquire) == epoch
    }

    /// Run `attach` while holding the gate, so `OnSurfaceDestroyed` cannot
    /// invalidate the window between the epoch check and the GPU attach.
    pub fn with_surface<R>(&self, epoch: u64, attach: impl FnOnce() -> R) -> Option<R> {
        let state = self.state.lock().unwrap();
        if *state != Gate::Up || self.epoch.load(Ordering::Acquire) != epoch {
            return None;
        }
        Some(attach())
    }

    /// Called from `OnSurfaceDestroyed` (OHOS main thread). Returns the epoch
    /// of the surface being torn down, or `None` when no live surface existed.
    /// The current epoch is invalidated even when no wait is needed.
    pub fn begin_destroy(&self) -> Option<u64> {
        let mut state = self.state.lock().unwrap();
        let destroyed = (*state == Gate::Up).then(|| self.epoch.load(Ordering::Acquire));
        self.epoch.fetch_add(1, Ordering::AcqRel);
        *state = if destroyed.is_some() {
            Gate::Destroying
        } else {
            Gate::Down
        };
        self.cv.notify_all();
        destroyed
    }

    /// Bounded wait for the Slint thread to acknowledge `epoch`'s destruction.
    /// A new surface (which increments the epoch) releases the wait too.
    pub fn wait_acked(&self, epoch: u64, timeout: Duration) {
        let mut state = self.state.lock().unwrap();
        let deadline = Instant::now() + timeout;
        while *state == Gate::Destroying
            && self.epoch.load(Ordering::Acquire) == epoch.wrapping_add(1)
        {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (guard, _) = self.cv.wait_timeout(state, deadline - now).unwrap();
            state = guard;
        }
    }

    /// Called by the Slint thread after it processed `SurfaceDestroyed`. A
    /// stale acknowledgement must not tear down a newer surface.
    pub fn acked(&self, epoch: u64) {
        let mut state = self.state.lock().unwrap();
        if *state == Gate::Destroying && self.epoch.load(Ordering::Acquire) == epoch.wrapping_add(1)
        {
            *state = Gate::Down;
            self.cv.notify_all();
        }
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
    configured_size: Option<(u32, u32)>,
    /// Persistent CPU frame: the software renderer repaints only dirty regions
    /// into it (`ReusedBuffer` semantics), so key repeats and cursor blinks
    /// never pay for a full-window rasterization.
    frame: Vec<PremultipliedRgbaColor>,
    frame_size: (u32, u32),
    /// Set by the XComponent surface show/hide callbacks. Guarded here as well
    /// as by the surface gate: a hidden window can stop the compositor from
    /// consuming buffers, which makes the buffer request block.
    visible: Arc<AtomicBool>,
}

impl Presenter {
    pub fn new(visible: Arc<AtomicBool>) -> Self {
        Self {
            window: null_mut(),
            configured_size: None,
            frame: Vec::new(),
            frame_size: (0, 0),
            visible,
        }
    }

    /// Attach (or re-attach) the surface pointer.
    pub fn set_surface(&mut self, window: *mut c_void) {
        if self.window != window {
            self.configured_size = None;
        }
        self.window = window.cast();
    }

    /// Detach the surface; draws become no-ops until the next `set_surface`.
    pub fn clear(&mut self) {
        self.window = null_mut();
        self.configured_size = None;
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
        if !self.visible.load(Ordering::Relaxed) {
            // Presenting into a hidden window can block the compositor buffer
            // queue; the retained frame is repainted when the surface returns.
            return PresentOutcome::Skipped;
        }

        // SAFETY: the pointer is valid between Surface and SurfaceDestroyed and
        // the destroy path waits for our ack, which this thread only sends from
        // its event loop (never while present() is running).
        unsafe {
            // The geometry must follow a window resize on the same native
            // window; a boolean "configured" flag would keep the first size.
            if self.configured_size != Some((width, height)) {
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
                self.configured_size = Some((width, height));
            }

            let mut buffer: *mut ffi::OHNativeWindowBuffer = null_mut();
            let mut fence_fd: i32 = -1;
            let rc = ffi::OH_NativeWindow_NativeWindowRequestBuffer(
                self.window,
                &mut buffer,
                &mut fence_fd,
            );
            // The acquire fence belongs to this call. Wait for the producer to
            // finish writing and close it; leaking it costs one fd per frame
            // and writing before it signals shows stale buffer contents.
            if fence_fd >= 0 {
                let mut poll_fd = ffi::PollFd {
                    fd: fence_fd,
                    events: ffi::POLLIN,
                    revents: 0,
                };
                let _ = ffi::poll(&mut poll_fd, 1, 1000);
                let _ = ffi::close(fence_fd);
            }
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

            let row_bytes = width as usize * PIXEL_BYTES;
            let stride = handle.stride as usize;
            let last_row_end = (height as usize - 1)
                .checked_mul(stride)
                .and_then(|offset| offset.checked_add(row_bytes));
            if stride < row_bytes || last_row_end.is_none_or(|end| end > handle.size as usize) {
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
                // A rejected flush leaves the buffer dequeued; return it so the
                // queue is not permanently one slot short.
                let _ = ffi::OH_NativeWindow_NativeWindowAbortBuffer(self.window, buffer);
                PresentOutcome::Failed(rc)
            }
        }
    }
}

impl Default for Presenter {
    fn default() -> Self {
        Self::new(Arc::new(AtomicBool::new(true)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_destroy_waits_for_ack() {
        let gate = SurfaceGate::new();
        let epoch = gate.surface_up();
        assert!(!gate.is_destroying());
        assert!(gate.is_surface_epoch(epoch));
        assert_eq!(gate.begin_destroy(), Some(epoch));
        assert!(gate.is_destroying());
        // The destroyed surface's queued event is no longer current.
        assert!(!gate.is_surface_epoch(epoch));
        gate.acked(epoch);
        assert!(!gate.is_destroying());
        // A second destroy without a live surface does not wait.
        assert_eq!(gate.begin_destroy(), None);
    }

    #[test]
    fn stale_ack_does_not_tear_down_a_new_surface() {
        let gate = SurfaceGate::new();
        let first = gate.surface_up();
        let destroyed = gate.begin_destroy().expect("first surface was live");
        assert_eq!(destroyed, first);
        let second = gate.surface_up();
        assert!(gate.is_surface_epoch(second));
        gate.acked(destroyed);
        assert!(!gate.is_destroying());
        assert!(gate.is_surface_epoch(second));
    }

    #[test]
    fn show_without_a_new_surface_preserves_the_epoch() {
        let gate = SurfaceGate::new();
        let epoch = gate.surface_up();
        gate.ensure_up();
        assert!(gate.is_surface_epoch(epoch));
    }

    #[test]
    fn with_surface_rejects_an_epoch_being_torn_down() {
        let gate = SurfaceGate::new();
        let epoch = gate.surface_up();
        assert_eq!(gate.with_surface(epoch, || 7), Some(7));
        let _ = gate.begin_destroy();
        assert_eq!(gate.with_surface(epoch, || 7), None);
    }

    #[test]
    fn clear_resets_the_frame() {
        let mut presenter = Presenter::new(Arc::new(AtomicBool::new(true)));
        presenter.frame = vec![PremultipliedRgbaColor::default(); 4];
        presenter.frame_size = (2, 2);
        presenter.clear();
        assert!(presenter.frame.is_empty());
        assert_eq!(presenter.frame_size, (0, 0));
    }
}
