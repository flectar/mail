//! The pixels of the unread count where the app draws it itself: over the
//! taskbar button of a Windows copy without package identity, and on the
//! tray icon. They follow the badges Windows 11 draws for packaged apps, a
//! disc in the accent colour with the number on it, widened into a pill for
//! longer numbers. The digits come from a text renderer as a coverage mask;
//! this sizes them, centres them and lays them on the badge.

#[cfg(all(linux_desktop, not(feature = "flatpak")))]
pub(crate) mod linux;
#[cfg(target_os = "windows")]
pub(crate) mod windows;

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

/// The height of the font for `characters` characters on a badge `height`
/// pixels tall, as Windows' own badge numbers have it: 11 sixteenths of the
/// badge for one character and 10 for more.
pub(crate) fn font_height(height: usize, characters: usize) -> usize {
    let sixteenths = if characters > 1 { 10 } else { 11 };
    (height * sixteenths + 8) / 16
}

/// How wide a badge `height` pixels tall is for text `extent` pixels wide:
/// wide enough that the text fills three quarters of it, from a disc up to
/// `max_width`.
pub(crate) fn badge_width(extent: f32, height: usize, max_width: usize) -> usize {
    ((extent * 4.0 / 3.0).ceil() as usize).clamp(height, max_width.max(height))
}

/// Whether text `extent` pixels wide fits a badge `width` pixels wide as it
/// is, in three quarters of it.
pub(crate) fn fits(extent: f32, width: usize) -> bool {
    extent * 4.0 <= width as f32 * 3.0
}

/// Whether text `extent` pixels wide fits a badge `width` pixels wide once
/// narrowed, in thirteen sixteenths of it. Text is narrowed rather than
/// shrunk, since 99+ on a 16 pixel disc would otherwise be 5 pixels tall.
pub(crate) fn fits_narrowed(extent: f32, width: usize) -> bool {
    extent * 16.0 <= width as f32 * 13.0
}

/// `text`, a coverage mask `width` by `height` pixels, moved by whole pixels
/// so its ink sits in the middle. The text renderer centres the line box,
/// whose room for descenders leaves digits low.
pub(crate) fn centre(width: usize, height: usize, text: &[u8]) -> Vec<u8> {
    debug_assert_eq!(text.len(), width * height);
    let inked = |index: usize| text[index] > 0;
    let rows: Vec<usize> = (0..height)
        .filter(|&y| (0..width).any(|x| inked(y * width + x)))
        .collect();
    let columns: Vec<usize> = (0..width)
        .filter(|&x| (0..height).any(|y| inked(y * width + x)))
        .collect();
    let (Some(top), Some(bottom), Some(left), Some(right)) =
        (rows.first(), rows.last(), columns.first(), columns.last())
    else {
        return text.to_vec();
    };
    // How far the ink's middle lies from the box's, in half pixels.
    let shift =
        |extent: usize, low: usize, high: usize| (extent as f32 - (low + high + 1) as f32) / 2.0;
    let (dx, dy) = (
        shift(width, *left, *right).round() as isize,
        shift(height, *top, *bottom).round() as isize,
    );
    let mut centred = vec![0; width * height];
    for y in 0..height {
        for x in 0..width {
            let (Some(to_x), Some(to_y)) = (x.checked_add_signed(dx), y.checked_add_signed(dy))
            else {
                continue;
            };
            if to_x < width && to_y < height {
                centred[to_y * width + to_x] = text[y * width + x];
            }
        }
    }
    centred
}

