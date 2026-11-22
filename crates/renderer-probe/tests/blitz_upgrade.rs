//! Behavioral coverage for the overlap between upstream layout and our email patches.
use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{DocumentConfig, Node};
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};

fn document(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        &format!("<style>body{{margin:0}}td{{padding:0}}</style>{html}"),
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn node<'a>(doc: &'a HtmlDocument, selector: &str) -> &'a Node {
    doc.get_node(doc.query_selector(selector).unwrap().unwrap())
        .unwrap()
}

fn close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() <= 1.0, "{actual} != {expected}");
}

#[test]
fn fixed_columns_keep_upstream_percentage_spacing_and_col_precedence() {
    let doc = document(
        "<table style='table-layout:fixed;width:300px;border-spacing:10px'>
         <col style='width:25%'><col>
         <tr><td id='a' style='width:200px;height:20px'></td><td id='b'></td></tr></table>",
    );
    close(node(&doc, "#a").final_layout().size.width, 67.5);
    close(node(&doc, "#b").final_layout().size.width, 202.5);
}

#[test]
fn fixed_cell_percentages_include_padding_and_leave_auto_tracks_flexible() {
    let doc = document(
        "<table style='table-layout:fixed;width:300px;border-spacing:10px'>
         <tr><td id='a' style='width:25%;padding:0 10px;height:20px'></td>
         <td id='b'></td></tr></table>",
    );
    close(node(&doc, "#a").final_layout().size.width, 87.5);
    close(node(&doc, "#b").final_layout().size.width, 182.5);
}

#[test]
fn table_row_order_and_unconstrained_height_survive_the_local_collector() {
    let doc = document(
        "<table style='height:100px;border-spacing:0'><tfoot><tr><td id='foot' style='height:10px'></td></tr></tfoot>
         <tbody><tr><td id='body'><div style='height:10px'></div></td></tr></tbody>
         <thead><tr><td id='head' style='height:20px'></td></tr></thead></table>",
    );
    let head = node(&doc, "#head").final_layout();
    let body = node(&doc, "#body").final_layout();
    let foot = node(&doc, "#foot").final_layout();
    close(head.location.y, 0.0);
    close(head.size.height, 20.0);
    close(body.location.y, 20.0);
    close(body.size.height, 70.0);
    close(foot.location.y, 90.0);
}

#[test]
fn collapsed_cell_alignment_preserves_authored_style_and_repeated_layout() {
    let mut doc = document(
        "<table style='border-collapse:collapse;width:200px'><tr style='height:100px'>
         <td id='cell' style='vertical-align:bottom;border:4px solid red;padding:5px'>
         <div id='content' style='width:20px;height:20px'></div></td></tr></table>",
    );
    for width in [400, 320, 400] {
        doc.set_viewport(Viewport::new(width, 300, 1.0, ColorScheme::Light));
        doc.resolve(0.0);
        let cell = node(&doc, "#cell");
        let content = node(&doc, "#content");
        close(cell.final_layout().border.bottom, 2.0);
        close(content.final_layout().location.y, 73.0);
        let style = cell.primary_styles().unwrap();
        close(style.get_border().border_bottom_width.0.to_f32_px(), 4.0);
        close(style.clone_padding_top().0.to_length().unwrap().px(), 5.0);
    }
}

#[test]
fn rowgroup_background_and_absolute_children_remain_visible() {
    let mut doc = document(
        "<table style='border-spacing:0;position:relative;width:100px'>
         <tbody style='background:rgb(20,180,40)'><tr><td style='height:60px'>
         <div id='positioned' style='position:absolute;right:0;top:0;width:10px;height:10px;background:red'></div>
         </td></tr></tbody></table>",
    );
    let positioned = node(&doc, "#positioned").final_layout();
    close(positioned.location.x, 90.0);
    close(positioned.location.y, 0.0);
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| {
            blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0);
        },
        400,
        300,
    );
    let at = |x: usize, y: usize| &pixels[(y * 400 + x) * 4..(y * 400 + x) * 4 + 4];
    assert_eq!(at(20, 20), &[20, 180, 40, 255]);
    assert_eq!(at(95, 5), &[255, 0, 0, 255]);
    // Re-resolving must not retain any transient table-cell style override.
    doc.resolve(0.0);
}

#[test]
fn language_hints_keep_xml_precedence_and_locale_sensitive_transform() {
    let doc = document(
        "<p id='turkish' lang='tr' style='text-transform:uppercase'>i</p>
         <svg><text id='override' lang='en' xml:lang='tr'>i</text></svg>",
    );
    assert_eq!(
        node(&doc, "#override")
            .primary_styles()
            .unwrap()
            .clone__x_lang()
            .0
            .to_string(),
        "tr"
    );
    let turkish = node(&doc, "#turkish");
    assert_eq!(
        turkish
            .primary_styles()
            .unwrap()
            .clone__x_lang()
            .0
            .to_string(),
        "tr"
    );
    assert_eq!(
        turkish
            .element_data()
            .unwrap()
            .inline_layout_data
            .as_ref()
            .unwrap()
            .text,
        "İ"
    );
}

#[test]
fn positioned_table_part_does_not_become_a_grid_cell() {
    let doc = document(
        "<div style='display:table;position:relative;width:100px;border-spacing:0'>
         <div style='display:table-row'>
         <div id='cell' style='display:table-cell;height:40px'></div>
         <div id='positioned' style='position:absolute;right:0;top:0;width:10px;height:10px'></div>
         </div></div>",
    );
    close(node(&doc, "#cell").final_layout().size.width, 100.0);
    close(node(&doc, "#positioned").final_layout().location.x, 90.0);
}

#[test]
fn rtl_collapsed_edges_keep_individual_colors_and_hidden_precedence() {
    let mut doc = document(
        "<table dir='rtl' style='border-collapse:collapse;width:100px'>
         <tr><td id='first' style='width:50px;height:40px;border:4px solid red'></td>
         <td id='second' style='width:50px;border:4px solid blue;border-right:hidden'></td></tr></table>",
    );
    let first = node(&doc, "#first").final_layout();
    let second = node(&doc, "#second").final_layout();
    assert!(first.location.x > second.location.x);
    // The hidden border wins against the neighboring red border.
    close(first.border.left, 0.0);
    close(second.border.right, 0.0);
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    let colors = pixels.as_chunks::<4>().0;
    assert!(colors.contains(&[255, 0, 0, 255]));
    assert!(colors.contains(&[0, 0, 255, 255]));
}

#[test]
fn scrolled_offscreen_elements_are_not_painted() {
    // The cull box must apply each element's position once. Otherwise blocks far
    // above the viewport still reach the scene after scrolling.
    let blocks = "<div style='height:100px;background:#c00'></div>".repeat(60);
    for scale in [1.0_f32, 2.0] {
        let (width, height) = ((400.0 * scale) as u32, (300.0 * scale) as u32);
        let mut doc = HtmlDocument::from_html(
            &format!("<style>body{{margin:0}}</style>{blocks}"),
            DocumentConfig {
                viewport: Some(Viewport::new(width, height, scale, ColorScheme::Light)),
                ..Default::default()
            },
        );
        doc.resolve(0.0);
        // Scroll as the reader does for each tile: the viewport shows the last
        // three blocks.
        doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 5700.0 });
        let mut scene = anyrender::Scene::default();
        blitz_paint::paint_scene(&mut scene, &mut doc, scale.into(), width, height, 0, 0);
        let fills = scene
            .commands
            .iter()
            .filter(|command| matches!(command, anyrender::recording::RenderCommand::Fill(_)))
            .count();
        assert!(fills <= 6, "scale {scale}: {fills} fills");
    }
}

