//! A retained RGBA framebuffer with the existing opaque RGB output contract.
//!
//! Slint still repaints the same allocation with `ReusedBuffer`. NativeWindow
//! buffers may rotate, so Native accumulates each buffer's missed damage before
//! copying its pending region with the acquired buffer's byte stride. Unknown
//! buffers begin with a full copy; only a successful submission clears damage.

use slint::platform::software_renderer::{PremultipliedRgbaColor, TargetPixel};

/// RGBA byte order, always opaque, including a transparent window background.
/// The old RGB framebuffer composited on black and native presentation set
/// alpha to 255. An opaque premultiplied destination preserves that behavior.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct OpaqueRgba(PremultipliedRgbaColor);

const _: () = {
    assert!(std::mem::size_of::<OpaqueRgba>() == 4);
    assert!(std::mem::align_of::<OpaqueRgba>() == 1);
    assert!(std::mem::offset_of!(PremultipliedRgbaColor, red) == 0);
    assert!(std::mem::offset_of!(PremultipliedRgbaColor, green) == 1);
    assert!(std::mem::offset_of!(PremultipliedRgbaColor, blue) == 2);
    assert!(std::mem::offset_of!(PremultipliedRgbaColor, alpha) == 3);
};

impl Default for OpaqueRgba {
    fn default() -> Self {
        Self::from_rgb(0, 0, 0)
    }
}

impl TargetPixel for OpaqueRgba {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        // Slint's own RGB blending and rounding; an opaque destination stays
        // opaque under source-over, including zero-alpha source pixels.
        self.0.blend(color);
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Self(PremultipliedRgbaColor::from_rgb(red, green, blue))
    }

    // The TargetPixel default is opaque black via from_rgb. Deliberately do
    // not use PremultipliedRgbaColor::background(), which is transparent.
}

impl OpaqueRgba {
    /// Bytes remain valid only while this immutable framebuffer borrow lives.
    pub(crate) fn as_bytes(pixels: &[Self]) -> &[u8] {
        // SAFETY: the assertions above verify four consecutive u8 channels,
        // with no padding. repr(transparent) preserves that layout. The slice
        // borrows the original storage and never outlives it.
        unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast(), std::mem::size_of_val(pixels)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, PlatformError, WindowAdapter};
    use slint::{Color, ComponentHandle, Rgb8Pixel};
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    #[test]
    fn rgba_bytes_and_source_over_preserve_rgb_output() {
        assert_eq!(
            OpaqueRgba::as_bytes(&[OpaqueRgba::from_rgb(0x12, 0x34, 0x56)]),
            &[0x12, 0x34, 0x56, 0xff]
        );
        assert_eq!(OpaqueRgba::as_bytes(&[]), &[]);
        assert_eq!(
            OpaqueRgba::as_bytes(&[OpaqueRgba::default()]),
            &[0, 0, 0, 255]
        );
        let color = PremultipliedRgbaColor::from(Color::from_argb_u8(128, 255, 64, 0));
        let mut transparent = PremultipliedRgbaColor::background();
        transparent.blend(color);
        assert_eq!(
            [
                transparent.red,
                transparent.green,
                transparent.blue,
                transparent.alpha
            ],
            [128, 32, 0, 128]
        );
        let mut opaque = OpaqueRgba::background();
        opaque.blend(color);
        assert_eq!(OpaqueRgba::as_bytes(&[opaque]), &[128, 32, 0, 255]);

        for background in [
            Rgb8Pixel::new(0, 0, 0),
            Rgb8Pixel::new(12, 78, 210),
            Rgb8Pixel::new(255, 255, 255),
        ] {
            let mut old = background;
            let mut new = OpaqueRgba::from_rgb(background.r, background.g, background.b);
            for alpha in [0, 1, 64, 127, 128, 254, 255] {
                let over = PremultipliedRgbaColor::from(Color::from_argb_u8(alpha, 200, 73, 241));
                old.blend(over);
                new.blend(over);
                assert_eq!(OpaqueRgba::as_bytes(&[new]), &[old.r, old.g, old.b, 255]);
            }
        }
    }

