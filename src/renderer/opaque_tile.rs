//! Preserve an opaque browser canvas as RGB8 for Slint's software renderer.
//!
//! This runs once when a CPU tile is created, after canvas compositing. RGB8
//! allows Slint to recognize opaque coverage and uses 25% fewer cached bytes.
//! A transparent input keeps its original RGBA representation, so an eventual
//! new caller cannot silently discard alpha.

use slint::{Image, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer};

pub(super) fn canvas_image(rgba: SharedPixelBuffer<Rgba8Pixel>) -> Image {
    let mut rgb = SharedPixelBuffer::<Rgb8Pixel>::new(rgba.width(), rgba.height());
    let mut opaque = true;
    for (source, target) in rgba.as_slice().iter().zip(rgb.make_mut_slice()) {
        opaque &= source.a == 255;
        *target = Rgb8Pixel::new(source.r, source.g, source.b);
    }
    if opaque {
        Image::from_rgb8(rgb)
    } else {
        Image::from_rgba8(rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, PlatformError, WindowAdapter, WindowEvent};
    use slint::{ComponentHandle, PhysicalSize};
    use std::rc::Rc;

    fn canvas(width: u32, height: u32) -> SharedPixelBuffer<Rgba8Pixel> {
        let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
        for (index, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
            let (x, y) = (index as u32 % width, index as u32 / width);
            // Varied colors and hard edges make sampling/rounding changes visible.
            *pixel = Rgba8Pixel::new(
                (x * 17 + y * 3) as u8,
                (x * 7 + y * 29) as u8,
                if (x / 3 + y / 2) % 2 == 0 { 0 } else { 255 },
                255,
            );
        }
        pixels
    }

    #[test]
    fn opaque_canvas_preserves_every_channel_and_declares_rgb() {
        let pixels = canvas(37, 19);
        let image = canvas_image(pixels.clone());
        let rgb = image
            .to_rgb8()
            .expect("opaque canvas must be represented as RGB8");
        assert_eq!(image.size(), pixels.size());
        assert_eq!(rgb.as_bytes().len() * 4, pixels.as_bytes().len() * 3);
        for (source, result) in pixels
            .as_slice()
            .iter()
            .zip(image.to_rgba8().unwrap().as_slice())
        {
            assert_eq!(source, result);
        }
    }

    #[test]
    fn unexpected_transparency_keeps_rgba_and_all_original_bytes() {
        for alpha in [0, 1, 63, 127, 254] {
            let mut pixels = canvas(37, 19);
            pixels.make_mut_slice()[311].a = alpha;
            let image = canvas_image(pixels.clone());
            assert!(
                image.to_rgb8().is_none(),
                "transparent source must remain RGBA"
            );
            assert_eq!(image.to_rgba8().unwrap().as_bytes(), pixels.as_bytes());
        }
    }

    struct ProbePlatform(Rc<MinimalSoftwareWindow>);
    impl Platform for ProbePlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
            Ok(self.0.clone())
        }
    }

    slint::slint! {
        export component CanvasProbe inherits Window {
            width: 128px; height: 88px;
            background: #34567a;
            in property <image> canvas;
            in property <float> alpha: 1;
            in property <length> left: 7px;
            in property <length> image-width: 83px;
            Rectangle {
                x: 3px; y: 2px; width: 116px; height: 80px;
                background: @linear-gradient(90deg, #fa714090, #25de7860);
            }
            Rectangle {
                x: root.left; y: 9px; width: root.image-width; height: 57px;
                opacity: root.alpha;
                Image { source: root.canvas; width: parent.width; height: parent.height; image-fit: fill; }
            }
        }
    }

    #[test]
    fn actual_slint_rgb_and_rgba_match_opacity_scaling_and_partial_updates() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        slint::platform::set_platform(Box::new(ProbePlatform(window.clone()))).unwrap();
        let ui = CanvasProbe::new().unwrap();
        ui.show().unwrap();
        let source = canvas(37, 19);
        let rgba = Image::from_rgba8(source.clone());
        let rgb = canvas_image(source);
        for scale in [1., 1.5, 2.] {
            window
                .window()
                .dispatch_event(WindowEvent::ScaleFactorChanged {
                    scale_factor: scale,
                });
            let size = PhysicalSize::new((128. * scale) as u32, (88. * scale) as u32);
            window.set_size(size);
            let stride = size.width as usize + 7;
            let sentinel = Rgb8Pixel::new(0xa5, 0x5a, 0xc3);
            let mut pixels = vec![sentinel; stride * size.height as usize];
            for alpha in [1., 0.75, 0.28, 0.] {
                for (left, image_width) in [(7., 83.), (15., 61.5), (39., 67.25)] {
                    ui.set_alpha(alpha);
                    ui.set_left(left);
                    ui.set_image_width(image_width);
                    ui.set_canvas(rgba.clone());
                    window.draw_if_needed(|renderer| {
                        renderer.render(&mut pixels, stride);
                    });
                    let expected = pixels.clone();
                    ui.set_canvas(rgb.clone());
                    let rendered = window.draw_if_needed(|renderer| {
                        renderer.render(&mut pixels, stride);
                    });
                    if alpha > 0. {
                        assert!(
                            rendered,
                            "a visible changed source must schedule a real render"
                        );
                    }
                    assert_eq!(
                        pixels, expected,
                        "scale={scale}, alpha={alpha}, x={left}, width={image_width}"
                    );
                    for row in pixels.chunks_exact(stride) {
                        assert!(
                            row[size.width as usize..]
                                .iter()
                                .all(|pixel| *pixel == sentinel),
                            "image rendering must preserve stride padding"
                        );
                    }
                }
            }
        }
    }
}
