//! Blitz hands each glyph run a `glyph_transform`, which it uses for the
//! skew of synthetic italics. Vello applies that transform in the font's
//! y-up space, before the outline is flipped into the page, so Blitz's
//! positive skew leans glyphs forward there. vello_cpu applies it after the
//! flip, in the page's y-down space, where the same skew leans them
//! backwards. Fonts without an italic face therefore rendered every italic
//! mirrored on the CPU renderer.
//!
//! AnyRender doesn't say yet which space the transform is meant in, see
//! DioxusLabs/anyrender#212. Remove this wrapper once its backends agree.

use anyrender::{
    Filter, Glyph, NormalizedCoord, PaintRef, PaintScene, RegisterResourceError, RenderContext,
    ResourceId,
};
use kurbo::{Affine, Rect, Shape, Stroke, Vec2};
use peniko::{BlendMode, Color, Fill, FontData, StyleRef};
use std::{any::Any, sync::Arc};

/// The CPU renderer's scene, seen the way Blitz paints for Vello: glyph
/// transforms arrive in font space and are re-expressed in page space.
///
/// Only the required methods are forwarded. The provided `append_scene`
/// replays recorded glyph runs through `draw_glyphs` below, so they are
/// converted too.
pub(super) struct CpuScene<'a, S>(pub(super) &'a mut S);

/// The same transform, applied on the other side of the y flip.
fn page_space(glyph_transform: Affine) -> Affine {
    Affine::FLIP_Y * glyph_transform * Affine::FLIP_Y
}

impl<S: RenderContext> RenderContext for CpuScene<'_, S> {
    fn try_register_custom_resource(
        &mut self,
        resource: Box<dyn Any>,
    ) -> Result<ResourceId, RegisterResourceError> {
        self.0.try_register_custom_resource(resource)
    }

    fn unregister_resource(&mut self, resource_id: ResourceId) {
        self.0.unregister_resource(resource_id);
    }

    fn renderer_specific_context(&self) -> Option<Box<dyn Any>> {
        self.0.renderer_specific_context()
    }
}

impl<S: PaintScene> PaintScene for CpuScene<'_, S> {
    fn reset(&mut self) {
        self.0.reset();
    }

    fn push_layer(
        &mut self,
        fill: Fill,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        clip: &impl Shape,
        filter: Option<Arc<Filter>>,
        backdrop_filter: Option<Arc<Filter>>,
    ) {
        self.0
            .push_layer(fill, blend, alpha, transform, clip, filter, backdrop_filter);
    }

    fn push_clip_layer(&mut self, fill: Fill, transform: Affine, clip: &impl Shape) {
        self.0.push_clip_layer(fill, transform, clip);
    }

    fn pop_layer(&mut self) {
        self.0.pop_layer();
    }

    fn stroke<'b>(
        &mut self,
        style: &Stroke,
        transform: Affine,
        brush: impl Into<PaintRef<'b>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        self.0
            .stroke(style, transform, brush, brush_transform, shape);
    }

    fn fill<'b>(
        &mut self,
        style: Fill,
        transform: Affine,
        brush: impl Into<PaintRef<'b>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        self.0.fill(style, transform, brush, brush_transform, shape);
    }

    fn draw_glyphs<'b, 's: 'b>(
        &'s mut self,
        font: &'b FontData,
        font_size: f32,
        hint: bool,
        normalized_coords: &'b [NormalizedCoord],
        embolden: Vec2,
        style: impl Into<StyleRef<'b>>,
        brush: impl Into<PaintRef<'b>>,
        brush_alpha: f32,
        transform: Affine,
        glyph_transform: Option<Affine>,
        glyphs: impl Iterator<Item = Glyph> + Clone,
    ) {
        self.0.draw_glyphs(
            font,
            font_size,
            hint,
            normalized_coords,
            embolden,
            style,
            brush,
            brush_alpha,
            transform,
            glyph_transform.map(page_space),
            glyphs,
        );
    }

    fn draw_box_shadow(
        &mut self,
        transform: Affine,
        rect: Rect,
        brush: Color,
        radius: f64,
        std_dev: f64,
    ) {
        self.0
            .draw_box_shadow(transform, rect, brush, radius, std_dev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_font_space_skew_becomes_the_mirrored_page_space_skew() {
        let italic = Affine::skew(14f64.to_radians().tan(), 0.0);
        let page = page_space(italic);
        assert_eq!(page, Affine::skew(-14f64.to_radians().tan(), 0.0));
        // The glyph's top-left corner, above the baseline in page space, moves
        // right in both spaces, as an italic should.
        let top = kurbo::Point::new(0.0, -10.0);
        assert!((page * top).x > 0.0);
        assert!((italic * kurbo::Point::new(0.0, 10.0)).x > 0.0);
    }
}
