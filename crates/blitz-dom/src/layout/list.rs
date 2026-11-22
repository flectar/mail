use blitz_traits::node_id::NodeId;
use markup5ever::local_name;
use parley::FontFamily;
use style::computed_values::list_style_type::T as ListStyleType;
use style::{
    computed_values::list_style_position::T as ListStylePosition, counter_style::CounterStyle,
};

use crate::{
    BaseDocument,
    node::{ListItemLayout, ListItemLayoutPosition, Marker},
    stylo_to_parley,
};

pub(super) const BULLET_FONT_FAMILY: &str = "Bullet, monospace, sans-serif";

/// Number the list items among `container_id`'s children, and the list
/// items among theirs, as the HTML standard numbers a list's items (section
/// 4.4.8). An `<ol>` counts from its `start`, or when `reversed` from its
/// number of items, down when `reversed` and up otherwise, and an `<li>`'s
/// `value` sets its own number, which the count goes on from.
pub(super) fn number_list_items(doc: &mut BaseDocument, container_id: NodeId) {
    let (start, reversed) = match doc.nodes[container_id].element_data() {
        Some(el) if el.name.local == local_name!("ol") => (
            el.attr(local_name!("start")).and_then(parse_html_integer),
            el.has_attr(local_name!("reversed")),
        ),
        _ => (None, false),
    };
    let mut items = Vec::new();
    collect_list_items(doc, container_id, &mut items);

    let mut ordinal = start.unwrap_or(if reversed { items.len() as i64 } else { 1 });
    for item in items {
        let value = doc.nodes[item]
            .element_data()
            .filter(|el| el.name.local == local_name!("li"))
            .and_then(|el| el.attr(local_name!("value")))
            .and_then(parse_html_integer);
        if let Some(value) = value {
            ordinal = value;
        }
        let layout = list_item_layout(doc, item, ordinal);
        if let Some(element_data) = doc.nodes[item].element_data_mut() {
            element_data.list_item_data = layout.map(Box::new);
        }
        ordinal += if reversed { -1 } else { 1 };
    }
}

/// The HTML rules for parsing integers, as Blink reads `start` and `value`:
/// leading white space, a sign and digits, whatever follows ignored, and no
/// number past 32 bits.
fn parse_html_integer(value: &str) -> Option<i64> {
    style::servo::attr::parse_integer(value.chars())
        .ok()
        .map(i64::from)
}

/// The list items among `node_id`'s children and, in tree order, the list
/// items among theirs. Other children lose any marker they had.
fn collect_list_items(doc: &mut BaseDocument, node_id: NodeId, items: &mut Vec<NodeId>) {
    for child in doc.nodes[node_id].children.clone() {
        if is_list_item(doc, child) {
            items.push(child);
            collect_list_items(doc, child, items);
        } else if let Some(element_data) = doc.nodes[child].element_data_mut() {
            element_data.list_item_data = None;
        }
    }
}

/// Whether a node is a list item of its parent's list: `display: list-item`,
/// except for lists of their own.
fn is_list_item(doc: &BaseDocument, node_id: NodeId) -> bool {
    let node = &doc.nodes[node_id];
    node.primary_styles()
        .is_some_and(|style| style.get_box().display.is_list_item())
        && !node.element_data().is_some_and(|element_data| {
            matches!(
                element_data.name.local,
                local_name!("ol") | local_name!("ul"),
            )
        })
}

