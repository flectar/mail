//! Cross-thread events and hand-off types for the OHOS backend.
//!
//! Threading model: the OHOS main thread only runs the native XComponent
//! callbacks and NAPI entry points. Slint runs on a dedicated thread spawned by
//! [`crate::start`]. Everything crossing between the two travels through the
//! `std::sync::mpsc` channel carrying these events.

use core::ffi::c_void;
use slint::SharedString;

/// A raw `OHNativeWindow*` handed from a surface callback to the Slint thread.
///
/// Safety contract: the pointer is only *used* by the Slint thread between a
/// [`BackendEvent::Surface`] event and the matching
/// [`BackendEvent::SurfaceDestroyed`] event. `OnSurfaceDestroyed` blocks
/// (bounded) until the Slint thread acknowledges, so there is no window where
/// both threads touch the surface.
#[derive(Clone, Copy)]
pub struct SendPtr(pub *mut c_void);

// SAFETY: `SendPtr` carries no ownership; access is serialized by the surface
// hand-off protocol described above and enforced by `present::SurfaceGate`.
unsafe impl Send for SendPtr {}

impl core::fmt::Debug for SendPtr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "SendPtr({:p})", self.0)
    }
}

/// Touch phase mapped from `OH_NativeXComponent_TouchEventType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    Down,
    Move,
    Up,
    Cancel,
}

/// Event delivered from the OHOS main thread (or a Slint proxy) to the Slint
/// application thread.
pub enum BackendEvent {
    /// A surface was created or resized: `OHNativeWindow*` plus physical size.
    Surface {
        window: SendPtr,
        width: u32,
        height: u32,
    },
    /// The surface is going away. The Slint thread must stop using the pointer
    /// and then acknowledge (see `present::SurfaceGate`).
    SurfaceDestroyed,
    /// A touch event in physical surface coordinates.
    Touch { x: f32, y: f32, phase: TouchPhase },
    /// A keyboard event translated to Slint's key-text representation.
    Key {
        text: SharedString,
        down: bool,
        repeat: bool,
    },
    /// Closure to run on the Slint thread (`slint::invoke_from_event_loop`).
    Task(Box<dyn FnOnce() + Send + 'static>),
    /// Wake the event loop without further work (`request_redraw` / quit).
    Wake,
}

impl core::fmt::Debug for BackendEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BackendEvent::Surface {
                window,
                width,
                height,
            } => {
                write!(
                    f,
                    "Surface {{ window: {window:?}, width: {width}, height: {height} }}"
                )
            }
            BackendEvent::SurfaceDestroyed => write!(f, "SurfaceDestroyed"),
            BackendEvent::Touch { x, y, phase } => {
                write!(f, "Touch {{ x: {x}, y: {y}, phase: {phase:?} }}")
            }
            // The translated text is deliberately omitted: it may contain
            // characters typed into password fields.
            BackendEvent::Key { down, repeat, .. } => {
                write!(f, "Key {{ down: {down}, repeat: {repeat} }}")
            }
            BackendEvent::Task(_) => write!(f, "Task(..)"),
            BackendEvent::Wake => write!(f, "Wake"),
        }
    }
}
