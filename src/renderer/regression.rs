use super::*;

fn renderer(html: &str) -> GpuEmailRenderer {
    let mut r = GpuEmailRenderer::default();
    r.set_email(prepare_email_html(html).unwrap());
    r
}

const BODY: &str = "<body style='margin:0'><p style='margin:0;font-size:20px'>Cafe\u{301} hello <a href='https://example.com'>linked world</a></p></body>";

fn font_ctx_with_color_emoji() -> (parley::FontContext, parley::fontique::Blob<u8>) {
    use parley::fontique::{Blob, GenericFamily};
    let mut ctx = create_email_font_ctx_with_system_fonts(false);
    let emoji = Blob::new(Arc::new(
        include_bytes!("../../resources/test-fonts/NotoColorEmoji-digits.ttf").as_slice(),
    ) as _);
    let family = ctx.collection.register_fonts(emoji.clone(), None)[0].0;
    ctx.collection
        .set_generic_families(GenericFamily::Emoji, [family].into_iter());
    (ctx, emoji)
}

#[test]
fn issue_23_missing_named_font_keeps_digits_out_of_emoji_fallback() {
    let (ctx, emoji) = font_ctx_with_color_emoji();
    let mut prepared = prepare_email_html_with_font_ctx(
        r#"<body style="margin:0;background:#fff"><div style="font-family:'Segoe UI';font-size:20px;color:#000">0123456789</div></body>"#,
        ctx,
    ).unwrap();
    let frame = render_prepared_cpu(&mut prepared, 300, 60, 1.0).unwrap();
    let pixels = frame.tiles[0].image.to_rgba8().unwrap();
    let dark_pixels = pixels
        .as_slice()
        .iter()
        .filter(|p| p.a > 0 && p.r < 100 && p.g < 100 && p.b < 100)
        .count();
    assert!(
        dark_pixels > 100,
        "digits produced only {dark_pixels} visible pixels"
    );
    prepared.document.visit(|_, node| {
        if let Some(inline) = node
            .element_data()
            .and_then(|e| e.inline_layout_data.as_ref())
        {
            for line in inline.layout.lines() {
                for item in line.items() {
                    if let PositionedLayoutItem::GlyphRun(run) = item {
                        assert_ne!(
                            run.run().font().data.id(),
                            emoji.id(),
                            "plain digits selected the bitmap emoji font"
                        );
                    }
                }
            }
        }
    });
}

#[test]
fn issue_23_report_matches_explicit_text_font_at_multiple_scales() {
    let html = include_str!("../../resources/test-emails/dmarc-report.html");
    let reference = html.replace("Segoe UI", crate::compose_editor::UI_FONT_FAMILY);
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let (ctx, _) = font_ctx_with_color_emoji();
        let mut actual = prepare_email_html_with_font_ctx(html, ctx.clone()).unwrap();
        let mut expected = prepare_email_html_with_font_ctx(&reference, ctx).unwrap();
        let actual = render_prepared_cpu(&mut actual, 520, 900, scale).unwrap();
        let expected = render_prepared_cpu(&mut expected, 520, 900, scale).unwrap();
        assert_eq!(actual.tiles.len(), expected.tiles.len());
        for (actual, expected) in actual.tiles.iter().zip(&expected.tiles) {
            assert_eq!(
                actual.image.to_rgba8().unwrap().as_bytes(),
                expected.image.to_rgba8().unwrap().as_bytes(),
                "fallback changed report glyphs or spacing at scale {scale}"
            );
        }
    }
}

#[test]
fn issue_23_text_fallback_preserves_real_emoji_and_authored_families() {
    let (ctx, emoji) = font_ctx_with_color_emoji();
    let html = r#"<body style="font-family:'Segoe UI'">
        <div id="text">0123456789 # * © ® 1︎ ©︎ ®︎</div>
        <div id="emoji">😀 1️⃣ ©️ ®️</div>
        <div id="authored" style="font-family:'Noto Color Emoji'">0123</div>
        </body>"#;
    let prepared = prepare_email_html_with_font_ctx(html, ctx).unwrap();
    for (id, expect_emoji) in [("text", false), ("emoji", true), ("authored", true)] {
        let node = prepared
            .document
            .get_node(prepared.document.get_element_by_id(id).unwrap())
            .unwrap();
        let inline = node
            .element_data()
            .unwrap()
            .inline_layout_data
            .as_ref()
            .unwrap();
        let mut checked = 0;
        for line in inline.layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    if inline.text[run.run().text_range()].trim().is_empty() {
                        continue;
                    }
                    assert_eq!(
                        run.run().font().data.id() == emoji.id(),
                        expect_emoji,
                        "{id}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "{id} must contain shaped glyphs");
    }
}

#[cfg(feature = "gpu-renderer")]
#[test]
#[ignore = "requires a working WGPU adapter"]
fn issue_23_gpu_paints_text_digits_with_color_emoji_installed() {
    let (ctx, _) = font_ctx_with_color_emoji();
    let mut prepared = prepare_email_html_with_font_ctx(
        "<body style=\"margin:0;color:black;background:white;font:20px 'Segoe UI'\">0123456789</body>", ctx,
    ).unwrap();
    let pixels = render_to_buffer::<anyrender_vello::VelloImageRenderer, _>(
        |scene| paint_scene(scene, &mut prepared.document, 1.0, 300, 60, 0, 0),
        300,
        60,
    );
    let dark_pixels = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 0 && p[0] < 100 && p[1] < 100 && p[2] < 100)
        .count();
    assert!(dark_pixels > 100);
}