/// The marker of the list item numbered `ordinal`, and where it goes.
fn list_item_layout(
    doc: &mut BaseDocument,
    child_id: NodeId,
    ordinal: i64,
) -> Option<ListItemLayout> {
    let styles = doc.nodes[child_id].primary_styles()?;
    let list_style_type = styles.clone_list_style_type();
    let list_style_position = styles.clone_list_style_position();
    let marker = marker_for_style(list_style_type.clone(), ordinal)?;

    let position = match list_style_position {
        ListStylePosition::Inside => ListItemLayoutPosition::Inside,
        ListStylePosition::Outside => {
            let mut parley_style = stylo_to_parley::style(child_id, &styles);
            // A marker keeps its suffix's space, as `::marker` has
            // `white-space: pre`, so an outside marker ends a space before
            // the item's content.
            parley_style.white_space_collapse = parley::WhiteSpaceCollapse::Preserve;

            if let Some(font_family) = font_for_bullet_style(list_style_type) {
                parley_style.font_family = font_family;
            }

            // Create a parley tree builder
            let mut font_ctx = doc.font_ctx.lock().unwrap();
            let mut builder = doc.layout_ctx.tree_builder(
                &mut font_ctx,
                doc.viewport.scale(),
                true,
                &parley_style,
            );

            match &marker {
                Marker::Char(char) => {
                    let mut buf = [0u8; 4];
                    builder.push_text(char.encode_utf8(&mut buf));
                }
                Marker::String(str) => builder.push_text(str),
            };

            let mut layout = builder.build().0;
            let width = layout.calculate_content_widths().max;
            layout.break_all_lines(Some(width));

            ListItemLayoutPosition::Outside(Box::new(layout))
        }
    };

    Some(ListItemLayout { marker, position })
}

/// The marker of the list item numbered `ordinal`.
fn marker_for_style(list_style_type: ListStyleType, ordinal: i64) -> Option<Marker> {
    Some(match list_style_type.0 {
        CounterStyle::None => return None,
        CounterStyle::Name(name) => match &*name.0 {
            "disc" => Marker::Char('•'),
            "circle" => Marker::Char('◦'),
            "square" => Marker::Char('▪'),
            "disclosure-open" => Marker::Char('▾'),
            "disclosure-closed" => Marker::Char('▸'),
            name => Marker::String(format!("{}. ", counter_representation(name, ordinal))),
        },
        CounterStyle::String(atom_string) => Marker::String(atom_string.as_ref().to_string()),
        CounterStyle::Symbols { symbols, .. } => {
            // Use symbols from counter-style definition
            let syms = &symbols.0;
            let len = syms.len();
            if len == 0 {
                Marker::Char('•')
            } else {
                // Cycle through symbols based on the number: 1st, 2nd, 1st, 2nd, etc.
                let idx = (ordinal - 1).rem_euclid(len as i64) as usize;
                // Symbol is String or Ident variant, extract the string value
                let sym = &syms[idx];
                let marker_str = match sym {
                    style::counter_style::Symbol::String(s) => s.to_string(),
                    style::counter_style::Symbol::Ident(id) => id.0.to_string(),
                };
                Marker::String(marker_str)
            }
        }
    })
}

/// `ordinal` in one of the predefined counter styles of CSS Counter Styles 3
/// (sections 6.1 to 6.3), or in `decimal` where the style has no
/// representation for it and for the styles Blitz doesn't define, as the
/// specification treats an undefined name.
fn counter_representation(name: &str, ordinal: i64) -> String {
    const LATIN: [char; 26] = [
        'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r',
        's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
    ];
    const GREEK: [char; 24] = [
        'α', 'β', 'γ', 'δ', 'ε', 'ζ', 'η', 'θ', 'ι', 'κ', 'λ', 'μ', 'ν', 'ξ', 'ο', 'π', 'ρ', 'σ',
        'τ', 'υ', 'φ', 'χ', 'ψ', 'ω',
    ];
    let representation = match name {
        "lower-alpha" | "lower-latin" => alphabetic(ordinal, &LATIN),
        "upper-alpha" | "upper-latin" => {
            alphabetic(ordinal, &LATIN).map(|marker| marker.to_ascii_uppercase())
        }
        "lower-greek" => alphabetic(ordinal, &GREEK),
        "lower-roman" => roman(ordinal),
        "upper-roman" => roman(ordinal).map(|marker| marker.to_ascii_uppercase()),
        // Padded to two digits, the minus sign included.
        "decimal-leading-zero" if ordinal >= 0 => Some(format!("{ordinal:02}")),
        _ => None,
    };
    representation.unwrap_or_else(|| ordinal.to_string())
}