fn solid_fills(scene: &anyrender::Scene) -> Vec<[u8; 3]> {
    scene
        .commands
        .iter()
        .filter_map(|command| match command {
            anyrender::recording::RenderCommand::Fill(fill) => match &fill.brush {
                anyrender::Paint::Solid(color) => {
                    let color = color.to_rgba8();
                    Some([color.r, color.g, color.b])
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn inline_backgrounds_stop_at_the_enclosing_block() {
    // Text beside a block child is wrapped in an anonymous inline root, but its
    // runs keep their DOM parents. The walk must stop at the first block instead
    // of painting that block's background again behind the text, while every
    // enclosing inline background still paints, outermost first.
    let mut doc = document(
        "<div style='background:rgb(1,2,3)'>Hello <b>world</b>
         <a style='background:rgb(4,5,6)'><span style='background:rgba(7,8,9,0.5)'>Go</span></a>
         <p>Block child</p></div>",
    );
    let mut scene = anyrender::Scene::default();
    blitz_paint::paint_scene(&mut scene, &mut doc, 1.0, 400, 300, 0, 0);
    let fills = solid_fills(&scene);
    let count = |rgb: [u8; 3]| fills.iter().filter(|fill| **fill == rgb).count();
    assert_eq!(count([1, 2, 3]), 1, "{fills:?}");
    let link: Vec<_> = fills
        .iter()
        .filter(|fill| [[4, 5, 6], [7, 8, 9]].contains(*fill))
        .collect();
    assert_eq!(link, [&[4, 5, 6], &[7, 8, 9]], "{fills:?}");
}

#[test]
fn float_slots_never_exceed_the_line_breaker_width() {
    // DioxusLabs/blitz#1443: f32 rounding with huge insets can make a float's
    // line slot wider than the layout, which trips Parley's line-breaker assert.
    let doc = document(
        "<div><div style='width:1000px;float:right'></div>
         <h2 style='padding:0 2147483652%;max-height:0'>x</h2></div>",
    );
    assert!(node(&doc, "h2").final_layout().size.width.is_finite());
}

fn decode_limits() -> blitz_dom::net::ImageDecodeLimits {
    blitz_dom::net::ImageDecodeLimits {
        max_source_dimension: 4096,
        max_source_pixels: 8 * 1024 * 1024,
        max_jpeg_source_dimension: 16_384,
        max_jpeg_source_pixels: 64 * 1024 * 1024,
        max_alloc: 32 * 1024 * 1024,
        target_dimension: 4096,
        target_pixels: 2048 * 2048,
    }
}

fn encoded(format: image::ImageFormat, image: image::DynamicImage) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}

#[test]
fn large_jpegs_keep_the_target_resolution() {
    // Halving this JPEG would leave 1250 px. It fits the allocation limit at
    // full size, so it is decoded whole and reduced to exactly the target.
    let jpeg = encoded(
        image::ImageFormat::Jpeg,
        image::DynamicImage::new_rgb8(2500, 100),
    );
    let limits = blitz_dom::net::ImageDecodeLimits {
        target_dimension: 2048,
        ..decode_limits()
    };
    let decoded = limits.decode(&jpeg).unwrap();
    assert_eq!((decoded.width, decoded.height), (2500, 100));
    assert_eq!(decoded.pixel_width, 2048);
}

#[test]
fn large_progressive_jpegs_are_refused() {
    // Progressive scans keep full-resolution coefficients at any output scale,
    // so they get the generic pixel limit. Only the frame header matters here.
    let mut jpeg = encoded(
        image::ImageFormat::Jpeg,
        image::DynamicImage::new_rgb8(16, 16),
    );
    let sof = jpeg
        .windows(2)
        .position(|marker| marker == [0xFF, 0xC0])
        .unwrap();
    jpeg[sof + 1] = 0xC2;
    jpeg[sof + 5..sof + 7].copy_from_slice(&3000u16.to_be_bytes());
    jpeg[sof + 7..sof + 9].copy_from_slice(&4000u16.to_be_bytes());
    let error = decode_limits().decode(&jpeg).unwrap_err();
    assert!(error.contains("Progressive"), "{error}");
}

#[test]
fn unscaled_jpegs_must_fit_the_allocation_limit() {
    let jpeg = encoded(
        image::ImageFormat::Jpeg,
        image::DynamicImage::new_rgb8(64, 64),
    );
    let limits = blitz_dom::net::ImageDecodeLimits {
        max_alloc: 1024,
        ..decode_limits()
    };
    assert!(limits.decode(&jpeg).is_err());
}

#[test]
fn translucent_images_shrink_without_dark_fringes() {
    // Columns alternate between transparent black and opaque white. Averaging
    // premultiplied colour keeps the visible half white.
    let pixels = image::RgbaImage::from_fn(4096, 2, |x, _| {
        if x % 2 == 0 {
            image::Rgba([0, 0, 0, 0])
        } else {
            image::Rgba([255, 255, 255, 255])
        }
    });
    let png = encoded(
        image::ImageFormat::Png,
        image::DynamicImage::ImageRgba8(pixels),
    );
    let limits = blitz_dom::net::ImageDecodeLimits {
        target_dimension: 2048,
        ..decode_limits()
    };
    let decoded = limits.decode(&png).unwrap();
    assert_eq!(decoded.pixel_width, 2048);
    let first = &decoded.data.data()[..4];
    assert!(
        first[0] > 250 && (100..156).contains(&first[3]),
        "{first:?}"
    );
}

/// Holds image requests until the test delivers them, so a test decides
/// whether pixels arrive before or after the first layout.
#[derive(Default)]
struct HeldImages(std::sync::Mutex<Vec<(String, Box<dyn blitz_traits::net::NetHandler>)>>);

impl blitz_traits::net::NetProvider for HeldImages {
    fn fetch(
        &self,
        _doc_id: usize,
        request: blitz_traits::net::Request,
        handler: Box<dyn blitz_traits::net::NetHandler>,
    ) {
        self.0
            .lock()
            .unwrap()
            .push((request.url.to_string(), handler));
    }
}

fn image_size_after_load(html: &str, image: &[u8], deliver_before_layout: bool) -> (f32, f32) {
    let images = std::sync::Arc::new(HeldImages::default());
    let mut doc = HtmlDocument::from_html(
        &format!("<style>body{{margin:0}} img{{max-width:100%}}</style>{html}"),
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            net_provider: Some(images.clone()),
            ..Default::default()
        },
    );
    if !deliver_before_layout {
        doc.resolve(0.0);
    }
    for (url, handler) in images.0.lock().unwrap().drain(..) {
        handler.bytes(url, blitz_traits::net::Bytes::copy_from_slice(image));
    }
    doc.resolve(0.0);
    let size = node(&doc, "img").final_layout().size;
    (size.width, size.height)
}

#[test]
fn image_layout_does_not_depend_on_load_timing() {
    // Clamped by max-width, this image's height follows its natural aspect
    // ratio. As a flex item, its automatic minimum width does too.
    let cases = [
        (
            "<div style='width:300px'><img src='https://example.com/a.png' width=600 height=300></div>",
            image::DynamicImage::new_rgb8(600, 200),
        ),
        (
            "<div style='display:flex;width:200px'>
             <img src='https://example.com/a.png' style='width:300px;height:100px;max-width:none'></div>",
            image::DynamicImage::new_rgb8(400, 100),
        ),
    ];
    for (html, image) in cases {
        let png = encoded(image::ImageFormat::Png, image);
        assert_eq!(
            image_size_after_load(html, &png, false),
            image_size_after_load(html, &png, true),
            "{html}"
        );
    }
}

/// The first and last pixel rows that contain `color` in a 400px-wide render.
fn rows_with(pixels: &[u8], color: [u8; 4]) -> Option<(usize, usize)> {
    let rows: Vec<usize> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, pixel)| **pixel == color)
        .map(|(index, _)| index / 400)
        .collect();
    Some((*rows.first()?, *rows.last()?))
}

#[test]
fn middle_aligned_cells_move_text_and_floats_once() {
    // The 20px line and the float are the cell's contents. In a 100px row
    // they start 40px down, painted where they were laid out.
    let mut doc = document(
        "<table style='border-spacing:0;font:16px/20px sans-serif'><tr>
         <td style='height:100px;width:50px'></td>
         <td style='vertical-align:middle'><span style='float:left;width:10px;height:10px;background:#00f'></span><span style='background:#f00'>x</span></td>
         </tr></table>",
    );
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    let (text_top, text_bottom) = rows_with(&pixels, [255, 0, 0, 255]).unwrap();
    let (float_top, _) = rows_with(&pixels, [0, 0, 255, 255]).unwrap();
    assert!(
        (text_top + text_bottom).abs_diff(100) <= 4,
        "text rows {text_top}..{text_bottom}"
    );
    assert!(float_top.abs_diff(40) <= 1, "float top {float_top}");
}

#[test]
fn fluid_tables_narrow_columns_with_authored_widths() {
    // Two `width=300` columns in a 400px-wide `width=100%` table: as in
    // browsers, the authored widths are preferred, not minimums, so the
    // columns share the table instead of overflowing it. Each column moves
    // from its min-content width (40px, 100px) toward 300px by the same
    // fraction, (400 - 140) / (600 - 140) (CSS Tables 3 §3.9.3).
    let doc = document(
        "<table id='t' width='100%' style='border-spacing:0'><tr>
         <td id='a' width='300'><span style='display:inline-block;width:40px'></span></td>
         <td id='b' width='300'><span style='display:inline-block;width:100px'></span></td></tr></table>",
    );
    close(node(&doc, "#t").final_layout().size.width, 400.0);
    close(node(&doc, "#a").final_layout().size.width, 187.0);
    close(node(&doc, "#b").final_layout().size.width, 213.0);
}

#[test]
fn authored_columns_shrink_before_auto_columns_overflow() {
    // A `width=296` column next to an auto column in a 300px container: the
    // auto column keeps its 45px min-content width and the authored column
    // takes the rest, instead of the table growing to 341px.
    let doc = document(
        "<div style='width:300px'><table id='t' style='border-spacing:0'><tr>
         <td id='a' width='296'><span style='display:inline-block;width:30px'></span></td>
         <td id='b'><span style='display:inline-block;width:45px'></span> <span style='display:inline-block;width:45px'></span></td>
         </tr></table></div>",
    );
    close(node(&doc, "#t").final_layout().size.width, 300.0);
    close(node(&doc, "#a").final_layout().size.width, 255.0);
    close(node(&doc, "#b").final_layout().size.width, 45.0);
}

#[test]
fn fixed_tables_grow_to_hold_their_columns() {
    // Google Sheets pastes fixed tables with `width:0px` and `<col>` widths.
    // The table is as wide as its columns (CSS 2.1 §17.5.2.1), so it encloses
    // its cells and centres by its real width.
    let doc = document(
        "<table id='t' align='center' style='table-layout:fixed;width:0px;border-spacing:0'>
         <colgroup><col width='100'><col width='120'></colgroup>
         <tr><td id='a'>a</td><td id='b'>b</td></tr></table>",
    );
    let table = node(&doc, "#t").final_layout();
    close(table.size.width, 220.0);
    close(table.location.x, 90.0);
    close(node(&doc, "#b").final_layout().location.x, 100.0);
}

#[test]
fn collapsed_borders_follow_later_colour_changes() {
    // The reader lays a message out, then adds its appearance overrides as a
    // user-agent sheet. Colour changes only repaint, so collapsed borders must
    // read the winning box's colour when they paint, `currentcolor` included.
    let mut doc = document(
        "<table style='border-collapse:collapse'><tr>
         <td style='width:50px;height:40px;border:4px solid red'></td>
         <td style='width:50px;border:4px solid;color:red'></td></tr></table>",
    );
    let pixels = |doc: &mut HtmlDocument| {
        render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| blitz_paint::paint_scene(scene, doc, 1.0, 400, 300, 0, 0),
            400,
            300,
        )
    };
    assert!(rows_with(&pixels(&mut doc), [255, 0, 0, 255]).is_some());
    doc.add_user_agent_stylesheet(blitz_dom::UserAgentStylesheet::parse(
        "td { border-color: rgb(0, 0, 255) !important; color: rgb(0, 0, 255) !important }",
    ));
    doc.resolve(0.0);
    let pixels = pixels(&mut doc);
    assert!(rows_with(&pixels, [255, 0, 0, 255]).is_none());
    assert!(rows_with(&pixels, [0, 0, 255, 255]).is_some());
}

