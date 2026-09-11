use super::*;

fn renderer(html: &str) -> GpuEmailRenderer {
    let mut r = GpuEmailRenderer::default();
    r.set_email(prepare_email_html(html).unwrap());
    r
}
const BODY: &str = "<body style='margin:0'><p style='margin:0;font-size:20px'>Cafe\u{301} hello <a href='https://example.com'>linked world</a></p></body>";

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
    assert!((r.zoom - 300.0 / 900.0).abs() < 0.02);
    assert!(
        r.layout_width <= 301.0,
        "Automatic fit must accommodate phone widths"
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
