//! Legacy HTML presentational attributes, mapped to CSS as the HTML rendering
//! section describes.
//!
//! Every hint for an element is serialized as one small, canonical CSS
//! declaration list and parsed once per document. Stylo compares hint blocks
//! by pointer when deciding whether siblings may share a computed style, so
//! handing out the same block for the same attributes keeps style sharing
//! effective for large legacy tables, where nearly every cell carries
//! `align`, `valign`, `width` or `bgcolor`.
//!
//! Attribute values are untrusted. Each value is parsed into a number, color
//! or keyword before it is written to the declaration list; raw attribute text
//! is never copied into CSS, except font family names, which are serialized as
//! escaped CSS strings.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{LazyLock, Mutex};

use markup5ever::{local_name, ns};
use selectors::sink::Push;
use style::applicable_declarations::ApplicableDeclarationBlock;
use style::context::QuirksMode;
use style::properties::{PropertyDeclarationBlock, parse_style_attribute};
use style::rule_tree::{CascadeLevel, CascadeOrigin};
use style::servo_arc::Arc as ServoArc;
use style::shared_lock::Locked;
use style::stylesheets::layer_rule::LayerOrder;
use style::stylesheets::{CssRuleType, UrlExtraData};

use crate::node::{ElementData, Node, NodeData};

type HintBlock = ServoArc<Locked<PropertyDeclarationBlock>>;

/// Hint blocks for one document, keyed by their canonical CSS text.
#[derive(Default)]
pub struct PresentationalHintCache(Mutex<HashMap<Box<str>, HintBlock>>);

impl std::fmt::Debug for PresentationalHintCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.0.lock().map(|map| map.len()).unwrap_or_default();
        f.debug_struct("PresentationalHintCache")
            .field("len", &len)
            .finish()
    }
}

// Hints never contain URLs, so they resolve against an inert base.
static HINT_URL_DATA: LazyLock<UrlExtraData> = LazyLock::new(|| {
    UrlExtraData(ServoArc::new(
        url::Url::parse("about:blank").expect("static URL is valid"),
    ))
});

pub(crate) fn synthesize<V>(node: &Node, hints: &mut V)
where
    V: Push<ApplicableDeclarationBlock>,
{
    let Some(elem) = node.data.downcast_element() else {
        return;
    };
    let accepts_lang = elem.name.ns == ns!(html) || elem.name.ns == ns!(svg);
    let mut lang = None;
    for attr in elem.attrs() {
        if attr.name.local == local_name!("lang") {
            if attr.name.ns == ns!(xml) {
                lang = Some(attr.value.as_str());
            } else if attr.name.ns == ns!() && accepts_lang {
                lang = lang.or(Some(attr.value.as_str()));
            }
        }
    }
    if let Some(lang) = lang {
        use style::properties::{Importance, PropertyDeclaration};
        use style::values::specified::XLang;
        let block = ServoArc::new(node.guard().wrap(PropertyDeclarationBlock::with_one(
            PropertyDeclaration::XLang(XLang(style::Atom::from(lang))),
            Importance::Normal,
        )));
        hints.push(ApplicableDeclarationBlock::from_declarations(
            block,
            CascadeLevel::new(CascadeOrigin::PresHints),
            LayerOrder::root(),
        ));
    }
    // Cells also inherit hints from their table's attributes.
    let is_cell = elem.name.local == local_name!("td") || elem.name.local == local_name!("th");
    if elem.attrs().is_empty() && !is_cell {
        return;
    }

    let mut css = String::new();
    collect(node, elem, &mut css);
    if css.is_empty() {
        return;
    }

    let block = cached_block(node, css);
    hints.push(ApplicableDeclarationBlock::from_declarations(
        block,
        CascadeLevel::new(CascadeOrigin::PresHints),
        LayerOrder::root(),
    ));
}

