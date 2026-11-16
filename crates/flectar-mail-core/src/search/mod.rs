//! Search query parser, in the syntax Gmail's search box uses.
//!
//! Words and `"quoted phrases"` become an FTS5 prefix query that every one of them has to
//! match. Operators filter: `from:` / `to:` / `in:` / `is:` / `has:`, the dates of
//! `before:` / `after:` / `older_than:` / `newer_than:`, the sizes of `larger:` (or `size:`)
//! / `smaller:`, and `filename:`. `exclude:term` and `-term` drop any thread whose messages
//! match `term`.
//!
//! `OR` between two parts, in capitals, matches either, and binds tighter than the space
//! between parts, as in Gmail: `a OR b c` is `(a OR b) c`. Parentheses group, and a dash
//! before a group or an operator negates it. A query with any of these becomes one boolean
//! [`Expr`] instead of the all-terms query, which keeps its relaxed any-term fallback.

pub(crate) mod server;

use chrono::{DateTime, Months, NaiveDate, TimeZone};

/// Lowercase and strip diacritics for accent-insensitive matching, so that
/// unaccented input ("be don dep") matches accented text ("Bé Dọn Dẹp").
pub fn fold(s: &str) -> String {
    deunicode::deunicode(s).to_lowercase()
}

#[derive(Debug, Default, Clone)]
pub struct ParsedQuery {
    pub fts: String,
    /// Same terms joined with OR - the relaxed fallback used when the
    /// all-terms-must-match query comes up empty.
    pub fts_or: String,
    /// Excluded terms (`exclude:foo` / `-foo`) joined with OR, as an FTS5 match
    /// string. A thread is dropped when any of its messages matches this. Empty
    /// when the query has no exclusions.
    pub fts_not: String,
    /// Free-text terms with operators stripped, joined by spaces and unquoted -
    /// used as the semantic-search embedding query.
    pub text: String,
    /// Folded (lowercase, diacritic-stripped) free-text terms - used to boost
    /// results whose sender name/address matches the query.
    pub terms: Vec<String>,
    /// The operators of a query whose parts all have to match, each a filter
    /// every match meets.
    pub filters: Vec<Filter>,
    /// A query with `OR`, parentheses or a negated operator, as one boolean
    /// expression. It stands in for `fts`, `fts_not` and `filters`, which stay
    /// empty, and leaves out semantic search, whose matches it can't check.
    pub expression: Option<Expr>,
    /// The whole query as one expression, plain or boolean, for searching a
    /// server with.
    pub tree: Option<Expr>,
    /// Scope results to a single account. `None` searches every account. Not
    /// parsed from the query string - set by the caller from the UI's active
    /// account selection.
    pub account_id: Option<i64>,
    /// Scope results to a set of accounts, such as a mail profile's; an empty
    /// set matches nothing. Combines with `account_id`. Set by the caller.
    pub account_ids: Option<Vec<i64>>,
    /// Scope results to messages in any of these folders, such as the ones an
    /// AI agent may see; an empty set matches nothing. Set by the caller.
    pub folder_ids: Option<Vec<i64>>,
    /// Messages a search of the servers found for this query. They match too,
    /// whether or not the copy kept here has what the server matched, such as
    /// a body not fetched yet. Set by the caller.
    pub found: Vec<i64>,
}

/// What an operator keeps: messages from or to an address, in a folder of a
/// role, unread, starred, with attachments, received before or from a time,
/// at least or under a size, or with an attachment whose name has a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    From(String),
    To(String),
    /// A folder role, such as `inbox` or `trash`.
    In(&'static str),
    /// `in:anywhere`: every folder, Trash and Spam too. It keeps everything.
    Anywhere,
    Unread,
    Starred,
    HasAttachment,
    /// Received before this time, in milliseconds since the epoch.
    Before(i64),
    /// Received at or after this time, in milliseconds since the epoch.
    After(i64),
    /// At least this many bytes.
    Larger(i64),
    /// Fewer than this many bytes.
    Smaller(i64),
    /// An attachment whose name has this text, lowercased.
    Filename(String),
}

/// Words or a phrase the full-text index matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    /// The FTS5 match expression.
    pub fts: String,
    /// The words as typed, for the embedding query and for servers.
    pub words: String,
    /// The words folded, to boost senders they name: unscoped text only.
    sender: Option<String>,
    /// The part of the message the words are in: `subject`, `body`, or
    /// anywhere.
    pub column: Option<&'static str>,
    /// Quoted: the words in that order, rather than words starting so.
    pub phrase: bool,
}

/// A boolean search expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Text(Text),
    Filter(Filter),
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
}