#[test]
fn default_link_color_preserves_sender_styles() {
    use blitz_dom::util::ToColorColor;
    let mut prepared = prepare_email_html(
        "<style>.brand { color:#008000 }</style><body style='margin:0;background:white;font-size:20px'>\
         <a href='https://example.com'>Default</a> \
         <a class='brand' href='https://example.com'>Brand</a> \
         <a style='color:white;background:#008000' href='https://example.com'>Button</a></body>",
    ).unwrap();
    let frame = render_prepared_cpu(&mut prepared, 520, 100, 1.0).unwrap();
    let pixels = frame.tiles[0].image.to_rgba8().unwrap();
    assert!(
        pixels
            .as_slice()
            .iter()
            .any(|p| (p.r, p.g, p.b) == (9, 105, 218))
    );
    assert!(
        pixels
            .as_slice()
            .iter()
            .any(|p| (p.r, p.g, p.b) == (0, 128, 0))
    );
    let document = &prepared.document;
    let colors: Vec<_> = prepared
        .links
        .iter()
        .map(|link| {
            let hit = document
                .hit(
                    (link.x + link.width / 2.0) * 520.0,
                    (link.y + link.height / 2.0) * 520.0,
                )
                .unwrap();
            document
                .get_node(hit.node_id)
                .unwrap()
                .primary_styles()
                .unwrap()
                .get_inherited_text()
                .color
                .as_color_color()
                .to_rgba8()
                .to_u8_array()
        })
        .collect();
    assert_eq!(
        colors,
        [[9, 105, 218, 255], [0, 128, 0, 255], [255, 255, 255, 255]]
    );
}

#[test]
fn issue_16_digits_paint_with_a_deterministic_fallback() {
    let font_ctx = create_email_font_ctx_with_system_fonts(false);
    let mut prepared = prepare_email_html_with_font_ctx(
        r#"<body style="margin:0;background:#fff"><div style="font-family:'Segoe UI';font-size:20px;color:#000">0123456789</div></body>"#,
        font_ctx,
    )
    .unwrap();
    let frame = render_prepared_cpu(&mut prepared, 240, 60, 1.0).unwrap();
    let pixels = frame.tiles[0].image.to_rgba8().unwrap();
    let dark_pixels = pixels
        .as_slice()
        .iter()
        .filter(|pixel| pixel.a > 0 && pixel.r < 100 && pixel.g < 100 && pixel.b < 100)
        .count();
    assert!(dark_pixels > 100, "digits produced no visible outlines");
}

