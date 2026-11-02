//! Search query parser: extracts `from:` / `to:` / `in:` / `is:` / `has:` /
//! `exclude:` operators, everything else becomes an FTS5 prefix query.
//! `exclude:term` (and the Gmail-style `-term`) drops any thread whose messages
//! match `term` from the results.

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
    pub from: Option<String>,
    pub to: Option<String>,
    pub in_folder: Option<String>,
    pub is_unread: Option<bool>,
    pub is_starred: Option<bool>,
    pub has_attachment: Option<bool>,
    /// Scope results to a single account. `None` searches every account. Not
    /// parsed from the query string - set by the caller from the UI's active
    /// account selection.
    pub account_id: Option<i64>,
    /// Scope results to a set of accounts, such as a mail profile's; an empty
    /// set matches nothing. Combines with `account_id`. Set by the caller.
    pub account_ids: Option<Vec<i64>>,
}

/// A raw query token: a bare word or a `"quoted phrase"`. A phrase can be
/// column-scoped when it directly follows `subject:` / `body:`, e.g.
/// `subject:"quarterly report"` carries `Some("subject")`.
enum Token {
    Word(String),
    Phrase(Option<String>, String),
}

/// `subject:` / `body:` (the column-scoped operators that accept a quoted
/// value) mapped to their FTS5 column name; anything else is `None`. A `!` or
/// `!!` case marker directly before the quote (`body:!"phrase"`,
/// `body:!!"Phrase"` - documented in the search help as "ignore case" /
/// "match case exactly") is tolerated here so the phrase still gets column
/// scoping; the marker itself is a no-op today since FTS5's unicode61
/// tokenizer already folds case unconditionally; case-exact matching isn't
/// implemented.
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

