//! A query as servers search with it: IMAP `UID SEARCH` criteria, a JMAP
//! `Email/query` filter, and Gmail's own search syntax. Each finds mail this
//! device doesn't keep.
//!
//! A part a server can't search for widens what it finds rather than narrows
//! it, as finding a message too many is better than missing one: it matches
//! everything where it counts for a match, and nothing where it counts
//! against one, under a negation.

use super::{Expr, Filter, Text};
use chrono::{NaiveTime, TimeDelta, TimeZone};
use std::collections::HashMap;

/// A server's take on part of a query: it matches everything, nothing, or
/// what a criterion says.
#[derive(Debug, Clone, PartialEq)]
enum Part<T> {
    All,
    Nothing,
    Some(T),
}

impl<T> Part<T> {
    /// Parts that all have to match, joined by `join`.
    fn all_of(parts: Vec<Part<T>>, join: impl FnOnce(Vec<T>) -> T) -> Part<T> {
        let mut kept = Vec::new();
        for part in parts {
            match part {
                Part::All => {}
                Part::Nothing => return Part::Nothing,
                Part::Some(criterion) => kept.push(criterion),
            }
        }
        match kept.len() {
            0 => Part::All,
            1 => Part::Some(kept.remove(0)),
            _ => Part::Some(join(kept)),
        }
    }

    /// Parts any of which matches, joined by `join`.
    fn any_of(parts: Vec<Part<T>>, join: impl FnOnce(Vec<T>) -> T) -> Part<T> {
        let mut kept = Vec::new();
        for part in parts {
            match part {
                Part::All => return Part::All,
                Part::Nothing => {}
                Part::Some(criterion) => kept.push(criterion),
            }
        }
        match kept.len() {
            0 => Part::Nothing,
            1 => Part::Some(kept.remove(0)),
            _ => Part::Some(join(kept)),
        }
    }

    fn not(self, negate: impl FnOnce(T) -> T) -> Part<T> {
        match self {
            Part::All => Part::Nothing,
            Part::Nothing => Part::All,
            Part::Some(criterion) => Part::Some(negate(criterion)),
        }
    }

    /// What a part the server can't search for stands for: everything where
    /// it counts for a match, and nothing where it counts against one.
    fn unsearchable(widening: bool) -> Part<T> {
        if widening { Part::All } else { Part::Nothing }
    }
}

/// What an IMAP server searches for: `UID SEARCH` criteria, whether they need
/// `CHARSET UTF-8` for text outside ASCII, or nothing to search for at all.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ImapSearch {
    Criteria { criteria: String, utf8: bool },
    Nothing,
}

/// Which folders of an IMAP account a search looks in. One `UID SEARCH`
/// covers a single folder.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Folders {
    /// The folders of these roles, which every match is in.
    Roles(Vec<&'static str>),
    /// Every folder, Trash and Spam too.
    All,
    /// Every folder but Trash and Spam, as mail lists leave those out.
    Usual,
}

/// The folders an IMAP search of `tree` looks in: those the query keeps every
/// match to, every one for `in:anywhere`, or the usual ones.
pub(crate) fn imap_folders(tree: &Expr) -> Folders {
    if names(tree, &|filter| *filter == Filter::Anywhere) {
        return Folders::All;
    }
    let parts: &[Expr] = match tree {
        Expr::And(parts) => parts,
        other => std::slice::from_ref(other),
    };
    let roles = parts
        .iter()
        .filter_map(|part| match part {
            Expr::Filter(Filter::In(role)) => Some(*role),
            _ => None,
        })
        .collect::<Vec<_>>();
    if roles.is_empty() {
        Folders::Usual
    } else {
        Folders::Roles(roles)
    }
}

/// Whether `tree` has a filter that `wanted` picks, anywhere in it.
fn names(tree: &Expr, wanted: &dyn Fn(&Filter) -> bool) -> bool {
    match tree {
        Expr::Text(_) => false,
        Expr::Filter(filter) => wanted(filter),
        Expr::Not(inner) => names(inner, wanted),
        Expr::And(parts) | Expr::Or(parts) => parts.iter().any(|part| names(part, wanted)),
    }
}

