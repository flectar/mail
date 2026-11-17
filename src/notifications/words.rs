//! The words of new-mail notifications without the window, as a check in the
//! background on Android shows them. build.rs reads the catalogs Slint
//! bundles, the way Slint reads them; this chooses and fills them in as its
//! `@tr` does, so the words come out the same (see the tests).

use super::FixedWords;

struct Catalog {
    /// The catalog's directory under lang/, as Slint selects it.
    language: &'static str,
    /// The gettext expression giving the plural form of `n`.
    plural: &'static str,
    /// SINGLES in the catalog's language, `None` where untranslated.
    singles: &'static [Option<&'static str>],
    /// PLURALS' forms in the catalog's language.
    plurals: &'static [Option<&'static [&'static str]>],
}

include!(concat!(env!("OUT_DIR"), "/notification_words.rs"));

/// The phrases app.slint's I18n uses, in build.rs's order.
const SINGLES: [&str; 8] = [
    "New message",
    "(No subject)",
    "Open",
    "Mark as read",
    "Archive",
    "Delete",
    "New mail",
    "New mail without sound",
];
const PLURALS: [[&str; 2]; 3] = [
    ["{n} new message", "{n} new messages"],
    ["{0} and {n} more", "{0} and {n} more"],
    ["{n} unread message", "{n} unread messages"],
];
const NEW_MESSAGES: usize = 0;
const MORE_SENDERS: usize = 1;
const UNREAD_COUNT: usize = 2;

/// One language's notification words.
pub(crate) struct Speech {
    catalog: Option<&'static Catalog>,
    rule: Option<Rule>,
}

impl Speech {
    /// The words in `language`, a name as Slint's `select_bundled_translation`
    /// takes it; the source phrases for one without a catalog, such as "en".
    pub(crate) fn new(language: &str) -> Self {
        let catalog = CATALOGS.iter().find(|catalog| catalog.language == language);
        Self {
            catalog,
            rule: catalog.and_then(|catalog| Rule::parse(catalog.plural)),
        }
    }

    fn single(&self, index: usize) -> String {
        self.catalog
            .and_then(|catalog| catalog.singles[index])
            .unwrap_or(SINGLES[index])
            .to_owned()
    }

    /// Slint's `translate_from_bundle_with_plural`: the catalog's forms by
    /// its rule, else the source forms by English's.
    fn plural(&self, index: usize, n: i32, first: Option<&str>) -> String {
        let english = usize::from(n != 1);
        let (forms, form) = match self.catalog.and_then(|catalog| catalog.plurals[index]) {
            Some(forms) => (
                forms,
                self.rule.as_ref().map_or(english, |rule| rule.form(n)),
            ),
            None => (&PLURALS[index][..], english),
        };
        let chosen = forms
            .get(form)
            .or(forms.first())
            .copied()
            .unwrap_or_default();
        format(chosen, first, n)
    }

    /// The words that don't depend on the mail.
    pub(crate) fn fixed(&self) -> FixedWords {
        FixedWords {
            new_message: self.single(0),
            no_subject: self.single(1),
            open: self.single(2),
            mark_read: self.single(3),
            archive: self.single(4),
            delete: self.single(5),
            new_mail_channel: self.single(6),
            quiet_mail_channel: self.single(7),
        }
    }

    /// "3 new messages", the title of a summary.
    pub(crate) fn new_messages(&self, count: i32) -> String {
        self.plural(NEW_MESSAGES, count, None)
    }

    /// "Ana, Bo and 2 more", the senders of a summary.
    pub(crate) fn more_senders(&self, senders: &str, count: i32) -> String {
        self.plural(MORE_SENDERS, count, Some(senders))
    }

    /// "3 unread messages", what a screen reader says for the app icon.
    pub(crate) fn unread_count(&self, count: i32) -> String {
        self.plural(UNREAD_COUNT, count, None)
    }
}

