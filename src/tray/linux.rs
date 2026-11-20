//! The count on the Linux tray icon. StatusNotifierItem hosts scale the one
//! picture the tray sends to their icon size: 22 pixels in KDE Plasma's tray
//! by default, 16 with its small icons and in GNOME's top bar. So the count
//! goes on the full tray picture, sized to read at 16 pixels. The disc takes
//! the desktop's accent colour from the settings portal, as the Windows one
//! takes the taskbar's, and a gap keeps it apart from the icon, as KDE
//! Plasma's task manager draws counts.

use crate::badge_pixels::{self, Badge};
use ashpd::desktop::{Color, settings::Settings};
use futures_util::StreamExt;
use std::cell::Cell;
use std::time::Duration;

/// The accent of desktops without one: GNOME's default blue.
const DEFAULT_ACCENT: [u8; 3] = [0x35, 0x84, 0xe4];
/// How long the portal may take to answer before the default stays.
const PORTAL_TIMEOUT: Duration = Duration::from_secs(5);
/// How tall the badge is where trays show the picture smallest, 16 pixels:
/// three quarters of it, as on the picture.
const SMALLEST_BADGE: usize = 12;

thread_local! {
    /// The desktop's accent colour, once the portal told it.
    static ACCENT: Cell<Option<[u8; 3]>> = const { Cell::new(None) };
}

/// `icon` with `unread` on it, or nothing to change for none.
pub(super) fn picture(icon: &slint::Image, unread: u32) -> Option<slint::Image> {
    if unread == 0 {
        return None;
    }
    let source = icon.to_rgba8()?;
    if source.height() != source.width() {
        return None;
    }
    let size = usize::try_from(source.width()).ok()?;
    // Three quarters of the picture: shown at 16 pixels a digit still reads,
    // and at 22 the icon still shows around it.
    let height = size * 3 / 4;
    // What the badge says where it is smallest: 9+ beyond nine.
    let label = badge_pixels::tray_label(unread, SMALLEST_BADGE);
    let (width, text) = badge_pixels::linux::text_mask(&label, height, size)?;
    let badge = Badge {
        width,
        height,
        text,
        palette: badge_pixels::on_accent(ACCENT.get().unwrap_or(DEFAULT_ACCENT)),
        gap: (size as f32 / 32.0).max(1.0),
    };
    let pixels = badge_pixels::draw(source.as_bytes(), size, &badge);
    let size = u32::try_from(size).ok()?;
    let pixels =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&pixels, size, size);
    Some(slint::Image::from_rgba8(pixels))
}

/// Keep the count in the desktop's accent colour, now and as it changes.
pub(super) fn follow_accent(runtime: &tokio::runtime::Handle) {
    runtime.spawn(async {
        let Ok(Ok(settings)) = tokio::time::timeout(PORTAL_TIMEOUT, Settings::new()).await else {
            return;
        };
        if let Ok(Ok(accent)) = tokio::time::timeout(PORTAL_TIMEOUT, settings.accent_color()).await
        {
            accent_changed(&accent);
        }
        let Ok(Ok(mut changes)) =
            tokio::time::timeout(PORTAL_TIMEOUT, settings.receive_accent_color_changed()).await
        else {
            return;
        };
        while let Some(accent) = changes.next().await {
            accent_changed(&accent);
        }
    });
}

fn accent_changed(accent: &Color) {
    let accent = rgb(accent.red(), accent.green(), accent.blue());
    let _ = slint::invoke_from_event_loop(move || {
        ACCENT.set(accent);
        super::refresh();
    });
}

/// The accent as bytes. The portal gives values outside 0 to 1 for a
/// desktop without one.
fn rgb(red: f64, green: f64, blue: f64) -> Option<[u8; 3]> {
    let byte = |value: f64| {
        (0.0..=1.0)
            .contains(&value)
            .then(|| (value * 255.0).round() as u8)
    };
    Some([byte(red)?, byte(green)?, byte(blue)?])
}

#[cfg(test)]
mod tests {
    use super::rgb;

    #[test]
    fn colours_outside_the_range_mean_no_accent() {
        assert_eq!(rgb(0.2, 0.5, 1.0), Some([51, 128, 255]));
        assert_eq!(rgb(-1.0, -1.0, -1.0), None);
        assert_eq!(rgb(0.5, 1.5, 0.5), None);
    }
}