/// A badge in `accent`, with whichever of black and white text stands out
/// more on it, by the contrast ratio of the Web Content Accessibility
/// Guidelines. Windows 11's own badges pair their accents the same way.
#[cfg_attr(not(linux_desktop), allow(dead_code))]
pub(crate) fn on_accent(accent: [u8; 3]) -> Palette {
    // Relative luminance, from sRGB.
    let linear = |channel: u8| {
        let value = f32::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let [red, green, blue] = accent.map(linear);
    let luminance = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
    let on_white = 1.05 / (luminance + 0.05);
    let on_black = (luminance + 0.05) / 0.05;
    Palette {
        disc: accent,
        text: if on_white > on_black {
            [0xff; 3]
        } else {
            [0; 3]
        },
    }
}

/// What a tray icon's badge `height` pixels tall says for `count`. As tall
/// as Windows' own badges, 16 pixels, it says what they say. On smaller ones
/// two digits blur into each other, so 9+ stands for more; the tooltip has
/// the number.
pub(crate) fn tray_label(count: u32, height: usize) -> String {
    if height >= 16 {
        label(count)
    } else if count > 9 {
        "9+".to_owned()
    } else {
        count.to_string()
    }
}

/// How tall the count is on a tray icon `size` pixels square: two thirds of
/// it, the share Windows 11 gives its badge on a taskbar icon, 16 of 24
/// pixels, but at least 13. Two thirds of a 16 pixel tray icon, at 100 %
/// scale, would draw a digit 8 pixels tall and 9+ 7, which blur; 13 draws
/// them at 9 and 8.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn tray_badge_height(size: usize) -> usize {
    ((size * 2 + 1) / 3).max(13).min(size)
}

/// The count on an icon.
pub(crate) struct Badge {
    /// Its width and height in pixels: a disc for one digit, a pill for
    /// more, as wide as the icon at most.
    pub(crate) width: usize,
    pub(crate) height: usize,
    /// How much its text covers each of its pixels, row by row.
    pub(crate) text: Vec<u8>,
    pub(crate) palette: Palette,
    /// The room cleared on the icon around it, in pixels.
    pub(crate) gap: f32,
}

/// `icon`, `size` pixels square in RGBA with straight alpha, with `badge` in
/// its top right corner, where Windows 11 and KDE Plasma put the counts of
/// their own icons. On a transparent icon, a badge as wide as the icon is
/// the taskbar's disc.
pub(crate) fn draw(icon: &[u8], size: usize, badge: &Badge) -> Vec<u8> {
    debug_assert_eq!(icon.len(), size * size * 4);
    debug_assert!(badge.width <= size && badge.height <= size && badge.height <= badge.width);
    debug_assert_eq!(badge.text.len(), badge.width * badge.height);
    let left = size - badge.width;
    let radius = badge.height as f32 / 2.0;
    // The line along the pill's middle that its edge keeps `radius` from.
    let (start, end) = (left as f32 + radius, size as f32 - radius);
    let mut pixels = icon.to_vec();
    for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (index % size, index / size);
        let (centre_x, centre_y) = (x as f32 + 0.5, y as f32 + 0.5);
        let (dx, dy) = (centre_x - centre_x.clamp(start, end), centre_y - radius);
        // How far the pixel's middle lies outside the badge's edge.
        let outside = (dx * dx + dy * dy).sqrt() - radius;
        let badge_alpha = (0.5 - outside).clamp(0.0, 1.0);
        let kept = if badge.gap > 0.0 {
            (outside - badge.gap + 0.5).clamp(0.0, 1.0)
        } else {
            1.0
        };
        if badge_alpha == 0.0 && kept == 1.0 {
            continue;
        }
        let ink = if x >= left && y < badge.height {
            f32::from(badge.text[y * badge.width + x - left]) / 255.0
        } else {
            0.0
        };
        let icon_alpha = f32::from(pixel[3]) / 255.0 * kept;
        // The badge over what is left of the icon, each weighted by its share
        // of the result: all the badge's where nothing is under it.
        let alpha = badge_alpha + icon_alpha * (1.0 - badge_alpha);
        let (badge_share, icon_share) = if alpha > 0.0 {
            (
                badge_alpha / alpha,
                icon_alpha * (1.0 - badge_alpha) / alpha,
            )
        } else {
            (0.0, 0.0)
        };
        for (channel, (disc, text)) in pixel
            .iter_mut()
            .zip(badge.palette.disc.iter().zip(badge.palette.text.iter()))
        {
            let colour = f32::from(*disc) * (1.0 - ink) + f32::from(*text) * ink;
            *channel = (colour * badge_share + f32::from(*channel) * icon_share)
                .round()
                .min(255.0) as u8;
        }
        pixel[3] = (alpha * 255.0).round() as u8;
    }
    pixels
}