impl Expr {
    /// Whether something must match rather than only not match: a part that
    /// isn't negated, past `in:anywhere`, which keeps everything.
    pub fn selects(&self) -> bool {
        match self {
            Expr::Text(_) => true,
            Expr::Filter(filter) => *filter != Filter::Anywhere,
            Expr::Not(_) => false,
            Expr::And(parts) | Expr::Or(parts) => parts.iter().any(Expr::selects),
        }
    }

    /// Whether the expression names a folder, `in:anywhere` included.
    pub fn names_folder(&self) -> bool {
        match self {
            Expr::Text(_) => false,
            Expr::Filter(filter) => matches!(filter, Filter::In(_) | Filter::Anywhere),
            Expr::Not(inner) => inner.names_folder(),
            Expr::And(parts) | Expr::Or(parts) => parts.iter().any(Expr::names_folder),
        }
    }

    /// The folded unscoped words the expression looks for, not negated.
    fn sender_terms(&self, out: &mut Vec<String>) {
        match self {
            Expr::Text(text) => out.extend(text.sender.clone()),
            Expr::Filter(_) | Expr::Not(_) => {}
            Expr::And(parts) | Expr::Or(parts) => {
                for part in parts {
                    part.sender_terms(out);
                }
            }
        }
    }

    /// Whether the expression fits an all-parts query: words, filters, or
    /// negated words.
    fn plain(&self) -> bool {
        match self {
            Expr::Text(_) | Expr::Filter(_) => true,
            Expr::Not(inner) => matches!(**inner, Expr::Text(_)),
            Expr::And(_) | Expr::Or(_) => false,
        }
    }
}

impl ParsedQuery {
    /// Whether the query names a folder, `in:anywhere` included. One that
    /// doesn't leaves Trash and Spam out, as mail lists do.
    pub fn names_folder(&self) -> bool {
        self.filters
            .iter()
            .any(|filter| matches!(filter, Filter::In(_) | Filter::Anywhere))
            || self.expression.as_ref().is_some_and(Expr::names_folder)
    }
}

/// A query token: a word, a `"quoted phrase"`, a parenthesis, `OR`, `AND`, or
/// the dash that negates what follows it. A phrase can be column-scoped when
/// it directly follows `subject:` / `body:`, e.g. `subject:"quarterly report"`
/// carries `Some("subject")`.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Phrase(Option<&'static str>, String),
    Open,
    Close,
    Or,
    And,
    Not,
}

/// `subject:` / `body:` (the column-scoped operators that accept a quoted
/// value) mapped to their FTS5 column name; anything else is `None`. A `!` or
/// `!!` case marker directly before the quote (`body:!"phrase"`,
/// `body:!!"Phrase"`, "ignore case" and "match case exactly" in the syntax
/// this comes from) is tolerated here so the phrase still gets column scoping;
/// the marker itself is a no-op today since FTS5's unicode61 tokenizer already
/// folds case unconditionally; case-exact matching isn't implemented.
fn scoped_col(token: &str) -> Option<&'static str> {
    let token = token
        .strip_suffix("!!")
        .or_else(|| token.strip_suffix('!'))
        .unwrap_or(token);
    match token.strip_suffix(':')?.to_ascii_lowercase().as_str() {
        "subject" => Some("subject"),
        "body" => Some("body"),
        _ => None,
    }
}

/// Ends the word in `current`, as `OR` / `AND` when it's one of those in
/// capitals.
fn end_word(current: &mut String, out: &mut Vec<Token>) {
    if current.is_empty() {
        return;
    }
    let word = std::mem::take(current);
    out.push(match word.as_str() {
        "OR" => Token::Or,
        "AND" => Token::And,
        _ => Token::Word(word),
    });
}

/// Split on whitespace and parentheses, keeping double-quoted spans together
/// as phrases. An unterminated quote still yields a phrase from what follows
/// it. A dash that starts a part negates it; dashes inside words stay.
fn tokenize(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    // Column carried from a `subject:`/`body:` prefix into the phrase that opens
    // immediately after it.
    let mut pending_col: Option<&'static str> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quote {
            if c == '"' {
                if !cur.is_empty() {
                    out.push(Token::Phrase(pending_col.take(), std::mem::take(&mut cur)));
                } else {
                    pending_col = None;
                }
                in_quote = false;
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '"' => {
                // A bare `subject:`/`body:` right before the quote scopes it.
                if !cur.is_empty() {
                    if let Some(col) = scoped_col(&cur) {
                        pending_col = Some(col);
                        cur.clear();
                    } else {
                        end_word(&mut cur, &mut out);
                    }
                }
                in_quote = true;
            }
            '(' => {
                end_word(&mut cur, &mut out);
                out.push(Token::Open);
            }
            ')' => {
                end_word(&mut cur, &mut out);
                out.push(Token::Close);
            }
            // Dashes that start a part negate it, once however many there are.
            // A dash on its own negates nothing.
            '-' if cur.is_empty() => {
                while chars.peek() == Some(&'-') {
                    chars.next();
                }
                if chars.peek().is_some_and(|next| !next.is_whitespace()) {
                    out.push(Token::Not);
                }
            }
            c if c.is_whitespace() => end_word(&mut cur, &mut out),
            c => cur.push(c),
        }
    }
    if in_quote {
        if !cur.is_empty() {
            out.push(Token::Phrase(pending_col.take(), cur));
        }
    } else {
        end_word(&mut cur, &mut out);
    }
    out
}

