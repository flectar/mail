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