#[test]
fn issue_16_reader_mode_keeps_text_around_nested_blocks() {
    let html = include_str!("../../resources/test-emails/dmarc-report.html");
    let r = renderer(html);
    let items = r.reader_items();
    let reader_text = items
        .iter()
        .filter(|item| item.url.is_empty())
        .map(|item| item.name.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(reader_text.contains("2026-09-09 00:00:00 UTC"));
    assert!(reader_text.contains("Please do not respond to this e-mail"));
    assert!(items.iter().any(|item| {
        item.url == "https://privacy.microsoft.com/en-us/privacystatement"
            && item.name == "Privacy Statement"
    }));
}

/// Run alone; reports application tile latency, not compositor frame time.
#[test]
#[ignore = "isolated scroll latency probe"]
fn scroll_latency_probe() {
    for (name, html) in [
        (
            "paragraphs",
            format!(
                "<body>{}</body>",
                "<p>Ordinary <b>formatted email text</b> and more words.</p>".repeat(250)
            ),
        ),
        (
            "long-inline",
            format!(
                "<body><p>{}</p></body>",
                "Ordinary <b>formatted email text</b> and more words. ".repeat(800)
            ),
        ),
        (
            "newsletter",
            include_str!("../../resources/test-emails/revolut-precision.html").to_owned(),
        ),
    ] {
        let mut r = renderer(&html);
        r.render_cpu_if_needed(700, 700, 1.0).unwrap();
        let layouts = r.layout_count;
        let mut timings = Vec::new();
        let end = (r.content_height - 700.0).max(0.0) as usize;
        for y in (0..end).step_by(30) {
            let start = Instant::now();
            if r.set_visible_region(y as f32, 700.0) {
                r.render_cpu_if_needed(700, 700, 1.0).unwrap();
                timings.push(start.elapsed().as_micros());
            }
            assert!(r.tiles.len() <= 5);
        }
        assert_eq!(r.layout_count, layouts, "scrolling must never relayout");
        timings.sort_unstable();
        if !timings.is_empty() {
            eprintln!(
                "{name}: {} tile updates, median {} us, p95 {} us, max {} us",
                timings.len(),
                timings[timings.len() / 2],
                timings[timings.len() * 95 / 100],
                timings.last().unwrap()
            );
        }
    }
}

#[test]
fn scrolling_reuses_pixels_and_layout_with_a_bounded_cache() {
    let mut r = renderer(&format!(
        "<body style='margin:0'>{}</body>",
        "<p style='height:110px;margin:0'>Scrollable words</p>".repeat(100)
    ));
    r.render_cpu_if_needed(520, 900, 1.0).unwrap();
    let first = r.tiles[&1].image.clone();
    let layouts = r.layout_count;
    let tiles = r.tile_count;
    assert!(!r.set_visible_region(30.0, 900.0));
    assert!(r.render_cpu_if_needed(520, 900, 1.0).unwrap().is_none());
    assert_eq!(r.tile_count, tiles);
    r.set_visible_region(520.0, 900.0);
    r.render_cpu_if_needed(520, 900, 1.0).unwrap().unwrap();
    assert_eq!(r.tiles[&1].image, first);
    assert_eq!(r.tile_count, tiles + 1);
    for y in [8000.0, 4000.0, 1000.0, 0.0] {
        r.set_visible_region(y, 900.0);
        r.render_cpu_if_needed(520, 900, 1.0).unwrap();
        assert!(r.tiles.len() <= 5);
        assert_eq!(r.layout_count, layouts);
    }
    r.clear();
    assert!(r.tiles.is_empty() && r.cpu_painter.is_none());
}

#[test]
fn exact_tile_boundary_has_only_one_overscan_tile() {
    assert_eq!(desired_tile_range(0.0, 512.0, 4096.0), 0..=1);
    assert_eq!(desired_tile_range(512.0, 512.0, 4096.0), 0..=2);
}

#[test]
fn scroll_steps_prioritize_visible_tiles_and_discard_obsolete_work() {
    let mut r = renderer(&format!(
        "<body>{}</body>",
        "<p style='height:100px'>Words</p>".repeat(150)
    ));
    r.render_cpu_if_needed(520, 900, 1.0).unwrap();
    let layouts = r.layout_count;
    r.set_visible_region(5000.0, 900.0);
    let before = r.tile_count;
    r.render_cpu_scroll_step(520, 900, 1.0).unwrap();
    assert_eq!(r.tile_count, before + 1);
    assert!(
        r.tiles.contains_key(&9),
        "the first visible tile precedes overscan tile 8"
    );
    assert!(r.needs_repaint());
    r.set_visible_region(0.0, 900.0);
    for _ in 0..6 {
        r.render_cpu_scroll_step(520, 900, 1.0).unwrap();
    }
    assert!(!r.needs_repaint());
    assert!(r.tiles.keys().all(|index| *index <= 2));
    assert_eq!(r.layout_count, layouts);
}

#[derive(Default)]
struct DeferredImages(std::sync::Mutex<Vec<(String, Box<dyn blitz_traits::net::NetHandler>)>>);
impl blitz_traits::net::NetProvider for DeferredImages {
    fn fetch(
        &self,
        _: usize,
        request: blitz_traits::net::Request,
        handler: Box<dyn blitz_traits::net::NetHandler>,
    ) {
        self.0
            .lock()
            .unwrap()
            .push((request.url.to_string(), handler));
    }
}
impl DeferredImages {
    fn deliver(&self, suffix: &str, bytes: Vec<u8>) {
        let mut queued = self.0.lock().unwrap();
        let index = queued
            .iter()
            .position(|(url, _)| url.ends_with(suffix))
            .unwrap();
        let (url, handler) = queued.remove(index);
        drop(queued);
        handler.bytes(url, blitz_traits::net::Bytes::from(bytes));
    }
}

#[test]
fn image_updates_preserve_fixed_layout_and_invalidate_only_affected_tiles() {
    let provider = Arc::new(DeferredImages::default());
    let html = "<body style='margin:0'><div style='height:600px'>Top</div><img src='https://example.com/fixed.png' width='90' height='60'><div style='height:1400px'>Bottom</div><img src='https://example.com/auto.png'></body>";
    let mut r = GpuEmailRenderer::default();
    r.set_email(prepare_email_html_with_provider(html, Some(provider.clone())).unwrap());
    r.render_cpu_if_needed(520, 900, 1.0).unwrap();
    let top = r.tiles[&0].image.clone();
    let layouts = r.layout_count;
    let tiles = r.tile_count;
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(90, 60, image::Rgba([0, 180, 0, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    provider.deliver("fixed.png", png.get_ref().clone());
    r.render_cpu_if_needed(520, 900, 1.0).unwrap().unwrap();
    assert_eq!(r.layout_count, layouts);
    assert_eq!(r.tile_count, tiles + 1);
    assert_eq!(r.tiles[&0].image, top);
    let pixels = r.tiles[&1].image.to_rgba8().unwrap();
    assert!(
        pixels
            .as_slice()
            .iter()
            .any(|p| p.r < 10 && p.g > 150 && p.b < 10)
    );
    provider.deliver("auto.png", png.into_inner());
    r.render_cpu_if_needed(520, 900, 1.0).unwrap();
    assert_eq!(
        r.layout_count,
        layouts + 1,
        "intrinsic sizing must still trigger layout"
    );
}

#[test]
fn fixed_image_delivery_retains_intrinsic_size_for_later_responsive_layout() {
    let provider = Arc::new(DeferredImages::default());
    let html = "<style>#image{width:60px;height:30px}@media(max-width:400px){#image{width:auto;height:auto}}</style><body style='margin:0'><img id='image' src='https://example.com/responsive.png'></body>";
    let mut r = GpuEmailRenderer::default();
    r.set_email(prepare_email_html_with_provider(html, Some(provider.clone())).unwrap());
    r.render_cpu_if_needed(520, 400, 1.0).unwrap();
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::new(120, 80)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    provider.deliver("responsive.png", png.into_inner());
    for (width, expected) in [
        (520, (60.0, 30.0)),
        (320, (120.0, 80.0)),
        (520, (60.0, 30.0)),
    ] {
        r.render_cpu_if_needed(width, 400, 1.0).unwrap();
        let doc = &r.email.as_ref().unwrap().document;
        let size = doc
            .get_node(doc.get_element_by_id("image").unwrap())
            .unwrap()
            .final_layout()
            .size;
        assert_eq!((size.width, size.height), expected);
    }
}

#[test]
fn transformed_image_delivery_repaints_cached_tiles_without_relayout() {
    let provider = Arc::new(DeferredImages::default());
    let html = "<body style='margin:0'><div style='height:600px'></div><div style='transform:translateY(-200px)'><img src='https://example.com/moved.png' width='90' height='60'></div><div style='height:1400px'></div></body>";
    let mut r = GpuEmailRenderer::default();
    r.set_email(prepare_email_html_with_provider(html, Some(provider.clone())).unwrap());
    r.render_cpu_if_needed(520, 900, 1.0).unwrap();
    let layouts = r.layout_count;
    let tiles = r.tile_count;
    let retained = r.tiles.len() as u64;
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(90, 60, image::Rgba([0, 180, 0, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    provider.deliver("moved.png", png.into_inner());
    r.render_cpu_if_needed(520, 900, 1.0).unwrap();
    assert_eq!(r.layout_count, layouts);
    assert_eq!(r.tile_count, tiles + retained);
    assert!(
        r.tiles[&0]
            .image
            .to_rgba8()
            .unwrap()
            .as_slice()
            .iter()
            .any(|p| p.g > 150 && p.r < 10 && p.b < 10)
    );
}

#[test]
fn downsampled_images_keep_intrinsic_layout_and_background_scale() {
    for format in [image::ImageFormat::Jpeg, image::ImageFormat::Png] {
        let provider = Arc::new(DeferredImages::default());
        let html = "<body style='margin:0'><img id='natural' src='https://example.com/large'><div id='background' style='height:100px;width:600px;background-image:url(https://example.com/background);background-size:600px 100px;background-repeat:no-repeat'></div></body>";
        let mut r = GpuEmailRenderer::default();
        r.set_email(prepare_email_html_with_provider(html, Some(provider.clone())).unwrap());
        let mut bytes = std::io::Cursor::new(Vec::new());
        let original = image::RgbImage::from_fn(3000, 300, |x, _| {
            if x < 1500 {
                image::Rgb([230, 0, 0])
            } else {
                image::Rgb([0, 0, 230])
            }
        });
        original.write_to(&mut bytes, format).unwrap();
        provider.deliver("large", bytes.get_ref().clone());
        provider.deliver("background", bytes.into_inner());
        let frame = r.render_cpu_if_needed(700, 700, 1.0).unwrap().unwrap();
        let doc = &r.email.as_ref().unwrap().document;
        let node = doc
            .get_node(doc.get_element_by_id("natural").unwrap())
            .unwrap();
        let decoded = node.element_data().unwrap().raster_image_data().unwrap();
        assert_eq!((decoded.width, decoded.height), (3000, 300));
        assert!(decoded.pixel_width <= 2048);
        assert_eq!(
            decoded.data.len(),
            (decoded.pixel_width * decoded.pixel_height * 4) as usize
        );
        // Blitz fits this auto-sized replaced element to available width.
        // Its original aspect ratio must survive decoder scaling.
        let size = node.final_layout().size;
        assert_eq!(size.width, 700.0);
        assert!((size.height - 70.0).abs() <= 1.0);
        let background = doc
            .get_node(doc.get_element_by_id("background").unwrap())
            .unwrap();
        let sample_y = (background.absolute_position(0.0, 0.0).y + 50.0) as usize;
        let pixels = frame.tiles[0].image.to_rgba8().unwrap();
        let red = pixels.as_slice()[sample_y * pixels.width() as usize + 100];
        let blue = pixels.as_slice()[sample_y * pixels.width() as usize + 500];
        assert!(red.r > 180 && red.b < 30, "{format:?}: left background");
        assert!(blue.b > 180 && blue.r < 30, "{format:?}: right background");
    }
}

#[test]
fn culled_text_tiles_match_a_full_surface() {
    for (name, style) in [
        ("cached-lines", "font-family:'DejaVu Sans';font-size:16px"),
        ("ordinary", ""),
        (
            "overhang",
            "font-style:italic;line-height:12px;font-size:24px",
        ),
        (
            "transformed",
            "transform:rotate(3deg);transform-origin:top left",
        ),
        ("filtered", "filter:opacity(0.8)"),
    ] {
        let text = if name == "cached-lines" {
            "Accents ÅÉgj <b>bold</b> <i>italic</i> café <span style='background:#cfc'>highlight</span> "
        } else {
            "Accents ÅÉgj <b>bold</b> <i>italic</i> <u>underlined</u> café 日本語 😀 <span style='background:#cfc'>highlight</span> "
        };
        let html = format!(
            "<body style='margin:0'><p style='{style}'>{}</p></body>",
            text.repeat(65)
        );
        for scale in [1.0, 1.25] {
            let mut email = prepare_email_html(&html).unwrap();
            let tiled = render_prepared_cpu(&mut email, 520, 900, scale).unwrap();
            if name == "cached-lines" {
                assert!(email.paint_cache.line_count() >= 16);
            }
            let mut full = render_to_buffer::<VelloCpuImageRenderer, _>(
                |scene| {
                    paint_scene(
                        scene,
                        &mut email.document,
                        scale as f64,
                        tiled.width,
                        tiled.height,
                        0,
                        0,
                    )
                },
                tiled.width,
                tiled.height,
            );
            composite_over_white(&mut full);
            let mut offset = 0;
            for tile in &tiled.tiles {
                let pixels = tile.image.to_rgba8().unwrap();
                let bytes = pixels.as_bytes();
                let reference = &full[offset..offset + bytes.len()];
                let different = bytes
                    .iter()
                    .zip(reference)
                    .filter(|(a, b)| a.abs_diff(**b) > 2)
                    .count();
                assert_eq!(different, 0, "{name} at {scale}, tile offset {offset}");
                offset += bytes.len();
            }
            assert_eq!(offset, full.len());
        }
    }
}

#[test]
fn ctrl_a_unicode_shift_caret_and_selection_restore() {
    let mut r = renderer(BODY);
    r.handle_key_event(
        "a",
        true,
        false,
        InputModifiers::new(true, false, false, false),
    );
    assert_eq!(
        r.selected_text().as_deref(),
        Some("Cafe\u{301} hello linked world")
    );
    r.handle_key_event(
        "\u{f702}",
        true,
        false,
        InputModifiers::new(false, true, false, false),
    );
    assert_eq!(
        r.selected_text().as_deref(),
        Some("Cafe\u{301} hello linked worl")
    );
    let bookmark = r.selection_bookmark();
    r.set_email(prepare_email_html(BODY).unwrap());
    r.restore_selection(bookmark);
    assert_eq!(
        r.selected_text().as_deref(),
        Some("Cafe\u{301} hello linked worl")
    );
    let (start, end) = r.selection_carets();
    assert!(start.valid && end.valid && end.x > start.x);
    r.clear_selection();
    r.handle_pointer_event(15.0, 10.0, "long-press", InputModifiers::default());
    r.handle_pointer_event(15.0, 10.0, "up", InputModifiers::default());
    assert_eq!(r.selected_text().as_deref(), Some("Cafe\u{301}"));
}

#[test]
fn links_include_images_padding_transforms_and_exclude_hidden() {
    let mut r = renderer(
        "<body style='margin:0'><a href='https://example.com/image'><img width='100' height='40' alt='Shop'></a><div><a style='display:inline-block;padding:20px' href='https://example.com/button'>Go</a></div><p style='transform:translateX(100px)'><a href='https://example.com/shift'>Shifted</a></p><a style='visibility:hidden' href='https://example.com/hidden'>Hidden</a></body>",
    );
    let links = &r.email.as_ref().unwrap().links;
    assert_eq!(links.len(), 3, "{links:?}");
    let img = links.iter().find(|l| l.url.ends_with("image")).unwrap();
    assert!(img.width * 520.0 >= 99.0);
    assert_eq!(
        r.link_at(20.0, 20.0).as_deref(),
        Some("https://example.com/image")
    );
    let button = links.iter().find(|l| l.url.ends_with("button")).unwrap();
    let (x, y) = (button.x * 520.0 + 3.0, button.y * 520.0 + 3.0);
    assert_eq!(
        r.link_at(x, y).as_deref(),
        Some("https://example.com/button")
    );
    let shifted = links.iter().find(|l| l.url.ends_with("shift")).unwrap();
    assert!(shifted.x * 520.0 >= 99.0, "{shifted:?}");
    r.handle_pointer_event(x, y, "down", InputModifiers::default());
    r.handle_pointer_event(x, y, "up", InputModifiers::default());
    assert_eq!(
        r.take_activation().as_deref(),
        Some("https://example.com/button")
    );
}

#[test]
fn dragging_from_link_selects_without_navigation() {
    let mut r = renderer(BODY);
    let link = r.email.as_ref().unwrap().links[0].clone();
    let (x, y) = (link.x * 520.0 + 2.0, link.y * 520.0 + 10.0);
    r.handle_pointer_event(x, y, "down", InputModifiers::default());
    r.handle_pointer_event(x + 60.0, y, "move", InputModifiers::default());
    r.handle_pointer_event(x + 60.0, y, "up", InputModifiers::default());
    assert!(r.has_selection());
    assert!(r.take_activation().is_none());
}

#[test]
fn copy_preserves_inline_adjacency_blocks_pre_and_alt() {
    let r = renderer(
        "<span style='display:none'>HIDDEN</span><p>hel<b>lo</b>, world</p><p>Second<br>line</p><pre>a  b\nc  d</pre><img alt='Chart'>",
    );
    assert_eq!(
        r.plain_text(),
        "hello, world\n\nSecond\nline\n\na  b\nc  d\n\nChart"
    );
}

#[test]
fn formatted_selection_keeps_safe_links_and_emphasis() {
    let mut r = renderer("<p>A <b>bold</b> <a href='https://example.com'>link</a></p>");
    r.select_all();
    let html = r.selected_html();
    assert!(html.contains("<strong>bold</strong>"), "{html}");
    assert!(html.contains("href=\"https://example.com/\""), "{html}");
    assert!(!html.contains("<style"));
}

#[test]
fn first_frame_reuses_prepared_layout_and_selection_only_repaints_changed_tiles() {
    let mut r = renderer(&format!(
        "<body style='margin:0'>{}</body>",
        "<p style='height:110px;margin:0'>Some selectable words</p>".repeat(30)
    ));
    r.render_cpu_if_needed(520, 900, 1.0).unwrap();
    assert_eq!(r.layout_count, 1);
    let tiles = r.tile_count;
    r.handle_pointer_event(3.0, 8.0, "down", InputModifiers::default());
    r.handle_pointer_event(70.0, 8.0, "move", InputModifiers::default());
    r.render_cpu_if_needed(520, 900, 1.0).unwrap();
    assert_eq!(r.layout_count, 1);
    assert_eq!(
        r.tile_count,
        tiles + 1,
        "Only the first tile contains changed selection"
    );
    assert!(r.render_cpu_if_needed(520, 900, 1.0).unwrap().is_none());
}

#[test]
fn wide_mail_and_zoom_keep_links_and_overflow_reachable() {
    let mut r = renderer(
        "<body style='margin:0'><div style='width:900px'><a style='display:block;margin-left:800px' href='https://example.com'>Edge</a></div></body>",
    );
    let frame = r.render_cpu_if_needed(520, 400, 1.0).unwrap().unwrap();
    assert!(frame.width >= 900);
    assert!(r.link_at(810.0, 8.0).is_some());
    r.set_zoom(0.5);
    let frame = r.render_cpu_if_needed(520, 400, 1.0).unwrap().unwrap();
    assert_eq!(frame.width, 520);
    assert!(r.link_at(405.0, 4.0).is_some());
}

#[test]
fn find_handles_unicode_cycles_and_decoded_fragments() {
    let mut r = renderer("<body><p>İstanbul café CAFÉ</p><p id='café'>Target</p></body>");
    assert_eq!(r.find("café", 0).1, 2);
    assert_eq!(r.selected_text().as_deref(), Some("café"));
    assert_eq!(r.find("café", 1).0, 2);
    assert_eq!(r.selected_text().as_deref(), Some("CAFÉ"));
    assert_eq!(r.find("i", 0).1, 1);
    assert_eq!(r.selected_text().as_deref(), Some("İ"));
    assert!(r.fragment_y("#caf%C3%A9").is_some());
    r.find("", 0);
    assert!(!r.has_selection());
}

#[test]
fn premultiplied_alpha_matches_white_canvas() {
    let mut email=prepare_email_html("<body style='margin:0'><div style='width:50px;height:50px;background:rgba(255,0,0,0.5)'></div></body>").unwrap();
    let mut pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut email.document, 1.0, 100, 100, 0, 0),
        100,
        100,
    );
    composite_over_white(&mut pixels);
    let pixel = &pixels[(10 * 100 + 10) * 4..(10 * 100 + 10) * 4 + 4];
    assert_eq!(pixel[0], 255);
    assert!((126..=129).contains(&pixel[1]));
    assert_eq!(pixel[3], 255);
}

#[test]
fn deeply_nested_and_malformed_messages_have_readable_fallbacks() {
    let r = renderer(&format!(
        "{}Readable{}",
        "<div>".repeat(200),
        "</div>".repeat(200)
    ));
    assert!(r.notice.is_some());
    assert!(r.plain_text().contains("Readable"));
    for html in [
        "<table><td><a>oops</table>tail",
        "<style>p{width:calc(0px / 0)}</style><p>text",
        "<div style='transform:matrix(0,0,0,0,0,0)'>zero</div>",
    ] {
        let mut r = renderer(html);
        assert!(r.render_cpu_if_needed(320, 480, 1.0).is_ok());
    }
}

#[test]
fn document_replacement_aborts_old_resource_generation() {
    let mut r = renderer("<p>First</p>");
    let signal = r
        .email
        .as_ref()
        .unwrap()
        .abort
        .as_ref()
        .unwrap()
        .signal
        .clone();
    r.set_email(prepare_email_html("<p>Next</p>").unwrap());
    assert!(signal.aborted());
}

#[test]
fn narrow_responsive_tables_keep_authored_block_display() {
    let mut r = renderer(
        "<style>@media(max-width:400px){table,tbody,tr,td{display:block;width:100%}}</style><table width='600'><tr><td id='left'>Left</td><td id='right'>Right</td></tr></table>",
    );
    r.render_cpu_if_needed(320, 480, 1.0).unwrap();
    let doc = &r.email.as_ref().unwrap().document;
    let left = doc
        .get_node(doc.query_selector("#left").unwrap().unwrap())
        .unwrap();
    let right = doc
        .get_node(doc.query_selector("#right").unwrap().unwrap())
        .unwrap();
    assert!(right.absolute_position(0.0, 0.0).y > left.absolute_position(0.0, 0.0).y);
}

#[test]
fn clipped_and_singular_links_do_not_activate_in_invisible_areas() {
    let r = renderer(
        "<body style='margin:0'><div style='width:50px;height:20px;overflow:hidden'><a style='display:block;margin-left:100px' href='https://example.com/clipped'>Invisible</a></div><div style='transform:scale(0)'><a href='https://example.com/zero'>Zero</a></div><p>Visible</p></body>",
    );
    assert!(r.link_at(105.0, 10.0).is_none());
    assert!(r.email.as_ref().unwrap().links.is_empty());
}

#[test]
fn retina_and_zoom_geometry_stays_in_logical_coordinates() {
    let html = "<body style='margin:0'><p style='margin:0;font-size:20px'><a href='https://example.com'>Scale this link</a></p></body>";
    let mut r = renderer(html);
    r.render_cpu_if_needed(520, 400, 1.0).unwrap();
    r.select_all();
    let base = r.selection_carets().1.x;
    let width = r.email.as_ref().unwrap().links[0].width * 520.0;
    r.render_cpu_if_needed(520, 400, 2.0).unwrap();
    assert!((r.selection_carets().1.x - base).abs() < 2.0);
    assert!((r.email.as_ref().unwrap().links[0].width * 520.0 - width).abs() < 2.0);
    r.set_zoom(1.2);
    r.render_cpu_if_needed(520, 400, 2.0).unwrap();
    assert!((r.selection_carets().1.x - base * 1.2).abs() < 2.0);
}

/// Run alone to keep unrelated tests out of the process RSS measurements.
#[test]
#[ignore = "isolated memory and latency probe"]
fn repeated_open_memory_probe() {
    fn rss_kib() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|line| line.starts_with("VmRSS:"))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|n| n.parse().ok())
            })
            .unwrap_or(0)
    }
    let mut renderer = GpuEmailRenderer::default();
    let mut timings = Vec::new();
    let mut warm_rss = 0;
    for index in 0..160 {
        let html = format!("<body><h1>Message {index}</h1>{}</body>",
            "<p>Selectable <b>message content</b> with <a href='https://example.com'>a link</a>.</p>".repeat(80));
        let start = Instant::now();
        renderer.clear();
        renderer.set_email(prepare_email_html(&html).unwrap());
        let frame = renderer.render_cpu_if_needed(700, 700, 1.0).unwrap();
        drop(frame);
        assert!(
            renderer.tiles.len() <= 4,
            "only viewport tiles may be retained"
        );
        let id = renderer.email.as_ref().unwrap().document.id();
        renderer.resources.lock().unwrap().insert(
            crate::remote::resource_key(
                id,
                &format!("data:image/png;base64,{}", "A".repeat(1_000_000)),
            ),
            crate::remote::ResourceState::Ready,
        );
        assert_eq!(renderer.resources.lock().unwrap().len(), 1);
        if index == 39 {
            warm_rss = rss_kib();
        }
        if index >= 40 {
            timings.push(start.elapsed().as_millis());
        }
    }
    let final_rss = rss_kib();
    timings.sort_unstable();
    eprintln!(
        "repeated-open: 160 messages; warm RSS {warm_rss} KiB; final RSS {final_rss} KiB; median {} ms; p95 {} ms",
        timings[timings.len() / 2],
        timings[timings.len() * 95 / 100]
    );
    if warm_rss > 0 {
        assert!(
            final_rss < warm_rss + 24 * 1024,
            "memory must stabilize after warmup"
        );
    }
    renderer.clear();
    assert!(renderer.tiles.is_empty());
    assert!(renderer.resources.lock().unwrap().is_empty());
}

