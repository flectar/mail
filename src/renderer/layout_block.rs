//! The pictures of layout blocks, the parts of a message being written that keep a layout of
//! their own, such as a signature laid out in a table with a logo beside its text (#549). The
//! composer shows each in its place as the reader would show it.

use super::{
    MIN_EMAIL_ZOOM, build_email_font_ctx, content_bottom, content_surface_width, cpu_scene,
    prepare_email_html_at_with_font_ctx,
};
use crate::message_appearance::EmailAppearance;
use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_paint::paint_scene_cached;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};
use blitz_traits::shell::Viewport;
use std::sync::Arc;

/// The tallest picture a block gets, in physical pixels; the rest of a longer one is cut off.
const MAX_PICTURE_HEIGHT: u32 = 8_192;

/// A layout block as the composer shows it.
#[derive(Debug)]
pub(crate) struct LayoutPicture {
    /// Its size in physical pixels.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Its pixels, premultiplied RGBA.
    pub(crate) pixels: Arc<[u8]>,
}

/// Hands a block the pictures it holds itself as soon as it asks for them, and nothing else:
/// its HTML was cleaned of all other addresses, and nothing is loaded from the web.
struct EmbeddedPictures;

impl NetProvider for EmbeddedPictures {
    fn fetch(&self, _document: usize, request: Request, handler: Box<dyn NetHandler>) {
        if request.url.scheme() != "data" {
            return;
        }
        if let Some(bytes) = crate::remote::load_data_image(request.url.as_str()) {
            handler.bytes(request.url.to_string(), Bytes::from(bytes));
        }
    }
}

/// Paints `html`, a layout block's HTML, the way the reader shows a message in a window `width`
/// logical pixels wide at `scale`, lined up as `align` says, in `colors`. Its unstyled text is
/// as large as the composer's, as the original below a reply is, and content laid out wider
/// than the window is scaled down to fit it whole.
pub(crate) fn paint_layout_block(
    html: &str,
    align: &str,
    width: f32,
    scale: f32,
    colors: EmailAppearance,
) -> Result<LayoutPicture, String> {
    let width = width.max(1.0);
    let scale = scale.clamp(0.5, 4.0);
    let html = if align == "left" {
        format!(
            "<body style=\"margin:0;font-size:{}px\">{html}</body>",
            crate::compose_editor::FONT_SIZE_LOGICAL
        )
    } else {
        format!(
            "<body style=\"margin:0;font-size:{}px\"><div align=\"{align}\" \
             style=\"text-align:{align}\">{html}</div></body>",
            crate::compose_editor::FONT_SIZE_LOGICAL
        )
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        paint(&html, width, scale, colors)
    }))
    .unwrap_or_else(|_| Err("The layout could not be shown".into()))
}

