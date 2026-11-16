//! What the rule editor offers (#238): the fields, comparisons and actions
//! in its order, and the folders and labels a rule's account can use.

use flectar_mail_core::models::{
    Account, FolderInfo, Label, MailRule, RuleAction, RuleField, RuleOp,
};

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
    Copy,
}

pub(super) const KINDS: [Kind; 8] = [
    Kind::Move,
    Kind::Label,
    Kind::MarkRead,
    Kind::Star,
    Kind::Archive,
    Kind::Trash,
    Kind::Junk,
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

/// Whether the folders and labels `rule` files into are still there.
pub(super) fn targets_exist(rule: &MailRule, folders: &[FolderInfo], labels: &[Label]) -> bool {
    rule.actions.iter().all(|action| match action {
        RuleAction::Move { folder } | RuleAction::Copy { folder } => {
            folders.iter().any(|candidate| candidate.id == *folder)
        }
        RuleAction::Label { label } => labels.iter().any(|candidate| candidate.id == *label),
        _ => true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flectar_mail_core::models::MailProtocol;

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
        assert_eq!(index_of(&KINDS, &Kind::Copy), 7);
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
            avatar_url: None,
            provider: flectar_mail_core::models::Provider::Generic,
            auth_kind: flectar_mail_core::models::AuthKind::Password,
            mail_protocol,
            sync_state: String::new(),
            sync_error: None,
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
                label(4, "News", None, true),
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
            [(1, "Mine".to_owned()), (2, "Work".to_owned())]
        );
        assert_eq!(choices.labels(None), [(1, "Mine".to_owned())]);
    }
}