/// An alphabetic counter (CSS Counter Styles 3, section 3.1.4): `a` to `z`,
/// then `aa`, from 1 up.
fn alphabetic(ordinal: i64, symbols: &[char]) -> Option<String> {
    if ordinal < 1 {
        return None;
    }
    let base = symbols.len() as i64;
    let mut value = ordinal;
    let mut reversed = Vec::new();
    while value > 0 {
        value -= 1;
        reversed.push(symbols[(value % base) as usize]);
        value /= base;
    }
    Some(reversed.into_iter().rev().collect())
}

/// A lowercase roman numeral, from 1 to 3999 (CSS Counter Styles 3, section
/// 6.1).
fn roman(ordinal: i64) -> Option<String> {
    const WEIGHTS: [(i64, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    if !(1..=3999).contains(&ordinal) {
        return None;
    }
    let mut value = ordinal;
    let mut numeral = String::new();
    for (weight, symbol) in WEIGHTS {
        while value >= weight {
            numeral.push_str(symbol);
            value -= weight;
        }
    }
    Some(numeral)
}

// Override the font to our specific bullet font when rendering bullets
fn font_for_bullet_style(list_style_type: ListStyleType) -> Option<FontFamily<'static>> {
    if list_style_type.0.is_bullet() {
        return Some(BULLET_FONT_FAMILY.into());
    }

    None
}

#[cfg(test)]
mod tests {
    use crate::node::Marker;
    use style::{
        Atom,
        counter_style::CounterStyle,
        values::{CustomIdent, computed::ListStyleType},
    };

    use super::marker_for_style;

    fn list_style(s: &str) -> ListStyleType {
        ListStyleType(CounterStyle::Name(CustomIdent(Atom::from(s))))
    }

    fn marker(style: &str, ordinal: i64) -> Option<Marker> {
        marker_for_style(list_style(style), ordinal)
    }

    fn string(marker: &str) -> Option<Marker> {
        Some(Marker::String(marker.to_string()))
    }

    #[test]
    fn test_marker_for_disc() {
        let result = marker_for_style(ListStyleType::disc(), 1);
        assert_eq!(result, Some(Marker::Char('•')));
    }

    #[test]
    fn test_marker_for_decimal() {
        assert_eq!(marker("decimal", 1), string("1. "));
        assert_eq!(marker("decimal", 2), string("2. "));
        assert_eq!(marker("decimal", 0), string("0. "));
        assert_eq!(marker("decimal", -3), string("-3. "));
        assert_eq!(marker("decimal-leading-zero", 5), string("05. "));
        assert_eq!(marker("decimal-leading-zero", -5), string("-5. "));
    }

    #[test]
    fn test_marker_for_lower_alpha() {
        assert_eq!(marker("lower-alpha", 1), string("a. "));
        assert_eq!(marker("lower-alpha", 2), string("b. "));
        assert_eq!(marker("lower-alpha", 26), string("z. "));
        assert_eq!(marker("lower-alpha", 27), string("aa. "));
        assert_eq!(marker("lower-alpha", 28), string("ab. "));
        assert_eq!(marker("lower-latin", 703), string("aaa. "));
    }

    #[test]
    fn test_marker_for_upper_alpha() {
        assert_eq!(marker("upper-alpha", 1), string("A. "));
        assert_eq!(marker("upper-alpha", 2), string("B. "));
        assert_eq!(marker("upper-alpha", 27), string("AA. "));
        assert_eq!(marker("upper-latin", 28), string("AB. "));
    }

    #[test]
    fn counter_styles_without_a_representation_fall_back_to_decimal() {
        // Alphabetic styles start at 1 and roman numerals end at 3999.
        assert_eq!(marker("lower-alpha", 0), string("0. "));
        assert_eq!(marker("upper-roman", 4000), string("4000. "));
        // As for a name no style defines.
        assert_eq!(marker("hebrew", 7), string("7. "));
    }

    #[test]
    fn roman_and_greek_markers() {
        assert_eq!(marker("lower-roman", 1), string("i. "));
        assert_eq!(marker("lower-roman", 4), string("iv. "));
        assert_eq!(marker("upper-roman", 1994), string("MCMXCIV. "));
        assert_eq!(marker("upper-roman", 3999), string("MMMCMXCIX. "));
        assert_eq!(marker("lower-greek", 1), string("α. "));
        assert_eq!(marker("lower-greek", 25), string("αα. "));
    }
}
