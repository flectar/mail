//! `text-decoration-skip-ink: auto` (CSS Text Decoration 4, 2.6), as Blink
//! does it (`TextPainter::ClipDecorationLine` over Skia's
//! `SkFont::getIntercepts`): an underline or overline is interrupted where a
//! glyph's outline crosses it, from a little before the outline's leftmost
//! crossing to a little after its rightmost one.
//!
//! Stylo only parses `text-decoration-skip-ink` for Gecko, so it is always
//! `auto` here.

use std::collections::HashMap;

use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// How many entries the intercept cache holds before it starts over.
const MAX_CACHED_INTERCEPTS: usize = 8192;

#[derive(Hash, PartialEq, Eq)]
struct InterceptKey {
    font: u64,
    index: u32,
    glyph: u32,
    size: u32,
    coords: u64,
    skew: u32,
    /// The band, in 1/64 px below the glyph's baseline.
    top: i32,
    bottom: i32,
}

/// Cached glyph intercepts and per-font glyph exclusions.
#[derive(Default)]
pub(crate) struct InkSkip {
    intercepts: HashMap<InterceptKey, Option<(f32, f32)>>,
    /// The glyphs of `/`, `\` and `_`, which Blink never skips: a line under a
    /// path or an identifier stays whole.
    excluded: HashMap<(u64, u32), [Option<u32>; 3]>,
}

impl InkSkip {
    /// The glyphs whose ink the decoration line runs through regardless.
    pub(crate) fn excluded_glyphs(&mut self, font: &parley::FontData) -> [Option<u32>; 3] {
        *self
            .excluded
            .entry((font.data.id(), font.index))
            .or_insert_with(|| {
                let Ok(font) = FontRef::from_index(font.data.as_ref(), font.index) else {
                    return [None; 3];
                };
                let charmap = font.charmap();
                ['/', '\\', '_'].map(|c| charmap.map(c).map(|glyph| glyph.to_u32()))
            })
    }

    /// Where glyph `glyph`'s outline crosses the band from `top` to `bottom`
    /// px below its baseline: its leftmost and rightmost x there, relative to
    /// the glyph's origin.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn intercept(
        &mut self,
        font: &parley::FontData,
        font_size: f32,
        coords: &[i16],
        skew: f32,
        glyph: u32,
        top: f32,
        bottom: f32,
    ) -> Option<(f32, f32)> {
        let key = InterceptKey {
            font: font.data.id(),
            index: font.index,
            glyph,
            size: font_size.to_bits(),
            coords: coords.iter().fold(0u64, |hash, coord| {
                hash.rotate_left(16) ^ u64::from(*coord as u16)
            }),
            skew: skew.to_bits(),
            top: (top * 64.0).round() as i32,
            bottom: (bottom * 64.0).round() as i32,
        };
        if let Some(intercept) = self.intercepts.get(&key) {
            return *intercept;
        }
        let intercept = compute_intercept(font, font_size, coords, skew, glyph, top, bottom);
        if self.intercepts.len() >= MAX_CACHED_INTERCEPTS {
            self.intercepts.clear();
        }
        self.intercepts.insert(key, intercept);
        intercept
    }
}

fn compute_intercept(
    font: &parley::FontData,
    font_size: f32,
    coords: &[i16],
    skew: f32,
    glyph: u32,
    top: f32,
    bottom: f32,
) -> Option<(f32, f32)> {
    let font = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    let coords: Vec<NormalizedCoord> = coords
        .iter()
        .map(|coord| NormalizedCoord::from_bits(*coord))
        .collect();
    let location = LocationRef::new(&coords);
    let size = Size::new(font_size);
    let glyph_id = GlyphId::new(glyph);
    // Outlines are y-up: the band below the baseline is at negative y.
    let (low, high) = (-bottom, -top);
    // Most glyphs don't reach the band, which their bounds tell cheaply.
    if skew == 0.0
        && let Some(bounds) = font.glyph_metrics(size, location).bounds(glyph_id)
        && (bounds.y_max < low || bounds.y_min > high)
    {
        return None;
    }
    let outline = font.outline_glyphs().get(glyph_id)?;
    let mut pen = BandPen {
        low,
        high,
        skew,
        start: (0.0, 0.0),
        current: (0.0, 0.0),
        min: f32::INFINITY,
        max: f32::NEG_INFINITY,
    };
    outline
        .draw(DrawSettings::unhinted(size, location), &mut pen)
        .ok()?;
    (pen.min < pen.max).then_some((pen.min, pen.max))
}