/// Reads tokens into expressions, against `now` for relative dates.
struct Parser<'a, Tz: TimeZone> {
    tokens: &'a [Token],
    at: usize,
    now: &'a DateTime<Tz>,
}

impl<'a, Tz: TimeZone> Parser<'a, Tz> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.at)
    }

    /// Parts that all have to match, up to a closing parenthesis when
    /// `nested`. Stray connectors and parentheses are skipped, and a group
    /// whose parts all have to match joins them.
    fn sequence(&mut self, nested: bool) -> Vec<Expr> {
        let mut items = Vec::new();
        while let Some(token) = self.peek() {
            match token {
                Token::Close if nested => break,
                Token::Close | Token::And | Token::Or => self.at += 1,
                _ => match self.alternatives() {
                    Some(Expr::And(parts)) => items.extend(parts),
                    Some(part) => items.push(part),
                    None => {}
                },
            }
        }
        items
    }

    /// A part, or parts joined by `OR`, any of which matches.
    fn alternatives(&mut self) -> Option<Expr> {
        let mut options = Vec::new();
        options.extend(self.unary());
        while self.peek() == Some(&Token::Or) {
            self.at += 1;
            if matches!(
                self.peek(),
                None | Some(Token::Close | Token::Or | Token::And)
            ) {
                break;
            }
            options.extend(self.unary());
        }
        match options.len() {
            0 => None,
            1 => options.pop(),
            _ => Some(Expr::Or(options)),
        }
    }

    /// One part, negated by a dash or grouped in parentheses. It reads at
    /// least one token.
    fn unary(&mut self) -> Option<Expr> {
        let token = self.peek()?;
        self.at += 1;
        match token {
            Token::Not => {
                if matches!(
                    self.peek(),
                    None | Some(Token::Close | Token::Or | Token::And)
                ) {
                    return None;
                }
                self.unary().map(|inner| match inner {
                    // Two negations cancel.
                    Expr::Not(inner) => *inner,
                    inner => Expr::Not(Box::new(inner)),
                })
            }
            Token::Open => {
                let items = self.sequence(true);
                if self.peek() == Some(&Token::Close) {
                    self.at += 1;
                }
                all_of(items)
            }
            Token::Word(word) => self.word(word),
            Token::Phrase(column, phrase) => phrase_text(*column, phrase).map(Expr::Text),
            Token::Close | Token::Or | Token::And => None,
        }
    }

    /// A word: an operator with its value, or a term. Operators are
    /// case-insensitive on both the key ("In:" == "in:") and the value
    /// ("in:Sent" == "in:sent"); users type them however they like.
    fn word(&self, token: &str) -> Option<Expr> {
        let (key, value) = match token.split_once(':') {
            Some((key, value)) => (key.to_ascii_lowercase(), value),
            None => (String::new(), ""),
        };
        // Tolerate list punctuation around operator values:
        // `from:a@b.com, report` must not poison the address filter.
        let value = value.trim_matches([',', ';']);
        let filter = |filter: Option<Filter>| filter.map(Expr::Filter);
        match key.as_str() {
            // Address operators keep the value's original case (matched
            // case-insensitively downstream); a bare "from:" is a no-op.
            "from" => filter((!value.is_empty()).then(|| Filter::From(value.to_owned()))),
            "to" => filter((!value.is_empty()).then(|| Filter::To(value.to_owned()))),
            "in" => filter(match value.to_ascii_lowercase().as_str() {
                "inbox" => Some(Filter::In("inbox")),
                "sent" => Some(Filter::In("sent")),
                "drafts" => Some(Filter::In("drafts")),
                "trash" => Some(Filter::In("trash")),
                "spam" => Some(Filter::In("spam")),
                "archive" | "done" => Some(Filter::In("archive")),
                "anywhere" => Some(Filter::Anywhere),
                _ => None, // unknown folder: ignore the operator, drop the token
            }),
            "is" => filter(match value.to_ascii_lowercase().as_str() {
                "unread" => Some(Filter::Unread),
                "starred" => Some(Filter::Starred),
                _ => None,
            }),
            "has" => filter(
                value
                    .eq_ignore_ascii_case("attachment")
                    .then_some(Filter::HasAttachment),
            ),
            "before" => filter(day_start(value, self.now).map(Filter::Before)),
            "after" => filter(day_start(value, self.now).map(Filter::After)),
            "older_than" => filter(ago(value, self.now).map(Filter::Before)),
            "newer_than" => filter(ago(value, self.now).map(Filter::After)),
            "larger" | "size" => filter(bytes(value).map(Filter::Larger)),
            "smaller" => filter(bytes(value).map(Filter::Smaller)),
            "filename" => {
                let name = clean_token(value).to_lowercase();
                filter((!name.is_empty()).then_some(Filter::Filename(name)))
            }
            // Column-scoped text: `subject:foo` / `body:foo` restrict the FTS
            // match to that column. A bare `subject:` / `body:` is a no-op.
            // Free-text (embedding) still sees the word; sender-boost does not.
            "subject" | "body" => {
                let clean = clean_token(value);
                let column = if key == "subject" { "subject" } else { "body" };
                (!clean.is_empty()).then(|| {
                    Expr::Text(Text {
                        fts: fts_col_term(Some(column), &clean),
                        words: clean,
                        sender: None,
                        column: Some(column),
                        phrase: false,
                    })
                })
            }
            // Exclude a term from results: "exclude:draft" drops any thread
            // whose messages match "draft". A bare "exclude:" is a no-op.
            "exclude" => {
                let clean = clean_token(value);
                (!clean.is_empty()).then(|| {
                    Expr::Not(Box::new(Expr::Text(Text {
                        fts: fts_term(&clean),
                        words: clean,
                        sender: None,
                        column: None,
                        phrase: false,
                    })))
                })
            }
            // Not an operator (includes non-operator tokens with ':' like "12:30").
            _ => {
                let clean = clean_token(token);
                (!clean.is_empty()).then(|| {
                    Expr::Text(Text {
                        fts: fts_term(&clean),
                        sender: Some(fold(&clean)),
                        words: clean,
                        column: None,
                        phrase: false,
                    })
                })
            }
        }
    }
}

