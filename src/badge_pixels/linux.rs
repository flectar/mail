//! The badge's digits on Linux, where trays have no badge of their own to
//! copy: the app's typeface, Google Sans Flex, bold, from the fonts Slint
//! loaded for the window, drawn by swash.

use slint::fontique_012::fontique::{
    Attributes, Blob, FontStyle, FontWeight, FontWidth, QueryFamily, QueryStatus, SourceCache,
};
use std::cell::RefCell;
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::{Format, Vector};
use swash::{FontRef, GlyphId};

/// The family app.slint embeds.
const FAMILY: &str = "Google Sans Flex";
/// Bold, as the badge's digits are on Windows.
const WEIGHT: f32 = 700.0;

thread_local! {
    /// The bold face, once found; it stays the same for the session.
    static FACE: RefCell<Option<(Blob<u8>, u32)>> = const { RefCell::new(None) };
    /// The scaler's caches, kept from one count to the next.
    static SCALING: RefCell<ScaleContext> = RefCell::new(ScaleContext::new());
}

/// How much bold text covers each pixel of a badge `height` pixels tall,
/// with the ink in the middle. The badge is as wide as the text needs, from
/// `height` up to `max_width`; that width comes with the mask. The sizes
/// follow the module's rules, with letters narrowed along the typeface's
/// width axis where the text needs it.
pub(crate) fn text_mask(text: &str, height: usize, max_width: usize) -> Option<(usize, Vec<u8>)> {
    let (data, index) = bold_face()?;
    let font = FontRef::from_index(data.as_ref(), usize::try_from(index).ok()?)?;
    let glyphs: Vec<GlyphId> = text
        .chars()
        .map(|letter| font.charmap().map(letter))
        .collect();
    if glyphs.is_empty() || glyphs.contains(&0) {
        return None;
    }
    let size = super::font_height(height, glyphs.len()) as f32;
    // The width axis' default, then narrower steps down to its minimum.
    let widths = font
        .variations()
        .find_by_tag(swash::tag_from_bytes(b"wdth"))
        .map_or(vec![100.0], |axis| {
            let (narrowest, normal) = (axis.min_value(), axis.default_value());
            let mut widths = vec![normal];
            let mut width = normal - 5.0;
            while width > narrowest {
                widths.push(width);
                width -= 5.0;
            }
            widths.push(narrowest);
            widths
        });
    let measure = |width: f32| {
        let coords: Vec<i16> = font
            .variations()
            .normalized_coords([("wght", WEIGHT), ("wdth", width)])
            .collect();
        let metrics = font.glyph_metrics(&coords).scale(size);
        let advances: Vec<f32> = glyphs
            .iter()
            .map(|&glyph| metrics.advance_width(glyph))
            .collect();
        (coords, advances)
    };
    let (mut coords, mut advances) = measure(widths[0]);
    let badge_width = super::badge_width(advances.iter().sum(), height, max_width);
    if !super::fits(advances.iter().sum(), badge_width) {
        for &width in &widths[1..] {
            (coords, advances) = measure(width);
            if super::fits_narrowed(advances.iter().sum(), badge_width) {
                break;
            }
        }
    }
    let extent = advances.iter().sum::<f32>();
    let cap_height = font.metrics(&coords).scale(size).cap_height;
    // The digits stand on this row; centring by ink follows anyway.
    let baseline = ((height as f32 + cap_height) / 2.0).round() as i32;
    let mask = SCALING.with(|scaling| {
        let mut context = scaling.borrow_mut();
        let mut scaler = context
            .builder(font)
            .size(size)
            .hint(true)
            .normalized_coords(&coords)
            .build();
        let mut mask = vec![0_u8; badge_width * height];
        let mut pen = (badge_width as f32 - extent) / 2.0;
        for (&glyph, advance) in glyphs.iter().zip(&advances) {
            let left = pen.floor();
            let image = Render::new(&[Source::Outline])
                .format(Format::Alpha)
                .offset(Vector::new(pen - left, 0.0))
                .render(&mut scaler, glyph)?;
            let placement = image.placement;
            for row in 0..placement.height as i32 {
                for column in 0..placement.width as i32 {
                    let x = left as i32 + placement.left + column;
                    let y = baseline - placement.top + row;
                    let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
                        continue;
                    };
                    if x >= badge_width || y >= height {
                        continue;
                    }
                    let cover = image.data[(row * placement.width as i32 + column) as usize];
                    let pixel = &mut mask[y * badge_width + x];
                    *pixel = pixel.saturating_add(cover);
                }
            }
            pen += advance;
        }
        Some(mask)
    })?;
    Some((badge_width, super::centre(badge_width, height, &mask)))
}

/// The typeface's bold upright face, matched as CSS matches fonts: the
/// variable font app.slint embeds, which Slint registers ahead of any copy
/// installed on the system.
fn bold_face() -> Option<(Blob<u8>, u32)> {
    if let Some(face) = FACE.with(|face| face.borrow().clone()) {
        return Some(face);
    }
    let face = find_bold_face()?;
    FACE.with(|slot| *slot.borrow_mut() = Some(face.clone()));
    Some(face)
}

fn find_bold_face() -> Option<(Blob<u8>, u32)> {
    let mut fonts = slint::fontique_012::shared_collection();
    let mut sources = SourceCache::default();
    let mut query = fonts.query(&mut sources);
    query.set_families([QueryFamily::Named(FAMILY)]);
    query.set_attributes(Attributes::new(
        FontWidth::NORMAL,
        FontStyle::Normal,
        FontWeight::BOLD,
    ));
    let mut face = None;
    query.matches_with(|font| {
        face = Some((font.blob.clone(), font.index));
        QueryStatus::Stop
    });
    face
}
