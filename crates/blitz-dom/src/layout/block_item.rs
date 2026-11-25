//! The style that Taffy's block layout reads for a block-level child.

use std::ops::Deref;

use style::properties::ComputedValues;
use stylo_taffy::TaffyStyloStyle;
use taffy::{BlockItemStyle, CompactLength, CoreStyle};

/// A block-level child's style. A compressible replaced element, such as an
/// image, whose `max-width` is a percentage reports an automatic width, so
/// that Taffy measures it when it sizes the container to its content. The
/// percentage then resolves against zero for the element's min-content
/// contribution (CSS Sizing 3 section 5.2.2), as `compute_replaced_layout`
/// does and Blink does, and a table column holding a block-level image can
/// shrink below the image's width, where the width was taken as fixed. Taffy
/// lays such an element out at the width it resolves itself either way. An
/// image that shows its alt text instead keeps its width, as a broken image
/// does in Chrome, which lays it out as a box holding its alt text.
pub(crate) struct BlockChildStyle<T: Deref<Target = ComputedValues>> {
    pub(crate) style: TaffyStyloStyle<T>,
    pub(crate) shows_alt_text: bool,
}

impl<T: Deref<Target = ComputedValues>> CoreStyle for BlockChildStyle<T> {
    type CustomIdent = <TaffyStyloStyle<T> as CoreStyle>::CustomIdent;

    fn box_generation_mode(&self) -> taffy::BoxGenerationMode {
        self.style.box_generation_mode()
    }
    fn is_block(&self) -> bool {
        self.style.is_block()
    }
    fn is_compressible_replaced(&self) -> bool {
        self.style.is_compressible_replaced()
    }
    fn is_replaced(&self) -> bool {
        self.style.is_replaced()
    }
    fn box_sizing(&self) -> taffy::BoxSizing {
        self.style.box_sizing()
    }
    fn direction(&self) -> taffy::Direction {
        self.style.direction()
    }
    fn overflow(&self) -> taffy::Point<taffy::Overflow> {
        self.style.overflow()
    }
    fn scrollbar_width(&self) -> f32 {
        self.style.scrollbar_width()
    }
    fn position(&self) -> taffy::Position {
        self.style.position()
    }
    fn inset(&self) -> taffy::Rect<taffy::LengthPercentageAuto> {
        self.style.inset()
    }
    fn is_containing_block(&self) -> taffy::ContainingBlockClaims {
        self.style.is_containing_block()
    }
    fn size(&self) -> taffy::Size<taffy::Dimension> {
        let size = self.style.size();
        let percent_max_width = matches!(
            self.style.max_size().width.into_raw().tag(),
            CompactLength::PERCENT_TAG | CompactLength::CALC_TAG
        );
        if self.style.is_compressible_replaced() && !self.shows_alt_text && percent_max_width {
            taffy::Size {
                width: taffy::Dimension::auto(),
                ..size
            }
        } else {
            size
        }
    }
    fn min_size(&self) -> taffy::Size<taffy::LengthPercentageAuto> {
        self.style.min_size()
    }
    fn max_size(&self) -> taffy::Size<taffy::LengthPercentageAuto> {
        self.style.max_size()
    }
    fn aspect_ratio(&self) -> Option<f32> {
        self.style.aspect_ratio()
    }
    fn margin(&self) -> taffy::Rect<taffy::LengthPercentageAuto> {
        self.style.margin()
    }
    fn padding(&self) -> taffy::Rect<taffy::LengthPercentage> {
        self.style.padding()
    }
    fn border(&self) -> taffy::Rect<taffy::LengthPercentage> {
        self.style.border()
    }
    fn contain(&self) -> taffy::Contain {
        self.style.contain()
    }
}

impl<T: Deref<Target = ComputedValues>> BlockItemStyle for BlockChildStyle<T> {
    fn is_table(&self) -> bool {
        self.style.is_table()
    }
    fn align_self(&self) -> Option<taffy::AlignSelf> {
        self.style.align_self()
    }
    fn justify_self(&self) -> Option<taffy::AlignSelf> {
        self.style.justify_self()
    }
    fn align_content(&self) -> taffy::AlignContent {
        self.style.align_content()
    }
    #[cfg(feature = "floats")]
    fn float(&self) -> taffy::Float {
        self.style.float()
    }
    #[cfg(feature = "floats")]
    fn clear(&self) -> taffy::Clear {
        self.style.clear()
    }
}
