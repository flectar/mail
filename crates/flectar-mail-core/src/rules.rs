//! Rules that sort new mail as it arrives in an Inbox (#238), as filters and
//! inbox rules do in other mail apps. They run in the app, in the transaction
//! sync stores a message in, before anything announces it: the user never
//! sees, nor is notified of, mail a rule files away.
//!
//! Rules are tried top to bottom. A rule's actions mark and label first,
//! then copy, then file the mail away, and filing it away ends the rules for
//! that message, as Stop does.
//!
//! A rule on the body, or on attachments where sync learns of them with the
//! body, can't tell at once. The rules stop at it and the message waits,
//! with its notification held back, until sync has fetched the body and
//! [`resume`] goes on, as Thunderbird applies filters that need the body once
//! it is downloaded.

use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::models::{
    ActionKind, ActionParams, Address, MailProtocol, MailRule, RuleAction, RuleCondition,
    RuleField, RuleOp, now_ms,
};
use regex::{Regex, RegexBuilder};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

/// The most a rule's pattern may compile to, so one can't stall sync.
const PATTERN_SIZE_LIMIT: usize = 1 << 20;

/// How much of a message's text rules look at.
const BODY_LIMIT: usize = 1 << 20;

/// How long rules wait for a body before they settle without it.
pub(crate) const WAIT_LIMIT_MS: i64 = 5 * 60 * 1000;

fn pattern(value: &str) -> std::result::Result<Regex, regex::Error> {
    RegexBuilder::new(value)
        .case_insensitive(true)
        .size_limit(PATTERN_SIZE_LIMIT)
        .dfa_size_limit(PATTERN_SIZE_LIMIT)
        .build()
}

/// Whether `value` is a pattern a rule can use.
pub fn is_valid_pattern(value: &str) -> bool {
    pattern(value).is_ok()
}

/// A size in KB as a condition writes it.
fn kilobytes(value: &str) -> Option<i64> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|kb| (0..=i64::MAX / 1024).contains(kb))
}

/// Whether `op` can test `field`.
pub fn op_fits(field: RuleField, op: RuleOp) -> bool {
    match field {
        RuleField::Size => matches!(op, RuleOp::Over | RuleOp::Under),
        RuleField::MailingList | RuleField::Header => !matches!(op, RuleOp::Over | RuleOp::Under),
        RuleField::Body => matches!(op, RuleOp::Contains | RuleOp::Matches),
        RuleField::Attachments => op == RuleOp::Exists,
        RuleField::From
        | RuleField::To
        | RuleField::Cc
        | RuleField::ToOrCc
        | RuleField::Subject => !matches!(op, RuleOp::Over | RuleOp::Under | RuleOp::Exists),
    }
}

/// Whether `name` can name a header in a rule: letters, digits and dashes,
/// which every server takes in a request for named headers.
pub fn is_valid_header_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// The headers `account_id`'s rules test, which the fetch of its new mail
/// brings along.
pub(crate) fn header_names(conn: &Connection, account_id: i64) -> Result<Vec<String>> {
    let mut names = Vec::<String>::new();
    for rule in repo::rules::enabled_for(conn, account_id)? {
        for condition in rule.conditions {
            if condition.field == RuleField::Header
                && is_valid_header_name(&condition.header)
                && !names
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&condition.header))
            {
                names.push(condition.header);
            }
        }
    }
    Ok(names)
}

/// Whether `action` files mail away, out of the Inbox.
pub fn files_away(action: &RuleAction) -> bool {
    matches!(
        action,
        RuleAction::Move { .. } | RuleAction::Archive | RuleAction::Trash | RuleAction::Junk
    )
}