    struct ProbePlatform(RefCell<VecDeque<Rc<MinimalSoftwareWindow>>>);
    impl Platform for ProbePlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
            Ok(self
                .0
                .borrow_mut()
                .pop_front()
                .expect("one adapter per probe"))
        }
    }

    slint::slint! {
        export component PixelProbe inherits Window {
            width: 96px; height: 64px;
            in-out property <color> canvas: #123456;
            in-out property <float> pos: 0;
            background: root.canvas;
            Rectangle {
                x: root.pos * 1px; y: 1px; width: 14px; height: 12px;
                border-radius: 5px;
                background: @linear-gradient(90deg, #e049a180, #19aada30);
                border-color: #eeddaa70; border-width: 1px;
            }
            Text {
                x: 2px; y: 32px; width: 92px; height: 24px;
                text: "Slint RGBA"; font-family: "Google Sans Flex";
                font-size: 17px; color: #ffdd1180;
            }
        }
    }

    fn copy_rows(
        source: &[OpaqueRgba],
        destination: &mut [u8],
        width: usize,
        height: usize,
        stride: usize,
    ) {
        assert!(stride >= width * 4);
        assert!(destination.len() >= stride * height);
        let source = OpaqueRgba::as_bytes(source);
        for y in 0..height {
            destination[y * stride..y * stride + width * 4]
                .copy_from_slice(&source[y * width * 4..(y + 1) * width * 4]);
        }
    }

    fn assert_frame(
        source: &[OpaqueRgba],
        destination: &[u8],
        width: usize,
        height: usize,
        stride: usize,
    ) {
        let source = OpaqueRgba::as_bytes(source);
        for y in 0..height {
            assert_eq!(
                &destination[y * stride..y * stride + width * 4],
                &source[y * width * 4..(y + 1) * width * 4]
            );
            assert!(
                destination[y * stride + width * 4..(y + 1) * stride]
                    .iter()
                    .all(|pixel| *pixel == 0xa5),
                "row padding modified"
            );
        }
    }

    #[test]
    fn actual_slint_partial_render_matches_rgb_with_stride_and_three_buffers() {
        let rgb_window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        let rgba_window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        slint::platform::set_platform(Box::new(ProbePlatform(RefCell::new(VecDeque::from([
            rgb_window.clone(),
            rgba_window.clone(),
        ])))))
        .unwrap();
        let font = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../resources/fonts/google-sans-flex/GoogleSansFlex-VariableFont_GRAD,ROND,opsz,slnt,wdth,wght.ttf"
        ));
        let blob = slint::fontique_011::fontique::Blob::new(std::sync::Arc::new(font.to_vec()));
        assert!(
            !slint::fontique_011::shared_collection()
                .register_fonts(blob, None)
                .is_empty()
        );
        let rgb_ui = PixelProbe::new().unwrap();
        let rgba_ui = PixelProbe::new().unwrap();
        rgb_ui.show().unwrap();
        rgba_ui.show().unwrap();
        let (width, height, stride) = (96, 64, 400);
        rgb_window.set_size(slint::PhysicalSize::new(width as u32, height as u32));
        rgba_window.set_size(slint::PhysicalSize::new(width as u32, height as u32));
        let mut rgb = vec![Rgb8Pixel::default(); width * height];
        let mut rgba = vec![OpaqueRgba::default(); width * height];
        assert!(rgb_window.draw_if_needed(|r| {
            r.render(&mut rgb, width);
        }));
        assert!(rgba_window.draw_if_needed(|r| {
            r.render(&mut rgba, width);
        }));
        let backdrop = rgb[32 * width];
        assert_eq!([backdrop.r, backdrop.g, backdrop.b], [0x12, 0x34, 0x56]);
        assert!(
            rgb[32 * width..56 * width]
                .iter()
                .any(|p| [p.r, p.g, p.b] != [backdrop.r, backdrop.g, backdrop.b]),
            "text must actually be rasterized"
        );
        let mut rotating = vec![vec![0xa5; stride * height]; 3];
        for buffer in &mut rotating {
            copy_rows(&rgba, buffer, width, height, stride);
        }
        for frame in 0..30 {
            let pos = match frame {
                0 => 24.,
                1 => 48.,
                _ => ((frame * 23) % 70) as f32,
            };
            rgb_ui.set_pos(pos);
            rgba_ui.set_pos(pos);
            if frame == 8 {
                let color = Color::from_argb_u8(96, 210, 123, 34);
                rgb_ui.set_canvas(color);
                rgba_ui.set_canvas(color);
            }
            if frame == 17 {
                let color = Color::from_argb_u8(0, 255, 255, 255);
                rgb_ui.set_canvas(color);
                rgba_ui.set_canvas(color);
            }
            slint::platform::update_timers_and_animations();
            rgb_window.draw_if_needed(|r| {
                r.render(&mut rgb, width);
            });
            let mut region = None;
            rgba_window.draw_if_needed(|r| {
                region = Some(r.render(&mut rgba, width));
            });
            for (old, new) in rgb.iter().zip(&rgba) {
                assert_eq!(
                    OpaqueRgba::as_bytes(std::slice::from_ref(new)),
                    &[old.r, old.g, old.b, 255]
                );
            }
            if frame == 1 {
                // This acquired buffer still holds the initial image. The
                // second frame's dirty region alone cannot repair that age.
                let mut partial = rotating[1].clone();
                let region = region.expect("moved rectangle must repaint");
                let origin = region.bounding_box_origin();
                let size = region.bounding_box_size();
                assert!(origin.x > 0);
                for y in origin.y as usize..(origin.y as usize + size.height as usize).min(height) {
                    let begin = origin.x as usize * 4;
                    let end = (origin.x as usize + size.width as usize).min(width) * 4;
                    partial[y * stride + begin..y * stride + end].copy_from_slice(
                        &OpaqueRgba::as_bytes(&rgba)[y * width * 4 + begin..y * width * 4 + end],
                    );
                }
                assert_ne!(
                    &partial[6 * stride + 5 * 4..6 * stride + 6 * 4],
                    &OpaqueRgba::as_bytes(&rgba)[(6 * width + 5) * 4..(6 * width + 6) * 4],
                    "dirty-only transfer leaves the initial rectangle in an older buffer"
                );
            }
            let buffer = &mut rotating[frame % 3];
            copy_rows(&rgba, buffer, width, height, stride);
            assert_frame(&rgba, buffer, width, height, stride);
        }
    }
}