fn paint(
    html: &str,
    width: f32,
    scale: f32,
    colors: EmailAppearance,
) -> Result<LayoutPicture, String> {
    let physical_width = (width * scale).ceil().max(1.0) as u32;
    let mut email = prepare_email_html_at_with_font_ctx(
        html,
        Some(Arc::new(EmbeddedPictures)),
        physical_width,
        physical_width,
        scale,
        build_email_font_ctx(),
        colors,
    )?;
    // The pictures it holds came in as it was read.
    email.document.drain_pending_updates();
    let scheme = colors.scheme();
    // As auto-fit does in the reader: measure the authored width at 100%, then lay out the
    // window's width at the scale that fits it.
    email.document.set_viewport(Viewport::new(
        width.ceil() as u32,
        width.ceil() as u32,
        1.0,
        scheme,
    ));
    email.appearance.resolve(&mut email.document);
    let natural = content_surface_width(&email.document, width);
    let zoom = (width / natural.max(1.0)).clamp(MIN_EMAIL_ZOOM, 1.0);
    let painted_scale = scale * zoom;
    email.document.set_viewport(Viewport::new(
        physical_width,
        physical_width,
        painted_scale,
        scheme,
    ));
    email.appearance.resolve(&mut email.document);
    email.paint_cache.clear();
    let physical_height = ((content_bottom(&email.document) * painted_scale).ceil() as u32)
        .clamp(1, MAX_PICTURE_HEIGHT);
    let mut rgba = vec![0; physical_width as usize * physical_height as usize * 4];
    let mut painter = VelloCpuImageRenderer::new(physical_width, physical_height);
    painter.render(
        |scene| {
            paint_scene_cached(
                &mut cpu_scene::CpuScene(scene),
                &mut email.document,
                f64::from(painted_scale),
                physical_width,
                physical_height,
                0,
                0,
                &email.paint_cache,
            );
        },
        &mut rgba,
    );
    Ok(LayoutPicture {
        width: physical_width,
        height: physical_height,
        pixels: rgba.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signature laid out in a table: a 40-pixel red logo beside two lines of text.
    fn signature() -> String {
        use base64::Engine;
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([230, 20, 20, 255]),
        ))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        format!(
            "<table cellpadding=\"0\" cellspacing=\"0\"><tr><td style=\"padding-right:12px\">\
             <img width=\"40\" height=\"40\" alt=\"Logo\" src=\"data:image/png;base64,{}\">\
             </td><td>Zoë Ortiz<br>Head of design</td></tr></table>",
            base64::engine::general_purpose::STANDARD.encode(png)
        )
    }

    fn pixel(picture: &LayoutPicture, x: u32, y: u32) -> [u8; 4] {
        let at = (y * picture.width + x) as usize * 4;
        picture.pixels[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn a_block_shows_with_the_picture_it_holds() {
        let picture =
            paint_layout_block(&signature(), "left", 300.0, 2.0, EmailAppearance::default())
                .unwrap();
        assert_eq!(picture.width, 600);
        // As tall as the logo, at twice the size, give or take the lines' rounding.
        assert!((80..=96).contains(&picture.height), "{}", picture.height);
        // The logo is painted at the left, embedded as it is.
        let [red, green, blue, alpha] = pixel(&picture, 20, 40);
        assert!(
            red > 200 && green < 60 && blue < 60 && alpha > 200,
            "{red} {green} {blue}"
        );
        // Dark text is painted beside it, on nothing: the editor's own background shows through.
        let text = (0..picture.height)
            .flat_map(|y| (110..400).map(move |x| (x, y)))
            .any(|(x, y)| {
                let [red, _, _, alpha] = pixel(&picture, x, y);
                alpha > 200 && red < 100
            });
        assert!(text, "no text beside the logo");
        assert_eq!(pixel(&picture, 599, 0)[3], 0, "the rest is see-through");
    }

    #[test]
    fn a_wider_block_fits_the_window() {
        // A table narrows to fit, as the reader lays tables out.
        let table = "<table width=\"600\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
                     <td height=\"60\" bgcolor=\"#00ff00\">&nbsp;</td></tr></table>";
        let picture =
            paint_layout_block(table, "left", 300.0, 1.0, EmailAppearance::default()).unwrap();
        assert_eq!((picture.width, picture.height), (300, 60));
        assert!(
            pixel(&picture, 290, 10)[1] > 200,
            "it reaches the right edge"
        );

        // What can't narrow is scaled down whole: half as wide, so half as tall.
        let fixed = "<div style=\"width:600px;height:60px;background-color:#00ff00\"></div>";
        let picture =
            paint_layout_block(fixed, "left", 300.0, 1.0, EmailAppearance::default()).unwrap();
        assert_eq!(picture.width, 300);
        assert!((29..=31).contains(&picture.height), "{}", picture.height);
        assert!(
            pixel(&picture, 290, 10)[1] > 200,
            "it reaches the right edge"
        );
    }

    #[test]
    fn nothing_but_embedded_pictures_loads() {
        let remote = "<div style=\"height:20px\">Text<img src=\"https://tracker.example/p.png\" \
                      width=\"20\" height=\"20\"></div>";
        // A picture from the web stays out without holding the block up.
        let picture =
            paint_layout_block(remote, "center", 200.0, 1.0, EmailAppearance::default()).unwrap();
        assert_eq!(picture.width, 200);
    }
}
