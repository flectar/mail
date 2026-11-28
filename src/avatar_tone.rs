//! The color an avatar without a picture takes: one of `Avatar`'s six
//! gradients (ui/components/avatar.slint), picked from the person's or the
//! account's address. The same address gets the same color in every view,
//! from the mail list and the reader to Contacts and the sidebars, and
//! across launches.

/// The palette slot, from 0 to 5, for `address`. Case and surrounding space
/// don't change it.
pub(crate) fn avatar_tone(address: &str) -> i32 {
    // FNV-1a gives a stable, inexpensive palette slot.
    let mut hash = 0x811c_9dc5_u32;
    let address = address.trim();
    // Unicode lowercasing can depend on surrounding characters (Greek sigma).
    // Keep that normalization for non-ASCII addresses, but hash ordinary
    // address bytes directly without an intermediate string.
    let normalized = if address.is_ascii() {
        std::borrow::Cow::Borrowed(address)
    } else {
        std::borrow::Cow::Owned(address.to_lowercase())
    };
    for byte in normalized.bytes().map(|byte| byte.to_ascii_lowercase()) {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    (hash % 6) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_keeps_its_color_whatever_its_case() {
        let tone = avatar_tone("person@example.com");
        assert_eq!(tone, avatar_tone("  Person@Example.COM  "));
        assert!((0..6).contains(&tone));
    }
}