/// Slint's `@tr` formatting: `{}` takes the next argument, `{0}` the first,
/// `{n}` the count, and doubled braces stand for one. There is at most one
/// argument here, `first`; a reference to a missing one stays as written.
fn format(text: &str, first: Option<&str>, n: i32) -> String {
    let argument = |key: &str, next: &mut usize| -> Option<String> {
        let index = if key.is_empty() {
            *next += 1;
            Some(*next - 1)
        } else {
            key.parse::<usize>().ok()
        };
        match index {
            Some(0) => first.map(str::to_owned),
            Some(_) => None,
            None => (key == "n").then(|| n.to_string()),
        }
    };
    let mut out = String::with_capacity(text.len() + 8);
    let (mut next, mut position) = (0, 0);
    while let Some(found) = text[position..].find(['{', '}']) {
        let brace = position + found;
        if text.len() - position < found + 1 {
            break;
        }
        if &text[brace..=brace] == "}" {
            out.push_str(&text[position..=brace]);
            position = if text.get(brace + 1..=brace + 1) == Some("}") {
                brace + 2
            } else {
                brace + 1
            };
            continue;
        }
        if text.get(brace + 1..=brace + 1) == Some("{") {
            out.push_str(&text[position..=brace]);
            position = brace + 2;
            continue;
        }
        let Some(end) = text[brace..].find('}').map(|end| end + brace) else {
            out.push_str(&text[position..=brace]);
            position = brace + 1;
            continue;
        };
        out.push_str(&text[position..brace]);
        match argument(text[brace + 1..end].trim(), &mut next) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&text[brace..=end]),
        }
        position = end + 1;
    }
    out.push_str(&text[position..]);
    out
}

/// A gettext plural expression, such as
/// `n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 ? 1 : 2`.
#[derive(Debug)]
enum Rule {
    N,
    Number(i64),
    Not(Box<Rule>),
    Binary(Box<Rule>, Op, Box<Rule>),
    Choose(Box<Rule>, Box<Rule>, Box<Rule>),
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Or,
    And,
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Remainder,
}

impl Rule {
    fn parse(text: &str) -> Option<Self> {
        let mut parser = Parser(text.as_bytes(), 0);
        let rule = parser.choose()?;
        parser.blank();
        (parser.1 == parser.0.len()).then_some(rule)
    }

    /// The plural form of `n`.
    fn form(&self, n: i32) -> usize {
        usize::try_from(self.value(i64::from(n))).unwrap_or(0)
    }

    fn value(&self, n: i64) -> i64 {
        match self {
            Self::N => n,
            Self::Number(number) => *number,
            Self::Not(rule) => i64::from(rule.value(n) == 0),
            Self::Choose(condition, yes, no) => {
                if condition.value(n) != 0 {
                    yes.value(n)
                } else {
                    no.value(n)
                }
            }
            Self::Binary(left, op, right) => {
                let (left, right) = (left.value(n), right.value(n));
                i64::from(match op {
                    Op::Or => left != 0 || right != 0,
                    Op::And => left != 0 && right != 0,
                    Op::Equal => left == right,
                    Op::NotEqual => left != right,
                    Op::Less => left < right,
                    Op::LessOrEqual => left <= right,
                    Op::Greater => left > right,
                    Op::GreaterOrEqual => left >= right,
                    Op::Remainder => return left.checked_rem(right).unwrap_or(0),
                })
            }
        }
    }
}

/// C's precedence: `?:`, `||`, `&&`, comparisons, `%`, `!`.
struct Parser<'a>(&'a [u8], usize);

impl Parser<'_> {
    fn blank(&mut self) {
        while self.0.get(self.1).is_some_and(u8::is_ascii_whitespace) {
            self.1 += 1;
        }
    }

    fn eat(&mut self, token: &str) -> bool {
        self.blank();
        let found = self.0[self.1..].starts_with(token.as_bytes());
        if found {
            self.1 += token.len();
        }
        found
    }

    fn choose(&mut self) -> Option<Rule> {
        let condition = self.or()?;
        if !self.eat("?") {
            return Some(condition);
        }
        let yes = self.choose()?;
        self.eat(":").then_some(())?;
        let no = self.choose()?;
        Some(Rule::Choose(condition.into(), yes.into(), no.into()))
    }

    fn or(&mut self) -> Option<Rule> {
        let mut rule = self.and()?;
        while self.eat("||") {
            rule = Rule::Binary(rule.into(), Op::Or, self.and()?.into());
        }
        Some(rule)
    }

    fn and(&mut self) -> Option<Rule> {
        let mut rule = self.compare()?;
        while self.eat("&&") {
            rule = Rule::Binary(rule.into(), Op::And, self.compare()?.into());
        }
        Some(rule)
    }

    fn compare(&mut self) -> Option<Rule> {
        let mut rule = self.remainder()?;
        loop {
            let op = if self.eat("==") {
                Op::Equal
            } else if self.eat("!=") {
                Op::NotEqual
            } else if self.eat("<=") {
                Op::LessOrEqual
            } else if self.eat(">=") {
                Op::GreaterOrEqual
            } else if self.eat("<") {
                Op::Less
            } else if self.eat(">") {
                Op::Greater
            } else {
                return Some(rule);
            };
            rule = Rule::Binary(rule.into(), op, self.remainder()?.into());
        }
    }

    fn remainder(&mut self) -> Option<Rule> {
        let mut rule = self.unary()?;
        while self.eat("%") {
            rule = Rule::Binary(rule.into(), Op::Remainder, self.unary()?.into());
        }
        Some(rule)
    }

    fn unary(&mut self) -> Option<Rule> {
        if self.eat("!") && !self.0[self.1..].starts_with(b"=") {
            return Some(Rule::Not(self.unary()?.into()));
        }
        if self.eat("(") {
            let rule = self.choose()?;
            return self.eat(")").then_some(rule);
        }
        if self.eat("n") {
            return Some(Rule::N);
        }
        self.blank();
        let start = self.1;
        while self.0.get(self.1).is_some_and(u8::is_ascii_digit) {
            self.1 += 1;
        }
        std::str::from_utf8(&self.0[start..self.1])
            .ok()?
            .parse()
            .ok()
            .map(Rule::Number)
    }
}

