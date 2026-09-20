// Copyright 2026 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::fill_cluster_in_place;
use crate::analysis::cluster::CharCluster;
use crate::{FontContext, LayoutContext};

#[test]
fn presentation_selects_fonts_without_changing_character_metadata() {
    let mut fonts = FontContext::new();
    let mut layout = LayoutContext::<()>::new();
    let mut cluster = CharCluster::default();
    // Each input is a single grapheme. Reuse the cluster, as shaping does,
    // to also check that emoji state cannot leak into the next text cluster.
    for (text, emoji) in [
        ("😀", true),
        ("0", false),
        ("1", false),
        ("2", false),
        ("3", false),
        ("4", false),
        ("5", false),
        ("6", false),
        ("7", false),
        ("8", false),
        ("9", false),
        ("#", false),
        ("*", false),
        ("©", false),
        ("®", false),
        ("™", false),
        ("☺", false),
        ("❤", false),
        ("©\u{fe0f}", true),
        ("®\u{fe0f}", true),
        ("☺\u{fe0f}", true),
        ("❤\u{fe0f}", true),
        ("⌚\u{fe0e}", false),
        ("⌚", true),
        ("1\u{fe0e}", false),
        ("1\u{fe0f}\u{20e3}", true),
        ("#\u{fe0f}\u{20e3}", true),
        ("*\u{fe0f}\u{20e3}", true),
        ("1\u{20e3}", true),
        ("🇪🇸", true),
        ("👍🏽", true),
        ("👩\u{200d}💻", true),
        (
            "🏴\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
            true,
        ),
        ("a", false),
        ("中", false),
        ("e\u{301}", false),
    ] {
        let _ = layout
            .ranged_builder(&mut fonts, text, 1.0, true)
            .build(text);
        if "0123456789#*©®™☺❤".contains(text) {
            assert!(
                layout.info[0].0.is_emoji_or_pictograph(),
                "text presentation must preserve the broader character property"
            );
        }
        let mut offset = 0;
        fill_cluster_in_place(text, &mut layout.info.iter(), &mut offset, &mut cluster);
        assert_eq!(cluster.is_emoji, emoji, "presentation of {text:?}");
        assert_eq!(offset, text.len(), "byte range of {text:?}");
        assert_eq!(cluster.chars.len(), text.chars().count(), "character count");
        for ch in &cluster.chars {
            if matches!(ch.ch, '\u{fe0e}' | '\u{fe0f}') {
                assert!(
                    !ch.contributes_to_shaping,
                    "selector must not require a glyph"
                );
            }
        }
    }
}
