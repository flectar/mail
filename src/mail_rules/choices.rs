//! What the rule editor offers (#238): the fields, comparisons and actions
//! in its order, and the folders and labels a rule's account can use.

use crate::MailboxRole;
use crate::mail_scope::label_role;
use flectar_mail_core::models::{
    Account, FolderInfo, Label, MailProtocol, MailRule, RuleAction, RuleField, RuleHereWhy, RuleOp,
    ServerRules, ServerRulesState,
};

/// What a category the rule editor offers is called.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum TabName {
    /// By the name the window gives the role in its language: Important,
    /// Other, and the automatic categories that still have the names the app
    /// gave them.
    Role(MailboxRole),
    /// By the name its label was given.
    Text(String),
}

/// The fields the editor lists, in its order. Those that need the whole
/// message come last.
pub(super) const FIELDS: [RuleField; 10] = [
    RuleField::From,
    RuleField::To,
    RuleField::Cc,
    RuleField::ToOrCc,
    RuleField::Subject,
    RuleField::MailingList,
    RuleField::Size,
    RuleField::Header,
    RuleField::Body,
    RuleField::Attachments,
];

/// How text compares, in the editor's order: the comparison and whether it
/// is turned around, as "doesn't contain".
pub(super) const TEXT_OPS: [(RuleOp, bool); 8] = [
    (RuleOp::Contains, false),
    (RuleOp::Contains, true),
    (RuleOp::Is, false),
    (RuleOp::Is, true),
    (RuleOp::StartsWith, false),
    (RuleOp::EndsWith, false),
    (RuleOp::Matches, false),
    (RuleOp::Matches, true),
];

/// How `field` compares, in the editor's order.
pub(super) fn ops(field: RuleField) -> Vec<(RuleOp, bool)> {
    match field {
        RuleField::Size => vec![(RuleOp::Over, false), (RuleOp::Under, false)],
        RuleField::MailingList | RuleField::Header => {
            [(RuleOp::Exists, false), (RuleOp::Exists, true)]
                .into_iter()
                .chain(TEXT_OPS)
                .collect()
        }
        RuleField::Body => vec![
            (RuleOp::Contains, false),
            (RuleOp::Contains, true),
            (RuleOp::Matches, false),
            (RuleOp::Matches, true),
        ],
        RuleField::Attachments => vec![(RuleOp::Exists, false), (RuleOp::Exists, true)],
        _ => TEXT_OPS.to_vec(),
    }
}

/// The actions the editor lists, in its order. Copying is last: only an
/// account that keeps a message in several folders offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Move,
    Label,
    MarkRead,
    Star,
    Archive,
    Trash,
    Junk,
    ShowIn,
    Copy,
}

pub(super) const KINDS: [Kind; 9] = [
    Kind::Move,
    Kind::Label,
    Kind::MarkRead,
    Kind::Star,
    Kind::Archive,
    Kind::Trash,
    Kind::Junk,
    Kind::ShowIn,
    Kind::Copy,
];

pub(super) fn kind_of(action: &RuleAction) -> Kind {
    match action {
        RuleAction::Move { .. } => Kind::Move,
        RuleAction::Copy { .. } => Kind::Copy,
        RuleAction::Label { .. } => Kind::Label,
        RuleAction::MarkRead => Kind::MarkRead,
        RuleAction::Star => Kind::Star,
        RuleAction::Archive => Kind::Archive,
        RuleAction::Trash => Kind::Trash,
        RuleAction::Junk => Kind::Junk,
        RuleAction::ShowIn { .. } => Kind::ShowIn,
    }
}

pub(super) fn index_of<T: PartialEq>(items: &[T], item: &T) -> i32 {
    items
        .iter()
        .position(|candidate| candidate == item)
        .map_or(0, |index| i32::try_from(index).unwrap_or(0))
}

/// What the editor offers to choose from.
#[derive(Default)]
pub(super) struct Choices {
    /// The accounts that receive mail, after "All accounts".
    pub(super) accounts: Vec<Account>,
    pub(super) folders: Vec<FolderInfo>,
    pub(super) labels: Vec<Label>,
}