/// `pixels`, `from` pixels square in RGBA with straight alpha, made `to`
/// pixels square by averaging the area each new pixel covers. Colours are
/// weighted by their alpha, so transparent edges don't darken.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn scale_down(pixels: &[u8], from: usize, to: usize) -> Vec<u8> {
    debug_assert_eq!(pixels.len(), from * from * 4);
    debug_assert!(to > 0 && to <= from);
    if to == from {
        return pixels.to_vec();
    }
    let ratio = from as f32 / to as f32;
    // The old pixels under each new row or column, with how much of each.
    let spans: Vec<Vec<(usize, f32)>> = (0..to)
        .map(|new| {
            let (first, last) = (new as f32 * ratio, (new + 1) as f32 * ratio);
            (first.floor() as usize..(last.ceil() as usize).min(from))
                .map(|old| (old, ((old + 1) as f32).min(last) - (old as f32).max(first)))
                .filter(|&(_, share)| share > 0.0)
                .collect()
        })
        .collect();
    let mut scaled = vec![0_u8; to * to * 4];
    for (pixel, index) in scaled.as_chunks_mut::<4>().0.iter_mut().zip(0..) {
        let (rows, columns) = (&spans[index / to], &spans[index % to]);
        let (mut colour, mut alpha, mut area) = ([0.0_f32; 3], 0.0_f32, 0.0_f32);
        for &(y, height) in rows {
            for &(x, width) in columns {
                let old = &pixels[(y * from + x) * 4..][..4];
                let weight = width * height;
                let opacity = f32::from(old[3]) / 255.0 * weight;
                for (sum, value) in colour.iter_mut().zip(old) {
                    *sum += f32::from(*value) * opacity;
                }
                alpha += opacity;
                area += weight;
            }
        }
        if alpha > 0.0 {
            for (channel, sum) in pixel.iter_mut().zip(colour) {
                *channel = (sum / alpha).round().min(255.0) as u8;
            }
        }
        pixel[3] = (alpha / area * 255.0).round().min(255.0) as u8;
    }
    scaled
}

#[cfg(test)]
mod tests {
    use super::{
        Badge, Palette, badge_width, centre, draw, fits, fits_narrowed, font_height, label,
        on_accent, scale_down, tray_badge_height, tray_label,
    };

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
    fn the_taskbar_badge_is_a_disc_with_the_text_in_its_colour() {
        let size = 16;
        let mut text = vec![0_u8; size * size];
        // A text pixel at the centre, half covered beside it.
        text[8 * size + 8] = 255;
        text[8 * size + 9] = 128;
        let badge = Badge {
            width: size,
            height: size,
            text,
            palette: DARK_TASKBAR,
            gap: 0.0,
        };
        let pixels = draw(&vec![0; size * size * 4], size, &badge);
        let at = |x: usize, y: usize| &pixels[(y * size + x) * 4..][..4];

        // Corners lie outside the disc; its middle is opaque.
        assert_eq!(at(0, 0)[3], 0);
        assert_eq!(at(15, 15)[3], 0);
        assert_eq!(at(4, 8), &[0x4c, 0xc2, 0xff, 255]);
        assert_eq!(at(8, 8), &[0, 0, 0, 255]);
        let blended = at(9, 8);
        assert!(blended[0] > 0 && blended[0] < 0x4c, "{blended:?}");
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
        let centred = centre(size, size, &text);
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
        assert_eq!(
            centre(size, size, &vec![0; size * size]),
            vec![0; size * size]
        );
    }

