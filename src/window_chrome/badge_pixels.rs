//! The pixels of the unread count Windows shows over the taskbar button of a
//! copy without package identity, drawn the way Windows 11 draws the badges
//! of packaged apps: a disc in the accent colour with the number on it. The
//! digits come from the system's text renderer as a coverage mask; this
//! centres them and lays them on the disc.

/// The badge's colours, as red, green, blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    pub(crate) disc: [u8; 3],
    pub(crate) text: [u8; 3],
}

/// What the badge says for `count`: the number, or 99+ beyond what fits, as
/// Windows' own badges do.
pub(crate) fn label(count: u32) -> String {
    if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

/// `text`, a `size` pixel square coverage mask, moved by whole pixels so its
/// ink sits in the middle. The text renderer centres the line box, whose
/// room for descenders leaves digits low.
pub(crate) fn centre(size: usize, text: &[u8]) -> Vec<u8> {
    debug_assert_eq!(text.len(), size * size);
    let inked = |index: usize| text[index] > 0;
    let rows: Vec<usize> = (0..size)
        .filter(|&y| (0..size).any(|x| inked(y * size + x)))
        .collect();
    let columns: Vec<usize> = (0..size)
        .filter(|&x| (0..size).any(|y| inked(y * size + x)))
        .collect();
    let (Some(top), Some(bottom), Some(left), Some(right)) =
        (rows.first(), rows.last(), columns.first(), columns.last())
    else {
        return text.to_vec();
    };
    // How far the ink's middle lies from the square's, in half pixels.
    let shift = |low: usize, high: usize| (size as f32 - (low + high + 1) as f32) / 2.0;
    let (dx, dy) = (
        shift(*left, *right).round() as isize,
        shift(*top, *bottom).round() as isize,
    );
    let mut centred = vec![0; size * size];
    for y in 0..size {
        for x in 0..size {
            let (Some(to_x), Some(to_y)) = (x.checked_add_signed(dx), y.checked_add_signed(dy))
            else {
                continue;
            };
            if to_x < size && to_y < size {
                centred[to_y * size + to_x] = text[y * size + x];
            }
        }
    }
    centred
}

/// A square badge `size` pixels wide, top-down BGRA with straight alpha: a
/// disc filling it in the palette's disc colour, in its text colour where
/// `text` covers it (0 to 255 per pixel, row by row).
pub(crate) fn compose(size: usize, text: &[u8], palette: Palette) -> Vec<u8> {
    debug_assert_eq!(text.len(), size * size);
    let radius = size as f32 / 2.0;
    let mut pixels = vec![0_u8; size * size * 4];
    for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (index % size, index / size);
        let dx = x as f32 + 0.5 - radius;
        let dy = y as f32 + 0.5 - radius;
        // How much of the pixel lies inside the disc's edge.
        let disc = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
        let cover = f32::from(text.get(index).copied().unwrap_or(0)) / 255.0;
        // Blue, green, red: the palette's colours in reverse.
        for (channel, (disc, text)) in pixel
            .iter_mut()
            .zip(palette.disc.iter().rev().zip(palette.text.iter().rev()))
        {
            *channel = (f32::from(*disc) * (1.0 - cover) + f32::from(*text) * cover).round() as u8;
        }
        pixel[3] = (disc * 255.0).round() as u8;
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::{Palette, centre, compose, label};

    const DARK_TASKBAR: Palette = Palette {
        disc: [0x4c, 0xc2, 0xff],
        text: [0, 0, 0],
    };

    #[test]
    fn counts_beyond_two_digits_read_99_plus() {
        assert_eq!(label(1), "1");
        assert_eq!(label(42), "42");
        assert_eq!(label(99), "99");
        assert_eq!(label(100), "99+");
        assert_eq!(label(u32::MAX), "99+");
    }

    #[test]
    fn the_badge_is_a_disc_with_the_text_in_its_colour() {
        let size = 16;
        let mut text = vec![0_u8; size * size];
        // A text pixel at the centre, half covered beside it.
        text[8 * size + 8] = 255;
        text[8 * size + 9] = 128;
        let pixels = compose(size, &text, DARK_TASKBAR);
        let at = |x: usize, y: usize| &pixels[(y * size + x) * 4..][..4];

        // Corners lie outside the disc; its middle is opaque, blue first.
        assert_eq!(at(0, 0)[3], 0);
        assert_eq!(at(15, 15)[3], 0);
        assert_eq!(at(4, 8), &[0xff, 0xc2, 0x4c, 255]);
        assert_eq!(at(8, 8), &[0, 0, 0, 255]);
        let blended = at(9, 8);
        assert!(blended[0] > 0 && blended[0] < 0xff, "{blended:?}");
        // The edge fades instead of stepping, all the way round.
        for edge in [at(0, 8)[3], at(8, 0)[3], at(15, 8)[3], at(8, 15)[3]] {
            assert!(edge > 0 && edge < 255, "{edge}");
        }
    }

    #[test]
    fn text_moves_to_the_middle_by_whole_pixels() {
        let size = 16;
        let mut text = vec![0_u8; size * size];
        // A 4 by 6 block of ink in rows 6 to 11, columns 3 to 6: low and left.
        for y in 6..12 {
            for x in 3..7 {
                text[y * size + x] = 200;
            }
        }
        text[6 * size + 3] = 90;
        let centred = centre(size, &text);
        let inked: Vec<(usize, usize)> = (0..size * size)
            .filter(|&index| centred[index] > 0)
            .map(|index| (index % size, index / size))
            .collect();
        assert_eq!(inked.len(), 24);
        assert_eq!(inked.first(), Some(&(6, 5)));
        assert_eq!(inked.last(), Some(&(9, 10)));
        // Coverage moves with its pixel.
        assert_eq!(centred[5 * size + 6], 90);
        // Nothing to centre stays as it was.
        assert_eq!(centre(size, &vec![0; size * size]), vec![0; size * size]);
    }
}