/// Parts that all have to match, as one expression.
fn all_of(mut items: Vec<Expr>) -> Option<Expr> {
    match items.len() {
        0 => None,
        1 => items.pop(),
        _ => Some(Expr::And(items)),
    }
}

/// A quoted phrase: an exact (non-prefix) FTS phrase match, optionally scoped
/// to a column (`subject:"quarterly report"`).
fn phrase_text(column: Option<&'static str>, phrase: &str) -> Option<Text> {
    let words = phrase
        .split_whitespace()
        .map(clean_token)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.is_empty() {
        return None;
    }
    let phrase = words.join(" ");
    Some(match column {
        Some(column) => Text {
            fts: format!("{column}:\"{phrase}\""),
            words: phrase,
            sender: None,
            column: Some(column),
            phrase: true,
        },
        // Sender-boost only applies to unscoped free text.
        None => Text {
            fts: format!("\"{phrase}\""),
            sender: Some(fold(&phrase)),
            words: phrase,
            column: None,
            phrase: true,
        },
    })
}

/// The start of the day `value` names, as `2024/03/31` or `2024-03-31`, in
/// the time zone of `now`, in milliseconds; or the time `value` gives in
/// seconds since the epoch, as Gmail also takes.
fn day_start<Tz: TimeZone>(value: &str, now: &DateTime<Tz>) -> Option<i64> {
    if value.len() >= 9 && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value.parse::<i64>().ok()?.checked_mul(1000);
    }
    let mut parts = value.split(['/', '-', '.']);
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1000..=9999).contains(&year) {
        return None;
    }
    let midnight = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(0, 0, 0)?;
    let zone = now.timezone();
    // A day whose midnight a clock change skips starts at the hour after.
    zone.from_local_datetime(&midnight)
        .earliest()
        .or_else(|| {
            zone.from_local_datetime(&(midnight + chrono::TimeDelta::hours(1)))
                .earliest()
        })
        .map(|start| start.timestamp_millis())
}