impl Choices {
    /// The account the editor's account choice `index` names.
    pub(super) fn account(&self, index: i32) -> Option<&Account> {
        usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| self.accounts.get(index))
    }

    /// The folders a rule for `account` moves mail into, by their place:
    /// those of the account's own, as "Projects / Clients".
    pub(super) fn folders(&self, account: Option<&Account>) -> Vec<(i64, String)> {
        let Some(account) = account else {
            return Vec::new();
        };
        let mut folders = self
            .folders
            .iter()
            .filter(|folder| {
                folder.account_id == account.id && folder.selectable && folder.role.is_none()
            })
            .map(|folder| (folder.id, self.place(folder)))
            .collect::<Vec<_>>();
        folders.sort_by_key(|(_, place)| place.to_lowercase());
        folders
    }

    fn place(&self, folder: &FolderInfo) -> String {
        let mut names = vec![folder.display_name.as_str()];
        let mut parent = folder.parent_id;
        // Broken provider data can loop; no real chain is longer.
        for _ in 0..self.folders.len() {
            let Some(found) = parent.and_then(|id| self.folders.iter().find(|f| f.id == id)) else {
                break;
            };
            names.push(found.display_name.as_str());
            parent = found.parent_id;
        }
        names.reverse();
        names.join(" / ")
    }

    /// The Inbox's categories a rule can show mail in, by key, with their
    /// names: Important and Other come first, and the app names them, as it
    /// names the automatic categories that keep their built-in names.
    pub(super) fn tabs(&self) -> Vec<(String, TabName)> {
        let mut tabs = vec![
            (
                "important".to_owned(),
                TabName::Role(MailboxRole::Important),
            ),
            ("other".to_owned(), TabName::Role(MailboxRole::Other)),
        ];
        tabs.extend(
            self.labels
                .iter()
                .filter(|label| label.is_auto)
                .map(|label| {
                    let name = match label_role(label) {
                        MailboxRole::None => TabName::Text(label.name.clone()),
                        role => TabName::Role(role),
                    };
                    (format!("label:{}", label.id), name)
                }),
        );
        tabs
    }

    /// The labels a rule for `account` can add: the account's own and those
    /// of this device, or only this device's for every account.
    pub(super) fn labels(&self, account: Option<&Account>) -> Vec<(i64, String)> {
        self.labels
            .iter()
            .filter(|label| {
                !label.is_auto
                    && label
                        .owner_account_id
                        .is_none_or(|owner| account.is_some_and(|account| account.id == owner))
            })
            .map(|label| (label.id, label.name.clone()))
            .collect()
    }
}

/// The days Trash and Junk keep mail, as the cleanup choices list them. 0
/// keeps it for ever.
pub(super) const CLEANUP_DAYS: [u32; 6] = [0, 7, 14, 30, 60, 90];

/// The cleanup choice for `days`: its own, or the longest shorter one for a
/// count the choices don't list, such as one a backup brought.
pub(super) fn cleanup_choice(days: u32) -> i32 {
    let index = CLEANUP_DAYS
        .iter()
        .rposition(|choice| *choice <= days)
        .unwrap_or(0);
    i32::try_from(index).unwrap_or_default()
}

/// Whether the folders and labels `rule` files into are still there.
pub(super) fn targets_exist(rule: &MailRule, folders: &[FolderInfo], labels: &[Label]) -> bool {
    rule.actions.iter().all(|action| match action {
        RuleAction::Move { folder } | RuleAction::Copy { folder } => {
            folders.iter().any(|candidate| candidate.id == *folder)
        }
        RuleAction::Label { label } => labels.iter().any(|candidate| candidate.id == *label),
        RuleAction::ShowIn { tab } => tab.strip_prefix("label:").is_none_or(|id| {
            labels
                .iter()
                .any(|candidate| candidate.is_auto && id.parse() == Ok(candidate.id))
        }),
        _ => true,
    })
}

