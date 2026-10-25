//! Soft-keyboard (IME) integration.
//!
//! Slint asks the window adapter to enable/disable the input method whenever
//! an editable field gains or loses focus (`InputMethodRequest`). The shell
//! registers ArkTS hooks that attach the OHOS input-method controller and
//! show/hide the system keyboard; committed text, deletions and the action
//! key come back through the exported NAPI functions and are dispatched to
//! Slint as ordinary key events.

use std::sync::OnceLock;

use crate::event::BackendEvent;
use crate::xcomponent;

/// ArkTS hooks installed by the shell (`show` attaches the controller and
/// raises the keyboard, `hide` lowers it).
struct Hooks {
    show: fn(bool),
    hide: fn(),
    update_cursor: fn(f32, f32, f32, f32),
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();

/// Install the platform hooks. First registration wins; the shell calls this
/// once during startup, before any text field can gain focus.
pub fn set_hooks(show: fn(bool), hide: fn(), update_cursor: fn(f32, f32, f32, f32)) {
    let _ = HOOKS.set(Hooks {
        show,
        hide,
        update_cursor,
    });
}

/// Called by the window adapter when Slint enables or disables the input
/// method (i.e. a text field gained or lost focus).
pub(crate) fn request(enable: bool, password: bool) {
    let Some(hooks) = HOOKS.get() else {
        return;
    };
    if enable {
        (hooks.show)(password);
    } else {
        (hooks.hide)();
    }
}

/// Forward the editable field's cursor rectangle to the system keyboard so
/// composing text and the candidate window follow the caret.
pub(crate) fn update_cursor(left: f32, top: f32, width: f32, height: f32) {
    if let Some(hooks) = HOOKS.get() {
        (hooks.update_cursor)(left, top, width, height);
    }
}

/// Committed text from the IME. Multi-character commits (autocomplete) arrive
/// as one event; Slint inserts the whole string.
pub fn commit_text(text: &str) {
    if text.is_empty() {
        return;
    }
    xcomponent::send(BackendEvent::Key {
        text: text.into(),
        down: true,
        repeat: false,
    });
}

/// Backspace from the soft keyboard. `count` mirrors the IME's `deleteLeft`
/// length (some keyboards delete a whole word).
pub fn delete_left(count: usize) {
    let text: slint::SharedString = slint::platform::Key::Backspace.into();
    for _ in 0..count.max(1) {
        xcomponent::send(BackendEvent::Key {
            text: text.clone(),
            down: true,
            repeat: false,
        });
    }
}

/// The keyboard's action key (e.g. Enter/Newline).
pub fn function_key_enter() {
    // Send the complete press/release pair, like a physical Enter key.
    let text: slint::SharedString = slint::platform::Key::Return.into();
    for down in [true, false] {
        xcomponent::send(BackendEvent::Key {
            text: text.clone(),
            down,
            repeat: false,
        });
    }
}
