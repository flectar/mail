//! Photos made smaller to send, when the person writing a message asks for it: no wider or
//! taller than [`SHRUNK_SIDE`], upright, and in the colors they had.

use image::metadata::Orientation;
use image::{DynamicImage, ImageEncoder, codecs::jpeg::JpegEncoder, imageops::FilterType};
use std::path::Path;

/// The longest side of a shrunk photo, in pixels: still sharp across a laptop's screen.
pub(crate) const SHRUNK_SIDE: u32 = 1600;
const QUALITY: u8 = 85;
/// A progressive JPEG is read whole before it can be scaled, so larger ones are left as they
/// are rather than take hundreds of megabytes to read.
const MAX_PROGRESSIVE_PIXELS: u64 = 24_000_000;
/// The most a read for shrinking can hold: under twice the shrunk side on both sides, in color.
const MAX_DECODED_BYTES: usize = 48 * 1024 * 1024;
/// Files larger than this aren't read to shrink.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Whether an image of this type and size would shrink: a JPEG photo larger than
/// [`SHRUNK_SIDE`] across or down.
pub(crate) fn can_shrink(mime_type: &str, width: u32, height: u32) -> bool {
    mime_type == "image/jpeg" && width.max(height) > SHRUNK_SIDE
}

/// Whether the file at `path` is a JPEG photo that would shrink. Reads only its header.
pub(crate) fn can_shrink_file(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    if file
        .metadata()
        .is_ok_and(|metadata| metadata.len() > MAX_FILE_BYTES)
    {
        return false;
    }
    let mut decoder = jpeg_decoder::Decoder::new(std::io::BufReader::new(file));
    decoder.read_info().is_ok() && decoder.info().is_some_and(shrinks)
}

fn shrinks(info: jpeg_decoder::ImageInfo) -> bool {
    use jpeg_decoder::{CodingProcess, PixelFormat};
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    matches!(info.pixel_format, PixelFormat::RGB24 | PixelFormat::L8)
        && width.max(height) > SHRUNK_SIDE
        && match info.coding_process {
            CodingProcess::DctSequential => true,
            CodingProcess::DctProgressive => {
                u64::from(width) * u64::from(height) <= MAX_PROGRESSIVE_PIXELS
            }
            CodingProcess::Lossless => false,
        }
}

/// `data`, a JPEG photo, no wider or taller than [`SHRUNK_SIDE`]: upright, in its own colors,
/// and without the other details cameras keep in photos, such as where they were taken.
/// `None` leaves the photo as it is: one that isn't a JPEG this reads, isn't larger, or
/// wouldn't get smaller.
pub(crate) fn shrink(data: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = jpeg_decoder::Decoder::new(data);
    decoder.read_info().ok()?;
    let info = decoder.info()?;
    if !shrinks(info) {
        return None;
    }
    let (width, height) = fitted(u32::from(info.width), u32::from(info.height));
    let mut picture = read_scaled(&mut decoder, SHRUNK_SIDE, MAX_DECODED_BYTES)?.resize_exact(
        width,
        height,
        FilterType::CatmullRom,
    );
    // The camera's turn goes into the pixels, as nothing else about the camera is kept.
    if let Some(orientation) = decoder.exif_data().and_then(exif_orientation) {
        picture.apply_orientation(orientation);
    }
    let mut shrunk = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut shrunk, QUALITY);
    if let Some(profile) = decoder.icc_profile() {
        // Without it, a phone's wide-color photo would look faded.
        let _ = encoder.set_icc_profile(profile);
    }
    picture.write_with_encoder(encoder).ok()?;
    (shrunk.len() < data.len()).then_some(shrunk)
}

/// The JPEG `decoder` reads, at a half, a quarter or an eighth of its size while its longest
/// side is still at least `side`, which takes a fraction of the time and memory of reading all
/// of it. As stored: turning it upright is left to the caller.
fn read_scaled(
    decoder: &mut jpeg_decoder::Decoder<&[u8]>,
    side: u32,
    max_bytes: usize,
) -> Option<DynamicImage> {
    let side = u16::try_from(side).unwrap_or(u16::MAX);
    decoder.scale(side, side).ok()?;
    decoder.set_max_decoding_buffer_size(max_bytes);
    let pixels = decoder.decode().ok()?;
    let read = decoder.info()?;
    let (width, height) = (u32::from(read.width), u32::from(read.height));
    match read.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => Some(DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(width, height, pixels)?,
        )),
        jpeg_decoder::PixelFormat::L8 => Some(DynamicImage::ImageLuma8(
            image::GrayImage::from_raw(width, height, pixels)?,
        )),
        _ => None,
    }
}