/// Finds the horizontal extent of an outline within a horizontal band.
struct BandPen {
    low: f32,
    high: f32,
    /// The synthetic italic's slant, as x per y.
    skew: f32,
    start: (f32, f32),
    current: (f32, f32),
    min: f32,
    max: f32,
}

impl BandPen {
    fn segment(&mut self, from: (f32, f32), to: (f32, f32)) {
        let (x0, y0) = (from.0 + self.skew * from.1, from.1);
        let (x1, y1) = (to.0 + self.skew * to.1, to.1);
        if y0.max(y1) < self.low || y0.min(y1) > self.high {
            return;
        }
        let dy = y1 - y0;
        let (a, b) = if dy.abs() < f32::EPSILON {
            (x0, x1)
        } else {
            // The part of the segment inside the band.
            let ta = ((self.low - y0) / dy).clamp(0.0, 1.0);
            let tb = ((self.high - y0) / dy).clamp(0.0, 1.0);
            (x0 + (x1 - x0) * ta, x0 + (x1 - x0) * tb)
        };
        self.min = self.min.min(a.min(b));
        self.max = self.max.max(a.max(b));
    }
}

impl OutlinePen for BandPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.start = (x, y);
        self.current = (x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.segment(self.current, (x, y));
        self.current = (x, y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (x0, y0) = self.current;
        const STEPS: usize = 8;
        let mut previous = (x0, y0);
        for step in 1..=STEPS {
            let t = step as f32 / STEPS as f32;
            let mt = 1.0 - t;
            let point = (
                mt * mt * x0 + 2.0 * mt * t * cx0 + t * t * x,
                mt * mt * y0 + 2.0 * mt * t * cy0 + t * t * y,
            );
            self.segment(previous, point);
            previous = point;
        }
        self.current = (x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (x0, y0) = self.current;
        const STEPS: usize = 12;
        let mut previous = (x0, y0);
        for step in 1..=STEPS {
            let t = step as f32 / STEPS as f32;
            let mt = 1.0 - t;
            let point = (
                mt * mt * mt * x0
                    + 3.0 * mt * mt * t * cx0
                    + 3.0 * mt * t * t * cx1
                    + t * t * t * x,
                mt * mt * mt * y0
                    + 3.0 * mt * mt * t * cy0
                    + 3.0 * mt * t * t * cy1
                    + t * t * t * y,
            );
            self.segment(previous, point);
            previous = point;
        }
        self.current = (x, y);
    }

    fn close(&mut self) {
        self.segment(self.current, self.start);
        self.current = self.start;
    }
}

/// Whether ink skipping leaves `c` out, as Blink's `CanTextDecorationSkipInk`
/// does: CJK ideographs and symbols, kana, bopomofo and Hangul.
pub(crate) fn never_skips_ink(c: char) -> bool {
    matches!(u32::from(c),
        0x1100..=0x11FF // Hangul Jamo
        | 0x2E80..=0x2FFF // CJK radicals, Kangxi radicals, ideographic description
        | 0x3000..=0x31FF // CJK symbols and punctuation, kana, bopomofo, strokes
        | 0x3200..=0x4DBF // enclosed CJK, CJK compatibility, extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xA960..=0xA97F // Hangul Jamo extended A
        | 0xAC00..=0xD7FF // Hangul syllables, Jamo extended B
        | 0xF900..=0xFAFF // CJK compatibility ideographs
        | 0xFE30..=0xFE4F // CJK compatibility forms
        | 0xFF00..=0xFFEF // half and full width forms
        | 0x10080..=0x100FF // Linear B ideograms
        | 0x1F200..=0x1F2FF // enclosed ideographic supplement
        | 0x20000..=0x3FFFF // supplementary ideographic planes
    )
}
