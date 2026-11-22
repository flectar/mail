//! The reader opens and closes collapsible sections as a browser does: a click
//! on the summary, find in page and links within the message (#430).

use super::*;

/// The reader showing `html` as it comes from the core's sanitizer.
fn renderer(html: &str) -> GpuEmailRenderer {
    let html = flectar_mail_core::mime::sanitize_html(html);
    let mut renderer = GpuEmailRenderer::default();
    renderer.set_email(prepare_email_html(&html).expect("email should parse"));
    renderer
}

fn node(renderer: &GpuEmailRenderer, id: &str) -> NodeId {
    renderer
        .email
        .as_ref()
        .unwrap()
        .document
        .query_selector(&format!("#{id}"))
        .unwrap()
        .unwrap_or_else(|| panic!("no #{id}"))
}

fn open(renderer: &GpuEmailRenderer, id: &str) -> bool {
    let email = renderer.email.as_ref().unwrap();
    email
        .document
        .get_node(node(renderer, id))
        .unwrap()
        .data
        .has_attr(local_name!("open"))
}

/// A point on the first line of the element's box.
fn first_line(renderer: &GpuEmailRenderer, id: &str) -> (f32, f32) {
    let email = renderer.email.as_ref().unwrap();
    let origin = email
        .document
        .get_node(node(renderer, id))
        .unwrap()
        .absolute_position(0.0, 0.0);
    (origin.x + 4.0, origin.y + 4.0)
}

/// The text of the reader view's rows, which shows what the reader shows.
fn shown(renderer: &GpuEmailRenderer) -> String {
    renderer
        .reader_items()
        .iter()
        .map(|item| item.name.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn click(renderer: &mut GpuEmailRenderer, (x, y): (f32, f32)) -> bool {
    renderer.handle_pointer_event(x, y, "down", InputModifiers::default());
    renderer.handle_pointer_event(x, y, "up", InputModifiers::default())
}

/// Shaped like the GitHub release mail in #430: the summary holds a heading.
const RELEASE: &str = r#"<html><body style="margin:0"><hr>
<details id="changes"><summary id="summary"><h3>Changelog</h3></summary>
<h4>Core changes</h4><p style="height:300px">Fixed the needle in utils</p></details>
<p>This release has 25 assets</p></body></html>"#;

#[test]
fn a_click_on_the_summary_opens_and_closes_its_section() {
    let mut renderer = renderer(RELEASE);
    let closed = renderer.content_height;
    assert!(!shown(&renderer).contains("Core changes"));

    let summary = first_line(&renderer, "summary");
    assert!(click(&mut renderer, summary));
    assert!(open(&renderer, "changes"));
    assert!(shown(&renderer).contains("Core changes"));
    assert!(
        renderer.content_height > closed + 250.0,
        "{} after {closed}",
        renderer.content_height
    );
    assert!(!renderer.has_selection());

    let summary = first_line(&renderer, "summary");
    assert!(click(&mut renderer, summary));
    assert!(!open(&renderer, "changes"));
    assert_eq!(renderer.content_height, closed);
}

#[test]
fn a_drag_from_the_summary_selects_instead() {
    let mut renderer = renderer(RELEASE);
    let (x, y) = first_line(&renderer, "summary");
    let y = y + 30.0;
    renderer.handle_pointer_event(x, y, "down", InputModifiers::default());
    renderer.handle_pointer_event(x + 60.0, y, "move", InputModifiers::default());
    renderer.handle_pointer_event(x + 60.0, y, "up", InputModifiers::default());
    assert!(!open(&renderer, "changes"));
}

#[test]
fn a_link_in_the_summary_opens_the_link_only() {
    let mut renderer = renderer(
        r#"<body style="margin:0"><details id="d"><summary><a id="link" href="https://example.com" style="display:block">Release notes</a></summary><p>Body</p></details></body>"#,
    );
    let (x, y) = first_line(&renderer, "link");
    renderer.handle_pointer_event(x, y + 30.0, "down", InputModifiers::default());
    renderer.handle_pointer_event(x, y + 30.0, "up", InputModifiers::default());
    assert!(!open(&renderer, "d"));
}

#[test]
fn find_opens_the_section_that_holds_the_match() {
    let mut renderer = renderer(RELEASE);
    let (index, count, y) = renderer.find("needle", 0);
    assert_eq!((index, count), (1, 1));
    assert!(open(&renderer, "changes"));
    assert!(y.is_some_and(|y| y > 0.0));
    assert_eq!(renderer.selected_text().as_deref(), Some("needle"));
}

#[test]
fn find_counts_matches_in_closed_sections_in_order() {
    let mut renderer = renderer(
        r#"<p>needle one</p><details id="a"><summary>A</summary><p>needle two</p></details><p>needle three</p>"#,
    );
    let (index, count, _) = renderer.find("needle", 0);
    assert_eq!((index, count), (1, 3));
    assert!(
        !open(&renderer, "a"),
        "the first match is outside the section"
    );
    assert_eq!(renderer.find("needle", 1).0, 2);
    assert!(open(&renderer, "a"));
    assert_eq!(renderer.selected_text().as_deref(), Some("needle"));
    assert_eq!(renderer.find("needle", 1).0, 3);
}

#[test]
fn a_link_into_a_closed_section_opens_it() {
    let mut renderer = renderer(
        r##"<body style="margin:0"><a href="#target">Jump</a><details id="d"><summary>Section</summary><p style="height:200px">Padding</p><p id="target">Target</p></details></body>"##,
    );
    let y = renderer.fragment_y("#target").expect("the target is found");
    assert!(open(&renderer, "d"));
    assert!(y > 150.0, "{y}");
}

#[test]
fn a_section_the_sender_opened_starts_open() {
    let renderer =
        renderer(r#"<details open id="d"><summary>Summary</summary><p>Shown</p></details>"#);
    assert!(open(&renderer, "d"));
    assert!(shown(&renderer).contains("Shown"));
}

#[test]
fn reader_mode_lists_a_summary_as_a_button_that_opens_its_section() {
    let mut renderer = renderer(RELEASE);
    let items = renderer.reader_items();
    let summary = items
        .iter()
        .find(|item| item.kind == "section")
        .expect("a row for the summary");
    assert_eq!(
        (summary.name.as_str(), summary.section, summary.open),
        ("Changelog", 0, false)
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| item.name.contains("Changelog"))
            .count(),
        1,
        "the heading inside the summary is part of its button"
    );

    assert!(renderer.toggle_section(0));
    assert!(open(&renderer, "changes"));
    let items = renderer.reader_items();
    assert!(items.iter().any(|item| item.kind == "section" && item.open));
    assert!(shown(&renderer).contains("Core changes"));
    assert!(!renderer.toggle_section(1), "there is no second section");
}

#[test]
fn plain_text_shows_what_closed_sections_hold() {
    let renderer = renderer(RELEASE);
    assert!(!open(&renderer, "changes"));
    let text = renderer.plain_text();
    assert!(
        text.contains("Changelog") && text.contains("Fixed the needle"),
        "{text}"
    );
}