#[test]
fn collapsed_tables_relayout_after_border_changes() {
    // A later style change widens a row's border to 30px. The table resolves
    // its edges again, and the unchanged cell above gets the new 15px half.
    let mut doc = document(
        "<table style='border-collapse:collapse'>
         <tr><td id='above' style='border:10px solid green'>a</td></tr>
         <tr class='target'><td id='middle' style='border:10px solid green'>b</td></tr></table>",
    );
    let before = node(&doc, "#above").final_layout().size.height;
    doc.add_user_agent_stylesheet(blitz_dom::UserAgentStylesheet::parse(
        ".target { border: 30px solid green !important }",
    ));
    doc.resolve(0.0);
    close(node(&doc, "#middle").final_layout().border.top, 15.0);
    close(
        node(&doc, "#above").final_layout().size.height,
        before + 10.0,
    );
}

#[test]
fn bgcolor_maps_only_on_body_and_table_elements() {
    // Like browsers and the HTML Standard, `bgcolor` on a div is ignored.
    let mut doc = document(
        "<div bgcolor='#ff0000' style='height:20px'></div>
         <table><tr><td bgcolor='#0000ff' style='width:20px;height:20px'></td></tr></table>",
    );
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    assert!(rows_with(&pixels, [255, 0, 0, 255]).is_none());
    assert!(rows_with(&pixels, [0, 0, 255, 255]).is_some());
}

#[test]
fn only_the_first_header_and_footer_groups_move() {
    // CSS 2.1 §17.2: later header and footer groups stay in document order.
    let doc = document(
        "<table style='border-spacing:0'>
         <tbody><tr><td id='b1' style='height:10px'></td></tr></tbody>
         <thead><tr><td id='h1' style='height:10px'></td></tr></thead>
         <thead><tr><td id='h2' style='height:10px'></td></tr></thead>
         <tfoot><tr><td id='f1' style='height:10px'></td></tr></tfoot>
         <tfoot><tr><td id='f2' style='height:10px'></td></tr></tfoot></table>",
    );
    let order: Vec<f32> = ["#h1", "#b1", "#h2", "#f2", "#f1"]
        .iter()
        .map(|id| node(&doc, id).final_layout().location.y)
        .collect();
    assert!(order.is_sorted_by(|a, b| a < b), "{order:?}");
}

#[test]
fn empty_collapsed_tables_have_no_border() {
    let doc = document(
        "<table id='t' style='border-collapse:collapse;border:10px solid'><tbody></tbody></table>",
    );
    let size = node(&doc, "#t").final_layout().size;
    close(size.width, 0.0);
    close(size.height, 0.0);
}

#[test]
fn collapsed_border_lines_land_on_whole_pixels() {
    // Six equal columns share 399px, which puts grid lines at fractional
    // positions. The 1px borders must sit on the cells' rounded edges instead
    // of smearing across two pixels.
    let cells = "<td style='height:20px;border:1px solid #000'></td>".repeat(6);
    let mut doc = document(&format!(
        "<table style='width:400px;border-collapse:collapse'><tr>{cells}</tr></table>"
    ));
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    // Row 10 crosses every vertical line and no horizontal one. A smeared
    // line shows as partly transparent pixels on the transparent canvas.
    let row = &pixels.as_chunks::<4>().0[10 * 400..11 * 400];
    let partial = row
        .iter()
        .filter(|pixel| (1..255).contains(&pixel[3]))
        .count();
    let solid = row.iter().filter(|pixel| pixel[3] == 255).count();
    assert_eq!((partial, solid), (0, 7));
}

#[test]
fn floats_shrink_within_their_margins() {
    // CSS 2.1 §10.3.5: a shrink-to-fit float gets the containing block's
    // width less its own margins, so its margin box stays inside the block.
    let doc = document(
        "<div style='width:300px'>Text <span id='f' style='float:left;margin-left:40px'>
         A long floated paragraph whose text is wide enough to wrap inside the block.</span></div>",
    );
    let float = node(&doc, "#f").final_layout();
    close(float.location.x, 40.0);
    close(float.size.width, 260.0);
}

#[test]
fn shrink_to_fit_boxes_make_room_for_their_floats() {
    // DioxusLabs/blitz#980: an outer float sized to fit its contents must
    // include its inner float, not just the short text beside it.
    let doc = document(
        "<div id='outer' style='float:left'>Hi <span style='float:left;width:150px;height:10px'></span></div>",
    );
    assert!(node(&doc, "#outer").final_layout().size.width >= 150.0);
}

#[test]
fn vertical_margin_percentages_of_floats_use_the_width() {
    // CSS 2.1 §8.3: `margin-top: 10%` of a float beside text in a 400px-wide
    // block is 40px, even though the block's height is auto.
    let doc = document(
        "<div style='width:400px'>Text <span id='f' style='float:left;margin-top:10%;width:50px;height:50px'></span></div>",
    );
    close(node(&doc, "#f").final_layout().location.y, 40.0);
}

#[test]
fn tiles_keep_ink_that_overflows_from_the_tile_above() {
    // The reader paints in tiles. A box ending 2px above this tile has a
    // shadow reaching 10px into it, which must not be culled with the box.
    let mut doc = document(
        "<div style='height:500px'></div>
         <div style='height:10px;box-shadow:0 12px 0 rgb(255,0,0)'></div>
         <div style='height:300px'></div>",
    );
    doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 512.0 });
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    assert_eq!(rows_with(&pixels, [255, 0, 0, 255]), Some((0, 9)));
}

#[test]
fn only_boxes_that_overflow_applies_to_clip() {
    // Rows and inline elements have no box of their own to clip to, so a link
    // inside them must not be clipped away (css-overflow-3 §3.1).
    let doc = document(
        "<table><tr id='row' style='overflow:hidden'><td id='cell' style='overflow:hidden'>
         <span id='inline' style='overflow:hidden'><a href='https://example.com/'>Offer</a></span></td></tr></table>",
    );
    assert!(!node(&doc, "#row").clips_content());
    assert!(!node(&doc, "#inline").clips_content());
    assert!(node(&doc, "#cell").clips_content());
}

#[test]
fn column_min_widths_floor_columns() {
    // A `<col>` min-width floors its column, with or without cells in it,
    // while a percentage column past the last cell adds nothing.
    let doc = document(
        "<table id='t' style='border-spacing:0'><col style='min-width:100px'><col style='min-width:50px'>
         <col style='width:50%'><tr><td id='a'>x</td></tr></table>",
    );
    close(node(&doc, "#a").final_layout().size.width, 100.0);
    close(node(&doc, "#t").final_layout().size.width, 150.0);
}

#[test]
fn cell_children_resolve_percent_heights_against_the_cell_height() {
    // The first cell is 50px tall but stretched to its 100px row; its child's
    // 200% height resolves against the cell's own 50px.
    let doc = document(
        "<table style='border-spacing:0'><tr><td style='height:50px'><div id='c' style='height:200%'></div></td>
         <td><div style='height:100px'></div></td></tr></table>",
    );
    close(node(&doc, "#c").final_layout().size.height, 100.0);
}

