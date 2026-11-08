use crate::node::RasterImageData;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use std::{io::Cursor, sync::Arc};

#[derive(Clone, Copy, Debug)]
pub struct ImageDecodeLimits {
    pub max_source_dimension: u32,
    pub max_source_pixels: u64,
    /// JPEGs can be admitted at a larger source size because the decoder uses
    /// native IDCT scaling before allocating the retained pixel buffer.
    /// Progressive and lossless JPEGs keep full-resolution coefficients while
    /// decoding, so they are held to `max_source_pixels`.
    pub max_jpeg_source_dimension: u32,
    pub max_jpeg_source_pixels: u64,
    /// The largest pixel buffer a JPEG decode may allocate.
    pub max_alloc: u64,
    /// The longest side a decoded image keeps.
    pub target_dimension: u32,
    /// The most pixels a decoded image keeps. Bounding the area rather than
    /// only the longer side keeps tall and wide images sharp for the same
    /// memory.
    pub target_pixels: u64,
}

impl ImageDecodeLimits {
    /// Bound decoding and retain intrinsic dimensions independently of pixels.
    /// JPEG uses native IDCT scaling; other formats are resized after their
    /// bounded decode. The first frame is used for animated formats.
    ///
    /// EXIF orientation is applied, as `image-orientation: from-image` is the
    /// CSS default, so the intrinsic size is that of the upright image.
    pub fn decode(self, bytes: &[u8]) -> Result<RasterImageData, String> {
        let reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        let is_jpeg = reader.format() == Some(ImageFormat::Jpeg);
        let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
        let (width, height) = decoder.dimensions();
        let (max_source_dimension, max_source_pixels) = if is_jpeg {
            (self.max_jpeg_source_dimension, self.max_jpeg_source_pixels)
        } else {
            (self.max_source_dimension, self.max_source_pixels)
        };
        if width == 0
            || height == 0
            || width > max_source_dimension
            || height > max_source_dimension
            || u64::from(width) * u64::from(height) > max_source_pixels
            || self.target_dimension == 0
            || self.target_pixels == 0
        {
            return Err("Image dimensions exceed the document decoder limits".into());
        }
        // Malformed metadata must not lose an otherwise valid image.
        let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);

        let target = self.target_size(width, height);
        let scaled_jpeg = if is_jpeg && target != (width, height) {
            self.decode_scaled_jpeg(bytes, width, height, target)?
        } else {
            None
        };
        let decoded = match scaled_jpeg {
            Some(image) => image,
            None => {
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(max_source_dimension);
                limits.max_image_height = Some(max_source_dimension);
                limits.max_alloc = Some(self.max_alloc);
                decoder.set_limits(limits).map_err(|e| e.to_string())?;
                // The general decoder allocates the whole source at once, and
                // `set_limits` does not check that allocation. Other formats are
                // already bounded by `max_source_pixels`; a JPEG admitted under
                // the larger JPEG limits must fit `max_alloc` at full size.
                if is_jpeg && decoder.total_bytes() > self.max_alloc {
                    return Err("JPEG exceeds the document decoder's allocation limit".into());
                }
                DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?
            }
        };
        let mut decoded = downscale(decoded, target);
        decoded.apply_orientation(orientation);
        let (width, height) = match orientation {
            Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH => (height, width),
            _ => (width, height),
        };