    #[test]
    fn text_centres_in_a_wide_badge() {
        let (width, height) = (16, 11);
        let mut text = vec![0_u8; width * height];
        // Two columns of ink at the left edge, rows 2 to 8.
        for y in 2..9 {
            text[y * width] = 255;
            text[y * width + 1] = 255;
        }
        let centred = centre(width, height, &text);
        let inked: Vec<(usize, usize)> = (0..width * height)
            .filter(|&index| centred[index] > 0)
            .map(|index| (index % width, index / width))
            .collect();
        assert_eq!(inked.first(), Some(&(7, 2)));
        assert_eq!(inked.last(), Some(&(8, 8)));
    }

    #[test]
    fn accents_get_the_text_colour_that_stands_out_more() {
        // Windows 11's own pairs: its light accent with black, its dark one
        // with white.
        assert_eq!(on_accent([0x4c, 0xc2, 0xff]).text, [0; 3]);
        assert_eq!(on_accent([0x00, 0x67, 0xc0]).text, [0xff; 3]);
        // GNOME's default blue and KDE Plasma's Breeze blue.
        assert_eq!(on_accent([0x35, 0x84, 0xe4]).text, [0; 3]);
        assert_eq!(on_accent([0x3d, 0xae, 0xe9]).text, [0; 3]);
        // Purple and deep red read better in white.
        assert_eq!(on_accent([0x91, 0x41, 0xac]).text, [0xff; 3]);
        assert_eq!(on_accent([0xc0, 0x1c, 0x28]).text, [0xff; 3]);
        assert_eq!(on_accent([0x35, 0x84, 0xe4]).disc, [0x35, 0x84, 0xe4]);
    }

    const RED: [u8; 4] = [0xd0, 0x30, 0x20, 255];
    const LIGHT_TASKBAR: Palette = Palette {
        disc: [0x00, 0x67, 0xc0],
        text: [0xff, 0xff, 0xff],
    };

    fn opaque_icon(size: usize) -> Vec<u8> {
        RED.repeat(size * size)
    }

    #[test]
    fn text_sizes_follow_windows_own_badges() {
        // One digit at 11 sixteenths of the badge, more at 10.
        assert_eq!(font_height(16, 1), 11);
        assert_eq!(font_height(16, 3), 10);
        assert_eq!(font_height(13, 1), 9);
        assert_eq!(font_height(13, 2), 8);
        // The text fills three quarters of the badge, which grows from a
        // disc to a pill and stops at the widest it may be.
        assert_eq!(badge_width(7.0, 13, 16), 13);
        assert_eq!(badge_width(11.0, 13, 16), 15);
        assert_eq!(badge_width(20.0, 13, 16), 16);
        assert!(fits(9.0, 12) && !fits(9.5, 12));
        assert!(fits_narrowed(13.0, 16) && !fits_narrowed(13.5, 16));
    }

    #[test]
    fn small_tray_badges_count_to_9() {
        // 100 % scale: a 16 pixel icon with a 13 pixel badge.
        let small: Vec<String> = [1, 9, 10, 42, 120]
            .into_iter()
            .map(|count| tray_label(count, 13))
            .collect();
        assert_eq!(small, ["1", "9", "9+", "9+", "9+"]);
        // From 150 %, as big as Windows' own badges.
        let large: Vec<String> = [1, 9, 10, 99, 100]
            .into_iter()
            .map(|count| tray_label(count, 16))
            .collect();
        assert_eq!(large, ["1", "9", "10", "99", "99+"]);
    }

    #[test]
    fn the_tray_badge_is_two_thirds_of_the_icon_but_at_least_13_pixels() {
        // The tray's sizes at 100, 125, 150 and 200 % scale, and on Linux.
        let heights: Vec<usize> = [16, 20, 24, 32, 64]
            .into_iter()
            .map(tray_badge_height)
            .collect();
        assert_eq!(heights, [13, 13, 16, 21, 43]);
        assert_eq!(tray_badge_height(10), 10);
    }