#[test]
fn length_columns_absorb_excess_width_without_auto_columns() {
    // CSS Tables 3 §3.9.3: with no auto columns, a definite-width table gives
    // its excess width to the length columns, so the cells fill the table.
    let doc = document(
        "<table id='t' style='width:300px;border-spacing:0'><tr><td id='a' style='width:20px'></td>
         <td id='b' style='width:40px'></td><td id='c' style='width:10%'></td></tr></table>",
    );
    let cells: f32 = ["#a", "#b", "#c"]
        .iter()
        .map(|id| node(&doc, id).final_layout().size.width)
        .sum();
    close(cells, 300.0);
    close(node(&doc, "#c").final_layout().size.width, 30.0);
}

#[test]
fn rowspans_end_with_their_row_group() {
    // HTML's "ending a row group": the rowspan stops at the end of its tbody,
    // so the next tbody's first cell starts in the first column.
    let doc = document(
        "<table style='border-spacing:0'><tbody><tr><td rowspan='3'>A</td><td>B</td></tr></tbody>
         <tbody><tr><td id='c'>C</td><td>D</td></tr></tbody></table>",
    );
    close(node(&doc, "#c").final_layout().location.x, 0.0);
}

#[test]
fn nested_tables_that_wrap_long_words_keep_their_row_height() {
    // The inner table only fits its 200px cell by wrapping the long word. The
    // outer row must be measured with that wrapping, or the inner table paints
    // over the next row.
    let mut doc = HtmlDocument::from_html(
        "<style>body{margin:0} html,body{overflow-wrap:break-word} table{max-width:100%}</style>
         <div style='width:200px'><table style='border-spacing:0'><tr><td id='outer'>
         <table style='border-spacing:0'><tr><td>Supercalifragilisticexpialidocious_and_more_text</td><td>x</td></tr></table>
         </td></tr><tr><td id='next'>Next row</td></tr></table></div>",
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let outer = node(&doc, "#outer").final_layout();
    let inner = node(&doc, "#outer table").final_layout();
    assert!(
        inner.location.y + inner.size.height <= outer.size.height + 0.5,
        "inner table {:?} overflows its cell {:?}",
        inner.size,
        outer.size
    );
}

#[test]
fn wider_collapsed_edges_own_their_joints() {
    // A 3px header rule outranks the 1px cell borders meeting it, so the cells'
    // vertical lines stop at the rule instead of notching it.
    let mut doc = document(
        "<table style='border-collapse:collapse'>
         <tr><th style='width:60px;height:20px;border:1px solid rgb(0,0,255);border-bottom:3px solid rgb(255,0,0)'></th>
         <th style='width:60px;border:1px solid rgb(0,0,255);border-bottom:3px solid rgb(255,0,0)'></th></tr>
         <tr><td style='height:20px;border:1px solid rgb(0,0,255)'></td><td style='border:1px solid rgb(0,0,255)'></td></tr></table>",
    );
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    let (top, bottom) = rows_with(&pixels, [255, 0, 0, 255]).unwrap();
    let rows = pixels.as_chunks::<4>().0;
    for y in top..=bottom {
        let row = &rows[y * 400..(y + 1) * 400];
        assert!(
            !row.contains(&[0, 0, 255, 255]),
            "blue notch in rule row {y}"
        );
    }
}

#[test]
fn table_align_maps_only_html_values() {
    // HTML maps table `align` left, right and center; `middle` is not one of them.
    let doc = document(
        "<div style='width:400px'><table id='m' align='middle' style='width:100px'><tr><td></td></tr></table>
         <table id='c' align='center' style='width:100px'><tr><td></td></tr></table></div>",
    );
    close(node(&doc, "#m").final_layout().location.x, 0.0);
    close(node(&doc, "#c").final_layout().location.x, 150.0);
}

#[test]
fn a_stale_line_cache_does_not_break_painting() {
    // Callers must clear the cache after a relayout. If one does not, painting
    // must still not index past the lines a relayout added.
    let text = "word ".repeat(200);
    let mut doc = document(&format!("<p id='p' style='width:380px'>{text}</p>"));
    let cache = blitz_paint::PaintCache::default();
    let mut scene = anyrender::Scene::default();
    blitz_paint::paint_scene_cached(&mut scene, &mut doc, 1.0, 400, 300, 0, 0, &cache);
    doc.add_user_agent_stylesheet(blitz_dom::UserAgentStylesheet::parse(
        "p { width: 100px !important }",
    ));
    doc.resolve(0.0);
    let mut scene = anyrender::Scene::default();
    blitz_paint::paint_scene_cached(&mut scene, &mut doc, 1.0, 400, 300, 0, 0, &cache);
}

#[test]
fn copied_text_keeps_cells_and_paragraphs_apart() {
    // Like innerText: a tab between the cells of a row and a blank line around
    // paragraphs, so a copied receipt keeps its columns.
    let mut doc = document(
        "<p id='a'>Receipt</p><table><tr><td>Item</td><td>$5</td></tr></table><p id='b'>Thanks</p>",
    );
    let first = doc.query_selector("#a").unwrap().unwrap();
    let last = doc.query_selector("#b").unwrap().unwrap();
    doc.set_text_selection(first, 0, last, "Thanks".len());
    assert_eq!(
        doc.get_selected_text().as_deref(),
        Some("Receipt\n\nItem\t$5\n\nThanks")
    );
}

#[test]
fn body_margin_attributes_set_both_sides() {
    // Per axis, the first of marginheight/topmargin and marginwidth/leftmargin
    // that exists sets both sides (HTML rendering, "The page").
    for (attrs, x, width, y) in [
        ("leftmargin='0' topmargin='0'", 0.0, 400.0, 0.0),
        (
            "marginwidth='20' leftmargin='5' marginheight='30' topmargin='5'",
            20.0,
            360.0,
            30.0,
        ),
        ("leftmargin='x' marginwidth='10'", 10.0, 380.0, 8.0),
    ] {
        let mut doc = HtmlDocument::from_html(
            &format!("<body {attrs}><div id='d' style='height:5px'></div></body>"),
            DocumentConfig {
                viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
                ..Default::default()
            },
        );
        doc.resolve(0.0);
        let body = node(&doc, "body").final_layout();
        let div = node(&doc, "#d").final_layout();
        close(body.location.x + div.location.x, x);
        close(div.size.width, width);
        close(body.location.y, y);
    }
}

#[test]
fn invalid_font_faces_keep_the_inherited_family() {
    // `face` is parsed as a CSS family list; like browsers, a value that is
    // not one (Word's "Calibri (Body)") sets nothing.
    let doc = document(
        "<div style='font-family:monospace'><font id='bad' face='Calibri (Body)'>a</font>
         <font id='good' face='Georgia, serif'>b</font></div>",
    );
    let family = |selector| {
        let styles = node(&doc, selector).primary_styles().unwrap();
        format!("{:?}", styles.get_font().font_family)
    };
    assert!(family("#bad").contains("Monospace"), "{}", family("#bad"));
    assert!(family("#good").contains("Georgia"), "{}", family("#good"));
}

#[test]
fn elements_with_the_same_lang_share_styles() {
    // Outlook marks most paragraphs `lang=EN-US`; equal hints must not stop
    // siblings from sharing one computed style.
    let doc = document("<p id='a' lang='EN-US'>a</p><p id='b' lang='EN-US'>b</p>");
    let a = node(&doc, "#a").primary_styles().unwrap();
    let b = node(&doc, "#b").primary_styles().unwrap();
    assert!(std::ptr::eq(&**a, &**b));
}

#[test]
fn copied_text_leaves_out_hidden_text() {
    // Like browsers, a copy skips `visibility: hidden` text, as it does
    // `display: none` text.
    let mut doc = document(
        "<p id='a'>One <span style='visibility:hidden'>secret</span> two</p>
         <p id='b'>end <span style='visibility:hidden'>x<b style='visibility:visible'>shown</b></span></p>",
    );
    let first = doc.query_selector("#a").unwrap().unwrap();
    let last = doc.query_selector("#b").unwrap().unwrap();
    doc.set_text_selection(first, 0, last, "end xshown".len());
    assert_eq!(
        doc.get_selected_text().as_deref(),
        Some("One  two\n\nend shown")
    );
}

#[test]
fn collapsed_bevels_and_dashes_paint_like_browsers() {
    // `<table border=2>` collapses to a 2px `outset` table edge, which paints
    // as a groove (CSS 2.1 §17.6.3): a dark outer half over a light inner
    // half, as in Chrome, where black bevels between #545454 and #a8a8a8. A
    // dashed edge starts and ends with a dash.
    let mut doc = document(
        "<table border='2' style='border-collapse:collapse'><tr><td style='width:60px;height:20px'></td></tr></table>
         <table style='border-collapse:collapse;margin-top:10px'><tr>
         <td style='width:100px;height:20px;border:2px dashed rgb(255,0,0)'></td></tr></table>",
    );
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 400 + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    assert_eq!(pixel(30, 0), [84, 84, 84, 255]);
    assert_eq!(pixel(30, 1), [168, 168, 168, 255]);

    let red = [255, 0, 0, 255];
    let (top, _) = rows_with(&pixels, red).unwrap();
    assert_eq!(pixel(0, top), red, "dash at the start");
    assert_eq!(pixel(103, top), red, "dash at the end");
}

#[test]
fn positioned_children_of_collapsed_tables_keep_their_border() {
    // An absolutely positioned cell takes no part in the collapsed grid, so it
    // paints its own border.
    let mut doc = document(
        "<table style='border-collapse:collapse;position:relative'><tr>
         <td style='position:absolute;top:30px;left:0;width:20px;height:20px;border:2px solid rgb(255,0,0)'></td>
         <td style='width:40px;height:10px'></td></tr></table>",
    );
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    assert!(rows_with(&pixels, [255, 0, 0, 255]).is_some());
}

#[test]
fn column_and_column_group_borders_collapse_with_cells() {
    // Column group borders join collapsed border resolution (CSS 2.1
    // §17.6.2.1), here between the first group's column and the second's.
    let mut doc = document(
        "<table style='border-collapse:collapse'>
         <colgroup style='border-right:4px solid rgb(255,0,0)'><col></colgroup>
         <colgroup><col><col></colgroup>
         <tr><td style='width:30px;height:20px'></td><td style='width:30px'></td><td style='width:30px'></td></tr>
         </table>",
    );
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    let red: Vec<usize> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, pixel)| **pixel == [255, 0, 0, 255])
        .map(|(index, _)| index % 400)
        .collect();
    let (min, max) = (red.iter().min().unwrap(), red.iter().max().unwrap());
    assert!(*min >= 28 && *max <= 35, "red columns {min}..={max}");
}

