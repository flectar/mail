// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

/// Minimal clipboard abstraction implemented by the platform backends below.
pub trait ClipboardProvider {
    fn get_contents(
        &mut self,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync + 'static>>;
    fn set_contents(
        &mut self,
        contents: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>;
}

/// The Default, and the selection clipboard
pub type ClipboardPair = (Box<dyn ClipboardProvider>, Box<dyn ClipboardProvider>);

pub fn select_clipboard(
    pair: &mut ClipboardPair,
    clipboard: i_slint_core::platform::Clipboard,
) -> Option<&mut dyn ClipboardProvider> {
    match clipboard {
        i_slint_core::platform::Clipboard::DefaultClipboard => Some(pair.0.as_mut()),
        i_slint_core::platform::Clipboard::SelectionClipboard => Some(pair.1.as_mut()),
        _ => None,
    }
}

/// Silent no-op fallback for platforms or configurations without clipboard support.
struct SilentClipboardContext;
impl ClipboardProvider for SilentClipboardContext {
    fn get_contents(
        &mut self,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync + 'static>> {
        Ok(Default::default())
    }

    fn set_contents(
        &mut self,
        _: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        Ok(())
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
struct SystemClipboard(arboard::Clipboard);
#[cfg(any(target_os = "windows", target_os = "macos"))]
impl ClipboardProvider for SystemClipboard {
    fn get_contents(
        &mut self,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.0.get_text().map_err(Into::into)
    }

    fn set_contents(
        &mut self,
        contents: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.0.set_text(contents).map_err(Into::into)
    }
}

#[cfg(all(
    unix,
    not(any(target_vendor = "apple", target_os = "android", target_os = "emscripten")),
    any(feature = "x11", feature = "wayland")
))]
struct SelectableClipboard {
    clipboard: arboard::Clipboard,
    kind: arboard::LinuxClipboardKind,
}
#[cfg(all(
    unix,
    not(any(target_vendor = "apple", target_os = "android", target_os = "emscripten")),
    any(feature = "x11", feature = "wayland")
))]
impl ClipboardProvider for SelectableClipboard {
    fn get_contents(
        &mut self,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync + 'static>> {
        use arboard::GetExtLinux;
        self.clipboard.get().clipboard(self.kind).text().map_err(Into::into)
    }

    fn set_contents(
        &mut self,
        contents: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        use arboard::SetExtLinux;
        self.clipboard.set().clipboard(self.kind).text(contents).map_err(Into::into)
    }
}

#[cfg(all(
    unix,
    not(any(target_vendor = "apple", target_os = "android", target_os = "emscripten")),
    feature = "wayland"
))]
struct WaylandClipboard {
    clipboard: std::rc::Rc<smithay_clipboard::Clipboard>,
    /// The primary selection rather than the regular clipboard.
    primary: bool,
}
#[cfg(all(
    unix,
    not(any(target_vendor = "apple", target_os = "android", target_os = "emscripten")),
    feature = "wayland"
))]
impl ClipboardProvider for WaylandClipboard {
    fn get_contents(
        &mut self,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync + 'static>> {
        let text = if self.primary { self.clipboard.load_primary() } else { self.clipboard.load() };
        text.map_err(Into::into)
    }

    fn set_contents(
        &mut self,
        contents: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        if self.primary {
            self.clipboard.store_primary(contents);
        } else {
            self.clipboard.store(contents);
        }
        Ok(())
    }
}