/// Check that `rule` can be saved as it is, so a mistake shows when it is
/// written and not as mail arrives.
pub(crate) fn check(conn: &Connection, rule: &MailRule) -> Result<()> {
    let invalid = |why: &str| Err(CoreError::Other(format!("rule: {why}")));
    if rule.name.trim().is_empty() {
        return invalid("it needs a name");
    }
    if rule.conditions.is_empty() {
        return invalid("it needs a condition");
    }
    if rule.actions.is_empty() {
        return invalid("it needs an action");
    }
    let protocol = match rule.account_id {
        Some(account_id) => Some(
            repo::accounts::get(conn, account_id)?
                .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?
                .mail_protocol,
        ),
        None => None,
    };
    for condition in &rule.conditions {
        if !op_fits(condition.field, condition.op) {
            return invalid("a condition compares in a way its field can't");
        }
        if condition.field == RuleField::Header && !is_valid_header_name(&condition.header) {
            return invalid("a header name isn't valid");
        }
        match condition.op {
            RuleOp::Exists => {}
            RuleOp::Over | RuleOp::Under => {
                if kilobytes(&condition.value).is_none() {
                    return invalid("a size isn't a number of KB");
                }
            }
            RuleOp::Matches => {
                if condition.value.is_empty() || !is_valid_pattern(&condition.value) {
                    return invalid("a pattern isn't valid");
                }
            }
            RuleOp::Contains | RuleOp::Is | RuleOp::StartsWith | RuleOp::EndsWith => {
                if condition.value.trim().is_empty() {
                    return invalid("a condition has nothing to look for");
                }
            }
        }
    }
    if rule
        .actions
        .iter()
        .filter(|action| files_away(action))
        .count()
        > 1
    {
        return invalid("it files mail away more than once");
    }
    if rule
        .actions
        .iter()
        .filter(|action| matches!(action, RuleAction::ShowIn { .. }))
        .count()
        > 1
    {
        return invalid("it shows mail in more than one tab");
    }
    for action in &rule.actions {
        match action {
            RuleAction::Move { folder } | RuleAction::Copy { folder } => {
                let Some(account_id) = rule.account_id else {
                    return invalid("only a rule for one account can use its folders");
                };
                let folder = repo::folders::get(conn, *folder)?
                    .ok_or_else(|| CoreError::NotFound(format!("folder {folder}")))?;
                if folder.account_id != account_id {
                    return invalid("a folder belongs to another account");
                }
                if matches!(action, RuleAction::Copy { .. }) && protocol != Some(MailProtocol::Jmap)
                {
                    return invalid("this account keeps each message in one folder");
                }
            }
            RuleAction::Label { label } => {
                let label = repo::labels::get(conn, *label)?
                    .ok_or_else(|| CoreError::NotFound(format!("label {label}")))?;
                if label.owner_account_id.is_some() && label.owner_account_id != rule.account_id {
                    return invalid("a label belongs to another account");
                }
            }
            RuleAction::ShowIn { tab } => {
                let tab_exists = match tab.as_str() {
                    "important" | "other" => true,
                    tab => match tab.strip_prefix("label:").and_then(|id| id.parse().ok()) {
                        Some(id) => repo::labels::get(conn, id)?.is_some_and(|label| label.is_auto),
                        None => false,
                    },
                };
                if !tab_exists {
                    return invalid("a tab isn't one of the Inbox's");
                }
            }
            RuleAction::MarkRead
            | RuleAction::Star
            | RuleAction::Archive
            | RuleAction::Trash
            | RuleAction::Junk => {}
        }
    }
    Ok(())
}

/// What rules can test about a message.
#[derive(Debug, Default)]
pub(crate) struct Facts {
    pub subject: String,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Lowercase, as `mime::list_id` gives it.
    pub list_id: Option<String>,
    /// In bytes.
    pub size: Option<i64>,
    /// Header fields, names and values. None until they are known.
    pub headers: Option<Vec<(String, String)>>,
    /// The message's text, lowercase. None until its body is here.
    pub body: Option<String>,
    /// Whether files are attached. None until that is known.
    pub attachments: Option<bool>,
}