        let pixel_width = decoded.width();
        let pixel_height = decoded.height();
        let mut image =
            RasterImageData::new(width, height, Arc::new(decoded.into_rgba8().into_raw()));
        image.pixel_width = pixel_width;
        image.pixel_height = pixel_height;
        Ok(image)
    }

    /// The largest size a `width` x `height` image is decoded to: within both
    /// the longest side and the pixel budget, keeping its aspect ratio.
    pub fn target_size(self, width: u32, height: u32) -> (u32, u32) {
        let (w, h) = (f64::from(width), f64::from(height));
        let scale = (f64::from(self.target_dimension) / w.max(h))
            .min((self.target_pixels as f64 / (w * h)).sqrt())
            .min(1.0);
        if scale >= 1.0 {
            return (width, height);
        }
        let fit = |side: f64| ((side * scale).floor() as u32).max(1);
        (fit(w), fit(h))
    }

    /// Decode a large JPEG at a reduced size using the decoder's IDCT scaling,
    /// at the smallest reduction that keeps the target resolution and fits
    /// `max_alloc`. `Ok(None)` defers to the general decoder for streams this
    /// decoder rejects.
    fn decode_scaled_jpeg(
        self,
        bytes: &[u8],
        width: u32,
        height: u32,
        (target_width, target_height): (u32, u32),
    ) -> Result<Option<DynamicImage>, String> {
        let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
        decoder.set_max_decoding_buffer_size(self.max_alloc.min(usize::MAX as u64) as usize);
        if decoder.read_info().is_err() {
            return Ok(None);
        }
        let Some(info) = decoder.info() else {
            return Ok(None);
        };
        // Progressive and lossless scans keep every full-resolution coefficient
        // until the last scan, whatever the output scale. The general decoder
        // does the same, so these are refused rather than deferred.
        if info.coding_process != jpeg_decoder::CodingProcess::DctSequential
            && u64::from(width) * u64::from(height) > self.max_source_pixels
        {
            return Err("Progressive JPEG exceeds the document decoder limits".into());
        }
        let channels: u64 = match info.pixel_format {
            jpeg_decoder::PixelFormat::RGB24 => 3,
            jpeg_decoder::PixelFormat::L8 => 1,
            jpeg_decoder::PixelFormat::CMYK32 => 4,
            _ => return Ok(None),
        };
        let mut divisor = [8, 4, 2]
            .into_iter()
            .find(|&divisor| {
                width.div_ceil(divisor) >= target_width && height.div_ceil(divisor) >= target_height
            })
            .unwrap_or(1);
        let scaled_bytes = |divisor: u32| {
            u64::from(width.div_ceil(divisor)) * u64::from(height.div_ceil(divisor)) * channels
        };
        while divisor < 8 && scaled_bytes(divisor) > self.max_alloc {
            divisor *= 2;
        }
        let Ok((w, h)) = decoder.scale(
            width.div_ceil(divisor) as u16,
            height.div_ceil(divisor) as u16,
        ) else {
            return Ok(None);
        };
        let Ok(pixels) = decoder.decode() else {
            return Ok(None);
        };
        let (w, h) = (u32::from(w), u32::from(h));
        Ok(match info.pixel_format {
            jpeg_decoder::PixelFormat::RGB24 => {
                image::RgbImage::from_raw(w, h, pixels).map(DynamicImage::ImageRgb8)
            }
            jpeg_decoder::PixelFormat::CMYK32 => {
                image::RgbImage::from_raw(w, h, cmyk_to_rgb(&pixels)).map(DynamicImage::ImageRgb8)
            }
            _ => image::GrayImage::from_raw(w, h, pixels).map(DynamicImage::ImageLuma8),
        })
    }
}

/// Convert the decoder's CMYK output to RGB, as `image` did while it decoded
/// JPEGs with jpeg-decoder.
fn cmyk_to_rgb(cmyk: &[u8]) -> Vec<u8> {
    cmyk.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| {
            let k = 255 - u16::from(pixel[3]);
            [0, 1, 2].map(|channel| ((255 - u16::from(pixel[channel])) * k / 255) as u8)
        })
        .collect()
}

/// Reduce an image to `target`, the size `target_size` gives its source.
/// Colour is averaged premultiplied, so fully transparent pixels do not
/// darken the edges of translucent images.
fn downscale(image: DynamicImage, (target_width, target_height): (u32, u32)) -> DynamicImage {
    if image.width() <= target_width && image.height() <= target_height {
        return image;
    }
    if !image.color().has_alpha() {
        return image.thumbnail_exact(target_width, target_height);
    }
    let mut rgba = image.into_rgba8();
    for pixel in rgba.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    let mut reduced = DynamicImage::ImageRgba8(rgba)
        .thumbnail_exact(target_width, target_height)
        .into_rgba8();
    for pixel in reduced.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            if let Some(value) = (u16::from(*channel) * 255 + alpha / 2).checked_div(alpha) {
                *channel = value.min(255) as u8;
            }
        }
    }
    DynamicImage::ImageRgba8(reduced)
}

#[cfg(test)]
mod tests {
    use super::cmyk_to_rgb;

    #[test]
    fn cmyk_ink_converts_to_rgb() {
        // White paper, full cyan, and full black.
        let cmyk = [0, 0, 0, 0, 255, 0, 0, 0, 0, 0, 0, 255];
        assert_eq!(cmyk_to_rgb(&cmyk), [255, 255, 255, 0, 255, 255, 0, 0, 0]);
    }
}