pub fn create_clipboard(
    _display_handle: &winit::raw_window_handle::DisplayHandle<'_>,
    _event_loop_proxy: &winit::event_loop::EventLoopProxy<crate::SlintEvent>,
) -> ClipboardPair {
    core::cfg_select! {
        all(
            unix,
            not(any(target_vendor = "apple", target_os = "android", target_os = "emscripten")),
            any(feature = "x11", feature = "wayland")
        ) => {
            // On Wayland, use the event loop's own connection: the core wl_data_device protocol
            // works on every compositor and in sandboxes, unlike the data-control protocol that
            // a separate connection needs.
            #[cfg(feature = "wayland")]
            if let winit::raw_window_handle::RawDisplayHandle::Wayland(wayland) =
                _display_handle.as_raw()
            {
                // Safety: the display outlives the clipboard, stored next to the event loop.
                let display = wayland.display.as_ptr();
                // winit doesn't report files dragged onto a window on Wayland, so they come
                // through the clipboard's own data device.
                let drops = Box::new(WaylandDrops { event_loop_proxy: _event_loop_proxy.clone() });
                let clipboard =
                    unsafe { smithay_clipboard::Clipboard::with_drops(display, Some(drops)) };
                let clipboard = std::rc::Rc::new(clipboard);
                return (
                    Box::new(WaylandClipboard { clipboard: clipboard.clone(), primary: false }),
                    Box::new(WaylandClipboard { clipboard, primary: true }),
                );
            }
            // arboard's X11 clipboard.
            let make = |kind| -> Box<dyn ClipboardProvider> {
                arboard::Clipboard::new().map_or(
                    Box::new(SilentClipboardContext) as Box<dyn ClipboardProvider>,
                    |clipboard| Box::new(SelectableClipboard { clipboard, kind }),
                )
            };
            (
                make(arboard::LinuxClipboardKind::Clipboard),
                make(arboard::LinuxClipboardKind::Primary),
            )
        }
        target_os = "ios" => {
            // iOS exposes a single general pasteboard; the selection clipboard is a no-op.
            (Box::new(crate::ios::UiPasteboardClipboard), Box::new(SilentClipboardContext))
        }
        any(target_os = "windows", target_os = "macos") => {
            (
                arboard::Clipboard::new().map_or(
                    Box::new(SilentClipboardContext) as Box<dyn ClipboardProvider>,
                    |clipboard| Box::new(SystemClipboard(clipboard)) as Box<dyn ClipboardProvider>,
                ),
                Box::new(SilentClipboardContext),
            )
        }
        _ => {
            (Box::new(SilentClipboardContext), Box::new(SilentClipboardContext))
        }
    }
}

/// Files dragged onto a window on Wayland, which the event loop turns into
/// the window events winit gives for them on other platforms.
#[cfg(wayland_file_drops)]
struct WaylandDrops {
    event_loop_proxy: winit::event_loop::EventLoopProxy<crate::SlintEvent>,
}

#[cfg(wayland_file_drops)]
impl smithay_clipboard::DropHandler for WaylandDrops {
    fn event(&mut self, event: smithay_clipboard::DropEvent) {
        let event = crate::SlintEvent(crate::event_loop::CustomEvent::FileDrop(event));
        let _ = self.event_loop_proxy.send_event(event);
    }

    fn retrieve_files(&mut self, key: &str) -> Option<Vec<std::path::PathBuf>> {
        futures::executor::block_on(retrieve_portal_files(key))
    }
}

/// The files a document portal transfer names, as this app can read them,
/// which in a sandbox are its own copies of what the source named.
#[cfg(wayland_file_drops)]
async fn retrieve_portal_files(key: &str) -> Option<Vec<std::path::PathBuf>> {
    let connection = zbus::connection::Builder::session()
        .ok()?
        .method_timeout(std::time::Duration::from_secs(5))
        .build()
        .await
        .ok()?;
    let options = std::collections::HashMap::<&str, zbus::zvariant::Value<'_>>::new();
    let reply = connection
        .call_method(
            Some("org.freedesktop.portal.Documents"),
            "/org/freedesktop/portal/documents",
            Some("org.freedesktop.portal.FileTransfer"),
            "RetrieveFiles",
            &(key, options),
        )
        .await
        .ok()?;
    let files: Vec<String> = reply.body().deserialize().ok()?;
    Some(files.into_iter().map(std::path::PathBuf::from).collect())
}
