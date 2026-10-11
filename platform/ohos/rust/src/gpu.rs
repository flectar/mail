//! Native-window ownership for the optional OHOS Vulkan/Skia29 renderer.
//! CPU rendering stays the default; acquiring a lease does not select a GPU.

use std::{ffi::c_void, marker::PhantomData, ptr::NonNull, rc::Rc};

unsafe extern "C" {
    fn feihong_gpu_acquire_window(generation: u64) -> *mut c_void;
    fn feihong_gpu_release_window(window: *mut c_void);
    fn feihong_gpu_surface_current(generation: u64) -> bool;
}

/// Owns one OH_NativeWindow_NativeObjectReference on the UI owner thread.
/// A Vulkan surface must be destroyed before its lease is dropped. Place the
/// lease after the surface in its owner struct (Rust drops fields in order).
pub(crate) struct NativeWindowLease {
    window: NonNull<c_void>,
    generation: u64,
    _ui_thread: PhantomData<Rc<()>>,
}

impl NativeWindowLease {
    pub(crate) fn acquire(generation: u64) -> Option<Self> {
        // Native code atomically validates generation/foreground and retains
        // the object under the same mutex used by surface destruction.
        NonNull::new(unsafe { feihong_gpu_acquire_window(generation) }).map(|window| Self {
            window,
            generation,
            _ui_thread: PhantomData,
        })
    }

    pub(crate) fn as_ptr(&self) -> *mut c_void {
        self.window.as_ptr()
    }

    /// Check before beginning a frame and again before presentation. Holding
    /// a lease keeps memory alive, but does not make a destroyed surface valid.
    pub(crate) fn is_current(&self) -> bool {
        unsafe { feihong_gpu_surface_current(self.generation) }
    }
}

impl Drop for NativeWindowLease {
    fn drop(&mut self) {
        unsafe { feihong_gpu_release_window(self.window.as_ptr()) }
    }
}