/// The time `value` ago before `now`, as `3d` days, `2m` months or `1y` years,
/// in milliseconds.
fn ago<Tz: TimeZone>(value: &str, now: &DateTime<Tz>) -> Option<i64> {
    let value = value.to_ascii_lowercase();
    let unit = value.chars().last()?;
    let count = value[..value.len() - unit.len_utf8()].parse::<u32>().ok()?;
    let then = match unit {
        'd' => now
            .clone()
            .checked_sub_signed(chrono::TimeDelta::try_days(i64::from(count))?)?,
        'm' => now.clone().checked_sub_months(Months::new(count))?,
        'y' => now
            .clone()
            .checked_sub_months(Months::new(count.checked_mul(12)?))?,
        _ => return None,
    };
    Some(then.timestamp_millis())
}

/// The bytes `value` gives, as `1000`, `10K`, `10M` or `1G`, with an optional
/// `B`, in thousands as Gmail counts them.
fn bytes(value: &str) -> Option<i64> {
    let upper = value.to_ascii_uppercase();
    let upper = upper.strip_suffix('B').unwrap_or(&upper);
    let (number, scale) = match upper.chars().last()? {
        'K' => (&upper[..upper.len() - 1], 1_000),
        'M' => (&upper[..upper.len() - 1], 1_000_000),
        'G' => (&upper[..upper.len() - 1], 1_000_000_000),
        _ => (upper, 1),
    };
    number.parse::<i64>().ok()?.checked_mul(scale)
}

pub fn parse(input: &str) -> ParsedQuery {
    parse_at(input, &chrono::Local::now())
}

/// [`parse`] against `now`, for relative dates and the day boundaries of its
/// time zone.
pub fn parse_at<Tz: TimeZone>(input: &str, now: &DateTime<Tz>) -> ParsedQuery {
    let tokens = tokenize(input);
    let mut parser = Parser {
        tokens: &tokens,
        at: 0,
        now,
    };
    let items = parser.sequence(false);
    let mut q = ParsedQuery {
        tree: all_of(items.clone()),
        ..ParsedQuery::default()
    };
    if !items.iter().all(Expr::plain) {
        // A boolean query: its parts can't join into one all-terms match.
        if let Some(expression) = all_of(items) {
            expression.sender_terms(&mut q.terms);
            q.expression = Some(expression);
        }
        return q;
    }

    let mut fts_terms: Vec<String> = Vec::new();
    let mut text_terms: Vec<String> = Vec::new();
    let mut not_terms: Vec<String> = Vec::new();
    for item in items {
        match item {
            Expr::Text(text) => {
                fts_terms.push(text.fts);
                q.terms.extend(text.sender);
                text_terms.push(text.words);
            }
            Expr::Filter(filter) => q.filters.push(filter),
            Expr::Not(inner) => {
                // Excluded terms stay out of positive FTS, embedding text, and
                // the sender-boost terms.
                if let Expr::Text(text) = *inner {
                    not_terms.push(text.fts);
                }
            }
            Expr::And(_) | Expr::Or(_) => {}
        }
    }

    // Explicit AND: FTS5 rejects implicit-AND juxtaposition of the
    // parenthesized groups fts_term() can produce.
    q.fts = fts_terms.join(" AND ");
    q.fts_or = fts_terms.join(" OR ");
    q.fts_not = not_terms.join(" OR ");
    q.text = text_terms.join(" ");
    q
}

/// Escape FTS special syntax by dropping everything but word-ish characters.
fn clean_token(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '@' || *c == '.' || *c == '-' || *c == '_')
        .collect()
}

/// One FTS5 prefix term for a cleaned token. The index tokenizer
/// (unicode61 remove_diacritics 2) folds Vietnamese vowel diacritics but not
/// đ/Đ - a distinct letter, not a combining mark - so a leading unaccented
/// "d" also tries the "đ" variant ("don" matches both "dọn" and "đơn").
fn fts_term(clean: &str) -> String {
    fts_col_term(None, clean)
}