    #[test]
    fn one_digit_sits_on_a_disc_at_the_top_right() {
        let size = 16;
        let mut text = vec![0_u8; 11 * 11];
        text[5 * 11 + 5] = 255;
        let badge = Badge {
            width: 11,
            height: 11,
            text,
            palette: LIGHT_TASKBAR,
            gap: 0.0,
        };
        let pixels = draw(&opaque_icon(size), size, &badge);
        let at = |x: usize, y: usize| &pixels[(y * size + x) * 4..][..4];

        // The icon stays where the disc doesn't reach: below it, left of it
        // and in the box's corners.
        assert_eq!(at(0, 15), RED);
        assert_eq!(at(4, 2), RED);
        assert_eq!(at(5, 0), RED);
        assert_eq!(at(15, 10), RED);
        // The disc's middle, and its text.
        assert_eq!(at(13, 5), &[0x00, 0x67, 0xc0, 255]);
        assert_eq!(at(10, 5), &[0xff, 0xff, 0xff, 255]);
        // Its edge blends into the icon, which stays opaque underneath.
        let edge = at(14, 1);
        assert_eq!(edge[3], 255);
        assert!(edge[0] > 0 && edge[0] < 0xd0, "{edge:?}");
    }

    #[test]
    fn longer_counts_widen_the_badge_into_a_pill() {
        let size = 16;
        let badge = Badge {
            width: 16,
            height: 11,
            text: vec![0; 16 * 11],
            palette: LIGHT_TASKBAR,
            gap: 0.0,
        };
        let pixels = draw(&opaque_icon(size), size, &badge);
        let at = |x: usize, y: usize| &pixels[(y * size + x) * 4..][..4];
        let disc = [0x00, 0x67, 0xc0, 255];
        // Straight along the top and bottom, round at both ends.
        assert_eq!(at(8, 0), disc);
        assert_eq!(at(8, 10), disc);
        assert_eq!(at(0, 5), disc);
        assert_eq!(at(15, 5), disc);
        assert_eq!(at(0, 0), RED);
        assert_eq!(at(15, 10), RED);
        assert_eq!(at(8, 11), RED);
    }

    #[test]
    fn a_gap_clears_the_icon_around_the_badge() {
        let size = 16;
        let badge = |gap| Badge {
            width: 11,
            height: 11,
            text: vec![0; 11 * 11],
            palette: LIGHT_TASKBAR,
            gap,
        };
        let icon = opaque_icon(size);
        let plain = draw(&icon, size, &badge(0.0));
        let cut = draw(&icon, size, &badge(1.0));
        let alpha = |pixels: &[u8], x: usize, y: usize| pixels[(y * size + x) * 4 + 3];
        // Just past the disc's edge the icon is gone, showing what is behind.
        assert_eq!(alpha(&plain, 13, 0), 255);
        assert!(alpha(&cut, 13, 0) < 128, "{}", alpha(&cut, 13, 0));
        assert_eq!(&cut[(13 * 4)..][..3], &[0x00, 0x67, 0xc0]);
        assert_eq!(alpha(&cut, 4, 5), 0);
        // Further out it stays.
        assert_eq!(&cut[(15 * size + 2) * 4..][..4], RED);
        assert_eq!(&cut[(12 * size + 10) * 4..][..4], RED);
    }

    #[test]
    fn scaling_down_averages_without_darkening_edges() {
        // White and transparent black in a 2 by 2 checkerboard of 2 pixel
        // squares, in a 4 pixel image.
        let white = [255_u8, 255, 255, 255];
        let clear = [0_u8, 0, 0, 0];
        let mut pixels = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                pixels.extend_from_slice(if (x / 2 + y / 2) % 2 == 0 {
                    &white
                } else {
                    &clear
                });
            }
        }
        assert_eq!(scale_down(&pixels, 4, 4), pixels);
        assert_eq!(
            scale_down(&pixels, 4, 2),
            [white, clear, clear, white].concat()
        );
        // A pixel straddling both halves is half covered and still white.
        let one = scale_down(&pixels, 4, 1);
        assert_eq!(one, [255, 255, 255, 128]);
        // Shares of uneven sizes still add up: one colour stays that colour.
        let red = RED.repeat(9);
        assert_eq!(scale_down(&red, 3, 2), RED.repeat(4));
    }
}
