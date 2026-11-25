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
//!
//! The wrapper also hands vello_cpu only the filters it can run, see
//! [`cpu_filter`].

use anyrender::filters::FilterEffect;
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

/// The part of a layer's filter that vello_cpu can run.
///
/// It runs only the first effect of a filter, and panics on the colour
/// matrices behind `grayscale()`, `opacity()` and the like, so only a blur or
/// a drop shadow is passed on, which is what blurred text shadows need. Its
/// filter padding is 16-bit, so how far an effect reaches is capped well
/// below that, in device pixels.
fn cpu_filter(filter: &Filter, transform: Affine) -> Option<Arc<Filter>> {
    const MAX_STD_DEV: f64 = 256.0;
    const MAX_OFFSET: f64 = 1024.0;
    let [a, b, c, d, _, _] = transform.as_coeffs();
    // vello_cpu scales a blur by the mean of the axis scales.
    let scale = (a.hypot(b) + c.hypot(d)) / 2.0;
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let std_dev = |std_dev: f32| std_dev.min((MAX_STD_DEV / scale) as f32);
    let offset = |offset: f32| {
        let limit = (MAX_OFFSET / scale) as f32;
        offset.clamp(-limit, limit)
    };
    let effect = match &filter.nodes().first()?.effect {
        FilterEffect::GaussianBlur(blur) => FilterEffect::blur(std_dev(blur.std_deviation)),
        FilterEffect::DropShadow(shadow) => FilterEffect::drop_shadow(
            offset(shadow.dx),
            offset(shadow.dy),
            std_dev(shadow.std_deviation),
            shadow.color,
        ),
        _ => return None,
    };
    Some(Arc::new(Filter::single(effect)))
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
        let filter = filter.and_then(|filter| cpu_filter(&filter, transform));
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

    fn only_effect(filter: &Filter) -> &FilterEffect {
        let [node] = filter.nodes() else {
            panic!("one effect expected");
        };
        &node.effect
    }

    #[test]
    fn vello_cpu_gets_only_the_filters_it_runs() {
        let blur = Filter::single(FilterEffect::blur(3.0));
        let passed = cpu_filter(&blur, Affine::IDENTITY).unwrap();
        assert!(
            matches!(only_effect(&passed), FilterEffect::GaussianBlur(blur) if blur.std_deviation == 3.0)
        );
        let shadow = Filter::single(FilterEffect::drop_shadow(2.0, 4.0, 1.0, Color::BLACK));
        assert!(matches!(
            only_effect(&cpu_filter(&shadow, Affine::IDENTITY).unwrap()),
            FilterEffect::DropShadow(shadow) if (shadow.dx, shadow.dy) == (2.0, 4.0)
        ));
        // vello_cpu panics on colour matrices, even behind a blur it would
        // otherwise run first.
        let grayscale = Filter::single(FilterEffect::grayscale(1.0));
        assert!(cpu_filter(&grayscale, Affine::IDENTITY).is_none());
        let list = Filter::linear_list(
            [FilterEffect::blur(2.0), FilterEffect::grayscale(1.0)].into_iter(),
        );
        assert!(matches!(
            only_effect(&cpu_filter(&list, Affine::IDENTITY).unwrap()),
            FilterEffect::GaussianBlur(_)
        ));
    }

    #[test]
    fn filter_reach_is_capped_in_device_pixels() {
        let huge = Filter::single(FilterEffect::blur(100_000.0));
        let at_2x = cpu_filter(&huge, Affine::scale(2.0)).unwrap();
        assert!(
            matches!(only_effect(&at_2x), FilterEffect::GaussianBlur(blur) if blur.std_deviation == 128.0)
        );
        let far = Filter::single(FilterEffect::drop_shadow(-1e6, 5.0, 0.0, Color::BLACK));
        assert!(matches!(
            only_effect(&cpu_filter(&far, Affine::IDENTITY).unwrap()),
            FilterEffect::DropShadow(shadow) if (shadow.dx, shadow.dy) == (-1024.0, 5.0)
        ));
        assert!(cpu_filter(&huge, Affine::scale(0.0)).is_none());
    }
}