#[test]
fn column_groups_without_columns_span_their_columns() {
    // `<colgroup span=2 width=50>` without `<col>` children stands for two
    // 50px columns, as in browsers.
    let doc = document(
        "<table style='border-spacing:0'><colgroup span='2' width='50'></colgroup>
         <tr><td id='a'>a</td><td id='b'>b</td><td id='c'>c</td></tr></table>",
    );
    close(node(&doc, "#a").final_layout().size.width, 50.0);
    close(node(&doc, "#b").final_layout().size.width, 50.0);
}

#[test]
fn svg_images_load_under_decode_limits() {
    // Decode limits bound raster images; an SVG still loads as without them.
    let images = std::sync::Arc::new(HeldImages::default());
    let mut doc = HtmlDocument::from_html(
        "<img src='https://example.com/logo.svg'>",
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            net_provider: Some(images.clone()),
            image_decode_limits: Some(decode_limits()),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30"><rect width="40" height="30" fill="red"/></svg>"#;
    for (url, handler) in images.0.lock().unwrap().drain(..) {
        handler.bytes(url, blitz_traits::net::Bytes::copy_from_slice(svg));
    }
    doc.resolve(0.0);
    let size = node(&doc, "img").final_layout().size;
    assert_eq!((size.width, size.height), (40.0, 30.0));
}

#[test]
fn resource_damage_survives_a_resolve() {
    // `resolve` handles resource events itself; the embedder's next drain
    // must still report the damage they caused.
    let images = std::sync::Arc::new(HeldImages::default());
    let mut doc = HtmlDocument::from_html(
        "<img src='https://example.com/a.png' width='10' height='10'>",
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            net_provider: Some(images.clone()),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc.drain_pending_updates();
    let png = encoded(
        image::ImageFormat::Png,
        image::DynamicImage::new_rgb8(10, 10),
    );
    for (url, handler) in images.0.lock().unwrap().drain(..) {
        handler.bytes(url, blitz_traits::net::Bytes::copy_from_slice(&png));
    }
    doc.resolve(0.0);
    let updates = doc.drain_pending_updates();
    assert!(updates.changed);
    assert_eq!(
        updates.paint_nodes,
        vec![doc.query_selector("img").unwrap().unwrap()]
    );
}

#[test]
fn cells_lay_out_with_their_current_style() {
    // `vertical-align` changes need no table rebuild, so the cell's layout must
    // read its current style rather than one captured when the table was built.
    let mut doc = document(
        "<table style='border-collapse:collapse'><tr style='height:100px'>
         <td style='vertical-align:top;border:2px solid red'><div id='d' style='height:10px'></div></td>
         </tr></table>",
    );
    close(node(&doc, "#d").final_layout().location.y, 1.0);
    doc.add_user_agent_stylesheet(blitz_dom::UserAgentStylesheet::parse(
        "td { vertical-align: bottom !important }",
    ));
    doc.resolve(0.0);
    close(node(&doc, "#d").final_layout().location.y, 89.0);
}

#[test]
fn scaled_images_do_not_bleed_their_opposite_edge() {
    // A white image with a black bottom row, drawn at 2.5x: interpolating its
    // top edge with the bottom row (`Extend::Repeat`) darkens the top line.
    let mut image = image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 255, 255, 255]));
    for x in 0..8 {
        image.put_pixel(x, 7, image::Rgba([0, 0, 0, 255]));
    }
    let png = encoded(
        image::ImageFormat::Png,
        image::DynamicImage::ImageRgba8(image),
    );
    let images = std::sync::Arc::new(HeldImages::default());
    let mut doc = HtmlDocument::from_html(
        "<style>body{margin:0}</style><img src='https://example.com/a.png' style='width:20px;height:20px'>",
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            net_provider: Some(images.clone()),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    for (url, handler) in images.0.lock().unwrap().drain(..) {
        handler.bytes(url, blitz_traits::net::Bytes::copy_from_slice(&png));
    }
    doc.resolve(0.0);
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    let top = &pixels[10 * 4..10 * 4 + 4];
    assert_eq!(top, [255, 255, 255, 255]);
}

#[test]
fn a_scale_change_rescales_every_overflow() {
    // Overflow is kept in device pixels. After a 1x layout, a 1.5x one must
    // rescale the overflow of text-free boxes too, or a tile further down
    // culls the red block before its real end at 900 device pixels.
    let mut doc =
        document("<div style='height:0'><div style='height:600px;background:#c00'></div></div>");
    doc.set_viewport(Viewport::new(600, 450, 1.5, ColorScheme::Light));
    doc.resolve(0.0);
    doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 500.0 });
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.5, 600, 450, 0, 0),
        600,
        450,
    );
    let red = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[0] > 150 && pixel[1] < 50)
        .count();
    assert_eq!(red, 600 * 150);
}

#[test]
fn overflow_under_anonymous_blocks_follows_changes() {
    // The inline-block sits in an anonymous block beside a block sibling. When
    // its child grows past it, the new overflow must reach the culling of a
    // tile further down, even though anonymous boxes carry no damage.
    let mut doc = document(
        "<div><span style='display:inline-block;height:10px;width:50px'>
         <div id='o' style='height:10px;background:#c00'></div></span><div>block</div></div>",
    );
    doc.add_user_agent_stylesheet(blitz_dom::UserAgentStylesheet::parse(
        "#o { height: 600px !important }",
    ));
    doc.resolve(0.0);
    doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 400.0 });
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    );
    assert!(rows_with(&pixels, [204, 0, 0, 255]).is_some());
}

#[test]
fn text_culling_follows_the_painter_offset() {
    // Painted 100px down the scene (as a sub-document is), the line near the
    // bottom of the document's viewport lands below 300 scene pixels and must
    // still be drawn.
    let mut doc = document(
        "<p style='position:absolute;top:250px;margin:0;font:20px/20px sans-serif;color:#c00'>MMMM</p>",
    );
    let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, 400, 300, 0, 100),
        400,
        400,
    );
    let (top, _) = rows_with(&pixels, [204, 0, 0, 255]).expect("text painted");
    assert!(top >= 340, "text at row {top}");
}

#[test]
fn tall_images_keep_their_detail_within_the_pixel_budget() {
    // Decoded images are bounded by their pixel count, not only their longer
    // side: a tall newsletter image within the budget keeps its resolution.
    let limits = blitz_dom::net::ImageDecodeLimits {
        target_dimension: 1000,
        target_pixels: 100 * 100,
        ..decode_limits()
    };
    let png = encoded(
        image::ImageFormat::Png,
        image::DynamicImage::new_rgb8(20, 400),
    );
    let decoded = limits.decode(&png).unwrap();
    assert_eq!((decoded.pixel_width, decoded.pixel_height), (20, 400));
    // Past the budget, both sides shrink by the same factor.
    let png = encoded(
        image::ImageFormat::Png,
        image::DynamicImage::new_rgb8(400, 200),
    );
    let decoded = limits.decode(&png).unwrap();
    assert_eq!((decoded.pixel_width, decoded.pixel_height), (141, 70));
}

fn paint(doc: &mut HtmlDocument) -> Vec<u8> {
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, doc, 1.0, 400, 300, 0, 0),
        400,
        300,
    )
}

