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
        target_dimension: 2048,
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
    let decoded = decode_limits().decode(&jpeg).unwrap();
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
    let decoded = decode_limits().decode(&png).unwrap();
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
    // half, as in Chrome. A dashed edge starts and ends with a dash.
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
    assert_eq!(pixel(30, 0), [0, 0, 0, 255]);
    assert_eq!(pixel(30, 1), [84, 84, 84, 255]);

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