/// Like `fts_term`, but optionally constrained to a single FTS5 column
/// (`subject` / `body`) so `subject:foo` only matches the subject. FTS5 accepts
/// a column filter in front of a phrase or a parenthesized group.
fn fts_col_term(col: Option<&str>, clean: &str) -> String {
    let base = if let Some(rest) = clean.strip_prefix(['d', 'D']) {
        format!("(\"{clean}\"* OR \"đ{rest}\"*)")
    } else {
        format!("\"{clean}\"*")
    };
    match col {
        Some(c) => format!("{c}:{base}"),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn text(fts: &str) -> Expr {
        Expr::Text(Text {
            fts: fts.to_owned(),
            words: String::new(),
            sender: None,
            column: None,
            phrase: false,
        })
    }

    /// `expr` with the words and sender terms of its text left out, to compare
    /// structure and match expressions only.
    fn shape(expr: &Expr) -> Expr {
        match expr {
            Expr::Text(found) => text(&found.fts),
            Expr::Filter(filter) => Expr::Filter(filter.clone()),
            Expr::Not(inner) => Expr::Not(Box::new(shape(inner))),
            Expr::And(parts) => Expr::And(parts.iter().map(shape).collect()),
            Expr::Or(parts) => Expr::Or(parts.iter().map(shape).collect()),
        }
    }

    fn expression(input: &str) -> Expr {
        shape(
            parse(input)
                .expression
                .as_ref()
                .unwrap_or_else(|| panic!("{input} is a boolean query")),
        )
    }

    #[test]
    fn parses_operators() {
        let q = parse("from:alice in:inbox is:unread has:attachment quarterly report");
        assert_eq!(
            q.filters,
            [
                Filter::From("alice".into()),
                Filter::In("inbox"),
                Filter::Unread,
                Filter::HasAttachment,
            ]
        );
        assert_eq!(q.fts, "\"quarterly\"* AND \"report\"*");
        assert!(q.expression.is_none());
    }

    #[test]
    fn operators_are_case_insensitive() {
        // "in:Sent" (capitalised) must resolve the same as "in:sent".
        let q = parse("in:Sent");
        assert_eq!(q.filters, [Filter::In("sent")]);
        assert!(q.fts.is_empty(), "operator token must not leak into FTS");

        let q = parse("IN:INBOX IS:Unread HAS:Attachment");
        assert_eq!(
            q.filters,
            [Filter::In("inbox"), Filter::Unread, Filter::HasAttachment]
        );
    }

    #[test]
    fn subject_and_body_scope_to_their_column() {
        let q = parse("subject:invoice body:overdue report");
        // Column-scoped terms carry an FTS5 column filter; free text does not.
        assert_eq!(
            q.fts,
            "subject:\"invoice\"* AND body:\"overdue\"* AND \"report\"*"
        );
        // "invoice"/"overdue" reach the embedding text but not sender-boost terms.
        assert!(q.text.contains("invoice") && q.text.contains("overdue"));
        assert_eq!(q.terms, vec!["report"]);
        // A bare operator is a no-op.
        assert!(parse("subject:").fts.is_empty());
    }

    #[test]
    fn subject_quoted_value_scopes_the_phrase() {
        let q = parse("subject:\"quarterly report\" budget");
        // The quoted value is an exact, column-scoped phrase; "budget" is free.
        assert_eq!(q.fts, "subject:\"quarterly report\" AND \"budget\"*");
        // Column-scoped phrases feed embedding text but not sender-boost.
        assert!(q.text.contains("quarterly report"));
        assert_eq!(q.terms, vec!["budget"]);
    }

    #[test]
    fn body_quoted_value_scopes_the_phrase() {
        let q = parse("body:\"past due\"");
        assert_eq!(q.fts, "body:\"past due\"");
    }

    #[test]
    fn case_marker_before_quote_still_scopes_the_phrase() {
        // `!`/`!!` right before the quote ("ignore case" / "match case exactly")
        // must not break column scoping - previously `body:!"..."` lost the
        // "body" prefix entirely and fell through as an unscoped, ungated
        // positive term.
        let q = parse("body:!\"15.235.209.251\"");
        assert_eq!(q.fts, "body:\"15.235.209.251\"");

        let q = parse("subject:!!\"Exact Case\"");
        assert_eq!(q.fts, "subject:\"Exact Case\"");
    }

    #[test]
    fn subject_column_term_expands_dj_variant() {
        // d-initial values still get the đ variant, inside the column filter.
        let q = parse("subject:don");
        assert_eq!(q.fts, "subject:(\"don\"* OR \"đon\"*)");
    }

    #[test]
    fn non_operator_colon_token_is_free_text() {
        // A time like "12:30" is not an operator and stays searchable.
        let q = parse("12:30");
        assert_eq!(q.fts, "\"1230\"*");
    }

    #[test]
    fn leading_d_expands_to_dj_variant() {
        let q = parse("don dep");
        assert_eq!(q.fts, "(\"don\"* OR \"đon\"*) AND (\"dep\"* OR \"đep\"*)");
        assert_eq!(q.fts_or, "(\"don\"* OR \"đon\"*) OR (\"dep\"* OR \"đep\"*)");
    }

    #[test]
    fn fold_removes_accents() {
        assert_eq!(fold("Bé Dọn Dẹp"), "be don dep");
    }

    #[test]
    fn operator_value_tolerates_trailing_comma() {
        let q = parse("from:hoang.ngyen@nextwaves.com, software");
        assert_eq!(
            q.filters,
            [Filter::From("hoang.ngyen@nextwaves.com".into())]
        );
        assert_eq!(q.fts, "\"software\"*");
    }

    #[test]
    fn quoted_phrase_is_exact_match() {
        let q = parse("\"quarterly report\" budget");
        assert_eq!(q.fts, "\"quarterly report\" AND \"budget\"*");
        assert_eq!(q.text, "quarterly report budget");
        assert_eq!(q.terms, vec!["quarterly report", "budget"]);
    }

    #[test]
    fn unterminated_quote_is_still_a_phrase() {
        let q = parse("\"hello world");
        assert_eq!(q.fts, "\"hello world\"");
    }

    #[test]
    fn terms_are_folded_for_sender_matching() {
        let q = parse("Bé software");
        assert_eq!(q.terms, vec!["be", "software"]);
    }

    #[test]
    fn exclude_operator_collects_negated_terms() {
        // "draft" is d-initial, so it also gets the đ variant; "report" doesn't.
        let q = parse("report exclude:draft");
        assert_eq!(q.fts, "\"report\"*");
        assert_eq!(q.fts_not, "(\"draft\"* OR \"đraft\"*)");
        // Excluded terms must not leak into positive FTS, embedding text, or
        // the sender-boost terms.
        assert!(!q.text.contains("draft"));
        assert!(!q.terms.iter().any(|t| t.contains("draft")));
    }

    #[test]
    fn dash_prefix_is_exclusion() {
        let q = parse("report -draft -old");
        assert_eq!(q.fts, "\"report\"*");
        assert_eq!(q.fts_not, "(\"draft\"* OR \"đraft\"*) OR \"old\"*");
        // However many dashes start it.
        assert_eq!(parse("report --old").fts_not, "\"old\"*");
        // A dash inside a word stays in it.
        assert_eq!(parse("e-mail").fts, "\"e-mail\"*");
    }

    #[test]
    fn exclude_only_query_has_no_positive_fts() {
        let q = parse("-spam");
        assert!(q.fts.is_empty());
        assert_eq!(q.fts_not, "\"spam\"*");
    }

    #[test]
    fn bare_exclude_and_dash_are_noops() {
        let q = parse("hello exclude: -");
        assert_eq!(q.fts, "\"hello\"*");
        assert!(q.fts_not.is_empty());
    }

    #[test]
    fn exclude_preserves_value_case_like_positive_terms() {
        // The operator key is case-insensitive; the value keeps its case (FTS5
        // matches case-insensitively anyway). "Draft" is D-initial → đ variant.
        let q = parse("EXCLUDE:Draft");
        assert_eq!(q.fts_not, "(\"Draft\"* OR \"đraft\"*)");
    }

    #[test]
    fn dates_are_days_in_the_local_time_zone() {
        let zone = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        let now = zone.with_ymd_and_hms(2026, 11, 16, 9, 30, 0).unwrap();
        let start = |y, m, d| {
            zone.with_ymd_and_hms(y, m, d, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
        };
        let q = parse_at("after:2024/03/01 before:2024-04-01", &now);
        assert_eq!(
            q.filters,
            [
                Filter::After(start(2024, 3, 1)),
                Filter::Before(start(2024, 4, 1)),
            ]
        );
        assert!(q.fts.is_empty());
        // Seconds since the epoch, as Gmail takes them.
        assert_eq!(
            parse_at("after:1700000000", &now).filters,
            [Filter::After(1_700_000_000_000)]
        );
        // Not a date: the operator does nothing.
        assert!(parse_at("before:soon", &now).filters.is_empty());
        assert!(parse_at("after:2024/02/30", &now).filters.is_empty());
    }

    #[test]
    fn ages_count_back_from_now() {
        let now = Utc.with_ymd_and_hms(2026, 3, 31, 12, 0, 0).unwrap();
        let at = |y, m, d, h| {
            Utc.with_ymd_and_hms(y, m, d, h, 0, 0)
                .unwrap()
                .timestamp_millis()
        };
        assert_eq!(
            parse_at("older_than:3d", &now).filters,
            [Filter::Before(at(2026, 3, 28, 12))]
        );
        // A month back from the 31st lands on the last day of the shorter month.
        assert_eq!(
            parse_at("newer_than:1m", &now).filters,
            [Filter::After(at(2026, 2, 28, 12))]
        );
        assert_eq!(
            parse_at("NEWER_THAN:2Y", &now).filters,
            [Filter::After(at(2024, 3, 31, 12))]
        );
        assert!(parse_at("older_than:3w", &now).filters.is_empty());
        assert!(parse_at("older_than:3é", &now).filters.is_empty());
    }

    #[test]
    fn sizes_count_bytes_in_thousands() {
        assert_eq!(parse("larger:10M").filters, [Filter::Larger(10_000_000)]);
        assert_eq!(parse("size:1000").filters, [Filter::Larger(1000)]);
        assert_eq!(parse("smaller:500kb").filters, [Filter::Smaller(500_000)]);
        assert_eq!(parse("larger:2G").filters, [Filter::Larger(2_000_000_000)]);
        assert!(parse("larger:big").filters.is_empty());
    }

    #[test]
    fn filename_matches_a_lowercased_part_of_a_name() {
        assert_eq!(
            parse("filename:Report.PDF").filters,
            [Filter::Filename("report.pdf".into())]
        );
        assert_eq!(
            parse("filename:pdf").filters,
            [Filter::Filename("pdf".into())]
        );
        assert!(parse("filename:").filters.is_empty());
    }

    #[test]
    fn in_anywhere_names_every_folder() {
        let q = parse("in:anywhere invoice");
        assert_eq!(q.filters, [Filter::Anywhere]);
        assert!(q.names_folder());
        assert!(!parse("invoice").names_folder());
    }

    #[test]
    fn or_matches_either_part_and_binds_tighter_than_spaces() {
        assert_eq!(
            expression("from:amy OR from:david"),
            Expr::Or(vec![
                Expr::Filter(Filter::From("amy".into())),
                Expr::Filter(Filter::From("david".into())),
            ])
        );
        // `a OR b c` is `(a OR b) c`, as in Gmail.
        assert_eq!(
            expression("dinner OR movie friday"),
            Expr::And(vec![
                Expr::Or(vec![
                    text("(\"dinner\"* OR \"đinner\"*)"),
                    text("\"movie\"*")
                ]),
                text("\"friday\"*"),
            ])
        );
        let q = parse("dinner OR movie");
        // A boolean query has no all-terms match, and no semantic search.
        assert!(q.fts.is_empty() && q.fts_or.is_empty() && q.text.is_empty());
        assert!(q.filters.is_empty());
        // Its words still name senders to boost.
        assert_eq!(q.terms, vec!["dinner", "movie"]);
        // Lowercase "or" is a word.
        assert_eq!(
            parse("this or that").fts,
            "\"this\"* AND \"or\"* AND \"that\"*"
        );
    }

    #[test]
    fn parentheses_group_and_dashes_negate_groups_and_operators() {
        assert_eq!(
            expression("(invoice OR receipt) -from:billing"),
            Expr::And(vec![
                Expr::Or(vec![text("\"invoice\"*"), text("\"receipt\"*")]),
                Expr::Not(Box::new(Expr::Filter(Filter::From("billing".into())))),
            ])
        );
        assert_eq!(
            expression("report -(draft OR old)"),
            Expr::And(vec![
                text("\"report\"*"),
                Expr::Not(Box::new(Expr::Or(vec![
                    text("(\"draft\"* OR \"đraft\"*)"),
                    text("\"old\"*"),
                ]))),
            ])
        );
        // A group of one is that part; AND joins like a space.
        assert_eq!(
            parse("(report) AND budget").fts,
            "\"report\"* AND \"budget\"*"
        );
        // Grouped parts that all have to match are still a plain query.
        assert_eq!(parse("(report budget)").fts, "\"report\"* AND \"budget\"*");
        assert!(parse("(report budget)").expression.is_none());
    }

    #[test]
    fn stray_connectors_and_parentheses_are_skipped() {
        assert_eq!(parse("OR report").fts, "\"report\"*");
        assert_eq!(parse("report OR").fts, "\"report\"*");
        assert_eq!(parse("report) budget").fts, "\"report\"* AND \"budget\"*");
        assert_eq!(
            expression("(report OR budget"),
            Expr::Or(vec![text("\"report\"*"), text("\"budget\"*")])
        );
        assert!(parse("( ) OR -").expression.is_none());
        assert!(parse("( ) OR -").fts.is_empty());
        // Two dashes in a row cancel out across a group.
        assert_eq!(
            expression("-(-report OR budget)"),
            Expr::Not(Box::new(Expr::Or(vec![
                Expr::Not(Box::new(text("\"report\"*"))),
                text("\"budget\"*"),
            ])))
        );
    }

    #[test]
    fn a_boolean_query_says_whether_it_selects_and_names_folders() {
        let q = parse("-from:billing");
        let expression = q.expression.as_ref().unwrap();
        assert!(!expression.selects());
        assert!(!q.names_folder());
        let q = parse("in:trash OR in:spam");
        assert!(q.expression.as_ref().unwrap().selects());
        assert!(q.names_folder());
    }
}