#[test]
fn marketing_precision_templates_render_text_and_links_without_fallback() {
    for (name, html, phrase) in [
        (
            "revolut",
            include_str!("../../resources/test-emails/revolut-precision.html"),
            "Te quedan 7 días",
        ),
        (
            "mailersend",
            include_str!("../../resources/test-emails/mailersend-precision.html"),
            "Dear customer",
        ),
    ] {
        let prepared = prepare_email_html(html).unwrap();
        assert!(prepared.notice.is_none(), "{name}: {:?}", prepared.notice);
        assert!(
            prepared.plain_text.contains(phrase),
            "{name}: {}",
            prepared.plain_text
        );
        assert!(!prepared.links.is_empty(), "{name} needs actionable links");
        let mut renderer = GpuEmailRenderer::default();
        renderer.set_email(prepared);
        let frame = renderer
            .render_cpu_if_needed(700, 700, 1.0)
            .unwrap()
            .unwrap();
        let pixels = frame.tiles[0].image.to_rgba8().unwrap();
        let ink = pixels
            .as_slice()
            .iter()
            .filter(|p| p.a > 0 && (p.r < 200 || p.g < 200 || p.b < 200))
            .count();
        assert!(ink > 100, "{name}: first viewport must not be blank");
        std::fs::create_dir_all("tmp/render-followup").unwrap();
        image::save_buffer(
            format!("tmp/render-followup/{name}.png"),
            pixels.as_bytes(),
            pixels.width(),
            pixels.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}

#[test]
fn complex_template_recovery_paints_text_in_the_first_viewport() {
    let html = format!(
        "<html><body>{}<div style='width:1e12px'>Readable recovery text</div>{}<p>Next paragraph</p></body></html>",
        "\n ".repeat(500),
        "\n ".repeat(500)
    );
    let mut renderer = renderer(&html);
    assert!(renderer.notice.is_some());
    let frame = renderer
        .render_cpu_if_needed(700, 700, 1.0)
        .unwrap()
        .unwrap();
    let pixels = frame.tiles[0].image.to_rgba8().unwrap();
    assert!(
        pixels
            .as_slice()
            .iter()
            .filter(|p| p.a > 0 && p.r < 200)
            .count()
            > 100
    );
    assert!(
        renderer
            .email
            .as_ref()
            .unwrap()
            .plain_text
            .contains("Readable recovery text")
    );
    std::fs::create_dir_all("tmp/render-followup").unwrap();
    image::save_buffer(
        "tmp/render-followup/fallback.png",
        pixels.as_bytes(),
        pixels.width(),
        pixels.height(),
        image::ColorType::Rgba8,
    )
    .unwrap();
}

#[test]
fn floated_inline_content_finishes_layout_and_remains_visible() {
    const CHILD: &str = "FLECTAR_FLOAT_LAYOUT_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // An infinite line-breaking loop must fail this regression, not hang
        // the entire test suite. Keep the renderer in a killable subprocess.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "renderer::regression::floated_inline_content_finishes_layout_and_remains_visible",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .spawn()
            .unwrap();
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "float layout regression failed: {status}");
                return;
            }
            if start.elapsed() > Duration::from_secs(20) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("floated inline content stalled during layout or painting");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    for attributes in [
        "align='left'",
        "align='right'",
        "style='float:left'",
        "style='float:right'",
    ] {
        // Surrounding text puts the floated table in an inline formatting
        // context, as whitespace/nbsp and aligned tables do in newsletters.
        let html = format!(
            "<body style='margin:0'><table width='100%'><tr><td id='container'>Before \
             <table id='float' width='140' {attributes}><tr><td style='background:#008000;padding:12px'>\
             <a href='https://example.com/offer'>Visible offer</a>\
             </td></tr></table> After</td></tr></table></body>"
        );
        for html in [html.clone(), flectar_mail_core::mime::sanitize_html(&html)] {
            let mut r = renderer(&html);
            assert!(r.notice.is_none());
            for phrase in ["Before", "Visible offer", "After"] {
                assert!(r.plain_text().contains(phrase), "{attributes}: {phrase}");
            }
            for width in [520, 900, 390] {
                let frame = r.render_cpu_if_needed(width, 700, 1.0).unwrap().unwrap();
                let link = frame
                    .links
                    .iter()
                    .find(|l| l.url.ends_with("/offer"))
                    .unwrap();
                assert!(link.width > 0.0 && link.height > 0.0);
                let document = &r.email.as_ref().unwrap().document;
                let container = document
                    .get_node(document.get_element_by_id("container").unwrap())
                    .unwrap();
                let floated = document
                    .get_node(document.get_element_by_id("float").unwrap())
                    .unwrap();
                let container_layout = container.final_layout();
                let left = container.absolute_position(0.0, 0.0).x
                    + container_layout.border.left
                    + container_layout.padding.left;
                let right = container.absolute_position(0.0, 0.0).x + container_layout.size.width
                    - container_layout.border.right
                    - container_layout.padding.right;
                let float_left = floated.absolute_position(0.0, 0.0).x;
                let aligned = if attributes.contains("left") {
                    float_left - left
                } else {
                    float_left + floated.final_layout().size.width - right
                };
                assert!(
                    aligned.abs() <= 1.0,
                    "{attributes} at {width}px: wrong float edge ({aligned})"
                );
                let pixels = frame.tiles[0].image.to_rgba8().unwrap();
                let green = pixels
                    .as_slice()
                    .iter()
                    .filter(|p| p.a > 0 && p.r < 30 && (100..160).contains(&p.g) && p.b < 30)
                    .count();
                assert!(green > 500, "{attributes}: floated table must be painted");
            }
        }
    }
    float_text_wrap_and_clear();
    float_responsive_columns_and_auto_width();
}