fn cached_block(node: &Node, css: String) -> HintBlock {
    let parse = |css: &str| {
        ServoArc::new(node.guard().wrap(parse_style_attribute(
            css,
            &HINT_URL_DATA,
            None,
            QuirksMode::NoQuirks,
            CssRuleType::Style,
        )))
    };

    let mut doc = node;
    while let Some(parent) = doc.parent {
        doc = doc.with(parent);
    }
    let NodeData::Document(data) = &doc.data else {
        // Detached subtrees are styled rarely; they just skip the cache.
        return parse(&css);
    };

    let mut cache = data
        .presentational_hints
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(block) = cache.get(css.as_str()) {
        return block.clone();
    }
    let block = parse(&css);
    cache.insert(css.into_boxed_str(), block.clone());
    block
}

fn attr<'a>(elem: &'a ElementData, name: &str) -> Option<&'a str> {
    elem.attrs()
        .iter()
        .find(|attr| &*attr.name.local == name)
        .map(|attr| attr.value.as_str())
}

/// The `<table>` that owns a cell or row, following the table model's
/// parent links (`td` → `tr` → optional row group → `table`).
fn owning_table(node: &Node) -> Option<&Node> {
    let mut current = node;
    for _ in 0..3 {
        current = current.with(current.parent?);
        let name = &current.element_data()?.name.local;
        if *name == local_name!("table") {
            return Some(current);
        }
        if *name != local_name!("tr")
            && *name != local_name!("tbody")
            && *name != local_name!("thead")
            && *name != local_name!("tfoot")
        {
            return None;
        }
    }
    None
}

