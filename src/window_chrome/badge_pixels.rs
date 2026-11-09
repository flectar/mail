//! The pixels of the unread count Windows shows over the taskbar button of a
//! copy without package identity. The digits come from the system's text
//! renderer as a coverage mask; this draws the disc and lays them on it.

/// The disc's colour, the red of Windows' own attention badges (#C42B1C),
/// as blue, green, red.
const DISC: [u8; 3] = [0x1c, 0x2b, 0xc4];

/// What the badge says for `count`: the number, or 99+ beyond what fits.
pub(crate) fn label(count: u32) -> String {
    if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

/// Whether `label` takes the badge's full width, as a pill, rather than a
/// disc: three characters don't fit a disc at a size anyone can read.
pub(crate) fn wide(label: &str) -> bool {
    label.chars().count() > 2
}

/// A square badge `size` pixels wide, top-down BGRA with straight alpha: a
/// red disc filling it, or a pill across it when `wide`, white where `text`
/// covers it (0 to 255 per pixel, row by row).
pub(crate) fn compose(size: usize, text: &[u8], wide: bool) -> Vec<u8> {
    debug_assert_eq!(text.len(), size * size);
    let middle = size as f32 / 2.0;
    // The pill keeps a quarter of the height free; the disc fills the square.
    let radius = if wide { middle * 0.75 } else { middle };
    // How far the pill's straight middle reaches either way from the centre.
    let reach = middle - radius;
    let mut pixels = vec![0_u8; size * size * 4];
    for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (index % size, index / size);
        let dx = ((x as f32 + 0.5 - middle).abs() - reach).max(0.0);
        let dy = y as f32 + 0.5 - middle;
        // How much of the pixel lies inside the shape's edge.
        let disc = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
        let cover = f32::from(text.get(index).copied().unwrap_or(0)) / 255.0;
        for (channel, colour) in pixel.iter_mut().zip(DISC) {
            *channel = (f32::from(colour) * (1.0 - cover) + 255.0 * cover).round() as u8;
        }
        pixel[3] = (disc * 255.0).round() as u8;
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::{compose, label, wide};

    #[test]
    fn counts_beyond_two_digits_read_99_plus() {
        assert_eq!(label(1), "1");
        assert_eq!(label(42), "42");
        assert_eq!(label(99), "99");
        assert_eq!(label(100), "99+");
        assert_eq!(label(u32::MAX), "99+");
        assert!(!wide("99"));
        assert!(wide("99+"));
    }

    #[test]
    fn a_wide_label_sits_on_a_pill_across_the_badge() {
        let size = 16;
        let pixels = compose(size, &vec![0; size * size], true);
        let alpha = |x: usize, y: usize| pixels[(y * size + x) * 4 + 3];
        // It reaches both sides at mid height, where a disc would too, but
        // keeps the top and bottom rows free, and its ends stay round.
        assert!(alpha(0, 8) > 200, "{}", alpha(0, 8));
        assert!(alpha(15, 8) > 200, "{}", alpha(15, 8));
        assert_eq!(alpha(1, 8), 255);
        assert_eq!(alpha(14, 8), 255);
        assert_eq!(alpha(8, 0), 0);
        assert_eq!(alpha(8, 15), 0);
        assert_eq!(alpha(8, 4), 255);
        assert!(alpha(0, 3) < 128, "{}", alpha(0, 3));
    }

    #[test]
    fn the_badge_is_a_red_disc_with_white_text() {
        let size = 16;
        let mut text = vec![0_u8; size * size];
        // A text pixel at the centre, half covered beside it.
        text[8 * size + 8] = 255;
        text[8 * size + 9] = 128;
        let pixels = compose(size, &text, false);
        let at = |x: usize, y: usize| &pixels[(y * size + x) * 4..][..4];

        // Corners lie outside the disc; its middle is opaque.
        assert_eq!(at(0, 0)[3], 0);
        assert_eq!(at(15, 15)[3], 0);
        assert_eq!(at(4, 8), &[0x1c, 0x2b, 0xc4, 255]);
        assert_eq!(at(8, 8), &[255, 255, 255, 255]);
        let blended = at(9, 8);
        assert!(blended[0] > 0x1c && blended[0] < 255, "{blended:?}");
        // The edge fades instead of stepping.
        let edge = at(0, 8)[3];
        assert!(edge > 0 && edge < 255, "{edge}");
    }
}