fn float_text_wrap_and_clear() {
    for direction in ["left", "right"] {
        let html = format!(
            "<body style='margin:0'><div id='root' style='display:flow-root;width:300px;padding:16px'>\
             <div id='float' style='float:{direction};width:80px;height:68px;margin:0 8px;background:green'></div>\
             <p id='copy' style='margin:0;font:16px/20px sans-serif'>{}</p>\
             <div id='last-float' style='float:right;width:80px;height:120px'></div>\
             <div id='cleared' style='clear:both;height:8px'></div></div></body>",
            "Words wrapping around the image and continuing below it. ".repeat(5)
        );
        let mut r = renderer(&html);
        for zoom in [1.0, 1.25] {
            r.set_zoom(zoom);
            r.render_cpu_if_needed(520, 700, 1.0).unwrap();
            let document = &r.email.as_ref().unwrap().document;
            let node = |id| {
                document
                    .get_node(document.get_element_by_id(id).unwrap())
                    .unwrap()
            };
            let floated = node("float");
            let pos = floated.absolute_position(0.0, 0.0);
            let bottom = pos.y + floated.final_layout().size.height;
            let copy = node("copy");
            let origin = copy.absolute_position(0.0, 0.0);
            let inline = copy
                .element_data()
                .unwrap()
                .inline_layout_data
                .as_ref()
                .unwrap();
            let mut beside = false;
            let mut below = false;
            for line in inline.layout.lines() {
                let y = origin.y + line.metrics().block_min_coord / inline.layout.scale();
                if y >= bottom {
                    below = true;
                    continue;
                }
                beside = true;
                for item in line.items() {
                    if let PositionedLayoutItem::GlyphRun(run) = item {
                        let left = origin.x + run.offset() / inline.layout.scale();
                        let right = left + run.advance() / inline.layout.scale();
                        assert!(
                            if direction == "left" {
                                left >= pos.x + 80.0 + 7.0
                            } else {
                                right <= pos.x - 7.0
                            },
                            "{direction}: text overlaps float at zoom {zoom}: {left}..{right}"
                        );
                    }
                }
            }
            assert!(
                beside && below,
                "text must wrap beside and then below the float"
            );
            let last = node("last-float");
            let cleared = node("cleared").absolute_position(0.0, 0.0).y;
            assert!(
                cleared
                    >= last.absolute_position(0.0, 0.0).y + last.final_layout().size.height - 1.0
            );
            let root = node("root");
            assert!(
                root.absolute_position(0.0, 0.0).y + root.final_layout().size.height
                    >= cleared + 8.0
            );
        }
    }
}

