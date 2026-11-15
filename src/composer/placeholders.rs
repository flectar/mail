//! Placeholders in email templates, such as `{{recipient.first_name}}` or `{{date}}`, filled in
//! as a template goes into a message. One the message can't fill stays as it's written, for
//! the writer to fill in, and sending checks for those.

/// What a template's placeholders are filled with: the message's first recipient, its sender
/// and today's date. What isn't known leaves its placeholders as they're written.
#[derive(Debug, Default)]
pub(super) struct PlaceholderValues {
    pub(super) recipient: Person,
    pub(super) sender: Person,
    pub(super) date: String,
}

/// A recipient or the sender, by name and address.
#[derive(Debug, Default)]
pub(super) struct Person {
    pub(super) name: Option<String>,
    pub(super) email: Option<String>,
}

impl Person {
    /// `name` and `email`, without a name that's only an address.
    pub(super) fn new(name: Option<&str>, email: Option<&str>) -> Self {
        let clean = |value: Option<&str>| {
            value
                .map(|value| value.trim().trim_matches('"').trim())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Self {
            name: clean(name).filter(|name| !name.contains('@')),
            email: clean(email),
        }
    }

    fn first_name(&self) -> Option<String> {
        self.name.as_deref().map(|name| name_parts(name).0)
    }

    fn last_name(&self) -> Option<String> {
        self.name.as_deref().map(|name| name_parts(name).1)
    }
}

/// The longest text a placeholder holds between its braces.
const MAX_PLACEHOLDER_CHARS: usize = 100;

/// `text` with each placeholder `values` knows filled in. One can name what fills it when
/// nothing does, as `{{recipient.first_name | 'there'}}`.
pub(super) fn fill(text: &str, values: &PlaceholderValues) -> String {
    let mut filled = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, inside, after)) = next_placeholder(rest) {
        filled.push_str(before);
        match value_of(inside, values) {
            Some(value) => filled.push_str(&value),
            None => {
                filled.push_str("{{");
                filled.push_str(inside);
                filled.push_str("}}");
            }
        }
        rest = after;
    }
    filled.push_str(rest);
    filled
}

/// The placeholders still in `text`, each once, as they're written.
pub(super) fn placeholders_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some((_, inside, after)) = next_placeholder(rest) {
        let placeholder = format!("{{{{{inside}}}}}");
        if !found.contains(&placeholder) {
            found.push(placeholder);
        }
        rest = after;
    }
    found
}

/// `text` with `insert` in place of the bytes from `start` to `end`, or at its end when those
/// don't fall between characters.
pub(super) fn splice(text: &str, start: i32, end: i32, insert: &str) -> String {
    let range = usize::try_from(start)
        .ok()
        .zip(usize::try_from(end).ok())
        .filter(|(start, end)| {
            start <= end && text.is_char_boundary(*start) && text.is_char_boundary(*end)
        })
        .unwrap_or((text.len(), text.len()));
    let mut spliced = String::with_capacity(text.len() + insert.len());
    spliced.push_str(&text[..range.0]);
    spliced.push_str(insert);
    spliced.push_str(&text[range.1..]);
    spliced
}

/// The text before the first placeholder in `text`, what's between its braces, and the text
/// after it. A placeholder stays on one line.
fn next_placeholder(text: &str) -> Option<(&str, &str, &str)> {
    let mut from = 0;
    while let Some(open) = text[from..].find("{{").map(|at| from + at) {
        let inside_start = open + 2;
        let close = inside_start + text[inside_start..].find("}}")?;
        let inside = &text[inside_start..close];
        if !inside.trim().is_empty()
            && !inside.contains(['\n', '\r', '{'])
            && inside.chars().count() <= MAX_PLACEHOLDER_CHARS
        {
            return Some((&text[..open], inside, &text[close + 2..]));
        }
        from = open + 1;
    }
    None
}

/// What fills the placeholder holding `inside`, if anything does.
fn value_of(inside: &str, values: &PlaceholderValues) -> Option<String> {
    let (name, fallback) = match inside.split_once('|') {
        Some((name, fallback)) => (name, Some(fallback_text(fallback))),
        None => (inside, None),
    };
    let value = match name.trim().to_ascii_lowercase().as_str() {
        "recipient.first_name" => values.recipient.first_name(),
        "recipient.last_name" => values.recipient.last_name(),
        "recipient.name" => values.recipient.name.clone(),
        "recipient.email" => values.recipient.email.clone(),
        "sender.first_name" => values.sender.first_name(),
        "sender.last_name" => values.sender.last_name(),
        "sender.name" => values.sender.name.clone(),
        "sender.email" => values.sender.email.clone(),
        "date" => Some(values.date.clone()),
        // One the app doesn't know is the writer's to fill in.
        _ => return None,
    };
    value.filter(|value| !value.is_empty()).or(fallback)
}