/// `text` lowercase, at most [`BODY_LIMIT`] of it.
fn body_text(text: &str) -> String {
    let mut end = text.len().min(BODY_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_lowercase()
}

/// A condition ready to test: its text lowercase, its pattern compiled.
struct Test {
    condition: RuleCondition,
    value: String,
    pattern: Option<Regex>,
}

impl Test {
    fn new(condition: &RuleCondition) -> Self {
        let pattern = (condition.op == RuleOp::Matches)
            .then(|| pattern(&condition.value).ok())
            .flatten();
        Self {
            condition: condition.clone(),
            value: condition.value.trim().to_lowercase(),
            pattern,
        }
    }

    fn text_holds(&self, text: &str) -> bool {
        match self.condition.op {
            RuleOp::Matches => self
                .pattern
                .as_ref()
                .is_some_and(|pattern| pattern.is_match(text)),
            op => {
                let text = text.trim().to_lowercase();
                match op {
                    RuleOp::Contains => text.contains(&self.value),
                    RuleOp::Is => text == self.value,
                    RuleOp::StartsWith => text.starts_with(&self.value),
                    RuleOp::EndsWith => text.ends_with(&self.value),
                    _ => false,
                }
            }
        }
    }

    /// Whether any address holds by its email or its name.
    fn addresses_hold<'a>(&self, addresses: impl IntoIterator<Item = &'a Address>) -> bool {
        addresses.into_iter().any(|address| {
            self.text_holds(&address.email)
                || address
                    .name
                    .as_deref()
                    .is_some_and(|name| self.text_holds(name))
        })
    }

    /// Whether the condition holds for `facts`. None when it can't tell yet.
    fn holds(&self, facts: &Facts) -> Option<bool> {
        let tested = match self.condition.field {
            RuleField::From => self.addresses_hold(&facts.from),
            RuleField::To => self.addresses_hold(&facts.to),
            RuleField::Cc => self.addresses_hold(&facts.cc),
            RuleField::ToOrCc => self.addresses_hold(facts.to.iter().chain(&facts.cc)),
            RuleField::Subject => self.text_holds(&facts.subject),
            RuleField::MailingList => match (&facts.list_id, self.condition.op) {
                (Some(_), RuleOp::Exists) => true,
                (Some(list_id), _) => self.text_holds(list_id),
                (None, _) => false,
            },
            RuleField::Size => {
                let limit = kilobytes(&self.condition.value)?.saturating_mul(1024);
                facts.size.is_some_and(|size| match self.condition.op {
                    RuleOp::Over => size > limit,
                    RuleOp::Under => size < limit,
                    _ => false,
                })
            }
            RuleField::Header => {
                let mut values = facts
                    .headers
                    .as_ref()?
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case(&self.condition.header))
                    .map(|(_, value)| value.as_str());
                match self.condition.op {
                    RuleOp::Exists => values.next().is_some(),
                    _ => values.any(|value| self.text_holds(value)),
                }
            }
            // The text is lowercase already, and patterns ignore case.
            RuleField::Body => {
                let body = facts.body.as_deref()?;
                match self.condition.op {
                    RuleOp::Contains => body.contains(&self.value),
                    RuleOp::Matches => self
                        .pattern
                        .as_ref()
                        .is_some_and(|pattern| pattern.is_match(body)),
                    _ => false,
                }
            }
            RuleField::Attachments => facts.attachments?,
        };
        Some(tested != self.condition.negate)
    }
}

/// A rule ready to test.
struct Compiled {
    rule: MailRule,
    tests: Vec<Test>,
}

impl Compiled {
    fn new(rule: MailRule) -> Self {
        let tests = rule.conditions.iter().map(Test::new).collect();
        Self { rule, tests }
    }

    /// Whether `facts` meet the rule. None when that depends on what isn't
    /// known yet.
    fn matches(&self, facts: &Facts) -> Option<bool> {
        let mut unknown = false;
        for test in &self.tests {
            match test.holds(facts) {
                Some(held) if held != self.rule.match_all => return Some(held),
                Some(_) => {}
                None => unknown = true,
            }
        }
        if unknown {
            None
        } else {
            Some(self.rule.match_all)
        }
    }
}

/// What rules did with a message.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sorted {
    /// It left the Inbox.
    pub filed: bool,
    pub read: bool,
    /// It went to Trash or Junk.
    pub unwanted: bool,
    /// A rule waits for its body: [`hold`] its notification, and fetch the
    /// body for [`resume`].
    pub waiting: bool,
}

impl Sorted {
    /// Whether the message should not notify: it isn't new, unread mail in
    /// the Inbox any more.
    pub fn silent(&self) -> bool {
        self.filed || self.read
    }
}

/// What sync knows of a message it just stored, beyond the stored row.
#[derive(Debug, Default)]
pub(crate) struct Arrival {
    /// Its header fields as fetched: the usual ones and those rules name.
    pub headers: Option<Vec<(String, String)>>,
    /// Its text, when the whole message came (POP3).
    pub text: Option<String>,
    /// Whether the row's attachment flag is known yet. IMAP and Gmail learn
    /// it with the body.
    pub attachments_known: bool,
}

/// Where rules stopped for a message's body, kept on the message.
#[derive(Debug, Serialize, Deserialize)]
struct Waiting {
    /// The rule to go on from.
    rule: i64,
    /// The notification held back for the rules, and whether it may play a
    /// sound.
    #[serde(default)]
    announce: Option<bool>,
    /// The header fields sync fetched with it, which the body may lack.
    #[serde(default)]
    headers: Option<Vec<(String, String)>>,
    since: i64,
}

