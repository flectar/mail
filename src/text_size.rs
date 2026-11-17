//! The interface text size (#231). Every rem size in the interface and the
//! standard widgets follow the window's default font size, so the chosen step
//! multiplies the system's own text size there. At the default step the
//! window leaves that size unset and keeps following the system as it
//! changes. Message zoom and the composer's text size stay separate.

use crate::{AppWindow, InterfaceText};
use flectar_mail_core::models::InterfaceTextSize;
use slint::ComponentHandle;

/// Show the interface at `size`.
pub(crate) fn apply(app: &AppWindow, size: InterfaceTextSize) {
    app.global::<InterfaceText>().set_size(size.as_str().into());
    // Unset, the window resolves rem to the system's text size, which is the
    // size to scale.
    app.set_interface_text_size(0.0);
    let factor = size.factor();
    if factor != 1.0 {
        let system = app.get_base_text_size();
        app.set_interface_text_size(system * factor);
    }
}

/// Scale the system's text size again. It may have changed while the app was
/// away, and a chosen step holds the size it scaled. UI thread only.
#[cfg(mobile)]
pub(crate) fn refresh(app: &AppWindow) {
    let size =
        InterfaceTextSize::parse(&app.global::<InterfaceText>().get_size()).unwrap_or_default();
    if size != InterfaceTextSize::Default {
        apply(app, size);
    }
}