fn float_responsive_columns_and_auto_width() {
    let mut r = renderer(
        "<html><head><style>
         .column { width:140px; height:80px; }
         @media (max-width:450px) { .column { float:none !important; width:100%; } }
         </style></head><body style='margin:0'>
         <div id='columns' style='display:flow-root'>
         <table id='left' class='column' align='left'><tr><td>Left column</td></tr></table>
         <table id='right' class='column' align='right'><tr><td>Right column</td></tr></table>
         </div>
         <div id='auto-container' style='display:flow-root;width:240px'>Before
         <span id='auto-float' style='float:left;background:green'>
         A floated box with enough ordinary words to need several lines in a narrow container.
         </span>After</div></body></html>",
    );
    // Resize both ways: media-query changes must invalidate float placement.
    for width in [520, 390, 900] {
        r.render_cpu_if_needed(width, 700, 1.0).unwrap();
        let document = &r.email.as_ref().unwrap().document;
        let node = |id| {
            document
                .get_node(document.get_element_by_id(id).unwrap())
                .unwrap()
        };
        let left = node("left");
        let right = node("right");
        let lp = left.absolute_position(0.0, 0.0);
        let rp = right.absolute_position(0.0, 0.0);
        if width > 450 {
            assert!((lp.y - rp.y).abs() <= 1.0, "columns must share a row");
            assert!((rp.x + right.final_layout().size.width - width as f32).abs() <= 1.0);
        } else {
            assert!(rp.y >= lp.y + left.final_layout().size.height - 1.0);
            assert!((right.final_layout().size.width - width as f32).abs() <= 1.0);
        }
        let auto = node("auto-float");
        assert!(
            auto.final_layout().size.width <= 241.0,
            "auto-width floats must shrink to fit"
        );
        assert!(
            auto.final_layout().size.height > 30.0,
            "auto-width float text must wrap"
        );
    }
}