/// The bounding columns and rows of the pixels that pass `test`.
fn bounds_where(
    pixels: &[u8],
    test: impl Fn(&[u8; 4]) -> bool,
) -> Option<(usize, usize, usize, usize)> {
    let mut bounds: Option<(usize, usize, usize, usize)> = None;
    for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        if test(pixel) {
            let (x, y) = (index % 400, index / 400);
            bounds = Some(match bounds {
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                None => (x, y, x, y),
            });
        }
    }
    bounds
}

#[test]
fn a_cell_height_is_the_minimum_of_its_row() {
    // CSS 2.1 §17.5.3: a row is as tall as its cells' heights and contents, so
    // a 1px cell grows to hold its content instead of spilling into the next
    // row.
    let doc = document(
        "<table style='border-spacing:0'>
         <tr><td id='short' style='height:1px'><div style='height:30px'></div></td></tr>
         <tr><td id='next' style='height:10px'></td></tr></table>",
    );
    close(node(&doc, "#short").final_layout().size.height, 30.0);
    close(node(&doc, "#next").final_layout().location.y, 30.0);
}

#[test]
fn block_cells_of_a_row_stack_in_one_anonymous_cell() {
    // A responsive email makes a row's cells blocks on a narrow screen. CSS 2.1
    // §17.2.1 wraps consecutive non-cells of a row in one anonymous cell, in
    // which they stack, comments between them notwithstanding.
    let doc = document(
        "<table style='width:200px;border-spacing:0'><tr>
         <!-- one --><td id='a' style='display:block'>A</td>
         <!-- two --><td id='b' style='display:block'>B</td></tr></table>",
    );
    let (a, b) = (node(&doc, "#a"), node(&doc, "#b"));
    let (a_pos, b_pos) = (a.absolute_position(0.0, 0.0), b.absolute_position(0.0, 0.0));
    close(a.final_layout().size.width, 200.0);
    close(b_pos.x, a_pos.x);
    close(b_pos.y, a_pos.y + a.final_layout().size.height);
}

#[test]
fn auto_columns_share_width_in_proportion_to_their_range() {
    // Chromium gives each auto column its min-content width plus the same
    // fraction of the way to its max-content width (CSS Tables 3 §3.9.3):
    // columns of 25..100px and 50..100px in 150px are 70px and 80px, where
    // equal shares would make them 62.5px and 87.5px.
    let boxes = |width: u32, count: usize| {
        vec![
            format!("<span style='display:inline-block;width:{width}px;height:5px'></span>");
            count
        ]
        .join(" ")
    };
    let doc = document(&format!(
        "<table style='width:150px;border-spacing:0;font-size:0'><tr>
         <td id='a'>{}</td><td id='b'>{}</td></tr></table>",
        boxes(25, 4),
        boxes(50, 2)
    ));
    close(node(&doc, "#a").final_layout().size.width, 70.0);
    close(node(&doc, "#b").final_layout().size.width, 80.0);
}

#[test]
fn inline_padding_and_margins_take_room_and_paint() {
    // The span's margins and padding take room in its line (CSS 2.1 §10.3.1)
    // and its background covers its padding box: 5px of margin, then 10px of
    // padding each side of a 20px box, so the next box starts at 50px.
    let mut doc = document(
        "<p style='margin:0;font-size:0'><span style='background:rgb(255,0,0);padding:4px 10px;margin:0 5px'>
         <span style='display:inline-block;width:20px;height:10px;background:rgb(0,0,255)'></span></span><span
         style='display:inline-block;width:10px;height:10px;background:rgb(0,255,0)'></span></p>",
    );
    let pixels = paint(&mut doc);
    let red =
        bounds_where(&pixels, |pixel| *pixel == [255, 0, 0, 255]).expect("background painted");
    assert_eq!((red.0, red.2), (5, 44), "{red:?}");
    let blue = bounds_where(&pixels, |pixel| *pixel == [0, 0, 255, 255]).unwrap();
    assert_eq!((blue.0, blue.2), (15, 34), "{blue:?}");
    let green = bounds_where(&pixels, |pixel| *pixel == [0, 255, 0, 255]).unwrap();
    assert_eq!(green.0, 50, "{green:?}");
}

#[test]
fn wrapped_inline_boxes_paint_their_edges_once() {
    // Sliced at its line breaks, a wrapped inline box has its left border on its
    // first line only and its right border on its last.
    let mut doc = document(
        "<div style='width:30px;font:10px/30px sans-serif'><span style='border:solid rgb(255,0,0);border-width:0 4px;color:transparent'>aaaa aaaa aaaa</span></div>",
    );
    let pixels = paint(&mut doc);
    let red_in_row = |y: usize| {
        (0..400)
            .filter(|x| pixels[(y * 400 + x) * 4..(y * 400 + x) * 4 + 4] == [255, 0, 0, 255])
            .count()
    };
    let lines: Vec<usize> = [15, 45, 75].into_iter().map(red_in_row).collect();
    // Four border columns on the first and last lines, none on the middle one.
    assert_eq!(lines, [4, 0, 4], "{lines:?}");
}

#[test]
fn inline_opacity_groups_text_and_boxes() {
    // An inline element's opacity applies to its text and to the boxes in it,
    // and at zero hides them, as for hidden preheader text.
    let mut doc = document(
        "<p style='margin:0;font:20px/24px sans-serif;color:rgb(255,0,0)'><span style='opacity:0'>HIDDEN</span></p>
         <p style='margin:0'><span style='opacity:0.5'><span style='display:inline-block;width:10px;height:10px;background:rgb(0,0,255)'></span></span></p>",
    );
    let pixels = paint(&mut doc);
    assert!(bounds_where(&pixels, |pixel| pixel[0] > 0 && pixel[3] > 0).is_none());
    let (x, y, _, _) = bounds_where(&pixels, |pixel| pixel[2] > 0).expect("box painted");
    let alpha = pixels[(y * 400 + x) * 4 + 3];
    assert!((120..=135).contains(&alpha), "alpha {alpha}");
}

#[test]
fn underlines_skip_descenders_but_not_trailing_spaces() {
    // `text-decoration-skip-ink: auto` interrupts the underline where a
    // descender crosses it, and a line's hanging space is not underlined.
    let mut doc = document(
        "<div style='width:120px;font:30px/40px sans-serif;color:rgb(0,0,0)'>
         <u style='text-decoration-color:rgb(255,0,0)'>xxgxx xxxxxxxx</u></div>",
    );
    let pixels = paint(&mut doc);
    let red =
        |x: usize, y: usize| pixels[(y * 400 + x) * 4..(y * 400 + x) * 4 + 4] == [255, 0, 0, 255];
    // The first line's underline row, and its runs of red.
    let row = (0..40)
        .max_by_key(|&y| (0..400).filter(|&x| red(x, y)).count())
        .unwrap();
    let mut runs = 0;
    for x in 0..400 {
        if red(x, row) && (x == 0 || !red(x - 1, row)) {
            runs += 1;
        }
    }
    assert!(
        runs >= 2,
        "the underline is interrupted under the g ({runs} runs)"
    );
    let underline_end = (0..400).rev().find(|&x| red(x, row)).unwrap();
    let text_end = (0..400)
        .rev()
        .find(|&x| {
            (0..40).any(|y| pixels[(y * 400 + x) * 4..(y * 400 + x) * 4 + 4] == [0, 0, 0, 255])
        })
        .unwrap();
    assert!(
        underline_end <= text_end + 2,
        "underline to {underline_end}, text to {text_end}"
    );
}

#[test]
fn a_wbr_breaks_text_that_does_not_wrap() {
    // As in Chromium, a <wbr> is a line break opportunity even under
    // `white-space: nowrap`.
    let doc = document(
        "<p id='p' style='margin:0;width:1px;white-space:nowrap;font:10px/20px sans-serif'>aaaa<wbr>bbbb</p>",
    );
    close(node(&doc, "#p").final_layout().size.height, 40.0);
}

#[test]
fn line_breaks_in_middle_aligned_cells_keep_their_line_height() {
    // A <br> sits on its parent's baseline: it isn't aligned to the cell's
    // middle a second time, which grew each line by a pixel.
    let doc = document(
        "<table style='border-spacing:0'><tr><td id='c' style='vertical-align:middle;font:16px/24px sans-serif'>a<br>b<br>c</td></tr></table>",
    );
    close(node(&doc, "#c").final_layout().size.height, 72.0);
}

#[test]
fn copied_text_leaves_out_inline_edges() {
    // An inline element's padding is laid out as an invisible placeholder,
    // which isn't the document's text.
    let mut doc =
        document("<p id='p'>a <span style='padding:0 5px;border:1px solid'>b</span>c<wbr>d</p>");
    let p = doc.query_selector("#p").unwrap().unwrap();
    let len = doc
        .get_node(p)
        .unwrap()
        .element_data()
        .unwrap()
        .inline_layout_data
        .as_ref()
        .unwrap()
        .text
        .len();
    doc.set_text_selection(p, 0, p, len);
    assert_eq!(doc.get_selected_text().as_deref(), Some("a bcd"));
}

