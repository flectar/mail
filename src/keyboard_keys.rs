//! Takes key bindings apart for the Kbd caps, which Slint names and orders.

use crate::KeyChord;
use slint::Keys;

pub(super) fn chord(keys: Keys) -> KeyChord {
    let mut chord = KeyChord::default();
    for part in keys.to_parts() {
        match part {
            "Control" => chord.control = true,
            "Alt" => chord.alt = true,
            "Shift" => chord.shift = true,
            "Meta" => chord.meta = true,
            // These only let layouts that need the modifier still match.
            "Shift?" | "Alt?" => {}
            key => chord.key = key.into(),
        }
    }
    chord
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord_of(parts: &[&str]) -> KeyChord {
        chord(Keys::from_parts(parts.iter().copied()).unwrap())
    }

    #[test]
    fn chords_keep_pressed_modifiers_and_the_key() {
        let reopen = chord_of(&["Control", "Shift", "T"]);
        assert!(reopen.control && reopen.shift && !reopen.alt && !reopen.meta);
        assert_eq!(reopen.key, "t");
        // Slash and digits match with or without Shift; nobody presses it.
        let search = chord_of(&["Slash"]);
        assert!(!search.shift);
        assert_eq!(search.key, "/");
        assert_eq!(chord_of(&["Control", "Digit0"]).key, "0");
        assert_eq!(chord_of(&["Meta", "Alt", "Delete"]).key, "\u{7f}");
        assert_eq!(chord(Keys::default()), KeyChord::default());
    }
}