#[test]
fn auto_fit_tracks_resize_and_preserves_explicit_zoom() {
    let mut r = renderer(
        "<body style='margin:0'><div style='width:900px;height:80px'>Wide newsletter</div></body>",
    );
    r.set_auto_fit(true);
    r.render_cpu_if_needed(600, 400, 1.0).unwrap();
    assert!((r.zoom - 600.0 / 900.0).abs() < 0.02);
    assert!(r.layout_width <= 601.0);
    r.render_cpu_if_needed(800, 400, 1.0).unwrap();
    assert!((r.zoom - 800.0 / 900.0).abs() < 0.02);
    r.render_cpu_if_needed(1100, 400, 1.0).unwrap();
    assert_eq!(r.zoom, 1.0);
    assert!(r.render_cpu_if_needed(1100, 400, 1.0).unwrap().is_none());
    r.render_cpu_if_needed(300, 500, 1.0).unwrap();
    assert_eq!(r.zoom, MIN_EMAIL_ZOOM);
    assert!(
        r.layout_width > 300.0,
        "Overflow below the readable zoom floor must remain scrollable"
    );
    r.set_auto_fit(false);
    r.set_zoom(1.25);
    r.render_cpu_if_needed(600, 400, 1.0).unwrap();
    assert_eq!(r.zoom, 1.25);
    r.set_auto_fit(true);
    r.render_cpu_if_needed(600, 400, 1.0).unwrap();
    assert!(r.zoom < 1.0);
    r.set_email(
        prepare_email_html("<body style='margin:0'><p>Responsive message</p></body>").unwrap(),
    );
    r.render_cpu_if_needed(600, 400, 1.0).unwrap();
    assert_eq!(r.zoom, 1.0);
}

#[test]
fn long_table_content_reflows_at_readable_zoom() {
    let identifier = "account.notification.delivery.preference.identifier".repeat(4);
    let mut html = String::from("<body style='margin:0'>");
    for index in 0..252 {
        html.push_str(&format!(
            "<h3><code>{identifier}.{index}</code></h3><table><tr><th>translation</th><th>last changed by</th></tr><tr><td>{identifier}</td><td>Contributor, 2026-09-20</td></tr></table>"
        ));
    }
    html.push_str("</body>");

    let mut r = renderer(&html);
    assert!(
        !r.prefers_software_rendering(),
        "large but ordinary structured mail should remain GPU eligible"
    );
    r.set_auto_fit(true);
    r.set_visible_region(0.0, 700.0);
    let frame = r
        .render_cpu_if_needed(560, 700, 1.0)
        .unwrap()
        .expect("large structured message should render");

    assert_eq!(
        r.zoom, 1.0,
        "ordinary structured text should reflow, not shrink"
    );
    assert!(
        r.layout_width <= 561.0,
        "layout overflowed: {}",
        r.layout_width
    );
    assert_eq!(frame.width, 560);
    assert!(frame.height > 10_000, "the complete issue body must remain reachable");
}