/// The photo at `path`, shrunk into a file of its own in `directory` that goes away when the
/// returned path is dropped, with that file's size.
pub(crate) fn shrink_file(path: &Path, directory: &Path) -> Option<(tempfile::TempPath, u64)> {
    use std::io::Write;
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let shrunk = shrink(&std::fs::read(path).ok()?)?;
    let mut file = tempfile::Builder::new()
        .prefix("shrunk-photo-")
        .suffix(".jpg")
        .tempfile_in(directory)
        .ok()?;
    file.write_all(&shrunk).ok()?;
    Some((file.into_temp_path(), shrunk.len() as u64))
}

/// `width` by `height` made to fit within [`SHRUNK_SIDE`] on both sides.
fn fitted(width: u32, height: u32) -> (u32, u32) {
    let longest = u64::from(width.max(height).max(1));
    let side = u64::from(SHRUNK_SIDE).min(longest);
    let scaled = |length: u32| {
        u32::try_from((u64::from(length) * side + longest / 2) / longest)
            .unwrap_or(1)
            .max(1)
    };
    (scaled(width), scaled(height))
}

/// The turn a photo's Exif data gives it, from data that starts at its TIFF header.
fn exif_orientation(exif: &[u8]) -> Option<Orientation> {
    let big_endian = match exif.get(..4)? {
        [b'I', b'I', 42, 0] => false,
        [b'M', b'M', 0, 42] => true,
        _ => return None,
    };
    let u16_at = |at: usize| {
        let bytes: [u8; 2] = exif.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if big_endian {
            u16::from_be_bytes(bytes)
        } else {
            u16::from_le_bytes(bytes)
        })
    };
    let u32_at = |at: usize| {
        let bytes: [u8; 4] = exif.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    };
    let directory = usize::try_from(u32_at(4)?).ok()?;
    for index in 0..usize::from(u16_at(directory)?) {
        let entry = directory.checked_add(2 + index * 12)?;
        // The orientation tag, one short.
        if u16_at(entry)? == 0x0112 && u16_at(entry + 2)? == 3 && u32_at(entry + 4)? == 1 {
            return Orientation::from_exif(u8::try_from(u16_at(entry + 8)?).ok()?);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A photo-like picture: a gradient with grain, red in its top left corner.
    fn picture(width: u32, height: u32) -> image::RgbImage {
        let mut seed = 0x2545_f491_u32;
        image::RgbImage::from_fn(width, height, |x, y| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let grain = (seed % 24) as u8;
            if x < width / 8 && y < height / 8 {
                image::Rgb([220, 30 + grain, 30])
            } else {
                image::Rgb([
                    (x * 200 / width) as u8 + grain,
                    (y * 200 / height) as u8 + grain,
                    120 + grain,
                ])
            }
        })
    }

    fn jpeg(picture: &image::RgbImage, quality: u8, icc: Option<&[u8]>) -> Vec<u8> {
        let mut data = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut data, quality);
        if let Some(icc) = icc {
            encoder.set_icc_profile(icc.to_vec()).unwrap();
        }
        encoder
            .write_image(
                picture.as_raw(),
                picture.width(),
                picture.height(),
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        data
    }

    /// `data` with an Exif segment holding `tiff`, after its start of image, as cameras write it.
    fn with_exif(data: &[u8], tiff: &[u8]) -> Vec<u8> {
        let length = u16::try_from(2 + 6 + tiff.len()).unwrap();
        let mut out = data[..2].to_vec();
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(tiff);
        out.extend_from_slice(&data[2..]);
        out
    }

    /// A little-endian TIFF header with the orientation tag and a GPS directory pointer.
    fn tiff(orientation: u16) -> Vec<u8> {
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        for (tag, kind, value) in [(0x0112u16, 3u16, u32::from(orientation)), (0x8825, 4, 0)] {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&kind.to_le_bytes());
            tiff.extend_from_slice(&1u32.to_le_bytes());
            tiff.extend_from_slice(&value.to_le_bytes());
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff
    }

    fn read(data: &[u8]) -> (jpeg_decoder::Decoder<&[u8]>, image::RgbImage) {
        let mut decoder = jpeg_decoder::Decoder::new(data);
        let pixels = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        let picture =
            image::RgbImage::from_raw(u32::from(info.width), u32::from(info.height), pixels)
                .unwrap();
        (decoder, picture)
    }

    fn is_red(pixel: &image::Rgb<u8>) -> bool {
        pixel[0] > 180 && pixel[1] < 90 && pixel[2] < 90
    }

    #[test]
    fn large_photos_shrink_upright_without_camera_details() {
        // Stored on its side, as phones save photos taken upright.
        let original = with_exif(&jpeg(&picture(1800, 900), 95, None), &tiff(6));
        let shrunk = shrink(&original).expect("a large photo shrinks");
        assert!(shrunk.len() < original.len() / 2, "{} bytes", shrunk.len());
        let (decoder, upright) = read(&shrunk);
        assert_eq!(upright.dimensions(), (800, 1600));
        // Turned a quarter clockwise, its top left corner is now at the top right.
        assert!(is_red(upright.get_pixel(780, 20)));
        assert!(!is_red(upright.get_pixel(20, 20)));
        // Nothing would turn it again, and where it was taken isn't sent.
        assert!(decoder.exif_data().is_none());
    }

    #[test]
    fn shrunk_photos_keep_their_colors() {
        let profile = b"a color profile, as cameras embed them".repeat(40);
        let original = jpeg(&picture(1800, 1200), 95, Some(&profile));
        let shrunk = shrink(&original).unwrap();
        let (decoder, picture) = read(&shrunk);
        assert_eq!(picture.dimensions(), (1600, 1067));
        assert_eq!(decoder.icc_profile().as_deref(), Some(&profile[..]));
    }

    #[test]
    fn small_photos_and_other_images_stay_as_they_are() {
        let small = jpeg(&picture(1600, 900), 95, None);
        assert_eq!(shrink(&small), None);
        let mut png = Vec::new();
        DynamicImage::ImageRgb8(picture(2000, 1000))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(shrink(&png), None);
        assert_eq!(shrink(b"not an image"), None);
        assert!(can_shrink("image/jpeg", 1200, 2000));
        assert!(!can_shrink("image/jpeg", 1600, 1600));
        assert!(!can_shrink("image/png", 4000, 3000));
    }

    #[test]
    fn photos_that_would_not_get_smaller_stay_as_they_are() {
        // Already squeezed hard: shrinking at a normal quality would only grow it.
        let squeezed = jpeg(&picture(1700, 1700), 2, None);
        assert_eq!(shrink(&squeezed), None);
    }

    #[test]
    fn files_are_told_apart_by_their_headers() {
        let directory = tempfile::tempdir().unwrap();
        let large = directory.path().join("large.jpg");
        std::fs::write(&large, jpeg(&picture(1800, 900), 90, None)).unwrap();
        let small = directory.path().join("small.jpg");
        std::fs::write(&small, jpeg(&picture(1200, 900), 90, None)).unwrap();
        let text = directory.path().join("notes.jpg");
        std::fs::write(&text, b"not a photo").unwrap();
        assert!(can_shrink_file(&large));
        assert!(!can_shrink_file(&small));
        assert!(!can_shrink_file(&text));
        assert!(!can_shrink_file(&directory.path().join("missing.jpg")));

        let (path, size) = shrink_file(&large, directory.path()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        assert!(size < std::fs::metadata(&large).unwrap().len());
        let kept = path.to_path_buf();
        drop(path);
        assert!(!kept.exists(), "the shrunk copy goes away with its path");
    }

    #[test]
    fn exif_orientation_reads_both_byte_orders() {
        assert_eq!(exif_orientation(&tiff(6)), Some(Orientation::Rotate90));
        let mut big = b"MM\0*".to_vec();
        big.extend_from_slice(&8u32.to_be_bytes());
        big.extend_from_slice(&1u16.to_be_bytes());
        big.extend_from_slice(&0x0112u16.to_be_bytes());
        big.extend_from_slice(&3u16.to_be_bytes());
        big.extend_from_slice(&1u32.to_be_bytes());
        big.extend_from_slice(&[0, 3, 0, 0]);
        assert_eq!(exif_orientation(&big), Some(Orientation::Rotate180));
        assert_eq!(exif_orientation(b"II*\0\xff\xff\xff\xff"), None);
        assert_eq!(exif_orientation(b"nothing"), None);
    }

    #[test]
    fn fitting_keeps_the_shape() {
        assert_eq!(fitted(4032, 3024), (1600, 1200));
        assert_eq!(fitted(3024, 4032), (1200, 1600));
        assert_eq!(fitted(12000, 10), (1600, 1));
        assert_eq!(fitted(1000, 500), (1000, 500));
    }
}