fn waiting(conn: &Connection, message_id: i64) -> Result<Option<Waiting>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT rules_waiting FROM messages WHERE id = ?1",
            [message_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(json.and_then(|json| serde_json::from_str(&json).ok()))
}

fn set_waiting(conn: &Connection, message_id: i64, waiting: Option<&Waiting>) -> Result<()> {
    let json = waiting.map(serde_json::to_string).transpose()?;
    conn.execute(
        "UPDATE messages SET rules_waiting = ?2 WHERE id = ?1",
        params![message_id, json],
    )?;
    Ok(())
}

/// Hold back the notification of `message_id` while a rule waits for its
/// body. [`resume`] shows it unless the rules file the message away or mark
/// it read. `audible` is whether it may play a sound.
pub(crate) fn hold(conn: &Connection, message_id: i64, audible: bool) -> Result<()> {
    if let Some(mut waiting) = waiting(conn, message_id)? {
        waiting.announce = Some(audible);
        set_waiting(conn, message_id, Some(&waiting))?;
    }
    Ok(())
}

/// What going on with the rules did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Resumed {
    pub thread_id: i64,
    /// The held notification was queued, so the notification dispatcher
    /// should be woken for the thread.
    pub announced: bool,
}

/// Go on with the rules that waited for `message_id`'s body, now stored,
/// from the rule that stopped them. What still can't be told doesn't match.
/// Then the notification they held back shows, unless they filed the
/// message away or marked it read, or it left the Inbox or was read
/// meanwhile. None when no rule waited.
pub(crate) fn resume(tx: &Transaction, message_id: i64) -> Result<Option<Resumed>> {
    settle(tx, message_id, true)
}

/// Settle the rules that waited for a body that didn't come: what can't be
/// told doesn't match, and the held notification shows.
pub(crate) fn give_up(tx: &Transaction, message_id: i64) -> Result<Option<Resumed>> {
    settle(tx, message_id, false)
}

