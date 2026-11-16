//! Rules that sort new mail as it arrives in an Inbox (#238), as filters and
//! inbox rules do in other mail apps. They run in the app, in the transaction
//! sync stores a message in, before anything announces it: the user never
//! sees, nor is notified of, mail a rule files away.
//!
//! Rules are tried top to bottom. A rule's actions mark and label first,
//! then copy, then file the mail away, and filing it away ends the rules for
//! that message, as Stop does.

use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::models::{
    ActionKind, ActionParams, Address, MailProtocol, MailRule, RuleAction, RuleCondition,
    RuleField, RuleOp,
};
use regex::{Regex, RegexBuilder};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// The most a rule's pattern may compile to, so one can't stall sync.
const PATTERN_SIZE_LIMIT: usize = 1 << 20;

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
        RuleField::MailingList => !matches!(op, RuleOp::Over | RuleOp::Under),
        RuleField::From
        | RuleField::To
        | RuleField::Cc
        | RuleField::ToOrCc
        | RuleField::Subject => !matches!(op, RuleOp::Over | RuleOp::Under | RuleOp::Exists),
    }
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
            RuleAction::MarkRead
            | RuleAction::Star
            | RuleAction::Archive
            | RuleAction::Trash
            | RuleAction::Junk => {}
        }
    }
    Ok(())
}

/// What rules can test about a message as it arrives.
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
}

impl Sorted {
    /// Whether the message should not notify: it isn't new, unread mail in
    /// the Inbox any more.
    pub fn silent(&self) -> bool {
        self.filed || self.read
    }
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
    /// incoming mail in the Inbox.
    pub(crate) fn arrived(&self, tx: &Transaction, message_id: i64) -> Result<Sorted> {
        if self.rules.is_empty() {
            return Ok(Sorted::default());
        }
        let Some((thread_id, facts)) = inbox_facts(tx, self.account_id, message_id)? else {
            return Ok(Sorted::default());
        };
        let mut sorted = Sorted::default();
        for compiled in &self.rules {
            if compiled.matches(&facts) != Some(true) {
                continue;
            }
            tracing::info!(
                account_id = self.account_id,
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
                if apply(tx, thread_id, message_id, action)? {
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
        Ok(sorted)
    }
}

/// The message's facts, when it is incoming mail in the account's Inbox.
fn inbox_facts(tx: &Transaction, account_id: i64, message_id: i64) -> Result<Option<(i64, Facts)>> {
    Ok(tx
        .query_row(
            "SELECT m.thread_id, m.subject, m.from_name, m.from_addr, m.to_json, m.cc_json,
                    m.list_id, m.size
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
                    },
                ))
            },
        )
        .optional()?)
}

/// Take `action` on the message, as the user would on that one message, so
/// the server follows and Undo works. False when it can't be taken, such as
/// a move to a folder that is gone, which leaves the message where it is.
fn apply(tx: &Transaction, thread_id: i64, message_id: i64, action: &RuleAction) -> Result<bool> {
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
    };
    match crate::apply_thread_action_in(tx, thread_id, Some(&[message_id]), kind, params.as_ref()) {
        Ok(_) => Ok(true),
        Err(error @ (CoreError::Db(_) | CoreError::Io(_))) => Err(error),
        Err(error) => {
            tracing::warn!(message_id, %error, "a rule's action couldn't be taken");
            Ok(false)
        }
    }
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