fn collect(node: &Node, elem: &ElementData, css: &mut String) {
    let tag = &elem.name.local;
    let is_cell = *tag == local_name!("td") || *tag == local_name!("th");

    // `<input type=image>` is the only input type that is replaced content,
    // and the only one that takes the embedded-content attributes.
    let is_image_input = *tag == local_name!("input")
        && attr(elem, "type").is_some_and(|value| value.eq_ignore_ascii_case("image"));
    let is_embedded = *tag == local_name!("iframe")
        || *tag == local_name!("embed")
        || *tag == local_name!("video")
        || *tag == local_name!("object")
        || *tag == local_name!("img")
        || *tag == local_name!("marquee")
        || is_image_input;

    // Inherited table attributes come first so the cell's own attributes,
    // such as `bgcolor`, win where they overlap.
    if is_cell {
        if let Some(table) = owning_table(node).and_then(|table| table.element_data()) {
            if let Some(padding) = attr(table, "cellpadding").and_then(parse_non_negative_integer) {
                let _ = write!(css, "padding:{padding}px;");
            }
            if table_border_width(table).is_some_and(|width| width > 0)
                && attr(table, "rules").is_none()
                && attr(table, "frame").is_none()
            {
                // No colour: cell borders follow the cell's `currentcolor`.
                css.push_str("border-width:1px;border-style:inset;");
            }
        }
    }

    for attr in elem.attrs() {
        let name = &*attr.name.local;
        let value = attr.value.as_str();

        match name {
            "align" => align_hint(
                tag,
                is_embedded && *tag != local_name!("video") && *tag != local_name!("marquee"),
                value,
                css,
            ),
            "valign"
                if is_cell
                    || *tag == local_name!("tr")
                    || *tag == local_name!("tbody")
                    || *tag == local_name!("thead")
                    || *tag == local_name!("tfoot")
                    || *tag == local_name!("col")
                    || *tag == local_name!("colgroup") =>
            {
                let keyword = match value.trim().to_ascii_lowercase().as_str() {
                    "top" => "top",
                    "middle" => "middle",
                    "bottom" => "bottom",
                    "baseline" => "baseline",
                    _ => continue,
                };
                let _ = write!(css, "vertical-align:{keyword};");
            }
            "width" | "height" => {
                let is_width = name == "width";
                // https://html.spec.whatwg.org/multipage/rendering.html#dimRendering
                let maps_to_dimension = is_embedded
                    || is_cell
                    || *tag == local_name!("table")
                    || if is_width {
                        *tag == local_name!("col")
                            || *tag == local_name!("colgroup")
                            || *tag == local_name!("hr")
                    } else {
                        *tag == local_name!("thead")
                            || *tag == local_name!("tbody")
                            || *tag == local_name!("tfoot")
                            || *tag == local_name!("tr")
                    };
                if maps_to_dimension {
                    // `table width` and `td`/`th` in both axes use the
                    // "(ignoring zero)" variant, where zero is dropped.
                    let ignoring_zero = is_cell || (is_width && *tag == local_name!("table"));
                    if let Some(size) = parse_dimension(value, ignoring_zero) {
                        let _ = write!(css, "{name}:{size};");
                    }
                } else if *tag == local_name!("svg") {
                    // SVG presentation attributes take any CSS length.
                    if let Some(size) = parse_svg_size(value) {
                        let _ = write!(css, "{name}:{size};");
                    }
                }
            }
            // hspace/vspace apply to a smaller set than width/height: not
            // `iframe` or `video`.
            "hspace" | "vspace"
                if is_embedded && *tag != local_name!("iframe") && *tag != local_name!("video") =>
            {
                if let Some(size) = parse_dimension(value, false) {
                    let (a, b) = if name == "hspace" {
                        ("left", "right")
                    } else {
                        ("top", "bottom")
                    };
                    let _ = write!(css, "margin-{a}:{size};margin-{b}:{size};");
                }
            }
            "border" => {
                if *tag == local_name!("table") {
                    if let Some(width) = table_border_width(elem) {
                        let _ = write!(css, "border-width:{width}px;");
                        if width > 0 {
                            css.push_str("border-style:outset;");
                        }
                    }
                } else if *tag == local_name!("img")
                    || *tag == local_name!("object")
                    || is_image_input
                {
                    // Width alone would compute back to zero against the
                    // default border-style of `none`. Like Blink and Gecko,
                    // zero is mapped too; the colour is left alone.
                    if let Some(width) = parse_non_negative_integer(value) {
                        let _ = write!(css, "border-width:{width}px;border-style:solid;");
                    }
                }
            }
            "cellspacing" if *tag == local_name!("table") => {
                if let Some(spacing) = parse_non_negative_integer(value) {
                    let _ = write!(css, "border-spacing:{spacing}px;");
                }
            }
            "bordercolor" if *tag == local_name!("table") => {
                if let Some(color) = parse_legacy_color(value) {
                    let _ = write!(css, "border-color:{color};");
                }
            }
            "nowrap" if is_cell => css.push_str("white-space:nowrap;"),
            "bgcolor"
                if *tag == local_name!("body")
                    || *tag == local_name!("table")
                    || *tag == local_name!("thead")
                    || *tag == local_name!("tbody")
                    || *tag == local_name!("tfoot")
                    || *tag == local_name!("tr")
                    || is_cell
                    || *tag == local_name!("marquee") =>
            {
                if let Some(color) = parse_legacy_color(value) {
                    let _ = write!(css, "background-color:{color};");
                }
            }
            "text" if *tag == local_name!("body") => {
                if let Some(color) = parse_legacy_color(value) {
                    let _ = write!(css, "color:{color};");
                }
            }
            "color" if *tag == local_name!("font") => {
                if let Some(color) = parse_legacy_color(value) {
                    let _ = write!(css, "color:{color};");
                }
            }
            "face" if *tag == local_name!("font") => {
                if let Some(families) = font_family_list(value) {
                    let _ = write!(css, "font-family:{families};");
                }
            }
            "size" if *tag == local_name!("font") => {
                if let Some(size) = legacy_font_size(value) {
                    let _ = write!(css, "font-size:{size};");
                }
            }
            "hidden" => css.push_str("display:none;"),
            _ => {}
        }
    }

    // `body` margins: per axis, the first of these attributes that exists
    // sets both sides; if it does not parse, the default margin stays. There
    // is no `rightmargin` or `bottommargin`.
    if *tag == local_name!("body") {
        for (attrs, sides) in [
            (["marginheight", "topmargin"], ["top", "bottom"]),
            (["marginwidth", "leftmargin"], ["left", "right"]),
        ] {
            if let Some(px) = attrs
                .iter()
                .find_map(|name| attr(elem, name))
                .and_then(parse_non_negative_integer)
            {
                for side in sides {
                    let _ = write!(css, "margin-{side}:{px}px;");
                }
            }
        }
    }
}

