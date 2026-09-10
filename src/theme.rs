//! Runtime helpers for the Slint theme editor.

use crate::{AppWindow, ThemeUtilities};
use slint::{Color, ComponentHandle};

pub(crate) fn register_theme_utilities(app: &AppWindow) {
    let utilities = app.global::<ThemeUtilities>();
    utilities.on_color_to_hex(|color| color_to_hex(color).into());
    utilities.on_valid_color(|value| parse_hex_color(value.as_str()).is_some());
    utilities.on_parse_color(|value, fallback| parse_hex_color(value.as_str()).unwrap_or(fallback));
}

pub(crate) fn color_to_hex(color: Color) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        color.red(),
        color.green(),
        color.blue()
    )
}

pub(crate) fn parse_hex_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let encoded = u32::from_str_radix(hex, 16).ok()?;
    Some(Color::from_rgb_u8(
        ((encoded >> 16) & 0xff) as u8,
        ((encoded >> 8) & 0xff) as u8,
        (encoded & 0xff) as u8,
    ))
}

pub(crate) fn stored_color(value: &str, fallback: &str) -> Color {
    parse_hex_color(value)
        .or_else(|| parse_hex_color(fallback))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_hex_colors_round_trip_in_canonical_form() {
        let color = parse_hex_color("#09aB7f").unwrap();
        assert_eq!(color_to_hex(color), "#09AB7F");
        assert!(parse_hex_color("09AB7F").is_none());
        assert!(parse_hex_color("#12345").is_none());
        assert!(parse_hex_color("#12345Z").is_none());
    }

    fn relative_luminance(color: Color) -> f32 {
        fn linear(channel: u8) -> f32 {
            let value = f32::from(channel) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linear(color.red())
            + 0.7152 * linear(color.green())
            + 0.0722 * linear(color.blue())
    }

    fn contrast(first: Color, second: Color) -> f32 {
        let first = relative_luminance(first);
        let second = relative_luminance(second);
        (first.max(second) + 0.05) / (first.min(second) + 0.05)
    }

    #[test]
    fn preset_core_colors_meet_wcag_aa_in_both_schemes() {
        let palettes = [
            ("#006B7A", "#EDF5F7", "#FBFEFE", "#173039"),
            ("#2F6F44", "#F1F5EF", "#FCFDFB", "#213026"),
            ("#78418A", "#F6F1F7", "#FDFBFE", "#332537"),
            ("#006B7A", "#0B1519", "#122026", "#E5F0F2"),
            ("#2F6F44", "#101711", "#182019", "#E9F0E6"),
            ("#78418A", "#171119", "#201722", "#F1E8F3"),
        ];

        for (primary, page, surface, text) in palettes {
            let primary = parse_hex_color(primary).unwrap();
            let page = parse_hex_color(page).unwrap();
            let surface = parse_hex_color(surface).unwrap();
            let text = parse_hex_color(text).unwrap();
            let faint_text = text.mix(&page, 0.68);
            assert!(contrast(Color::from_rgb_u8(255, 255, 255), primary) >= 4.5);
            assert!(contrast(text, page) >= 4.5);
            assert!(contrast(text, surface) >= 4.5);
            assert!(contrast(faint_text, page) >= 4.5);
            assert!(contrast(faint_text, surface) >= 4.5);
        }
    }
}