#[test]
fn captions_sit_above_the_grid_outside_the_table_border() {
    // CSS 2.1 §17.4: a caption is as wide as the table, above its grid (below
    // it with `caption-side: bottom`), and the table's border surrounds the
    // grid alone.
    let mut doc = document(
        "<table style='border:4px solid rgb(255,0,0);border-spacing:0;width:100px'>
         <caption id='top' style='height:20px'></caption>
         <caption id='bottom' style='height:10px;caption-side:bottom'></caption>
         <tr><td id='cell' style='height:30px'></td></tr></table>",
    );
    let (top, cell, bottom) = (
        node(&doc, "#top"),
        node(&doc, "#cell"),
        node(&doc, "#bottom"),
    );
    close(top.final_layout().size.width, 100.0);
    close(top.final_layout().location.y, 0.0);
    close(cell.final_layout().location.y, 24.0);
    close(bottom.final_layout().location.y, 58.0);
    let pixels = paint(&mut doc);
    let red = bounds_where(&pixels, |pixel| *pixel == [255, 0, 0, 255]).expect("border painted");
    assert_eq!((red.1, red.3), (20, 57), "{red:?}");
}

#[test]
fn table_parts_outside_a_table_get_an_anonymous_table() {
    // CSS 2.1 §17.2.1: the rows of a table made `display: block` stack in an
    // anonymous table, and cells without a table share an anonymous row.
    let doc = document(
        "<table style='display:block'><tr><td id='a'>A</td></tr><tr><td id='b'>B</td></tr></table>
         <div><div id='c' style='display:table-cell'>C</div>
         <div id='d' style='display:table-cell'>D</div></div>",
    );
    let position = |selector: &str| node(&doc, selector).absolute_position(0.0, 0.0);
    let (a, b, c, d) = (
        position("#a"),
        position("#b"),
        position("#c"),
        position("#d"),
    );
    close(b.x, a.x);
    assert!(b.y > a.y, "rows stack: {a:?} {b:?}");
    close(d.y, c.y);
    assert!(d.x > c.x, "cells share a row: {c:?} {d:?}");
}

#[test]
fn image_minimums_keep_the_other_specified_size() {
    // A minimum doesn't change a specified size on the other axis (sections
    // 10.3.2 and 10.6.2 of CSS 2.1), so an email spacer with
    // `height:1px;min-width:300px` is 300 by 1, as in browsers, not a 300 by
    // 300 gap.
    let square = encoded(
        image::ImageFormat::Png,
        image::DynamicImage::new_rgb8(10, 10),
    );
    for (attributes, size) in [
        (
            "style='display:block;height:1px;min-width:300px'",
            (300.0, 1.0),
        ),
        (
            "style='display:block;width:1px;min-height:50px'",
            (1.0, 50.0),
        ),
        (
            "width=1 height=1 style='display:block;min-width:300px'",
            (300.0, 1.0),
        ),
        (
            "style='display:block;width:100%;height:1px;min-width:500px'",
            (500.0, 1.0),
        ),
        (
            "style='display:block;width:1px;height:1px;min-width:300px;min-height:2px'",
            (300.0, 2.0),
        ),
        (
            "style='display:block;width:1px;min-width:300px;min-height:2px'",
            (300.0, 300.0),
        ),
        ("style='display:block;width:600px'", (400.0, 400.0)),
    ] {
        let html = format!("<img src='https://example.com/a.png' {attributes}>");
        assert_eq!(
            image_size_after_load(&html, &square, false),
            size,
            "{attributes}"
        );
    }

    // A maximum scales both sizes, unlike in CSS 2.1, so that an image the
    // reader's `max-width: 100%` fits to the window keeps its proportions.
    let wide = encoded(
        image::ImageFormat::Png,
        image::DynamicImage::new_rgb8(20, 10),
    );
    assert_eq!(
        image_size_after_load(
            "<img src='https://example.com/a.png' width=600 height=300 style='display:block'>",
            &wide,
            false
        ),
        (400.0, 200.0)
    );
}

#[test]
fn lines_break_where_chromium_breaks_them() {
    // Between two ASCII characters, Chromium breaks only where its table
    // allows: not after a slash, so a path stays whole, but after a hyphen
    // between digits, so a date may wrap. An inline element's padding adds
    // no opportunity, and text that may break anywhere keeps UAX #14.
    for (style, text, lines) in [
        ("", "a/b/c", 1.0),
        ("", "2026-09-10", 3.0),
        ("", "a/<span style='padding:0 2px'>b</span>/c", 1.0),
        ("word-break:break-all", "a/b/c", 3.0),
    ] {
        let doc = document(&format!(
            "<p id='p' style='margin:0;width:1px;font:10px/20px sans-serif;{style}'>{text}</p>"
        ));
        close(node(&doc, "#p").final_layout().size.height, 20.0 * lines);
    }
}

#[test]
fn table_border_attributes_draw_borders_as_blink_does() {
    // `bordercolor` makes the table's border and its cells' borders solid in
    // that colour, where they were outset and inset bevels.
    let mut doc = document(
        "<table border=2 bordercolor='#ff0000' cellspacing=4><tr><td style='width:20px;height:20px'></td></tr></table>",
    );
    let pixels = paint(&mut doc);
    let red = |pixel: &[u8; 4]| *pixel == [255, 0, 0, 255];
    assert_eq!(bounds_where(&pixels, red), Some((0, 0, 33, 33)));
    let cell_edge = (15 * 400 + 6) * 4;
    assert_eq!(&pixels[cell_edge..cell_edge + 4], &[255, 0, 0, 255]);

    // `frame=void` hides the table's own border, and `rules=rows` draws lines
    // between rows only, in the table's border colour.
    let mut doc = document(
        "<table border=3 frame=void rules=rows style='color:rgb(0,0,255)'>
         <tr><td style='width:20px;height:10px'></td></tr>
         <tr><td style='width:20px;height:10px'></td></tr></table>",
    );
    let pixels = paint(&mut doc);
    let (x0, y0, x1, y1) = bounds_where(&pixels, |pixel| pixel[2] > 128 && pixel[0] < 128).unwrap();
    assert_eq!((x0, x1), (0, 19));
    assert!(
        y0 == y1 && (9..=11).contains(&y0),
        "one rule between the rows: {y0}..{y1}"
    );

    // Cells take the table's border colour, not their own text colour.
    let mut doc = document(
        "<table border=1 style='border-color:rgb(0,160,0)'><tr>
         <td style='width:20px;height:20px;color:rgb(0,0,255)'></td></tr></table>",
    );
    let pixels = paint(&mut doc);
    assert!(bounds_where(&pixels, |pixel| pixel[2] > 64 && pixel[2] > pixel[1]).is_none());
    assert!(
        bounds_where(&pixels, |pixel| pixel[1] > 64
            && pixel[0] < 64
            && pixel[2] < 64)
        .is_some()
    );
}

#[test]
fn wide_tables_share_spare_width_in_proportion_to_content() {
    // A 300px table whose auto columns want 100px, 50px and nothing gives
    // them 200px, 100px and none, as Chromium does (CSS Tables 3 §3.9.3),
    // where grid tracks would each grow by 50px.
    let doc = document(
        "<table width=300 cellspacing=0><tr>
         <td id=a><div style='width:100px;height:5px'></div></td>
         <td id=b><div style='width:50px;height:5px'></div></td>
         <td id=c></td></tr></table>",
    );
    close(node(&doc, "#a").final_layout().size.width, 200.0);
    close(node(&doc, "#b").final_layout().size.width, 100.0);
    close(node(&doc, "#c").final_layout().size.width, 0.0);
}

#[test]
fn tables_without_columns_have_no_spacing() {
    // As in Blink, an empty `<table cellspacing=10>` is only its border.
    let doc = document("<table id=t cellspacing=10 style='border:2px solid'><tr></tr></table>");
    let size = node(&doc, "#t").final_layout().size;
    close(size.width, 4.0);
    close(size.height, 4.0);
}

#[test]
fn a_selection_paints_the_line_breaks_it_selects() {
    // As in Blink, a line whose end the selection goes past ends in a box as
    // wide as a space, which shows that its line break is selected: a
    // selection going on into the next paragraph reaches further right than
    // one that ends with the first.
    let right_edge = |into_next: bool| {
        let mut doc = document(
            "<p id=a style='margin:0;font:20px/30px sans-serif'>ab</p>
             <p id=b style='margin:0;font:20px/30px sans-serif'>cd</p>",
        );
        let a = doc.query_selector("#a").unwrap().unwrap();
        let b = doc.query_selector("#b").unwrap().unwrap();
        if into_next {
            doc.set_text_selection(a, 0, b, 1);
        } else {
            doc.set_text_selection(a, 0, a, 2);
        }
        let pixels = paint(&mut doc);
        let first_line = &pixels[..400 * 30 * 4];
        bounds_where(first_line, |pixel| *pixel == [180, 213, 255, 255])
            .unwrap()
            .2
    };
    let (ending, going_on) = (right_edge(false), right_edge(true));
    assert!(going_on >= ending + 3, "{ending} {going_on}");
}