fn align_hint(tag: &markup5ever::LocalName, is_embedded: bool, value: &str, css: &mut String) {
    let value = value.trim().to_ascii_lowercase();
    if *tag == local_name!("table") {
        // A table aligns itself, not its contents.
        css.push_str(match value.as_str() {
            "left" => "float:left;",
            "right" => "float:right;",
            "center" => "margin-left:auto;margin-right:auto;",
            _ => "",
        });
    } else if is_embedded {
        css.push_str(match value.as_str() {
            "left" => "float:left;",
            "right" => "float:right;",
            "top" => "vertical-align:top;",
            // The spec wants the element's middle on the parent's baseline, which
            // Stylo cannot express outside Gecko; `middle` is the closest value.
            "middle" | "center" | "absmiddle" | "abscenter" => "vertical-align:middle;",
            "bottom" | "baseline" => "vertical-align:baseline;",
            "texttop" => "vertical-align:text-top;",
            "absbottom" => "vertical-align:bottom;",
            _ => "",
        });
    } else if *tag == local_name!("hr") {
        css.push_str(match value.as_str() {
            "left" => "margin-left:0;margin-right:auto;",
            "right" => "margin-left:auto;margin-right:0;",
            "center" => "margin-left:auto;margin-right:auto;",
            _ => "",
        });
    } else if *tag == local_name!("caption") {
        // Only `bottom` maps; other values leave the caption's text alone.
        if value == "bottom" {
            css.push_str("caption-side:bottom;");
        }
    } else if *tag == local_name!("div")
        || *tag == local_name!("thead")
        || *tag == local_name!("tbody")
        || *tag == local_name!("tfoot")
        || *tag == local_name!("tr")
        || *tag == local_name!("td")
        || *tag == local_name!("th")
    {
        // These also align block-level descendants, which is what the
        // `-moz-` keywords add over their plain counterparts.
        css.push_str(match value.as_str() {
            "left" => "text-align:-moz-left;",
            "right" => "text-align:-moz-right;",
            "center" | "middle" => "text-align:-moz-center;",
            "justify" => "text-align:justify;",
            "absmiddle" if *tag != local_name!("div") => "text-align:center;",
            _ => "",
        });
    } else if *tag == local_name!("p")
        || *tag == local_name!("h1")
        || *tag == local_name!("h2")
        || *tag == local_name!("h3")
        || *tag == local_name!("h4")
        || *tag == local_name!("h5")
        || *tag == local_name!("h6")
    {
        css.push_str(match value.as_str() {
            "left" => "text-align:left;",
            "right" => "text-align:right;",
            "center" | "middle" => "text-align:center;",
            "justify" => "text-align:justify;",
            _ => "",
        });
    }
}

/// The table `border` attribute: a missing value or a parse error means 1px.
fn table_border_width(table: &ElementData) -> Option<u32> {
    attr(table, "border").map(|value| parse_non_negative_integer(value).unwrap_or(1))
}

/// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#rules-for-parsing-non-negative-integers
fn parse_non_negative_integer(value: &str) -> Option<u32> {
    // Clamp absurd values, which only hostile markup uses.
    style::servo::attr::parse_unsigned_integer(value.chars())
        .ok()
        .map(|value| value.min(100_000))
}

/// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#rules-for-parsing-dimension-values
fn parse_dimension(value: &str, ignoring_zero: bool) -> Option<String> {
    use style::servo::attr::{LengthOrPercentageOrAuto, parse_length, parse_nonzero_length};
    let parsed = if ignoring_zero {
        parse_nonzero_length(value)
    } else {
        parse_length(value)
    };
    match parsed {
        LengthOrPercentageOrAuto::Length(length) => Some(format!("{}px", length.to_f32_px())),
        LengthOrPercentageOrAuto::Percentage(fraction) => Some(format!("{}%", fraction * 100.0)),
        LengthOrPercentageOrAuto::Auto => None,
    }
}

