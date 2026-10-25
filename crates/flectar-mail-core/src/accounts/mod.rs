pub mod backup;
pub mod credentials;
pub mod providers;

/// The form every spelling of a mailbox's address shares, for telling whether
/// two addresses belong to the same account. Addresses compare without ASCII
/// case, and Gmail also ignores dots before the `@` and treats googlemail.com
/// as gmail.com. Google Workspace and other domains keep their dots.
pub fn mailbox_key(address: &str) -> String {
    let address = address.trim().to_ascii_lowercase();
    match address.rsplit_once('@') {
        Some((name, "gmail.com" | "googlemail.com")) => {
            format!("{}@gmail.com", name.replace('.', ""))
        }
        _ => address,
    }
}

/// Whether two addresses belong to the same mailbox; see [`mailbox_key`].
pub fn same_mailbox(a: &str, b: &str) -> bool {
    mailbox_key(a) == mailbox_key(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_spellings_share_one_mailbox() {
        for spelling in [
            "johndoe@gmail.com",
            "john.doe@gmail.com",
            "J.O.H.N.Doe@Gmail.com",
            " johndoe@googlemail.com ",
            "John.Doe@GOOGLEMAIL.COM",
        ] {
            assert_eq!(mailbox_key(spelling), "johndoe@gmail.com", "{spelling}");
        }
    }

    #[test]
    fn other_addresses_only_ignore_case() {
        assert!(same_mailbox("Jane.Doe@Example.com", "jane.doe@example.com"));
        // Workspace domains are hosted by Google but keep their dots.
        assert!(!same_mailbox("jane.doe@example.com", "janedoe@example.com"));
        assert!(!same_mailbox("john+news@gmail.com", "john@gmail.com"));
        assert!(!same_mailbox("johndoe@google.com", "johndoe@gmail.com"));
        assert!(!same_mailbox(
            "johndoe@gmail.com.example",
            "johndoe@gmail.com"
        ));
    }
}