/// Where `rule` runs, as the list and the editor tell: "server" when the
/// servers of all its accounts run it, "some" for some of them, or "app"
/// while Flectar Mail is open, with why. Empty when no server of its
/// accounts runs rules, or the rule is off.
pub(super) fn place(
    rule: &MailRule,
    accounts: &[Account],
    servers: &[ServerRules],
) -> (&'static str, &'static str) {
    if !rule.enabled {
        return ("", "");
    }
    let ids = match rule.account_id {
        Some(id) => vec![id],
        None => accounts
            .iter()
            .filter(|account| account.mail_protocol != MailProtocol::Local)
            .map(|account| account.id)
            .collect(),
    };
    let checked = ids
        .iter()
        .filter_map(|id| servers.iter().find(|server| server.account_id == *id))
        .filter(|server| server.state != ServerRulesState::Unavailable)
        .collect::<Vec<_>>();
    if checked.is_empty() {
        return ("", "");
    }
    let on = checked
        .iter()
        .filter(|server| server.on_server.contains(&rule.id))
        .count();
    if on == ids.len() {
        return ("server", "");
    }
    if on > 0 {
        return ("some", "");
    }
    let why = checked
        .iter()
        .find_map(|server| server.here.iter().find(|here| here.rule_id == rule.id))
        .map_or("", |here| match here.why {
            RuleHereWhy::Pattern => "pattern",
            RuleHereWhy::Attachments => "attachments",
            RuleHereWhy::Case => "case",
            RuleHereWhy::Name => "name",
            RuleHereWhy::Folder => "folder",
            RuleHereWhy::Label => "label",
            RuleHereWhy::Unsupported => "unsupported",
            RuleHereWhy::After => "after",
        });
    ("app", why)
}

/// What an account's server needs before it runs the rules, as its notice
/// tells.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Notice {
    pub(super) account_id: i64,
    pub(super) account: String,
    /// As `RulesNotice.kind` says.
    pub(super) kind: &'static str,
    pub(super) detail: String,
    pub(super) has_rules: bool,
}