/// An SVG `width`/`height`: any non-negative CSS length or percentage, where
/// a unitless number means user units (CSS px).
fn parse_svg_size(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(number) = value.strip_suffix('%') {
        let number: f32 = number.trim().parse().ok()?;
        return (number >= 0.0 && number.is_finite()).then(|| format!("{number}%"));
    }
    // The unit is the trailing run of ASCII letters, which never eats into a
    // scientific exponent such as `1e3`.
    let number_len = value
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .len();
    let (number, unit) = value.split_at(number_len);
    let number: f32 = number.trim().parse().ok()?;
    if number < 0.0 || !number.is_finite() {
        return None;
    }
    if unit.is_empty() {
        return Some(format!("{number}px"));
    }
    const UNITS: &[&str] = &[
        "px", "em", "ex", "ch", "rem", "vw", "vh", "vmin", "vmax", "cm", "mm", "q", "in", "pt",
        "pc",
    ];
    let unit = unit.to_ascii_lowercase();
    UNITS
        .contains(&unit.as_str())
        .then(|| format!("{number}{unit}"))
}

/// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#rules-for-parsing-a-legacy-colour-value
///
/// Email commonly relies on the legacy forms: named colors, hex without `#`
/// and malformed values such as `#fffff`, which browsers still accept.
fn parse_legacy_color(value: &str) -> Option<String> {
    let value = value.trim_matches(|c: char| c.is_ascii_whitespace());
    if value.is_empty() || value.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if let Ok((r, g, b)) = cssparser::color::parse_named_color(&value.to_ascii_lowercase()) {
        return Some(format!("#{r:02x}{g:02x}{b:02x}"));
    }
    let bytes = value.as_bytes();
    if bytes.len() == 4 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit) {
        let expand = |c: u8| (c as char).to_digit(16).unwrap() * 17;
        return Some(format!(
            "#{:02x}{:02x}{:02x}",
            expand(bytes[1]),
            expand(bytes[2]),
            expand(bytes[3])
        ));
    }

    let mut digits: Vec<u8> = Vec::with_capacity(value.len().min(128));
    for c in value.chars() {
        if c as u32 > 0xFFFF {
            digits.extend_from_slice(b"00");
        } else if c.is_ascii_hexdigit() {
            digits.push(c as u8);
        } else {
            digits.push(b'0');
        }
    }
    digits.truncate(128);
    if value.starts_with('#') {
        digits.remove(0);
    }
    while digits.is_empty() || digits.len() % 3 != 0 {
        digits.push(b'0');
    }
    let mut length = digits.len() / 3;
    let mut parts: Vec<&[u8]> = digits.chunks(length).collect();
    if length > 8 {
        for part in &mut parts {
            *part = &part[length - 8..];
        }
        length = 8;
    }
    while length > 2 && parts.iter().all(|part| part[0] == b'0') {
        for part in &mut parts {
            *part = &part[1..];
        }
        length -= 1;
    }
    let component = |part: &[u8]| {
        let part = &part[..part.len().min(2)];
        u8::from_str_radix(std::str::from_utf8(part).ok()?, 16).ok()
    };
    Some(format!(
        "#{:02x}{:02x}{:02x}",
        component(parts[0])?,
        component(parts[1])?,
        component(parts[2])?
    ))
}

