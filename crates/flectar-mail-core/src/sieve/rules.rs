//! Rules (#238) as Sieve (RFC 5228), kept in the app's script beside the
//! automatic replies, so the server sorts new mail while Flectar Mail is
//! closed. A rule goes to the server only when Sieve gives it the result the
//! app does. The first rule that can't go, and every rule after it, run in
//! the app alone, since the server's rules run first, as mail arrives. The
//! app still tries every rule on new mail, and finds the work of those the
//! server ran already done.

use super::script::{RULES_END, RULES_START, list, string};
use crate::models::{RuleCondition, RuleField, RuleOp};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Bumped when the same rules compile to other Sieve, so that scripts
/// written before are written again.
const FORMAT: &str = "1";
/// Compares text without regard to case beyond ASCII (RFC 5051).
const UNICODE_CASEMAP: &str = "comparator-i;unicode-casemap";
/// The largest size in KB a script names, as servers read numbers into 32
/// bits. No message comes near it.
const MOST_KB: i64 = i32::MAX as i64;
/// Lines up the tests of `if allof (`.
const INDENT: &str = "          ";

/// What a server's Sieve runs.
pub(crate) struct Engine<'a> {
    /// Whether scripts there may use an extension.
    pub(crate) offers: &'a dyn Fn(&str) -> bool,
    /// Its name and version, such as "Dovecot Pigeonhole".
    pub(crate) implementation: &'a str,
}

impl Engine<'_> {
    fn has(&self, extension: &str) -> bool {
        (self.offers)(extension)
            // Pigeonhole compares this way without listing it.
            || (extension == UNICODE_CASEMAP && self.named("pigeonhole"))
    }

    fn named(&self, name: &str) -> bool {
        self.implementation.to_ascii_lowercase().contains(name)
    }
}

/// A rule as the server runs it, its folders by name and labels by keyword.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Rule {
    #[serde(skip)]
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) match_all: bool,
    pub(crate) conditions: Vec<RuleCondition>,
    pub(crate) actions: Vec<Action>,
    pub(crate) stop: bool,
}

/// What a rule does, as the server does it. Archive, Trash and Junk move
/// mail into the account's folder for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum Action {
    Move { folder: Folder },
    Copy { folder: Folder },
    Label { keyword: String },
    MarkRead,
    Star,
}

/// A folder a rule files into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Folder {
    /// Its name as IMAP gives it, modified UTF-7 where the server uses that.
    pub(crate) path: String,
    /// Its id, on a JMAP server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) jmap_id: Option<String>,
}

/// The rules' part of the app's script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section {
    /// From `# Rules` to `# End of rules`, each line ending in CRLF.
    pub(crate) text: String,
    /// The extensions it uses.
    pub(crate) needs: Vec<String>,
    /// Tells these rules from others, however the app writes them.
    pub(crate) digest: String,
}

/// Why a rule runs in the app alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Why {
    /// It looks for a pattern, which Sieve reads another way.
    Pattern,
    /// It asks whether files are attached, which Sieve can't tell exactly.
    Attachments,
    /// It compares text beyond ASCII, which the server compares with case.
    Case,
    /// It compares a name, which Sieve can't tell from an address.
    Name,
    /// The server can't name one of its folders, or the folder is gone.
    Folder,
    /// One of its labels isn't ASCII, which IMAP keywords must be.
    Label,
    /// The server lacks a Sieve extension it needs.
    Extension(&'static str),
    /// One of its values isn't one the app reads.
    Value,
    /// A rule before it runs in the app.
    After,
}

/// The rules the server can run, in order, and where each runs.
#[derive(Debug, Default)]
pub(crate) struct Compiled {
    /// None when the server runs none of them.
    pub(crate) section: Option<Section>,
    /// The rules the server runs.
    pub(crate) on_server: Vec<i64>,
    /// The rules that run in the app alone, and why.
    pub(crate) here: Vec<(i64, Why)>,
}

/// The longest run of `rules`, from the first, that `engine` runs as the
/// app does.
pub(crate) fn compile(rules: &[Rule], engine: &Engine) -> Compiled {
    let mut compiled = Compiled::default();
    let mut needs = Needs::default();
    let mut blocks = String::new();
    let mut mirrored = Vec::new();
    for rule in rules {
        if !compiled.here.is_empty() {
            compiled.here.push((rule.id, Why::After));
            continue;
        }
        let mut wanted = Needs::default();
        match block(rule, engine, &mut wanted) {
            Ok(text) => {
                for extension in wanted.0 {
                    needs.push(extension);
                }
                blocks.push_str(&text);
                compiled.on_server.push(rule.id);
                mirrored.push(rule);
            }
            Err(why) => compiled.here.push((rule.id, why)),
        }
    }
    if !mirrored.is_empty() {
        compiled.section = Some(Section {
            text: format!("{RULES_START}\r\n{blocks}{RULES_END}\r\n"),
            needs: needs.0.into_iter().map(str::to_owned).collect(),
            digest: digest(&mirrored),
        });
    }
    compiled
}