/// The notices for the servers that need the user before they run the
/// rules, in the order of `accounts`.
pub(super) fn notices(
    accounts: &[Account],
    servers: &[ServerRules],
    rules: &[MailRule],
) -> Vec<Notice> {
    accounts
        .iter()
        .filter_map(|account| {
            let server = servers
                .iter()
                .find(|server| server.account_id == account.id)?;
            let (kind, detail) = match &server.state {
                ServerRulesState::RunBefore(theirs) => ("run-before", theirs.clone()),
                ServerRulesState::Others => ("others", String::new()),
                ServerRulesState::Edited => ("edited", String::new()),
                // As the notice's second sentence.
                ServerRulesState::Failed(why) if why.ends_with(['.', '!', '?']) => {
                    ("failed", why.clone())
                }
                ServerRulesState::Failed(why) => ("failed", format!("{why}.")),
                ServerRulesState::Runs | ServerRulesState::Unavailable => return None,
            };
            Some(Notice {
                account_id: account.id,
                account: account.email.clone(),
                kind,
                detail,
                has_rules: rules
                    .iter()
                    .any(|rule| rule.enabled && rule.account_id.is_none_or(|id| id == account.id)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flectar_mail_core::models::RuleHere;

    fn account(id: i64, mail_protocol: MailProtocol) -> Account {
        Account {
            id,
            email: format!("{id}@test.dev"),
            display_name: None,
            name: None,
            avatar_url: None,
            provider: flectar_mail_core::models::Provider::Generic,
            auth_kind: flectar_mail_core::models::AuthKind::Password,
            mail_protocol,
            sync_state: String::new(),
            sync_error: None,
            sync_retry_at: None,
            can_create_top_level_mailbox: true,
            server_keeps_mail_days: None,
            caught_up_at: None,
        }
    }

    fn mail_rule(id: i64, account_id: Option<i64>) -> MailRule {
        MailRule {
            id,
            account_id,
            name: format!("Rule {id}"),
            enabled: true,
            match_all: true,
            conditions: vec![],
            actions: vec![RuleAction::MarkRead],
            stop: false,
        }
    }

    fn server(
        account_id: i64,
        state: ServerRulesState,
        on_server: &[i64],
        here: &[(i64, RuleHereWhy)],
    ) -> ServerRules {
        ServerRules {
            account_id,
            state,
            on_server: on_server.to_vec(),
            here: here
                .iter()
                .map(|(rule_id, why)| RuleHere {
                    rule_id: *rule_id,
                    why: *why,
                })
                .collect(),
            checked_at: 0,
        }
    }

    #[test]
    fn rules_tell_where_they_run() {
        let accounts = [
            account(1, MailProtocol::Imap),
            account(2, MailProtocol::Jmap),
            account(3, MailProtocol::Local),
        ];
        let servers = [
            server(
                1,
                ServerRulesState::Runs,
                &[10, 11],
                &[(12, RuleHereWhy::Pattern)],
            ),
            server(
                2,
                ServerRulesState::Runs,
                &[11],
                &[(10, RuleHereWhy::After)],
            ),
        ];
        assert_eq!(
            place(&mail_rule(10, Some(1)), &accounts, &servers),
            ("server", "")
        );
        assert_eq!(
            place(&mail_rule(12, Some(1)), &accounts, &servers),
            ("app", "pattern")
        );
        // For every account that receives mail.
        assert_eq!(
            place(&mail_rule(11, None), &accounts, &servers),
            ("server", "")
        );
        assert_eq!(
            place(&mail_rule(10, None), &accounts, &servers),
            ("some", "")
        );
        // A server that can't run rules leaves nothing to tell.
        let unavailable = [server(1, ServerRulesState::Unavailable, &[], &[])];
        assert_eq!(
            place(&mail_rule(10, Some(1)), &accounts, &unavailable),
            ("", "")
        );
        assert_eq!(place(&mail_rule(10, Some(1)), &accounts, &[]), ("", ""));
        // Nor does a rule that is off.
        let off = MailRule {
            enabled: false,
            ..mail_rule(10, Some(1))
        };
        assert_eq!(place(&off, &accounts, &servers), ("", ""));
        // Waiting for the user, the rules run in the app.
        let waiting = [server(1, ServerRulesState::Others, &[], &[])];
        assert_eq!(
            place(&mail_rule(10, Some(1)), &accounts, &waiting),
            ("app", "")
        );
    }

    #[test]
    fn servers_that_need_the_user_get_a_notice() {
        let accounts = [
            account(1, MailProtocol::Imap),
            account(2, MailProtocol::Imap),
        ];
        let servers = [
            server(2, ServerRulesState::RunBefore("roundcube".into()), &[], &[]),
            server(1, ServerRulesState::Others, &[], &[]),
        ];
        let rules = [mail_rule(10, Some(2))];
        assert_eq!(
            notices(&accounts, &servers, &rules),
            [
                Notice {
                    account_id: 1,
                    account: "1@test.dev".into(),
                    kind: "others",
                    detail: String::new(),
                    has_rules: false,
                },
                Notice {
                    account_id: 2,
                    account: "2@test.dev".into(),
                    kind: "run-before",
                    detail: "roundcube".into(),
                    has_rules: true,
                },
            ]
        );
        let fine = [server(1, ServerRulesState::Runs, &[], &[])];
        assert!(notices(&accounts, &fine, &rules).is_empty());
        let failed = [server(
            1,
            ServerRulesState::Failed("No room".into()),
            &[],
            &[],
        )];
        assert_eq!(notices(&accounts, &failed, &rules)[0].detail, "No room.");
    }

    #[test]
    fn cleanup_choices_show_the_days_set() {
        assert_eq!(cleanup_choice(0), 0);
        assert_eq!(cleanup_choice(30), 3);
        assert_eq!(cleanup_choice(90), 5);
        // Not listed: the longest shorter choice, or for ever.
        assert_eq!(cleanup_choice(45), 3);
        assert_eq!(cleanup_choice(3), 0);
        assert_eq!(cleanup_choice(400), 5);
        assert_eq!(
            CLEANUP_DAYS[usize::try_from(cleanup_choice(14)).unwrap()],
            14
        );
    }

    #[test]
    fn every_field_lists_comparisons_its_rules_can_save() {
        for field in FIELDS {
            for (op, _) in ops(field) {
                assert!(
                    flectar_mail_core::rules::op_fits(field, op),
                    "{field:?} {op:?}"
                );
            }
        }
        // The editor's order is the core's: a rule reopens as it was saved.
        assert_eq!(
            index_of(&ops(RuleField::MailingList), &(RuleOp::Exists, true)),
            1
        );
        assert_eq!(
            index_of(&ops(RuleField::Subject), &(RuleOp::Matches, true)),
            7
        );
        assert_eq!(index_of(&KINDS, &Kind::ShowIn), 7);
        assert_eq!(index_of(&KINDS, &Kind::Copy), 8);
        // The Slint editor names fields and comparisons by these places.
        assert_eq!(index_of(&FIELDS, &RuleField::MailingList), 5);
        assert_eq!(index_of(&FIELDS, &RuleField::Size), 6);
        assert_eq!(index_of(&FIELDS, &RuleField::Header), 7);
        assert_eq!(index_of(&FIELDS, &RuleField::Body), 8);
        assert_eq!(index_of(&FIELDS, &RuleField::Attachments), 9);
        assert_eq!(
            index_of(&ops(RuleField::Body), &(RuleOp::Matches, false)),
            2
        );
    }

    #[test]
    fn folders_show_where_they_are_and_labels_whose_they_are() {
        let account = |id, mail_protocol| Account {
            id,
            email: format!("{id}@test.dev"),
            display_name: None,
            name: None,
            avatar_url: None,
            provider: flectar_mail_core::models::Provider::Generic,
            auth_kind: flectar_mail_core::models::AuthKind::Password,
            mail_protocol,
            sync_state: String::new(),
            sync_error: None,
            sync_retry_at: None,
            can_create_top_level_mailbox: true,
            server_keeps_mail_days: None,
            caught_up_at: None,
        };
        let folder = |id, account_id, parent_id, name: &str, role: Option<&str>| FolderInfo {
            id,
            account_id,
            parent_id,
            display_name: name.into(),
            is_jmap: false,
            path: name.into(),
            delimiter: Some("/".into()),
            role: role.map(str::to_owned),
            selectable: true,
            can_create_children: true,
            can_rename: true,
            can_delete: true,
        };
        let label = |id, name: &str, owner: Option<i64>, is_auto| Label {
            id,
            name: name.into(),
            color: String::new(),
            keyword: String::new(),
            position: 0,
            owner_account_id: owner,
            is_auto,
        };
        // The categories the app seeds, whose keywords tell them apart.
        let category = |id, name: &str, keyword: &str| Label {
            keyword: keyword.into(),
            ..label(id, name, None, true)
        };
        let choices = Choices {
            accounts: vec![
                account(1, MailProtocol::Imap),
                account(2, MailProtocol::Jmap),
            ],
            folders: vec![
                folder(10, 1, None, "Inbox", Some("inbox")),
                folder(11, 1, None, "Projects", None),
                folder(12, 1, Some(11), "Clients", None),
                folder(13, 2, None, "Lists", None),
            ],
            labels: vec![
                label(1, "Mine", None, false),
                label(2, "Work", Some(1), false),
                label(3, "Other", Some(2), false),
                category(4, "News", "FlectarMailAutoNews"),
                category(5, "Marketing", "FlectarMailAutoMarketing"),
                // Given a name of its own, a category keeps it.
                category(6, "Offers", "FlectarMailAutoSocial"),
                // A name alone makes no category.
                label(7, "Pitch", None, true),
                // Neither does a manual label, whatever its keyword.
                Label {
                    keyword: "FlectarMailAutoPitch".into(),
                    ..label(8, "Pitch", None, false)
                },
            ],
        };
        assert!(choices.account(0).is_none());
        let first = choices.account(1);
        assert_eq!(
            choices.folders(first),
            [
                (11, "Projects".to_owned()),
                (12, "Projects / Clients".to_owned())
            ]
        );
        assert!(choices.folders(None).is_empty());
        assert_eq!(
            choices.labels(first),
            [
                (1, "Mine".to_owned()),
                (2, "Work".to_owned()),
                (8, "Pitch".to_owned())
            ]
        );
        assert_eq!(
            choices.labels(None),
            [(1, "Mine".to_owned()), (8, "Pitch".to_owned())]
        );
        // Important and Other, then the automatic categories: named by the
        // app in its language while they have the names it gave them.
        assert_eq!(
            choices.tabs(),
            [
                (
                    "important".to_owned(),
                    TabName::Role(MailboxRole::Important)
                ),
                ("other".to_owned(), TabName::Role(MailboxRole::Other)),
                ("label:4".to_owned(), TabName::Role(MailboxRole::News)),
                ("label:5".to_owned(), TabName::Role(MailboxRole::Marketing)),
                ("label:6".to_owned(), TabName::Text("Offers".to_owned())),
                ("label:7".to_owned(), TabName::Text("Pitch".to_owned())),
            ]
        );
        let show_in = |tab: &str| MailRule {
            id: 1,
            account_id: None,
            name: "Tab".into(),
            enabled: true,
            match_all: true,
            conditions: vec![],
            actions: vec![RuleAction::ShowIn { tab: tab.into() }],
            stop: false,
        };
        assert!(targets_exist(
            &show_in("other"),
            &choices.folders,
            &choices.labels
        ));
        assert!(targets_exist(
            &show_in("label:4"),
            &choices.folders,
            &choices.labels
        ));
        // A category that is gone, or a label that isn't one.
        assert!(!targets_exist(
            &show_in("label:9"),
            &choices.folders,
            &choices.labels
        ));
        assert!(!targets_exist(
            &show_in("label:1"),
            &choices.folders,
            &choices.labels
        ));
    }
}