/// Split on whitespace, keeping double-quoted spans together as phrases.
/// An unterminated quote still yields a phrase from what follows it.
fn tokenize(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    // Column carried from a `subject:`/`body:` prefix into the phrase that opens
    // immediately after it.
    let mut pending_col: Option<String> = None;
    for c in input.chars() {
        match c {
            '"' => {
                if in_quote {
                    if !cur.is_empty() {
                        out.push(Token::Phrase(pending_col.take(), std::mem::take(&mut cur)));
                    } else {
                        pending_col = None;
                    }
                    in_quote = false;
                } else {
                    // A bare `subject:`/`body:` right before the quote scopes it.
                    if !cur.is_empty() {
                        if let Some(col) = scoped_col(&cur) {
                            pending_col = Some(col.to_string());
                            cur.clear();
                        } else {
                            out.push(Token::Word(std::mem::take(&mut cur)));
                        }
                    }
                    in_quote = true;
                }
            }
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(Token::Word(std::mem::take(&mut cur)));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(if in_quote {
            Token::Phrase(pending_col.take(), cur)
        } else {
            Token::Word(cur)
        });
    }
    out
}

pub fn parse(input: &str) -> ParsedQuery {
    let mut q = ParsedQuery::default();
    let mut fts_terms: Vec<String> = Vec::new();
    let mut text_terms: Vec<String> = Vec::new();
    let mut not_terms: Vec<String> = Vec::new();

    for token in tokenize(input) {
        let token = match token {
            // Quoted phrase: exact (non-prefix) FTS phrase match, optionally
            // scoped to a column (`subject:"quarterly report"`).
            Token::Phrase(col, p) => {
                let words: Vec<String> = p
                    .split_whitespace()
                    .map(clean_token)
                    .filter(|w| !w.is_empty())
                    .collect();
                if !words.is_empty() {
                    let phrase = words.join(" ");
                    match col {
                        Some(c) => fts_terms.push(format!("{c}:\"{phrase}\"")),
                        None => {
                            fts_terms.push(format!("\"{phrase}\""));
                            // Sender-boost only applies to unscoped free text.
                            q.terms.push(fold(&phrase));
                        }
                    }
                    text_terms.push(phrase);
                }
                continue;
            }
            Token::Word(w) => w,
        };
        // Gmail-style negation: a token starting with '-' excludes the rest of
        // it (e.g. "-draft"). A bare "-" or "-:" cleans to nothing and is
        // dropped. Handled before operator parsing so "-from" is a text
        // exclusion, not a malformed operator.
        if let Some(rest) = token.strip_prefix('-') {
            let clean = clean_token(rest.trim_start_matches('-'));
            if !clean.is_empty() {
                not_terms.push(fts_term(&clean));
            }
            continue;
        }
        // Operators are case-insensitive on both the key ("In:" == "in:") and
        // the value ("in:Sent" == "in:sent"); users type them however they like.
        let (key, value) = match token.split_once(':') {
            Some((k, v)) => (k.to_ascii_lowercase(), v),
            None => (String::new(), ""),
        };
        // Tolerate list punctuation around operator values:
        // `from:a@b.com, report` must not poison the address filter.
        let value = value.trim_matches([',', ';']);
        match key.as_str() {
            // Address operators keep the value's original case (matched
            // case-insensitively downstream); a bare "from:" is a no-op.
            "from" => {
                if !value.is_empty() {
                    q.from = Some(value.to_string());
                }
            }
            "to" => {
                if !value.is_empty() {
                    q.to = Some(value.to_string());
                }
            }
            "in" => {
                match value.to_ascii_lowercase().as_str() {
                    "inbox" => q.in_folder = Some("inbox".into()),
                    "sent" => q.in_folder = Some("sent".into()),
                    "drafts" => q.in_folder = Some("drafts".into()),
                    "trash" => q.in_folder = Some("trash".into()),
                    "spam" => q.in_folder = Some("spam".into()),
                    "archive" | "done" => q.in_folder = Some("archive".into()),
                    _ => {} // unknown folder: ignore the operator, drop the token
                }
            }
            "is" => match value.to_ascii_lowercase().as_str() {
                "unread" => q.is_unread = Some(true),
                "starred" => q.is_starred = Some(true),
                _ => {}
            },
            "has" => {
                if value.eq_ignore_ascii_case("attachment") {
                    q.has_attachment = Some(true);
                }
            }
            // Column-scoped text: `subject:foo` / `body:foo` restrict the FTS
            // match to that column. A bare `subject:` / `body:` is a no-op.
            // Free-text (embedding) still sees the word; sender-boost does not.
            "subject" | "body" => {
                let clean = clean_token(value);
                if !clean.is_empty() {
                    fts_terms.push(fts_col_term(Some(key.as_str()), &clean));
                    text_terms.push(clean);
                }
            }
            // Exclude a term from results: "exclude:draft" drops any thread
            // whose messages match "draft". A bare "exclude:" is a no-op.
            "exclude" => {
                let clean = clean_token(value);
                if !clean.is_empty() {
                    not_terms.push(fts_term(&clean));
                }
            }
            // Not an operator (includes non-operator tokens with ':' like "12:30").
            _ => {
                let clean = clean_token(&token);
                if !clean.is_empty() {
                    fts_terms.push(fts_term(&clean));
                    q.terms.push(fold(&clean));
                    text_terms.push(clean);
                }
            }
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

    #[test]
    fn parses_operators() {
        let q = parse("from:alice in:inbox is:unread has:attachment quarterly report");
        assert_eq!(q.from.as_deref(), Some("alice"));
        assert_eq!(q.in_folder.as_deref(), Some("inbox"));
        assert_eq!(q.is_unread, Some(true));
        assert_eq!(q.has_attachment, Some(true));
        assert_eq!(q.fts, "\"quarterly\"* AND \"report\"*");
    }

    #[test]
    fn operators_are_case_insensitive() {
        // "in:Sent" (capitalised) must resolve the same as "in:sent".
        let q = parse("in:Sent");
        assert_eq!(q.in_folder.as_deref(), Some("sent"));
        assert!(q.fts.is_empty(), "operator token must not leak into FTS");

        let q = parse("IN:INBOX IS:Unread HAS:Attachment");
        assert_eq!(q.in_folder.as_deref(), Some("inbox"));
        assert_eq!(q.is_unread, Some(true));
        assert_eq!(q.has_attachment, Some(true));
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
        // `!`/`!!` right before the quote (documented as "ignore case" /
        // "match case exactly") must not break column scoping - previously
        // `body:!"..."` lost the "body" prefix entirely and fell through as
        // an unscoped, ungated positive term.
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
        assert_eq!(q.from.as_deref(), Some("hoang.ngyen@nextwaves.com"));
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
}
