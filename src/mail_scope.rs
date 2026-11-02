//! Typed mail navigation scopes.
//!
//! Slint models, saved tabs and the warm-start cache carry a scope as an
//! opaque string key. Rust parses that key once at the boundary and otherwise
//! works with [`Scope`], so a selection never depends on display names and
//! every consumer handles every kind of mailbox exhaustively.

use flectar_mail_core::models::Account;
use std::{fmt, str::FromStr};

/// Mailboxes every account presents, in sidebar order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StandardMailbox {
    Inbox,
    Starred,
    Sent,
    Archive,
    Spam,
    Trash,
    Drafts,
}

impl StandardMailbox {
    pub(crate) const ALL: [Self; 7] = [
        Self::Inbox,
        Self::Starred,
        Self::Sent,
        Self::Archive,
        Self::Spam,
        Self::Trash,
        Self::Drafts,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Starred => "Starred",
            Self::Sent => "Sent",
            Self::Archive => "Archive",
            Self::Spam => "Spam",
            Self::Trash => "Trash",
            Self::Drafts => "Drafts",
        }
    }

    pub(crate) fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mailbox| mailbox.label() == label)
    }
}

/// Which sidebar section a label is listed in. The id alone selects the
/// label; the kind keeps the key identical to the row that was clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LabelKind {
    /// Automatic categories shared by every account.
    Category,
    /// Manual labels shared by every account.
    Global,
    /// Labels owned by one account.
    Account,
}

impl LabelKind {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Category => "Category",
            Self::Global => "GlobalLabel",
            Self::Account => "AccountLabel",
        }
    }

    /// The section a label with these properties belongs to.
    pub(crate) const fn of(is_auto: bool, owner_account_id: Option<i64>) -> Self {
        if is_auto {
            Self::Category
        } else if owner_account_id.is_some() {
            Self::Account
        } else {
            Self::Global
        }
    }
}

/// A selectable mail view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Scope {
    /// One standard mailbox across every account.
    Unified(StandardMailbox),
    /// Gmail-style split inbox, primary half.
    Important,
    /// Gmail-style split inbox, remaining half.
    Other,
    Label { kind: LabelKind, id: i64 },
    /// A provider folder outside the standard mailboxes.
    Folder(i64),
    /// One standard mailbox of one account, whether or not the provider
    /// reported a matching role folder.
    Account {
        account_id: i64,
        mailbox: StandardMailbox,
    },
}

impl Default for Scope {
    fn default() -> Self {
        Self::UNIFIED_INBOX
    }
}

impl Scope {
    pub(crate) const UNIFIED_INBOX: Self = Self::Unified(StandardMailbox::Inbox);

    pub(crate) const fn label_id(self) -> Option<i64> {
        match self {
            Self::Label { id, .. } => Some(id),
            _ => None,
        }
    }

    /// The same place across every shown account: an account's standard
    /// mailbox becomes the unified one, and its folders and labels the
    /// unified inbox.
    #[must_use]
    pub(crate) const fn unified(self) -> Self {
        match self {
            Self::Account { mailbox, .. } => Self::Unified(mailbox),
            Self::Folder(_)
            | Self::Label {
                kind: LabelKind::Account,
                ..
            } => Self::UNIFIED_INBOX,
            Self::Unified(_) | Self::Important | Self::Other | Self::Label { .. } => self,
        }
    }

    /// The scope the sidebar can show for these accounts. Unified shortcuts
    /// are absent with exactly one account, so they resolve to that account's
    /// own mailbox; the message list is identical. Other scopes and the
    /// zero/multiple-account cases keep their meaning.
    #[must_use]
    pub(crate) fn visible_for(self, accounts: &[Account]) -> Self {
        match (self, accounts) {
            (Self::Unified(mailbox), [account]) => Self::Account {
                account_id: account.id,
                mailbox,
            },
            _ => self,
        }
    }
}

const UNIFIED_PREFIX: &str = "Unified ";
const FOLDER_PREFIX: &str = "Folder:";
const ACCOUNT_PREFIX: &str = "Account:";

impl fmt::Display for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unified(mailbox) => write!(formatter, "{UNIFIED_PREFIX}{}", mailbox.label()),
            Self::Important => formatter.write_str("Important"),
            Self::Other => formatter.write_str("Other"),
            Self::Label { kind, id } => write!(formatter, "{}:{id}", kind.prefix()),
            Self::Folder(id) => write!(formatter, "{FOLDER_PREFIX}{id}"),
            Self::Account {
                account_id,
                mailbox,
            } => write!(formatter, "{ACCOUNT_PREFIX}{account_id}:{}", mailbox.label()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParseScopeError(String);

impl fmt::Display for ParseScopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown mail scope {:?}", self.0)
    }
}

impl std::error::Error for ParseScopeError {}

impl FromStr for Scope {
    type Err = ParseScopeError;