/// Tells `rules` from any others, by what they do rather than by the Sieve
/// written for them.
pub(crate) fn digest(rules: &[&Rule]) -> String {
    let json = serde_json::to_string(rules).unwrap_or_default();
    Sha256::digest(format!("{FORMAT}\n{json}").as_bytes())[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The extensions a rule uses, in the order it first uses them.
#[derive(Default)]
struct Needs(Vec<&'static str>);

impl Needs {
    fn add(&mut self, engine: &Engine, extension: &'static str) -> Result<(), Why> {
        if !engine.has(extension) {
            return Err(Why::Extension(extension));
        }
        self.push(extension);
        Ok(())
    }

    fn push(&mut self, extension: &'static str) {
        if !self.0.contains(&extension) {
            self.0.push(extension);
        }
    }
}

/// The rule as an `if` with its name above it.
fn block(rule: &Rule, engine: &Engine, needs: &mut Needs) -> Result<String, Why> {
    let tests = rule
        .conditions
        .iter()
        .map(|condition| test(condition, engine, needs))
        .collect::<Result<Vec<_>, _>>()?;
    let test = match tests.as_slice() {
        [one] => one.clone(),
        _ => format!(
            "{} ({})",
            if rule.match_all { "allof" } else { "anyof" },
            tests.join(&format!(",\r\n{INDENT}"))
        ),
    };
    let commands = commands(rule, engine, needs)?
        .into_iter()
        .map(|command| format!("  {command};\r\n"))
        .collect::<String>();
    // A line break would end the comment early.
    let name = rule.name.replace(['\r', '\n'], " ");
    Ok(format!(
        "# Rule: {name}\r\nif {test} {{\r\n{commands}}}\r\n"
    ))
}

fn test(condition: &RuleCondition, engine: &Engine, needs: &mut Needs) -> Result<String, Why> {
    let value = condition.value.trim();
    let tested = match condition.field {
        RuleField::From | RuleField::To | RuleField::Cc | RuleField::ToOrCc => {
            let fields = match condition.field {
                RuleField::From => string("from"),
                RuleField::To => string("to"),
                RuleField::Cc => string("cc"),
                _ => list(&["to", "cc"]),
            };
            address(&fields, condition.op, value, engine, needs)?
        }
        RuleField::Subject => text(&string("subject"), condition.op, value, engine, needs)?,
        RuleField::Header if condition.op == RuleOp::Exists => {
            format!("exists {}", string(&condition.header))
        }
        RuleField::Header => text(
            &string(&condition.header),
            condition.op,
            value,
            engine,
            needs,
        )?,
        RuleField::MailingList => mailing_list(condition.op, value, engine, needs)?,
        RuleField::Size => size(condition.op, value)?,
        RuleField::Body => match condition.op {
            RuleOp::Contains => {
                needs.add(engine, "body")?;
                let comparator = comparator(value, engine, needs)?;
                format!("body{comparator} :text :contains {}", string(value))
            }
            _ => return Err(Why::Pattern),
        },
        RuleField::Attachments => return Err(Why::Attachments),
    };
    Ok(if condition.negate {
        format!("not {tested}")
    } else {
        tested
    })
}

/// A test of address fields. The app compares each address by its email
/// and by its name. Text found anywhere in the field is found in one of
/// them, unless it spans the marks that set them apart. A whole address,
/// its start or its end, is compared with the email alone, which only a
/// value with an @ is sure to mean.
fn address(
    fields: &str,
    op: RuleOp,
    value: &str,
    engine: &Engine,
    needs: &mut Needs,
) -> Result<String, Why> {
    let whole = matches!(op, RuleOp::Is | RuleOp::StartsWith | RuleOp::EndsWith);
    if !whole && op != RuleOp::Contains {
        return Err(Why::Pattern);
    }
    if value.contains(['<', '>', '"', ',', ';', ':', '(', ')'])
        || (whole && (!value.contains('@') || value.contains(char::is_whitespace)))
    {
        return Err(Why::Name);
    }
    let comparator = comparator(value, engine, needs)?;
    let (kind, key) = match op {
        RuleOp::Contains => {
            return Ok(format!(
                "header{comparator} :contains {fields} {}",
                string(value)
            ));
        }
        RuleOp::Is => (":is", value.to_owned()),
        RuleOp::StartsWith => (":matches", format!("{}*", literal(value))),
        _ => (":matches", format!("*{}", literal(value))),
    };
    Ok(format!(
        "address{comparator} :all {kind} {fields} {}",
        string(&key)
    ))
}

/// A test of a text header, such as the subject.
fn text(
    header: &str,
    op: RuleOp,
    value: &str,
    engine: &Engine,
    needs: &mut Needs,
) -> Result<String, Why> {
    let (kind, key) = match op {
        RuleOp::Contains => (":contains", value.to_owned()),
        RuleOp::Is => (":is", value.to_owned()),
        RuleOp::StartsWith => (":matches", format!("{}*", literal(value))),
        RuleOp::EndsWith => (":matches", format!("*{}", literal(value))),
        _ => return Err(Why::Pattern),
    };
    let comparator = comparator(value, engine, needs)?;
    Ok(format!(
        "header{comparator} {kind} {header} {}",
        string(&key)
    ))
}

/// A test of the List-Id, which the app reads as the id between its angle
/// brackets, or as the whole field when it has none.
fn mailing_list(
    op: RuleOp,
    value: &str,
    engine: &Engine,
    needs: &mut Needs,
) -> Result<String, Why> {
    let header = string("List-Id");
    if op == RuleOp::Exists {
        return Ok(format!("exists {header}"));
    }
    let escaped = literal(value);
    let (bracketed, kind, bare) = match op {
        RuleOp::Contains => (format!("*<*{escaped}*>*"), ":contains", value.to_owned()),
        RuleOp::Is => (format!("*<{escaped}>*"), ":is", value.to_owned()),
        RuleOp::StartsWith => (format!("*<{escaped}*>*"), ":matches", format!("{escaped}*")),
        RuleOp::EndsWith => (format!("*<*{escaped}>*"), ":matches", format!("*{escaped}")),
        _ => return Err(Why::Pattern),
    };
    let comparator = comparator(value, engine, needs)?;
    Ok(format!(
        "anyof (header{comparator} :matches {header} {}, allof (not header :contains {header} \
         \"<\", header{comparator} {kind} {header} {}))",
        string(&bracketed),
        string(&bare)
    ))
}

fn size(op: RuleOp, value: &str) -> Result<String, Why> {
    let kb = value
        .parse::<i64>()
        .ok()
        .filter(|kb| *kb >= 0)
        .ok_or(Why::Value)?
        .min(MOST_KB);
    match op {
        RuleOp::Over => Ok(format!("size :over {kb}K")),
        RuleOp::Under => Ok(format!("size :under {kb}K")),
        _ => Err(Why::Pattern),
    }
}

/// The comparator that ignores case as the app does. The default one is
/// enough for ASCII, and on Stalwart, which lowercases all text for it as
/// the app does. Other text needs Unicode's (RFC 5051).
fn comparator(value: &str, engine: &Engine, needs: &mut Needs) -> Result<&'static str, Why> {
    if value.is_ascii() || engine.named("stalwart") {
        return Ok("");
    }
    needs.add(engine, UNICODE_CASEMAP).map_err(|_| Why::Case)?;
    Ok(" :comparator \"i;unicode-casemap\"")
}

/// `value` for `:matches`, its wildcards taken as they are.
fn literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '*' | '?' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// What the rule does, in the app's order: marks and labels, then copies,
/// then filing away. Filing away ends the rules, as Stop does.
fn commands(rule: &Rule, engine: &Engine, needs: &mut Needs) -> Result<Vec<String>, Why> {
    let mut flags = Vec::new();
    let mut copies = Vec::new();
    let mut file = None;
    for action in &rule.actions {
        match action {
            Action::MarkRead => flags.push("\\Seen".to_owned()),
            Action::Star => flags.push("\\Flagged".to_owned()),
            Action::Label { keyword } => {
                // RFC 5232 section 2 has interpreters ignore other flags.
                if keyword.is_empty()
                    || !keyword
                        .bytes()
                        .all(|byte| byte.is_ascii_graphic() && byte != b'\\')
                {
                    return Err(Why::Label);
                }
                flags.push(keyword.clone());
            }
            Action::Copy { folder } => copies.push(mailbox(folder, engine, needs)?),
            Action::Move { folder } => file = Some(mailbox(folder, engine, needs)?),
        }
    }
    let mut commands = Vec::new();
    if !flags.is_empty() {
        needs.add(engine, "imap4flags")?;
        let flags = match flags.as_slice() {
            [one] => string(one),
            _ => list(&flags),
        };
        commands.push(format!("addflag {flags}"));
    }
    for copy in copies {
        needs.add(engine, "fileinto")?;
        needs.add(engine, "copy")?;
        commands.push(format!("fileinto :copy {copy}"));
    }
    if let Some(file) = &file {
        needs.add(engine, "fileinto")?;
        commands.push(format!("fileinto {file}"));
    }
    if rule.stop || file.is_some() {
        commands.push("stop".to_owned());
    }
    Ok(commands)
}

/// `folder` as `fileinto` names it. A JMAP folder goes by its id, falling
/// back to the Inbox, where the app leaves mail when the folder is gone
/// (RFC 9042 section 4). An IMAP folder goes by its name in UTF-8 (RFC 5228
/// section 4.1). Cyrus reads names as modified UTF-7 unless set up
/// otherwise, so there only names that read the same both ways go.
fn mailbox(folder: &Folder, engine: &Engine, needs: &mut Needs) -> Result<String, Why> {
    if let Some(id) = &folder.jmap_id {
        needs.add(engine, "mailboxid").map_err(|_| Why::Folder)?;
        return Ok(format!(":mailboxid {} \"INBOX\"", string(id)));
    }
    let name = crate::imap::decode_mailbox_name(&folder.path);
    if name.is_empty() || (engine.named("cyrus") && name != folder.path) {
        return Err(Why::Folder);
    }
    Ok(string(&name))
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