/// What a placeholder names to fill it when nothing else does: the text after its `|`, in
/// quotes or not, also as Liquid's `default: 'text'`.
fn fallback_text(text: &str) -> String {
    let text = text.trim();
    let text = text
        .get(..8)
        .filter(|start| start.eq_ignore_ascii_case("default:"))
        .map_or(text, |_| text[8..].trim());
    for quote in ['\'', '"'] {
        if let Some(inner) = text
            .strip_prefix(quote)
            .and_then(|text| text.strip_suffix(quote))
        {
            return inner.to_owned();
        }
    }
    text.to_owned()
}

/// A name's first and last parts: "Ana María Ruiz" is Ana and María Ruiz, and "Ruiz, Ana
/// María", as address books file names, is Ana María and Ruiz. A name of one word is a first
/// name.
fn name_parts(name: &str) -> (String, String) {
    let name = name.trim();
    if let Some((last, first)) = name.split_once(',') {
        let (first, last) = (first.trim(), last.trim());
        if !first.is_empty() && !last.is_empty() {
            return (first.to_owned(), last.to_owned());
        }
    }
    match name.split_once(char::is_whitespace) {
        Some((first, last)) => (first.to_owned(), last.trim().to_owned()),
        None => (name.to_owned(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(recipient: Option<&str>, recipient_email: Option<&str>) -> PlaceholderValues {
        PlaceholderValues {
            recipient: Person::new(recipient, recipient_email),
            sender: Person::new(Some("Alex Rivera"), Some("alex@example.com")),
            date: "Nov 14, 2026".to_owned(),
        }
    }

    #[test]
    fn placeholders_take_the_recipient_sender_and_date() {
        let text = "Hi {{recipient.first_name}},\n\nAs of {{ date }}, {{recipient.name}} \
                    <{{recipient.email}}> owes nothing.\n\n{{sender.first_name}} \
                    ({{Sender.Email}})";
        assert_eq!(
            fill(
                text,
                &values(Some("Ana María Ruiz"), Some("ana@example.org"))
            ),
            "Hi Ana,\n\nAs of Nov 14, 2026, Ana María Ruiz <ana@example.org> owes \
             nothing.\n\nAlex (alex@example.com)"
        );
        // Names filed last name first, and names of one word, whose last name isn't known.
        let parts = |name| {
            let values = values(Some(name), None);
            fill("{{recipient.first_name}}/{{recipient.last_name}}", &values)
        };
        assert_eq!(parts("Ruiz, Ana María"), "Ana María/Ruiz");
        assert_eq!(parts("Ana"), "Ana/{{recipient.last_name}}");
        assert_eq!(parts("\"Ana Ruiz\""), "Ana/Ruiz");
    }

    #[test]
    fn what_the_message_cant_fill_stays_for_the_writer() {
        // No recipient yet, and a name that's only an address.
        let empty = values(None, None);
        let text = "Hi {{recipient.first_name}}, about {{company}} {{ recipient.name | 'there' }}";
        assert_eq!(
            fill(text, &empty),
            "Hi {{recipient.first_name}}, about {{company}} there"
        );
        let address = values(Some("ana@example.org"), Some("ana@example.org"));
        assert_eq!(
            fill("Hi {{recipient.first_name | default: \"there\"}}", &address),
            "Hi there"
        );
        assert_eq!(
            placeholders_in(&fill(text, &empty)),
            ["{{recipient.first_name}}", "{{company}}"]
        );
        // Braces that hold no placeholder are left alone.
        let code = "fn main() {{}} and {{\n}} or {{ {{date}}";
        assert_eq!(
            fill(code, &empty),
            "fn main() {{}} and {{\n}} or {{ Nov 14, 2026"
        );
        assert_eq!(placeholders_in("{{a}} {{a}} {{b|c}}"), ["{{a}}", "{{b|c}}"]);
    }

    #[test]
    fn a_placeholder_goes_in_at_the_caret() {
        assert_eq!(splice("Hi ,", 3, 3, "{{date}}"), "Hi {{date}},");
        assert_eq!(splice("Hi you,", 3, 6, "{{date}}"), "Hi {{date}},");
        // Offsets that don't fall between characters put it at the end.
        assert_eq!(splice("Olá", 3, 3, "!"), "Olá!");
        assert_eq!(splice("Olá", -1, 0, "!"), "Olá!");
        assert_eq!(splice("Olá", 2, 1, "!"), "Olá!");
    }
}