/// The `UID SEARCH` criteria for `tree`, with dates as days in `zone`. Folders
/// the query names are left to [`imap_folders`]; attachments, their names,
/// and folders named under `OR` or a negation widen the search.
pub(crate) fn imap<Tz: TimeZone>(tree: &Expr, zone: &Tz) -> ImapSearch {
    let mut utf8 = false;
    match imap_part(tree, zone, true, &mut utf8) {
        Part::All => ImapSearch::Criteria {
            criteria: "ALL".to_owned(),
            utf8: false,
        },
        Part::Nothing => ImapSearch::Nothing,
        Part::Some(criteria) => ImapSearch::Criteria { criteria, utf8 },
    }
}

/// `value` as an IMAP quoted string.
fn imap_string(value: &str, utf8: &mut bool) -> String {
    if !value.is_ascii() {
        *utf8 = true;
    }
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The local day of `at`, in milliseconds, and whether `at` is that day's
/// start.
fn local_day<Tz: TimeZone>(at: i64, zone: &Tz) -> Option<(chrono::NaiveDate, bool)> {
    let local = zone.timestamp_millis_opt(at).single()?;
    Some((local.date_naive(), local.time() == NaiveTime::MIN))
}

fn imap_date(day: chrono::NaiveDate) -> String {
    day.format("%d-%b-%Y").to_string()
}

fn imap_part<Tz: TimeZone>(
    expr: &Expr,
    zone: &Tz,
    widening: bool,
    utf8: &mut bool,
) -> Part<String> {
    match expr {
        Expr::Text(Text { words, column, .. }) => {
            let key = match column {
                Some("subject") => "SUBJECT",
                Some("body") => "BODY",
                _ => "TEXT",
            };
            Part::Some(format!("{key} {}", imap_string(words, utf8)))
        }
        Expr::Filter(filter) => match filter {
            Filter::From(from) => Part::Some(format!("FROM {}", imap_string(from, utf8))),
            Filter::To(to) => Part::Some(format!("TO {}", imap_string(to, utf8))),
            Filter::Unread => Part::Some("UNSEEN".to_owned()),
            Filter::Starred => Part::Some("FLAGGED".to_owned()),
            // IMAP dates are whole days: a time inside a day takes in all of
            // it to widen the search, or none of it to narrow it.
            Filter::Before(at) => match local_day(*at, zone) {
                Some((day, true)) => Part::Some(format!("BEFORE {}", imap_date(day))),
                Some((day, false)) if widening => day
                    .checked_add_signed(TimeDelta::days(1))
                    .map_or(Part::All, |next| {
                        Part::Some(format!("BEFORE {}", imap_date(next)))
                    }),
                Some((day, false)) => Part::Some(format!("BEFORE {}", imap_date(day))),
                None => Part::unsearchable(widening),
            },
            Filter::After(at) => match local_day(*at, zone) {
                Some((day, true)) => Part::Some(format!("SINCE {}", imap_date(day))),
                Some((day, false)) if widening => Part::Some(format!("SINCE {}", imap_date(day))),
                Some((day, false)) => day
                    .checked_add_signed(TimeDelta::days(1))
                    .map_or(Part::Nothing, |next| {
                        Part::Some(format!("SINCE {}", imap_date(next)))
                    }),
                None => Part::unsearchable(widening),
            },
            // LARGER is "more than": at least `size` is more than one less.
            Filter::Larger(size) => Part::Some(format!("LARGER {}", size.saturating_sub(1).max(0))),
            Filter::Smaller(size) => Part::Some(format!("SMALLER {size}")),
            // Every folder is every message, which a search of every folder finds.
            Filter::Anywhere => Part::All,
            Filter::In(_) | Filter::HasAttachment | Filter::Filename(_) => {
                Part::unsearchable(widening)
            }
        },
        Expr::Not(inner) => {
            imap_part(inner, zone, !widening, utf8).not(|inner| format!("NOT {inner}"))
        }
        Expr::And(parts) => Part::all_of(
            parts
                .iter()
                .map(|part| imap_part(part, zone, widening, utf8))
                .collect(),
            |parts| format!("({})", parts.join(" ")),
        ),
        Expr::Or(parts) => Part::any_of(
            parts
                .iter()
                .map(|part| imap_part(part, zone, widening, utf8))
                .collect(),
            |parts| {
                // OR takes two keys: the rest nest in the second.
                let mut parts = parts.into_iter().rev();
                let last = parts.next().unwrap_or_default();
                parts.fold(last, |rest, first| format!("OR {first} {rest}"))
            },
        ),
    }
}

/// A JMAP condition, and conditions joined by AND, OR or NOT.
pub(crate) type JmapFilter = jmap_client::core::query::Filter<jmap_client::email::query::Filter>;

/// What a JMAP server searches for: a filter, every message with none, or
/// nothing to search for at all.
#[derive(Debug)]
pub(crate) enum JmapSearch {
    Filter(Option<JmapFilter>),
    Nothing,
}

/// The JMAP `Email/query` filter for `tree`. `mailboxes` has the account's
/// mailbox ids by folder role, for folders the query names and for the Trash
/// and Spam a search without one leaves out. Attachment names widen the
/// search.
pub(crate) fn jmap(tree: &Expr, mailboxes: &HashMap<&str, String>) -> JmapSearch {
    use jmap_client::email::query::Filter as Condition;
    let filter = match jmap_part(tree, mailboxes, true) {
        Part::All => None,
        Part::Nothing => return JmapSearch::Nothing,
        Part::Some(filter) => Some(filter),
    };
    if names(tree, &|filter| {
        matches!(filter, Filter::In(_) | Filter::Anywhere)
    }) {
        return JmapSearch::Filter(filter);
    }
    // Mail lists leave Trash and Spam out unless a search names them.
    let left_out = ["trash", "spam"]
        .iter()
        .filter_map(|role| mailboxes.get(role).cloned())
        .collect::<Vec<_>>();
    if left_out.is_empty() {
        return JmapSearch::Filter(filter);
    }
    let away = JmapFilter::from(Condition::in_mailbox_other_than(left_out));
    JmapSearch::Filter(Some(match filter {
        None => away,
        Some(filter) => JmapFilter::and([filter, away]),
    }))
}

fn jmap_part(expr: &Expr, mailboxes: &HashMap<&str, String>, widening: bool) -> Part<JmapFilter> {
    use jmap_client::email::query::Filter as Condition;
    let condition = |condition: Condition| Part::Some(JmapFilter::from(condition));
    // JMAP sizes are 32-bit: anything larger is as large as they go.
    let size = |size: i64| u32::try_from(size.max(0)).unwrap_or(u32::MAX);
    match expr {
        Expr::Text(Text { words, column, .. }) => condition(match column {
            Some("subject") => Condition::subject(words.as_str()),
            Some("body") => Condition::body(words.as_str()),
            _ => Condition::text(words.as_str()),
        }),
        Expr::Filter(filter) => match filter {
            Filter::From(from) => condition(Condition::from(from.as_str())),
            Filter::To(to) => condition(Condition::to(to.as_str())),
            Filter::In(role) => match mailboxes.get(role) {
                Some(id) => condition(Condition::in_mailbox(id.as_str())),
                None => Part::unsearchable(widening),
            },
            Filter::Anywhere => Part::All,
            Filter::Unread => condition(Condition::not_keyword("$seen")),
            Filter::Starred => condition(Condition::has_keyword("$flagged")),
            Filter::HasAttachment => condition(Condition::has_attachment(true)),
            Filter::Before(at) => condition(Condition::before(at.div_euclid(1000))),
            Filter::After(at) => condition(Condition::after(at.div_euclid(1000))),
            Filter::Larger(bytes) => condition(Condition::min_size(size(*bytes))),
            Filter::Smaller(bytes) => condition(Condition::max_size(size(*bytes))),
            Filter::Filename(_) => Part::unsearchable(widening),
        },
        Expr::Not(inner) => {
            jmap_part(inner, mailboxes, !widening).not(|inner| JmapFilter::not([inner]))
        }
        Expr::And(parts) => Part::all_of(
            parts
                .iter()
                .map(|part| jmap_part(part, mailboxes, widening))
                .collect(),
            JmapFilter::and,
        ),
        Expr::Or(parts) => Part::any_of(
            parts
                .iter()
                .map(|part| jmap_part(part, mailboxes, widening))
                .collect(),
            JmapFilter::or,
        ),
    }
}

/// `tree` in Gmail's search syntax, with times in seconds since the epoch so
/// Gmail doesn't read days in its own time zone.
pub(crate) fn gmail(tree: &Expr) -> String {
    gmail_part(tree, false)
}

/// A value as Gmail reads one word, quoted when it has spaces.
fn gmail_word(value: &str) -> String {
    let value = value.replace('"', "");
    if value.contains(char::is_whitespace) {
        format!("\"{value}\"")
    } else {
        value
    }
}

fn gmail_part(expr: &Expr, nested: bool) -> String {
    match expr {
        Expr::Text(Text {
            words,
            column,
            phrase,
            ..
        }) => {
            let words = if *phrase {
                format!("\"{}\"", words.replace('"', ""))
            } else {
                gmail_word(words)
            };
            // Gmail has no operator for the body alone: it searches it anyway.
            match column {
                Some("subject") => format!("subject:{words}"),
                _ => words,
            }
        }
        Expr::Filter(filter) => match filter {
            Filter::From(from) => format!("from:{}", gmail_word(from)),
            Filter::To(to) => format!("to:{}", gmail_word(to)),
            Filter::In("archive") => "-in:inbox".to_owned(),
            Filter::In(role) => format!("in:{role}"),
            Filter::Anywhere => "in:anywhere".to_owned(),
            Filter::Unread => "is:unread".to_owned(),
            Filter::Starred => "is:starred".to_owned(),
            Filter::HasAttachment => "has:attachment".to_owned(),
            Filter::Before(at) => format!("before:{}", at.div_euclid(1000)),
            // With seconds, Gmail's `after:` leaves its own second out.
            Filter::After(at) => format!("after:{}", at.div_euclid(1000) - 1),
            Filter::Larger(size) => format!("larger:{size}"),
            Filter::Smaller(size) => format!("smaller:{size}"),
            Filter::Filename(name) => format!("filename:{}", gmail_word(name)),
        },
        // Groups come back in parentheses when nested.
        Expr::Not(inner) => format!("-{}", gmail_part(inner, true)),
        Expr::And(parts) => {
            let joined = parts
                .iter()
                .map(|part| gmail_part(part, true))
                .collect::<Vec<_>>()
                .join(" ");
            if nested {
                format!("({joined})")
            } else {
                joined
            }
        }
        Expr::Or(parts) => format!(
            "({})",
            parts
                .iter()
                .map(|part| gmail_part(part, true))
                .collect::<Vec<_>>()
                .join(" OR ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::parse_at;
    use chrono::{DateTime, FixedOffset};
    use serde_json::{Value, json};

    fn now() -> DateTime<FixedOffset> {
        FixedOffset::east_opt(3600)
            .unwrap()
            .with_ymd_and_hms(2026, 11, 16, 15, 30, 0)
            .unwrap()
    }

    fn tree(query: &str) -> Expr {
        parse_at(query, &now()).tree.expect("a query")
    }

    fn imap_criteria(query: &str) -> ImapSearch {
        imap(&tree(query), &FixedOffset::east_opt(3600).unwrap())
    }

    fn criteria(query: &str) -> String {
        match imap_criteria(query) {
            ImapSearch::Criteria { criteria, .. } => criteria,
            ImapSearch::Nothing => panic!("{query} searches for nothing"),
        }
    }

    #[test]
    fn imap_criteria_say_the_query_as_search_keys() {
        assert_eq!(
            criteria("invoice from:ana is:unread"),
            "(TEXT \"invoice\" FROM \"ana\" UNSEEN)"
        );
        assert_eq!(
            criteria("subject:plan body:\"next week\""),
            "(SUBJECT \"plan\" BODY \"next week\")"
        );
        assert_eq!(
            criteria("from:a OR from:b OR from:c"),
            "OR FROM \"a\" OR FROM \"b\" FROM \"c\""
        );
        assert_eq!(
            criteria("report -(draft OR old)"),
            "(TEXT \"report\" NOT OR TEXT \"draft\" TEXT \"old\")"
        );
        assert_eq!(
            criteria("larger:1M smaller:5M"),
            "(LARGER 999999 SMALLER 5000000)"
        );
        assert_eq!(criteria("is:starred"), "FLAGGED");
        // Backslashes are escaped; text outside ASCII needs UTF-8.
        assert_eq!(
            imap_criteria("from:a\\b café"),
            ImapSearch::Criteria {
                criteria: "(FROM \"a\\\\b\" TEXT \"café\")".to_owned(),
                utf8: true,
            }
        );
    }

    #[test]
    fn imap_dates_are_whole_days_widened_by_times_inside_them() {
        // Days from the query are whole days in the local zone.
        assert_eq!(
            criteria("after:2026/01/05 before:2026/02/01"),
            "(SINCE 05-Jan-2026 BEFORE 01-Feb-2026)"
        );
        // Three days ago at 15:30: since the whole of that day, and before
        // the end of it.
        assert_eq!(criteria("newer_than:3d"), "SINCE 13-Nov-2026");
        assert_eq!(criteria("older_than:3d"), "BEFORE 14-Nov-2026");
        // Negated, the days narrow instead, so the search still widens.
        assert_eq!(criteria("-newer_than:3d"), "NOT SINCE 14-Nov-2026");
    }

    #[test]
    fn what_imap_cant_search_for_widens_the_search() {
        assert_eq!(criteria("invoice has:attachment"), "TEXT \"invoice\"");
        assert_eq!(criteria("filename:pdf"), "ALL");
        // Negated, it still widens: every message rather than none.
        assert_eq!(criteria("invoice -has:attachment"), "TEXT \"invoice\"");
        assert_eq!(criteria("invoice OR has:attachment"), "ALL");
        assert_eq!(criteria("-(invoice has:attachment)"), "ALL");
        assert_eq!(imap_criteria("-in:anywhere"), ImapSearch::Nothing);
    }

    #[test]
    fn imap_searches_the_folders_a_query_keeps_to() {
        assert_eq!(imap_folders(&tree("invoice")), Folders::Usual);
        assert_eq!(
            imap_folders(&tree("in:sent invoice")),
            Folders::Roles(vec!["sent"])
        );
        assert_eq!(imap_folders(&tree("in:anywhere invoice")), Folders::All);
        // Under OR the folder doesn't keep every match: the usual folders.
        assert_eq!(imap_folders(&tree("in:sent OR from:ana")), Folders::Usual);
        assert_eq!(criteria("in:sent invoice"), "TEXT \"invoice\"");
    }

    fn mailboxes() -> HashMap<&'static str, String> {
        [("inbox", "m1"), ("trash", "m7"), ("spam", "m8")]
            .into_iter()
            .map(|(role, id)| (role, id.to_owned()))
            .collect()
    }

    /// The filter JMAP gets for `query`, as JSON; `null` for every message.
    fn jmap_json(query: &str, mailboxes: &HashMap<&str, String>) -> Option<Value> {
        match jmap(&tree(query), mailboxes) {
            JmapSearch::Filter(filter) => Some(serde_json::to_value(filter).unwrap()),
            JmapSearch::Nothing => None,
        }
    }

    #[test]
    fn jmap_filters_say_the_query_and_leave_out_trash_and_spam() {
        assert_eq!(
            jmap_json("invoice from:ana", &mailboxes()),
            Some(json!({"operator": "AND", "conditions": [
                {"operator": "AND", "conditions": [{"text": "invoice"}, {"from": "ana"}]},
                {"inMailboxOtherThan": ["m7", "m8"]},
            ]}))
        );
        assert_eq!(
            jmap_json("in:inbox is:unread larger:2K", &mailboxes()),
            Some(json!({"operator": "AND", "conditions": [
                {"inMailbox": "m1"}, {"notKeyword": "$seen"}, {"minSize": 2000},
            ]}))
        );
        assert_eq!(
            jmap_json("in:anywhere -subject:draft", &mailboxes()),
            Some(json!({"operator": "NOT", "conditions": [{"subject": "draft"}]}))
        );
        assert_eq!(
            jmap_json("in:trash OR is:starred", &mailboxes()),
            Some(json!({"operator": "OR", "conditions": [
                {"inMailbox": "m7"}, {"hasKeyword": "$flagged"},
            ]}))
        );
        assert_eq!(
            jmap_json("in:anywhere after:2026/01/05", &mailboxes()),
            Some(json!({"after": "2026-01-04T23:00:00Z"}))
        );
        // An attachment name widens: every message of the account.
        assert_eq!(
            jmap_json("filename:pdf", &HashMap::new()),
            Some(Value::Null)
        );
        assert_eq!(
            jmap_json("filename:pdf", &mailboxes()),
            Some(json!({"inMailboxOtherThan": ["m7", "m8"]}))
        );
        assert_eq!(jmap_json("-in:anywhere", &mailboxes()), None);
    }

    #[test]
    fn gmail_searches_with_its_own_syntax_and_exact_times() {
        assert_eq!(
            gmail(&tree("invoice from:ana has:attachment")),
            "invoice from:ana has:attachment"
        );
        assert_eq!(
            gmail(&tree("subject:\"next week\" body:plan \"two words\"")),
            "subject:\"next week\" plan \"two words\""
        );
        assert_eq!(
            gmail(&tree("dinner OR movie friday")),
            "(dinner OR movie) friday"
        );
        assert_eq!(
            gmail(&tree("report -(draft OR old) -from:billing")),
            "report -(draft OR old) -from:billing"
        );
        assert_eq!(
            gmail(&tree("after:2026/01/05 larger:10M filename:pdf in:archive")),
            "after:1767567599 larger:10000000 filename:pdf -in:inbox"
        );
        assert_eq!(
            gmail(&tree("in:anywhere is:starred")),
            "in:anywhere is:starred"
        );
    }
}

#[cfg(test)]
mod core_tests {
    use crate::Core;
    use crate::accounts::credentials::DevelopmentFileCredentialStore;
    use crate::config::Paths;
    use std::sync::Arc;

    /// What a search of the servers found shows in the same search of the
    /// mail list, whether or not the copy kept here matches it, until it goes
    /// to Trash. Flags still count.
    #[tokio::test]
    async fn the_mail_list_shows_what_the_server_found() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = Arc::new(DevelopmentFileCredentialStore::new(
            directory.path().join("credentials.json"),
        ));
        let core =
            Core::start_mail_ui_with_credentials(Paths::for_tests(directory.path()), credentials)
                .await
                .unwrap();
        let (thread_id, message_id) = core
            .db
            .write(|conn| {
                crate::db::testutil::seed_account(conn);
                let seeded =
                    crate::db::testutil::seed_message(conn, "ana@one.dev", "Numbers", false);
                crate::db::repo::search::index_message(conn, seeded.1)?;
                Ok(seeded)
            })
            .await
            .unwrap();
        let threads = |query: &str| {
            let core = core.clone();
            let query = query.to_owned();
            async move {
                let page = core
                    .search_chronological_page(query.clone(), None, None, 20)
                    .await
                    .unwrap();
                let messages = core
                    .search_messages_page(query, Some(1), None, 20)
                    .await
                    .unwrap();
                (
                    page.threads
                        .iter()
                        .map(|thread| thread.id)
                        .collect::<Vec<_>>(),
                    messages
                        .messages
                        .iter()
                        .map(|message| message.id)
                        .collect::<Vec<_>>(),
                )
            }
        };
        assert_eq!(threads("invoice").await, (vec![], vec![]));

        // The server matched its body, which isn't here.
        core.remember_found(" invoice ", &[message_id]);
        core.remember_found("is:starred invoice", &[message_id]);
        assert_eq!(
            threads("invoice").await,
            (vec![thread_id], vec![message_id])
        );
        assert_eq!(threads("receipt").await, (vec![], vec![]));
        // Starred is checked here, and it isn't.
        assert_eq!(threads("is:starred invoice").await, (vec![], vec![]));

        core.db
            .write(move |conn| {
                conn.execute(
                    "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Trash', 'trash')",
                    [],
                )?;
                conn.execute(
                    "UPDATE messages SET folder_id = 2 WHERE id = ?1",
                    [message_id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(threads("invoice").await, (vec![], vec![]));
    }

    /// Accounts with no server to search say so, and one that isn't
    /// connected says why, without holding up the rest.
    #[tokio::test]
    async fn accounts_that_cant_be_searched_say_so() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = Arc::new(DevelopmentFileCredentialStore::new(
            directory.path().join("credentials.json"),
        ));
        let core =
            Core::start_mail_ui_with_credentials(Paths::for_tests(directory.path()), credentials)
                .await
                .unwrap();
        core.db
            .write(|conn| {
                crate::db::testutil::seed_account(conn);
                conn.execute(
                    "INSERT INTO accounts (id, email, provider, auth_kind, username,
                     incoming_host, incoming_port, smtp_host, smtp_port, created_at,
                     mail_protocol, settings_json)
                     VALUES (2, 'pop@test.dev', 'generic', 'password', 'pop', 'h', 995,
                             'h', 587, 0, 'pop3', '{}')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let page = core
            .search_server("invoice".into(), vec![1, 2, 3], Vec::new(), 25)
            .await
            .unwrap();
        // 3 is no account at all.
        assert_eq!(page.unsearchable, [2, 3]);
        assert_eq!(
            page.failed,
            [(1, "The account isn't connected.".to_owned())]
        );
        assert!(page.threads.is_empty() && page.messages.is_empty() && page.cursors.is_empty());

        // An empty search has nothing to look for.
        let page = core
            .search_server("  ".into(), vec![1, 2], Vec::new(), 25)
            .await
            .unwrap();
        assert!(page.unsearchable.is_empty() && page.failed.is_empty());
    }
}