/// The `<font face>` value parsed as a CSS `font-family` value, as browsers
/// parse it, and serialized back. A value that is not a valid family list
/// maps to nothing, so the element keeps its inherited family.
fn font_family_list(value: &str) -> Option<String> {
    use style::properties::{
        PropertyDeclaration, PropertyId, SourcePropertyDeclaration, parse_one_declaration_into,
    };
    use style::stylesheets::Origin;
    use style_traits::ParsingMode;

    let property = PropertyId::parse_enabled_for_all_content("font-family").ok()?;
    let mut declarations = SourcePropertyDeclaration::default();
    parse_one_declaration_into(
        &mut declarations,
        property,
        value,
        Origin::Author,
        &HINT_URL_DATA,
        None,
        ParsingMode::DEFAULT,
        QuirksMode::NoQuirks,
        CssRuleType::Style,
    )
    .ok()?;
    let declaration = declarations.drain().declarations.next()?;
    // Only a list of families: no CSS-wide keywords or variables.
    let PropertyDeclaration::FontFamily(_) = declaration else {
        return None;
    };
    let mut css = String::new();
    declaration.to_css(&mut css).ok()?;
    Some(css)
}

/// https://html.spec.whatwg.org/multipage/rendering.html#rules-for-parsing-a-legacy-font-size
fn legacy_font_size(value: &str) -> Option<&'static str> {
    let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (sign, digits) = match value.as_bytes().first()? {
        b'+' => (1, &value[1..]),
        b'-' => (-1, &value[1..]),
        _ => (0, value),
    };
    let len = digits.len()
        - digits
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    if len == 0 {
        return None;
    }
    let number: i64 = digits[..len].parse().unwrap_or(i64::MAX).min(7);
    let size = match sign {
        1 => 3 + number,
        -1 => 3 - number,
        _ => number,
    }
    .clamp(1, 7);
    Some(match size {
        1 => "x-small",
        2 => "small",
        3 => "medium",
        4 => "large",
        5 => "x-large",
        6 => "xx-large",
        _ => "xxx-large",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_colors_follow_the_html_algorithm() {
        assert_eq!(parse_legacy_color("#FFF").as_deref(), Some("#ffffff"));
        assert_eq!(parse_legacy_color("ff0000").as_deref(), Some("#ff0000"));
        assert_eq!(parse_legacy_color(" Red ").as_deref(), Some("#ff0000"));
        assert_eq!(parse_legacy_color("#fffff").as_deref(), Some("#fffff0"));
        assert_eq!(
            parse_legacy_color("chucknorris").as_deref(),
            Some("#c00000")
        );
        assert_eq!(parse_legacy_color("transparent"), None);
        assert_eq!(parse_legacy_color(""), None);
    }

    #[test]
    fn legacy_font_sizes_are_relative_to_three() {
        assert_eq!(legacy_font_size("+2"), Some("x-large"));
        assert_eq!(legacy_font_size("-1"), Some("small"));
        assert_eq!(legacy_font_size("1"), Some("x-small"));
        assert_eq!(legacy_font_size("99"), Some("xxx-large"));
        assert_eq!(legacy_font_size("x"), None);
    }

    #[test]
    fn font_faces_parse_as_css_family_lists() {
        assert_eq!(
            font_family_list(r#"Georgia, "Times New Roman", serif"#).as_deref(),
            Some(r#"Georgia, "Times New Roman", serif"#)
        );
        assert_eq!(
            font_family_list("Segoe UI,Tahoma").as_deref(),
            Some("Segoe UI, Tahoma")
        );
        // Not a family list: browsers keep the inherited family.
        assert_eq!(font_family_list("Calibri (Body)"), None);
        assert_eq!(font_family_list("inherit"), None);
        assert_eq!(font_family_list(r#"a"\;}x{color:red"#), None);
        assert_eq!(
            font_family_list(r#""a;}x{color:red"#).as_deref(),
            Some(r#""a;}x{color:red""#)
        );
    }

    #[test]
    fn integers_ignore_trailing_garbage() {
        assert_eq!(parse_non_negative_integer(" 10px"), Some(10));
        assert_eq!(parse_non_negative_integer("0"), Some(0));
        assert_eq!(parse_non_negative_integer("-1"), None);
        // The integer rules accept one sign, so "-0" is zero.
        assert_eq!(parse_non_negative_integer("-0foo"), Some(0));
        assert_eq!(parse_non_negative_integer("+0%"), Some(0));
        assert_eq!(parse_non_negative_integer("+-1"), None);
    }
}