#[test]
fn highlighted_text_paints_as_blink_paints_find_matches() {
    // Matches are yellow and the active one orange, as tall as the text
    // rather than the line, and their text is black whatever its colour.
    let mut doc = document(
        "<p id=p style='margin:0;font:20px/40px sans-serif;color:rgb(255,255,255);background:rgb(0,0,128)'>one two</p>",
    );
    let p = doc.query_selector("#p").unwrap().unwrap();
    doc.set_text_highlights(vec![
        blitz_dom::TextHighlight {
            node: p,
            range: 0..3,
            active: false,
        },
        blitz_dom::TextHighlight {
            node: p,
            range: 4..7,
            active: true,
        },
    ]);
    let pixels = paint(&mut doc);
    let yellow = bounds_where(&pixels, |pixel| *pixel == [255, 255, 0, 255]).unwrap();
    let orange = bounds_where(&pixels, |pixel| *pixel == [255, 150, 50, 255]).unwrap();
    assert!(yellow.2 < orange.0, "{yellow:?} {orange:?}");
    assert!(yellow.3 - yellow.1 < 30, "{yellow:?}");
    let any_inside = |test: &dyn Fn(&[u8]) -> bool| {
        (yellow.1..=yellow.3).any(|y| {
            (yellow.0..=yellow.2).any(|x| {
                let i = (y * 400 + x) * 4;
                test(&pixels[i..i + 4])
            })
        })
    };
    assert!(any_inside(&|pixel| pixel[..3].iter().all(|&c| c < 60)));
    assert!(!any_inside(&|pixel| pixel[..3].iter().all(|&c| c > 200)));
}

#[test]
fn cell_percentage_padding_resolves_against_the_table() {
    // As in Blink, a cell's percentage padding is a share of the table's
    // content width, not of its own column, so MJML's hero spacer cell with
    // `padding-bottom: 70%` is as tall as 70% of the table.
    let doc = document(
        "<table style='width:400px;border-collapse:collapse'><tr>
         <td id=a style='width:0.01%;padding-bottom:70%'></td><td>Hero</td></tr></table>",
    );
    close(node(&doc, "#a").final_layout().size.height, 280.0);
    // An auto-width table resolves it against the width it takes.
    let doc = document(
        "<table style='border-spacing:0'><tr>
         <td id=b style='padding-bottom:50%'><div style='width:200px'></div></td></tr></table>",
    );
    close(node(&doc, "#b").final_layout().size.height, 100.0);
}

#[test]
fn legacy_word_break_lets_long_words_break_in_narrow_columns() {
    // Browsers read the `word-break: break-word` that email templates put on
    // their cells as `overflow-wrap: anywhere`, so a long word doesn't hold
    // its column open. Stylo drops the keyword outside Gecko.
    let doc = document(
        "<table style='width:200px;border-spacing:0'><tr><td style='width:50%'>a</td>
         <td id=c style='width:50%;word-break:break-word;font:10px monospace'>xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx</td>
         </tr></table>",
    );
    close(node(&doc, "#c").final_layout().size.width, 100.0);
}

#[test]
fn images_that_wont_load_lay_out_with_their_alt_text() {
    // As browsers lay out an image they can't show, one that the embedder
    // blocks or that fails to decode takes the room of its alt text, on one
    // line or wrapped inside a width it is given. An empty alt takes no room.
    let images = std::sync::Arc::new(HeldImages::default());
    let mut doc = HtmlDocument::from_html(
        "<style>body{margin:0} img{display:block;font:20px/30px sans-serif}</style>
         <img id=a src='https://example.com/a.png' alt='Weekly header'>
         <img id=b src='https://example.com/b.png' alt=''>
         <img id=c src='https://example.com/c.png' alt='Hero' style='width:100%'>
         <img id=d src='https://example.com/d.png' alt='Broken'>
         <img id=e src='https://example.com/e.png' alt='Weekly header' style='width:80px'>",
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            net_provider: Some(images.clone()),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    for (url, handler) in images.0.lock().unwrap().drain(..) {
        if url.ends_with("d.png") {
            handler.bytes(url, blitz_traits::net::Bytes::from_static(b"not an image"));
        }
    }
    for url in ["a", "b", "c", "e"] {
        doc.fail_image(&format!("https://example.com/{url}.png"));
    }
    doc.resolve(0.0);
    let size = |selector| node(&doc, selector).final_layout().size;
    assert!(size("#a").width > 60.0, "{:?}", size("#a"));
    close(size("#a").height, 30.0);
    assert_eq!((size("#b").width, size("#b").height), (0.0, 0.0));
    close(size("#c").width, 400.0);
    close(size("#c").height, 30.0);
    assert!(size("#d").width > 30.0, "{:?}", size("#d"));
    close(size("#d").height, 30.0);
    close(size("#e").width, 80.0);
    close(size("#e").height, 60.0);
}

#[test]
fn rules_and_bevels_paint_as_blink_paints_them() {
    // Blink shades an `inset`, `outset`, `groove` or `ridge` border whose
    // colour is `currentcolor` from rgb(238, 238, 238), except on tables and
    // their parts, so a default `<hr>` is pale. `<hr noshade>` fills with
    // Blink's dark gray, `color` with the colour, and `size` sets the height.
    let mut doc = document(
        "<style>body{margin:0} hr{margin:0 0 10px}</style>
         <hr id=a>
         <hr id=b noshade size=4>
         <hr id=c color=red size=1>
         <hr id=d size=6>
         <table id=t style='border:2px outset;border-spacing:0;color:rgb(0,0,255)'>
         <tr><td style='width:20px;height:10px;padding:0'></td></tr></table>",
    );
    let pixels = paint(&mut doc);
    let pixel = |x: usize, y: f32| {
        let i = (y as usize * 400 + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let top = |selector| node(&doc, selector).final_layout().location.y;
    let height = |selector| node(&doc, selector).final_layout().size.height;

    assert_eq!(pixel(100, top("#a")), [154, 154, 154, 255]);
    assert_eq!(pixel(100, top("#a") + 1.0), [238, 238, 238, 255]);
    close(height("#b"), 4.0);
    assert_eq!(pixel(100, top("#b") + 2.0), [128, 128, 128, 255]);
    close(height("#c"), 1.0);
    assert_eq!(pixel(100, top("#c")), [255, 0, 0, 255]);
    close(height("#d"), 6.0);
    // The table's bevel keeps its text colour: blue, darker at the bottom.
    let bottom = top("#t") + height("#t") - 1.0;
    let [r, g, b, _] = pixel(10, bottom);
    assert!(
        r == 0 && g == 0 && (150..200).contains(&b),
        "{:?}",
        [r, g, b]
    );
}

#[test]
fn text_shadows_paint_beneath_their_text() {
    // As in browsers, a shadow is the text drawn again, offset, beneath the
    // line's text, the first of a list on top, and it shadows the text's
    // decorations too.
    let mut doc = document(
        "<style>body{margin:0;font:bold 60px/80px sans-serif}</style>
         <div style='text-shadow:0 0 0 rgb(255,0,0)'>H</div>
         <div style='text-shadow:5px 5px 0 rgb(255,0,0), 5px 5px 0 rgb(0,0,255)'>H</div>
         <div style='font-size:20px;line-height:30px;text-decoration:underline;
                     text-shadow:0 8px 0 rgb(0,160,0)'>&nbsp;&nbsp;&nbsp;</div>",
    );
    let pixels = paint(&mut doc);
    let count = |rows: std::ops::Range<usize>, test: &dyn Fn(&[u8; 4]) -> bool| {
        pixels[rows.start * 400 * 4..rows.end * 400 * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| test(pixel))
            .count()
    };
    // The canvas is transparent, so colours must be opaque to count.
    let red = |p: &[u8; 4]| p[3] > 200 && p[0] > 200 && p[1] < 60 && p[2] < 60;
    let blue = |p: &[u8; 4]| p[3] > 200 && p[2] > 200 && p[0] < 60 && p[1] < 60;
    let black = |p: &[u8; 4]| p[3] > 200 && p[0] < 30 && p[1] < 30 && p[2] < 30;
    // A shadow right under its text is covered by it.
    assert!(count(0..80, &black) > 100);
    assert_eq!(count(0..80, &red), 0);
    // The first shadow covers the second.
    assert!(count(80..160, &red) > 100);
    assert_eq!(count(80..160, &blue), 0);
    // The underline's shadow lies 8px under it.
    let row_of = |color: [u8; 4]| {
        (160..300).find(|&y| {
            pixels[y * 400 * 4..(y + 1) * 400 * 4]
                .as_chunks::<4>()
                .0
                .contains(&color)
        })
    };
    let underline = row_of([0, 0, 0, 255]).expect("underline");
    assert_eq!(row_of([0, 160, 0, 255]), Some(underline + 8));
}