#[cfg(test)]
mod tests {
    use super::{CATALOGS, Rule, Speech, format};
    use crate::{AppWindow, I18n};
    use slint::ComponentHandle;
    use std::rc::Rc;

    fn headless_app() -> AppWindow {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        struct Headless(Rc<MinimalSoftwareWindow>);
        impl slint::platform::Platform for Headless {
            fn create_window_adapter(
                &self,
            ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        AppWindow::new().unwrap()
    }

    /// Every bundled catalog, and the source phrases, say the same as the
    /// window does, for every count a notification can show.
    #[test]
    fn the_words_match_the_window_in_every_language() {
        let app = headless_app();
        let i18n = app.global::<I18n>();
        let languages =
            std::iter::once("en").chain(CATALOGS.iter().map(|catalog| catalog.language));
        for language in languages {
            slint::select_bundled_translation(language).unwrap();
            let speech = Speech::new(language);
            let fixed = speech.fixed();
            assert_eq!(
                fixed.new_message,
                i18n.invoke_notification_new_message().as_str(),
                "{language}"
            );
            assert_eq!(
                fixed.no_subject,
                i18n.invoke_notification_no_subject().as_str(),
                "{language}"
            );
            assert_eq!(
                fixed.open,
                i18n.invoke_notification_open().as_str(),
                "{language}"
            );
            assert_eq!(
                fixed.mark_read,
                i18n.invoke_notification_mark_read().as_str(),
                "{language}"
            );
            assert_eq!(
                fixed.archive,
                i18n.invoke_notification_archive().as_str(),
                "{language}"
            );
            assert_eq!(
                fixed.delete,
                i18n.invoke_notification_delete().as_str(),
                "{language}"
            );
            assert_eq!(
                fixed.new_mail_channel,
                i18n.invoke_notification_new_mail_channel().as_str(),
                "{language}"
            );
            assert_eq!(
                fixed.quiet_mail_channel,
                i18n.invoke_notification_quiet_mail_channel().as_str(),
                "{language}"
            );
            for count in (0..=250).chain([999, 1000, 1001, 12_345]) {
                assert_eq!(
                    speech.new_messages(count),
                    i18n.invoke_notification_new_messages(count).as_str(),
                    "{language} {count}"
                );
                assert_eq!(
                    speech.more_senders("Ana, Bo", count),
                    i18n.invoke_notification_more_senders("Ana, Bo".into(), count)
                        .as_str(),
                    "{language} {count}"
                );
                assert_eq!(
                    speech.unread_count(count),
                    i18n.invoke_notification_unread_count(count).as_str(),
                    "{language} {count}"
                );
            }
        }
    }

    #[test]
    fn every_plural_rule_parses() {
        for catalog in CATALOGS {
            assert!(
                Rule::parse(catalog.plural).is_some(),
                "{}",
                catalog.language
            );
        }
        let russian = Rule::parse(
            "n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2",
        )
        .unwrap();
        let forms = [1, 2, 5, 11, 12, 21, 22, 25, 111, 112].map(|n| russian.form(n));
        assert_eq!(forms, [0, 1, 2, 2, 2, 0, 1, 2, 2, 2]);
        assert!(Rule::parse("n != 1 ?").is_none());
        assert!(Rule::parse("(n").is_none());
    }

    #[test]
    fn formatting_follows_slint() {
        assert_eq!(format("{n} new", None, 3), "3 new");
        assert_eq!(format("{0} and {n} more", Some("Ana"), 2), "Ana and 2 more");
        assert_eq!(format("{} and {n}", Some("Ana"), 2), "Ana and 2");
        assert_eq!(format("{{0}} {}", Some("Ana"), 1), "{0} Ana");
        assert_eq!(format("{1} {x}", Some("Ana"), 1), "{1} {x}");
        assert_eq!(format("{0} new", None, 1), "{0} new");
    }
}