    fn from_str(key: &str) -> Result<Self, Self::Err> {
        let error = || ParseScopeError(key.to_owned());
        let id = |value: &str| value.parse::<i64>().map_err(|_| error());
        if let Some(label) = key.strip_prefix(UNIFIED_PREFIX) {
            return StandardMailbox::from_label(label)
                .map(Self::Unified)
                .ok_or_else(error);
        }
        if let Some(folder_id) = key.strip_prefix(FOLDER_PREFIX) {
            return id(folder_id).map(Self::Folder);
        }
        if let Some(rest) = key.strip_prefix(ACCOUNT_PREFIX) {
            let (account_id, label) = rest.split_once(':').ok_or_else(error)?;
            return Ok(Self::Account {
                account_id: id(account_id)?,
                mailbox: StandardMailbox::from_label(label).ok_or_else(error)?,
            });
        }
        if let Some((prefix, label_id)) = key.split_once(':')
            && let Some(kind) = [LabelKind::Category, LabelKind::Global, LabelKind::Account]
                .into_iter()
                .find(|kind| kind.prefix() == prefix)
        {
            return Ok(Self::Label {
                kind,
                id: id(label_id)?,
            });
        }
        match key {
            "Important" => Ok(Self::Important),
            "Other" => Ok(Self::Other),
            _ => Err(error()),
        }
    }
}

impl From<Scope> for String {
    fn from(scope: Scope) -> Self {
        scope.to_string()
    }
}

impl From<Scope> for slint::SharedString {
    fn from(scope: Scope) -> Self {
        scope.to_string().into()
    }
}

impl TryFrom<String> for Scope {
    type Error = ParseScopeError;

    fn try_from(key: String) -> Result<Self, Self::Error> {
        key.parse()
    }
}

impl serde::Serialize for Scope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Scope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let key = String::deserialize(deserializer)?;
        key.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flectar_mail_core::models::{AuthKind, MailProtocol, Provider};

    fn account(id: i64) -> Account {
        Account {
            id,
            email: format!("{id}@example.test"),
            display_name: Some("Work / Unified Folder:1".into()),
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            sync_state: "idle".into(),
            sync_error: None,
            can_create_top_level_mailbox: true,
            server_keeps_mail_days: None,
        }
    }

    fn every_scope() -> Vec<Scope> {
        let mut scopes = vec![Scope::Important, Scope::Other, Scope::Folder(-3)];
        for mailbox in StandardMailbox::ALL {
            scopes.push(Scope::Unified(mailbox));
            scopes.push(Scope::Account {
                account_id: 42,
                mailbox,
            });
        }
        for kind in [LabelKind::Category, LabelKind::Global, LabelKind::Account] {
            scopes.push(Scope::Label { kind, id: 7 });
        }
        scopes
    }

    #[test]
    fn keys_round_trip_and_stay_compatible_with_the_slint_sidebar() {
        for scope in every_scope() {
            assert_eq!(scope.to_string().parse::<Scope>(), Ok(scope));
            let json = serde_json::to_string(&scope).unwrap();
            assert_eq!(serde_json::from_str::<Scope>(&json).unwrap(), scope);
        }
        // ui/components/sidebar.slint builds and compares these keys itself.
        assert_eq!(Scope::UNIFIED_INBOX.to_string(), "Unified Inbox");
        assert_eq!(
            Scope::Unified(StandardMailbox::Trash).to_string(),
            "Unified Trash"
        );
        assert_eq!(
            Scope::Label {
                kind: LabelKind::Category,
                id: 3
            }
            .to_string(),
            "Category:3"
        );
        assert_eq!(
            Scope::Label {
                kind: LabelKind::Global,
                id: 3
            }
            .to_string(),
            "GlobalLabel:3"
        );
        assert_eq!(
            Scope::Label {
                kind: LabelKind::Account,
                id: 3
            }
            .to_string(),
            "AccountLabel:3"
        );
    }

    #[test]
    fn malformed_and_display_name_keys_are_rejected() {
        for key in [
            "",
            "Unified",
            "Unified Unknown",
            "Folder:",
            "Folder:x",
            "Account:1",
            "Account:x:Inbox",
            "Account:1:Custom",
            "Label:4",
            "Category:",
            "Person / Inbox",
            "Person",
            "inbox",
        ] {
            assert!(key.parse::<Scope>().is_err(), "{key:?}");
        }
        assert!(serde_json::from_str::<Scope>("\"Person / Inbox\"").is_err());
    }

    #[test]
    fn an_accounts_places_unify_to_the_same_mailbox_or_the_inbox() {
        for scope in every_scope() {
            let expected = match scope {
                Scope::Account { mailbox, .. } => Scope::Unified(mailbox),
                Scope::Folder(_)
                | Scope::Label {
                    kind: LabelKind::Account,
                    ..
                } => Scope::UNIFIED_INBOX,
                other => other,
            };
            assert_eq!(scope.unified(), expected, "{scope}");
        }
    }

    #[test]
    fn unified_scopes_resolve_to_the_only_account() {
        for scope in every_scope() {
            let expected = match scope {
                Scope::Unified(mailbox) => Scope::Account {
                    account_id: 9,
                    mailbox,
                },
                other => other,
            };
            assert_eq!(scope.visible_for(&[account(9)]), expected);
            assert_eq!(scope.visible_for(&[]), scope);
            assert_eq!(scope.visible_for(&[account(9), account(10)]), scope);
            assert_eq!(expected.visible_for(&[account(9)]), expected);
        }
    }
}
