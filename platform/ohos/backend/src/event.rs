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

/// Mouse action mapped from the XComponent mouse callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Move,
    Cancel,
}

/// Mouse button mapped to Slint's pointer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other,
}

impl From<MouseButton> for slint::platform::PointerEventButton {
    fn from(button: MouseButton) -> Self {
        match button {
            MouseButton::Left => Self::Left,
            MouseButton::Right => Self::Right,
            MouseButton::Middle => Self::Middle,
            MouseButton::Other => Self::Other,
        }
    }
}

/// Event delivered from the OHOS main thread (or a Slint proxy) to the Slint
/// application thread.
pub enum BackendEvent {
    /// A surface was created or resized: `OHNativeWindow*` plus physical size.
    /// `epoch` ties the event to the gate generation; an event queued across a
    /// teardown is stale and must be ignored.
    Surface {
        epoch: u64,
        window: SendPtr,
        width: u32,
        height: u32,
    },
    /// The surface is going away. The Slint thread must stop using the pointer
    /// and then acknowledge `epoch` (see `present::SurfaceGate`).
    SurfaceDestroyed { epoch: Option<u64> },
    /// The window became invisible (another window covers it, the app went to
    /// the background, or a system picker opened). Presenting into a hidden
    /// window can block on the compositor's buffer queue, so the render loop
    /// keeps the last frame until the surface is shown again.
    SurfaceHidden,
    /// The window is visible again; repaint from the retained frame.
    SurfaceShown,
    /// A touch event in physical surface coordinates.
    Touch { x: f32, y: f32, phase: TouchPhase },
    /// A mouse wheel / touchpad scroll in physical surface coordinates; the
    /// deltas are logical pixels, positive toward the start of the content.
    Scroll {
        x: f32,
        y: f32,
        delta_x: f32,
        delta_y: f32,
    },
    /// The system color scheme changed (dark = true).
    ColorScheme(bool),
    /// A mouse event in physical surface coordinates.
    Mouse {
        x: f32,
        y: f32,
        action: MouseAction,
        button: MouseButton,
    },
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
                epoch,
                window,
                width,
                height,
            } => {
                write!(
                    f,
                    "Surface {{ epoch: {epoch}, window: {window:?}, width: {width}, height: {height} }}"
                )
            }
            BackendEvent::SurfaceDestroyed { epoch } => {
                write!(f, "SurfaceDestroyed {{ epoch: {epoch:?} }}")
            }
            BackendEvent::SurfaceHidden => write!(f, "SurfaceHidden"),
            BackendEvent::SurfaceShown => write!(f, "SurfaceShown"),
            BackendEvent::Touch { x, y, phase } => {
                write!(f, "Touch {{ x: {x}, y: {y}, phase: {phase:?} }}")
            }
            BackendEvent::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                write!(
                    f,
                    "Scroll {{ x: {x}, y: {y}, delta_x: {delta_x}, delta_y: {delta_y} }}"
                )
            }
            // The translated text is deliberately omitted: it may contain
            // characters typed into password fields.
            BackendEvent::ColorScheme(dark) => write!(f, "ColorScheme({dark})"),
            BackendEvent::Mouse {
                x,
                y,
                action,
                button,
            } => write!(
                f,
                "Mouse {{ x: {x}, y: {y}, action: {action:?}, button: {button:?} }}"
            ),
            BackendEvent::Key { down, repeat, .. } => {
                write!(f, "Key {{ down: {down}, repeat: {repeat} }}")
            }
            BackendEvent::Task(_) => write!(f, "Task(..)"),
            BackendEvent::Wake => write!(f, "Wake"),
        }
    }
}
