//! Choosing among many sender addresses (#252). An account can have up to a
//! thousand, which the composer's From and Settings' default sender find by
//! searching a picker rather than scrolling a list.

/// Matches a search shows at most, so the picker stays quick. Accounts with
/// more than ten sender addresses get the picker, as the composer's and
/// Settings' .slint say.
pub(crate) const SHOWN_AT_MOST: usize = 100;
/// Addresses Settings lists at once, with their buttons.
pub(crate) const LISTED_AT_MOST: usize = 50;

/// The rows a picker shows for `choices`, as labels with their addresses,
/// that `query` matches, and how many match in all.
pub(crate) fn rows(
    choices: &[(String, String)],
    query: &str,
) -> (Vec<crate::SenderChoiceRow>, usize) {
    let pairs = choices
        .iter()
        .map(|(label, email)| (label.as_str(), email.as_str()))
        .collect::<Vec<_>>();
    let (shown, total) = matching(&pairs, query, SHOWN_AT_MOST);
    let rows = shown
        .into_iter()
        .map(|index| crate::SenderChoiceRow {
            index: i32::try_from(index).unwrap_or_default(),
            label: choices[index].0.as_str().into(),
        })
        .collect();
    (rows, total)
}

/// The positions in `choices`, as labels with their addresses, that `query`
/// matches, best first: the address itself, then names and addresses that
/// start with it or have a word that does, then those that hold it anywhere.
/// Returns at most `limit` of them, and how many match in all.
pub(crate) fn matching(choices: &[(&str, &str)], query: &str, limit: usize) -> (Vec<usize>, usize) {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return ((0..choices.len().min(limit)).collect(), choices.len());
    }
    let mut ranked = choices
        .iter()
        .enumerate()
        .filter_map(|(index, (label, email))| {
            let label = label.to_lowercase();
            let email = email.trim().to_lowercase();
            let rank = if email == query {
                0
            } else if email.starts_with(&query)
                || label
                    .split(|character: char| !character.is_alphanumeric())
                    .any(|word| word.starts_with(&query))
            {
                1
            } else if email.contains(&query) || label.contains(&query) {
                2
            } else {
                return None;
            };
            Some((rank, index))
        })
        .collect::<Vec<_>>();
    ranked.sort_unstable();
    let total = ranked.len();
    (
        ranked
            .into_iter()
            .take(limit)
            .map(|(_, index)| index)
            .collect(),
        total,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHOICES: [(&str, &str); 5] = [
        ("Alex Rivera  <alex@flectar.com>", "alex@flectar.com"),
        ("sales@flectar.com", "sales@flectar.com"),
        (
            "Flectar Sales  <sales.eu@flectar.com>",
            "sales.eu@flectar.com",
        ),
        ("Press  <press@flectar.com>", "press@flectar.com"),
        ("Pressure Tests  <qa@example.com>", "qa@example.com"),
    ];

    #[test]
    fn an_empty_search_lists_the_first_ones() {
        assert_eq!(matching(&CHOICES, "  ", 3), (vec![0, 1, 2], 5));
    }

    #[test]
    fn the_address_comes_first_then_words_then_anywhere() {
        // The address itself, then one starting with it, then a word.
        assert_eq!(matching(&CHOICES, "sales@flectar.com", 10), (vec![1], 1));
        assert_eq!(matching(&CHOICES, "Sales", 10), (vec![1, 2], 2));
        assert_eq!(matching(&CHOICES, "press", 10), (vec![3, 4], 2));
        // Anywhere in a name or address comes after.
        assert_eq!(matching(&CHOICES, "ectar", 10), (vec![0, 1, 2, 3], 4));
        assert_eq!(matching(&CHOICES, "nobody", 10), (vec![], 0));
    }

    #[test]
    fn a_search_shows_only_so_many() {
        assert_eq!(matching(&CHOICES, "flectar", 2), (vec![0, 1], 4));
    }
}