fn settle(tx: &Transaction, message_id: i64, with_body: bool) -> Result<Option<Resumed>> {
    let Some(waiting) = waiting(tx, message_id)? else {
        return Ok(None);
    };
    set_waiting(tx, message_id, None)?;
    let account_id: i64 = tx.query_row(
        "SELECT account_id FROM messages WHERE id = ?1",
        [message_id],
        |row| row.get(0),
    )?;
    let Some((thread_id, mut facts, attachments)) = inbox_facts(tx, account_id, message_id)? else {
        return Ok(None);
    };
    facts.headers = waiting.headers;
    if with_body {
        let stored: Option<(Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT text_body, html_body FROM message_bodies WHERE message_id = ?1",
                [message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        facts.body = stored.and_then(|(text, html)| {
            crate::mime::body_text(text.as_deref(), html.as_deref()).map(|text| body_text(&text))
        });
        facts.attachments = Some(attachments);
    }
    let sorter = Sorter::load(tx, account_id)?;
    // From the rule that waited, or from the top when it is gone.
    let start = sorter
        .rules
        .iter()
        .position(|compiled| compiled.rule.id == waiting.rule)
        .unwrap_or(0);
    let (sorted, _) = run(
        tx,
        account_id,
        thread_id,
        message_id,
        &facts,
        &sorter.rules[start..],
        true,
    )?;
    let unread = tx.query_row(
        "SELECT is_read = 0 FROM messages WHERE id = ?1",
        [message_id],
        |row| row.get::<_, bool>(0),
    )?;
    let announced = match waiting.announce {
        Some(audible) if unread && !sorted.silent() => {
            repo::notifications::enqueue(tx, message_id, audible)?;
            true
        }
        _ => false,
    };
    Ok(Some(Resumed {
        thread_id,
        announced,
    }))
}

/// The messages of `account_id` whose rules have waited since before
/// `before` (milliseconds since the epoch).
pub(crate) fn overdue(conn: &Connection, account_id: i64, before: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM messages
         WHERE account_id = ?1 AND rules_waiting IS NOT NULL
           AND COALESCE(json_extract(rules_waiting, '$.since'), 0) < ?2",
    )?;
    let ids = stmt
        .query_map(params![account_id, before], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

/// An account's rules, loaded once for the mail one sync transaction stores.
pub(crate) struct Sorter {
    account_id: i64,
    rules: Vec<Compiled>,
}

impl Sorter {
    pub(crate) fn load(conn: &Connection, account_id: i64) -> Result<Self> {
        Ok(Self {
            account_id,
            rules: repo::rules::enabled_for(conn, account_id)?
                .into_iter()
                .map(Compiled::new)
                .collect(),
        })
    }

    /// Sort `message_id`, which sync just stored as new mail, when it is
    /// incoming mail in the Inbox. A rule that needs the body leaves it
    /// waiting (see [`Sorted::waiting`]).
    pub(crate) fn arrived(
        &self,
        tx: &Transaction,
        message_id: i64,
        arrival: Arrival,
    ) -> Result<Sorted> {
        if self.rules.is_empty() {
            return Ok(Sorted::default());
        }
        let Some((thread_id, mut facts, attachments)) =
            inbox_facts(tx, self.account_id, message_id)?
        else {
            return Ok(Sorted::default());
        };
        facts.headers = arrival.headers;
        facts.body = arrival.text.as_deref().map(body_text);
        facts.attachments = arrival.attachments_known.then_some(attachments);
        let (sorted, waiting_rule) = run(
            tx,
            self.account_id,
            thread_id,
            message_id,
            &facts,
            &self.rules,
            false,
        )?;
        let Some(rule) = waiting_rule else {
            return Ok(sorted);
        };
        set_waiting(
            tx,
            message_id,
            Some(&Waiting {
                rule,
                announce: None,
                headers: facts.headers,
                since: now_ms(),
            }),
        )?;
        Ok(Sorted {
            waiting: true,
            ..sorted
        })
    }
}

/// Try `rules` on the message in order. When one can't tell yet, the rules
/// stop there and its id comes back, unless `settle` takes what isn't known
/// as not matching.
fn run(
    tx: &Transaction,
    account_id: i64,
    thread_id: i64,
    message_id: i64,
    facts: &Facts,
    rules: &[Compiled],
    settle: bool,
) -> Result<(Sorted, Option<i64>)> {
    let mut sorted = Sorted::default();
    for compiled in rules {
        match compiled.matches(facts) {
            Some(true) => {}
            Some(false) => continue,
            None if settle => continue,
            None => return Ok((sorted, Some(compiled.rule.id))),
        }
        tracing::info!(
            account_id,
            message_id,
            rule = compiled.rule.id,
            "receive: a rule matched new mail"
        );
        let mut actions = compiled.rule.actions.iter().collect::<Vec<_>>();
        // Marks and labels, then copies, then filing away.
        actions.sort_by_key(|action| match action {
            RuleAction::Copy { .. } => 1,
            action if files_away(action) => 2,
            _ => 0,
        });
        for action in actions {
            if apply(tx, thread_id, message_id, action)?.is_some() {
                match action {
                    RuleAction::MarkRead => sorted.read = true,
                    RuleAction::Trash | RuleAction::Junk => {
                        sorted.filed = true;
                        sorted.unwanted = true;
                    }
                    action if files_away(action) => sorted.filed = true,
                    _ => {}
                }
            }
        }
        if compiled.rule.stop || sorted.filed {
            break;
        }
    }
    Ok((sorted, None))
}

/// The message's facts from its row, with its thread and its attachment
/// flag, when it is incoming mail in the account's Inbox.
fn inbox_facts(
    conn: &Connection,
    account_id: i64,
    message_id: i64,
) -> Result<Option<(i64, Facts, bool)>> {
    Ok(conn
        .query_row(
            "SELECT m.thread_id, m.subject, m.from_name, m.from_addr, m.to_json, m.cc_json,
                    m.list_id, m.size, m.has_attachments
             FROM messages m
             WHERE m.id = ?1 AND m.account_id = ?2 AND m.thread_id IS NOT NULL
               AND m.is_draft = 0 AND m.is_outgoing = 0
               AND (EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role = 'inbox')
                    OR EXISTS (SELECT 1 FROM message_folders mf
                               JOIN folders f ON f.id = mf.folder_id
                               WHERE mf.message_id = m.id AND f.role = 'inbox'))",
            params![message_id, account_id],
            |row| {
                let from = row
                    .get::<_, Option<String>>(3)?
                    .map(|email| Address {
                        name: row.get(2).ok().flatten(),
                        email,
                    })
                    .into_iter()
                    .collect();
                Ok((
                    row.get(0)?,
                    Facts {
                        subject: row.get(1)?,
                        from,
                        to: repo::parse_addrs(&row.get::<_, String>(4)?, 4)?,
                        cc: repo::parse_addrs(&row.get::<_, String>(5)?, 5)?,
                        list_id: row.get(6)?,
                        size: row.get(7)?,
                        ..Facts::default()
                    },
                    row.get(8)?,
                ))
            },
        )
        .optional()?)
}

/// Take `action` on the message, as the user would on that one message, so
/// the server follows and Undo works: the actions it queued. None when it
/// can't be taken, such as a move to a folder that is gone, which leaves the
/// message where it is.
fn apply(
    tx: &Transaction,
    thread_id: i64,
    message_id: i64,
    action: &RuleAction,
) -> Result<Option<Vec<i64>>> {
    if let RuleAction::ShowIn { tab } = action {
        return show_in(tx, thread_id, message_id, tab).map(|action_id| Some(vec![action_id]));
    }
    let params = |target_folder_id, label_id| {
        Some(ActionParams {
            wake_at: None,
            target_folder_id,
            label_id,
        })
    };
    let (kind, params) = match action {
        RuleAction::Move { folder } => (ActionKind::Move, params(Some(*folder), None)),
        RuleAction::Copy { folder } => (ActionKind::Copy, params(Some(*folder), None)),
        RuleAction::Label { label } => (ActionKind::AddLabel, params(None, Some(*label))),
        RuleAction::MarkRead => (ActionKind::MarkRead, None),
        RuleAction::Star => (ActionKind::Star, None),
        RuleAction::Archive => (ActionKind::Archive, None),
        RuleAction::Trash => (ActionKind::Trash, None),
        RuleAction::Junk => (ActionKind::Spam, None),
        RuleAction::ShowIn { .. } => unreachable!("shown in a tab above"),
    };
    match crate::apply_thread_action_in(tx, thread_id, Some(&[message_id]), kind, params.as_ref()) {
        Ok(queued) => Ok(Some(
            queued.into_iter().map(|(action_id, _)| action_id).collect(),
        )),
        Err(error @ (CoreError::Db(_) | CoreError::Io(_))) => Err(error),
        Err(error) => {
            tracing::warn!(message_id, %error, "a rule's action couldn't be taken");
            Ok(None)
        }
    }
}

/// Puts the conversation of `message_id` in Inbox tab `tab` for good, and
/// keeps where it was, so Undo can put it back. Only this device has tabs, so
/// the action is done once queued.
fn show_in(tx: &Transaction, thread_id: i64, message_id: i64, tab: &str) -> Result<i64> {
    let (account_id, pinned, routed): (i64, Option<String>, Option<String>) = tx.query_row(
        "SELECT account_id, pinned_tab, routed_tab FROM threads WHERE id = ?1",
        [thread_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    crate::route::pin_tab(tx, thread_id, Some(tab))?;
    repo::actions::enqueue(
        tx,
        account_id,
        "show_in",
        Some(message_id),
        Some(thread_id),
        &serde_json::json!({"tab": tab, "pinned": pinned, "routed": routed}),
        None,
    )
}

/// A new rule for mail like `message_id`: for the list it came from, when
/// it came from one, otherwise for its sender, as peers prefill a rule made
/// from a message, List-Id first.
pub(crate) fn draft_for(conn: &Connection, message_id: i64) -> Result<MailRule> {
    let (account_id, from_name, from_addr, list_id): (
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn.query_row(
        "SELECT account_id, from_name, from_addr, list_id FROM messages WHERE id = ?1",
        [message_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let sender = from_addr.unwrap_or_default();
    let (field, value) = match list_id {
        Some(list_id) => (RuleField::MailingList, list_id),
        None => (RuleField::From, sender.clone()),
    };
    let name = from_name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            if value.is_empty() {
                sender
            } else {
                value.clone()
            }
        });
    Ok(MailRule {
        id: 0,
        account_id: Some(account_id),
        name,
        enabled: true,
        match_all: true,
        conditions: vec![RuleCondition {
            field,
            header: String::new(),
            op: RuleOp::Is,
            value,
            negate: false,
        }],
        // The editor offers the account's first folder.
        actions: vec![RuleAction::Move { folder: 0 }],
        stop: false,
    })
}

/// The most Inbox messages a rule is tried on at once.
const RUN_LIMIT: i64 = 10_000;

/// What a rule finds in the Inbox now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxMatches {
    /// The messages it matches, newest first.
    pub message_ids: Vec<i64>,
    /// Messages it can't tell about: rules on the body or attachments need
    /// their body, and rules on any header the original message.
    pub unknown: usize,
}

/// The incoming mail in the Inbox of `rule`'s account, or of every account,
/// that `rule` matches now, with what is kept here. Peers run rules on
/// existing mail without counting first; the count lets the user see what a
/// rule would do before it does it.
pub(crate) fn inbox_matches(conn: &Connection, rule: &MailRule) -> Result<InboxMatches> {
    let compiled = Compiled::new(rule.clone());
    let uses = |field| {
        rule.conditions
            .iter()
            .any(|condition| condition.field == field)
    };
    let (needs_body, needs_headers) = (uses(RuleField::Body), uses(RuleField::Header));
    let mut stmt = conn.prepare(
        "SELECT m.id, m.account_id, m.body_state, m.raw_path, a.mail_protocol
         FROM messages m JOIN accounts a ON a.id = m.account_id
         WHERE (?1 IS NULL OR m.account_id = ?1) AND a.mail_protocol <> 'local'
           AND m.is_draft = 0 AND m.is_outgoing = 0 AND m.thread_id IS NOT NULL
           AND (EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role = 'inbox')
                OR EXISTS (SELECT 1 FROM message_folders mf
                           JOIN folders f ON f.id = mf.folder_id
                           WHERE mf.message_id = m.id AND f.role = 'inbox'))
         ORDER BY m.date DESC, m.id DESC
         LIMIT ?2",
    )?;
    let candidates = stmt
        .query_map(params![rule.account_id, RUN_LIMIT], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, MailProtocol>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut found = InboxMatches::default();
    for (message_id, account_id, body_state, raw_path, protocol) in candidates {
        let Some((_, mut facts, attachments)) = inbox_facts(conn, account_id, message_id)? else {
            continue;
        };
        let cached = body_state == "cached";
        // IMAP and Gmail learn of attachments with the body.
        if cached || matches!(protocol, MailProtocol::Jmap | MailProtocol::Pop3) {
            facts.attachments = Some(attachments);
        }
        if needs_body && cached {
            let stored: Option<(Option<String>, Option<String>)> = conn
                .query_row(
                    "SELECT text_body, html_body FROM message_bodies WHERE message_id = ?1",
                    [message_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            facts.body = stored.and_then(|(text, html)| {
                crate::mime::body_text(text.as_deref(), html.as_deref())
                    .map(|text| body_text(&text))
            });
        }
        if needs_headers && let Some(path) = raw_path {
            facts.headers = header_block(&path).map(|block| crate::mime::header_fields(&block));
        }
        match compiled.matches(&facts) {
            Some(true) => found.message_ids.push(message_id),
            Some(false) => {}
            None => found.unknown += 1,
        }
    }
    Ok(found)
}

/// The header block at the start of the message saved at `path`.
fn header_block(path: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut start = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(256 * 1024)
        .read_to_end(&mut start)
        .ok()?;
    let end = start
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
        .or_else(|| {
            start
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|at| at + 2)
        })
        .unwrap_or(start.len());
    start.truncate(end);
    Some(start)
}

/// Take `rule`'s actions on `message_ids`, as the user would, so the server
/// follows and Undo works: the actions queued. Mail that left the Inbox
/// since it was counted is left alone.
pub(crate) fn run_on(tx: &Transaction, rule: &MailRule, message_ids: &[i64]) -> Result<Vec<i64>> {
    let mut actions = rule.actions.iter().collect::<Vec<_>>();
    // Marks and labels, then copies, then filing away.
    actions.sort_by_key(|action| match action {
        RuleAction::Copy { .. } => 1,
        action if files_away(action) => 2,
        _ => 0,
    });
    let mut queued = Vec::new();
    for &message_id in message_ids {
        let account_id: Option<i64> = tx
            .query_row(
                "SELECT account_id FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(account_id) = account_id else {
            continue;
        };
        let Some((thread_id, _, _)) = inbox_facts(tx, account_id, message_id)? else {
            continue;
        };
        for action in &actions {
            if let Some(ids) = apply(tx, thread_id, message_id, action)? {
                queued.extend(ids);
            }
        }
    }
    Ok(queued)
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
