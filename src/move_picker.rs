//! Move to and Copy to: moving or copying mail into a folder without
//! dragging it (WCAG 2.5.7).
//!
//! The picker lists the folders the mail could be dropped on in the sidebar,
//! and a move is carried out as that drop, so both always agree. A folder of
//! another account gets a copy there, and a move removes the original once
//! the copy is in place. A copy within an account is offered where a message
//! can be in several folders: JMAP.
use super::*;
use flectar_mail_core::events::TransferReport;

/// Folders listed under Recent, at most.
const RECENT_TARGETS: usize = 5;

/// What the picker does with the mail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PickerMode {
    #[default]
    Move,
    Copy,
}

/// A folder the picker offers: the sidebar row the mail would be dropped on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MoveTarget {
    pub(crate) scope: Scope,
    /// -1 for the unified mailboxes.
    pub(crate) account_id: i64,
    pub(crate) folder_id: i64,
    pub(crate) kind: &'static str,
    pub(crate) label: String,
    /// The folders it is in, outermost first.
    pub(crate) path: String,
    pub(crate) depth: usize,
    /// The name of its account; empty for the unified mailboxes.
    pub(crate) account: String,
}

/// One row of the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerRow {
    Heading(&'static str),
    /// The account of the targets that follow, named by the first of them.
    Account(usize),
    Target {
        index: usize,
        /// Whether the row shows where the folder is, rather than its
        /// indentation in the folder tree.
        located: bool,
    },
}

/// Where mail of `mail_accounts` can go, in sidebar order: for mail of
/// several accounts the unified mailboxes, then the mail's own account and
/// every other one. A copy within an account is offered only where
/// `copies_within` says a message can be in several folders. These are the
/// sidebar's drop targets, less the folder the list shows the mail in.
pub(crate) fn move_targets(
    mailboxes: &[MailboxEntry],
    mail_accounts: &[i64],
    viewed: Scope,
    mode: PickerMode,
    copies_within: &dyn Fn(i64) -> bool,
) -> Vec<MoveTarget> {
    let mut targets = Vec::new();
    if mode == PickerMode::Move && mail_accounts.len() > 1 {
        targets.extend(
            StandardMailbox::ALL
                .into_iter()
                .filter_map(|mailbox| Some((Scope::Unified(mailbox), standard_kind(mailbox)?)))
                .filter(|(scope, _)| !viewed.shows_mail_in(*scope))
                .map(|(scope, kind)| MoveTarget {
                    scope,
                    account_id: -1,
                    folder_id: -1,
                    kind,
                    label: String::new(),
                    path: String::new(),
                    depth: 0,
                    account: String::new(),
                }),
        );
    }
    let mut accounts = Vec::new();
    for mailbox in mailboxes {
        if mailbox.account_id >= 0 && !accounts.contains(&mailbox.account_id) {
            accounts.push(mailbox.account_id);
        }
    }
    // The mail's own account first.
    if let [own] = mail_accounts
        && let Some(position) = accounts.iter().position(|account| account == own)
    {
        accounts[..=position].rotate_right(1);
    }
    for account_id in accounts {
        let own = mail_accounts.contains(&account_id);
        if mode == PickerMode::Copy && own && !copies_within(account_id) {
            continue;
        }
        targets.extend(account_targets(
            mailboxes,
            account_id,
            own.then_some(viewed),
        ));
    }
    targets
}

/// What a standard mailbox the picker offers shows as.
fn standard_kind(mailbox: StandardMailbox) -> Option<&'static str> {
    match mailbox {
        StandardMailbox::Inbox => Some("inbox"),
        StandardMailbox::Archive => Some("archive"),
        StandardMailbox::Spam => Some("spam"),
        StandardMailbox::Trash => Some("trash"),
        StandardMailbox::Starred | StandardMailbox::Sent | StandardMailbox::Drafts => None,
    }
}

/// One account's folders that can take mail, but the one the list shows
/// the mail in when it is the mail's own account (`viewed`).
fn account_targets(
    mailboxes: &[MailboxEntry],
    account_id: i64,
    viewed: Option<Scope>,
) -> Vec<MoveTarget> {
    let labels = mailboxes
        .iter()
        .filter(|mailbox| mailbox.account_id == account_id && mailbox.folder_id >= 0)
        .map(|mailbox| {
            (
                mailbox.folder_id,
                (mailbox.parent_folder_id, mailbox.label.as_str()),
            )
        })
        .collect::<HashMap<_, _>>();
    mailboxes
        .iter()
        .filter(|mailbox| {
            mailbox.account_id == account_id
                && !mailbox.is_account
                && mailbox.is_selectable
                && mailbox.folder_id >= 0
        })
        .filter_map(|mailbox| {
            let scope = mailbox.scope?;
            let kind = match scope {
                Scope::Account { mailbox, .. } => standard_kind(mailbox)?,
                Scope::Folder(_) => "folder",
                _ => return None,
            };
            if viewed.is_some_and(|viewed| viewed.shows_mail_in(scope)) {
                return None;
            }
            Some(MoveTarget {
                scope,
                account_id,
                folder_id: mailbox.folder_id,
                kind,
                label: mailbox.label.clone(),
                path: folder_path(&labels, mailbox.parent_folder_id),
                depth: mailbox.depth,
                account: mailbox.context.clone(),
            })
        })
        .collect()
}

/// The names of a folder's parents, outermost first.
fn folder_path(labels: &HashMap<i64, (i64, &str)>, mut parent: i64) -> String {
    let mut names = Vec::new();
    // Broken provider data can loop; no real chain is longer than this.
    for _ in 0..labels.len() {
        let Some((grandparent, label)) = labels.get(&parent) else {
            break;
        };
        names.push(*label);
        parent = *grandparent;
    }
    names.reverse();
    names.join(" / ")
}

/// Whether rows name the targets' accounts: unless they are all of the one
/// account the mail is in.
pub(crate) fn names_accounts(targets: &[MoveTarget], mail_accounts: &[i64]) -> bool {
    !matches!(mail_accounts, [own] if targets.iter().all(|target| target.account_id == *own))
}

/// The picker's rows: with no search, the recent folders and then every
/// folder as a tree, under its account's name where rows are `named`; with
/// one, the folders whose name, location or account contains it.
pub(crate) fn picker_rows(
    targets: &[MoveTarget],
    recent: &[i64],
    query: &str,
    named: bool,
) -> Vec<PickerRow> {
    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        return targets
            .iter()
            .enumerate()
            .filter(|(_, target)| {
                target.label.to_lowercase().contains(&query)
                    || target.path.to_lowercase().contains(&query)
                    || (named && target.account.to_lowercase().contains(&query))
            })
            .map(|(index, _)| PickerRow::Target {
                index,
                located: true,
            })
            .collect();
    }
    let recent = recent
        .iter()
        .filter_map(|folder_id| {
            targets
                .iter()
                .position(|target| target.folder_id == *folder_id)
        })
        .take(RECENT_TARGETS)
        .collect::<Vec<_>>();
    let mut rows = Vec::with_capacity(targets.len() + recent.len() + 8);
    if !recent.is_empty() {
        rows.push(PickerRow::Heading("recent"));
        rows.extend(recent.into_iter().map(|index| PickerRow::Target {
            index,
            located: true,
        }));
        if !named {
            rows.push(PickerRow::Heading("folders"));
        }
    }
    for (index, target) in targets.iter().enumerate() {
        if named
            && targets
                .get(index.wrapping_sub(1))
                .is_none_or(|previous| previous.account_id != target.account_id)
        {
            rows.push(PickerRow::Account(index));
        }
        rows.push(PickerRow::Target {
            index,
            located: false,
        });
    }
    rows
}

fn slint_rows(
    targets: &[MoveTarget],
    rows: Vec<PickerRow>,
    local: Option<i64>,
    named: bool,
) -> Vec<MoveTargetRow> {
    rows.into_iter()
        .filter_map(|row| match row {
            PickerRow::Heading(kind) => Some(MoveTargetRow {
                key: -1,
                kind: kind.into(),
                ..Default::default()
            }),
            PickerRow::Account(index) => {
                let target = targets.get(index)?;
                Some(MoveTargetRow {
                    key: -1,
                    kind: if target.account_id < 0 {
                        "unified"
                    } else if Some(target.account_id) == local {
                        "device"
                    } else {
                        "account"
                    }
                    .into(),
                    label: target.account.clone().into(),
                    label_has_emoji: contains_emoji(&target.account),
                    ..Default::default()
                })
            }
            PickerRow::Target { index, located } => {
                let target = targets.get(index)?;
                let detail = if !located {
                    String::new()
                } else if named && target.account_id >= 0 {
                    let account = if Some(target.account_id) == local {
                        String::new()
                    } else {
                        target.account.clone()
                    };
                    [target.path.as_str(), account.as_str()]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ")
                } else {
                    target.path.clone()
                };
                Some(MoveTargetRow {
                    key: i32::try_from(index).ok()?,
                    kind: target.kind.into(),
                    label: target.label.clone().into(),
                    detail: detail.into(),
                    depth: if located {
                        0
                    } else {
                        i32::try_from(target.depth).unwrap_or(0)
                    },
                    label_has_emoji: contains_emoji(&target.label),
                })
            }
        })
        .collect()
}

/// What to say once mail copied or moved to other accounts got there, or
/// didn't.
pub(super) fn transfer_message(report: &TransferReport) -> UiMessage {
    if report.failed > 0 {
        return UiMessage::arguments(
            "Could not move or copy {} messages: {}",
            report.failed,
            report.error.as_deref().unwrap_or_default(),
        );
    }
    match (report.moved, report.copied) {
        (0, 0) if report.present > 0 => {
            UiMessage::detail("The other account had {} messages already.", report.present)
        }
        (moved, 0) => UiMessage::detail("Moved {} messages to the other account.", moved),
        (0, copied) => UiMessage::detail("Copied {} messages to the other account.", copied),
        (moved, copied) => UiMessage::detail(
            "Moved or copied {} messages to the other account.",
            moved + copied,
        ),
    }
}

/// The open picker: what it does, the mail it acts on and where to.
#[derive(Default)]
struct MovePicker {
    mode: PickerMode,
    ids: Vec<i32>,
    targets: Vec<MoveTarget>,
    recent: Vec<i64>,
    local: Option<i64>,
    named: bool,
}

/// The mail the picker acts on: a list row and the rows checked along with
/// it, the checked rows, or the open conversation.
fn picker_ids(state: &InboxState, source: &str, id: i32) -> Vec<i32> {
    match source {
        "row" => mail_operation_ids(&state.messages, &state.checked_ids, id),
        "checked" => state
            .messages
            .iter()
            .filter(|message| state.checked_ids.contains(&message.id))
            .map(|message| message.id)
            .collect(),
        "open" => state.selected_id.into_iter().collect(),
        _ => Vec::new(),
    }
}

pub(super) fn register(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    let picker = Rc::new(RefCell::new(MovePicker::default()));
    let ui = app.global::<MovePickerUi>();

    let weak = app.as_weak();
    let request_state = state.clone();
    let request_runtime = runtime.clone();
    let request_picker = picker.clone();
    ui.on_request(move |mode, source, id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let mode = if mode == "copy" {
            PickerMode::Copy
        } else {
            PickerMode::Move
        };
        let (ids, targets, core, local, named) = {
            let state = request_state.borrow();
            if !state.using_core {
                return;
            }
            let ids = picker_ids(&state, &source, id);
            let mut account_ids = ids
                .iter()
                .filter_map(|id| state.messages.iter().find(|message| message.id == *id))
                .map(|message| message.account_id)
                .collect::<Vec<_>>();
            account_ids.sort_unstable();
            account_ids.dedup();
            if account_ids.is_empty() {
                return;
            }
            let jmap = |account_id: i64| {
                state.connected_accounts.iter().any(|account| {
                    account.id == account_id
                        && account.mail_protocol == flectar_mail_core::models::MailProtocol::Jmap
                })
            };
            let targets = move_targets(&state.mailboxes, &account_ids, state.scope, mode, &jmap);
            let named = names_accounts(&targets, &account_ids);
            (
                ids,
                targets,
                state.core.clone(),
                state.local_mail.map(|local| local.account_id),
                named,
            )
        };
        let Some(core) = core else {
            return;
        };
        if targets.is_empty() {
            app.set_render_status(match mode {
                PickerMode::Move => UiMessage::plain("No other folder to move this mail to."),
                PickerMode::Copy => UiMessage::plain("No folder to copy this mail to."),
            });
            return;
        }
        // Search matches the names shown, and standard mailboxes show
        // translated ones.
        let ui = app.global::<MovePickerUi>();
        let targets = targets
            .into_iter()
            .map(|mut target| {
                target.label = ui
                    .invoke_display_name(target.kind.into(), target.label.as_str().into())
                    .into();
                target
            })
            .collect::<Vec<_>>();
        let recent = request_runtime
            .block_on(core.recent_move_targets(4 * RECENT_TARGETS))
            .unwrap_or_default();
        let rows = slint_rows(
            &targets,
            picker_rows(&targets, &recent, "", named),
            local,
            named,
        );
        *request_picker.borrow_mut() = MovePicker {
            mode,
            ids,
            targets,
            recent,
            local,
            named,
        };
        ui.set_mode(match mode {
            PickerMode::Move => "move".into(),
            PickerMode::Copy => "copy".into(),
        });
        ui.set_query(Default::default());
        ui.set_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_open(true);
    });

    let weak = app.as_weak();
    let search_picker = picker.clone();
    ui.on_search(move |query| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let picker = search_picker.borrow();
        let rows = slint_rows(
            &picker.targets,
            picker_rows(&picker.targets, &picker.recent, &query, picker.named),
            picker.local,
            picker.named,
        );
        app.global::<MovePickerUi>()
            .set_rows(ModelRc::new(VecModel::from(rows)));
    });

    let weak = app.as_weak();
    let choose_state = state.clone();
    let choose_runtime = runtime.clone();
    ui.on_choose(move |key| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let MovePicker {
            mode, ids, targets, ..
        } = std::mem::take(&mut *picker.borrow_mut());
        let Some(target) = usize::try_from(key)
            .ok()
            .and_then(|index| targets.into_iter().nth(index))
        else {
            return;
        };
        let result = match mode {
            PickerMode::Move => {
                let items = {
                    let state = choose_state.borrow();
                    ids.iter()
                        .filter_map(|id| state.messages.iter().find(|message| message.id == *id))
                        .filter_map(|message| {
                            Some(MailDragItem {
                                message_id: message.id,
                                account_id: i32::try_from(message.account_id).ok()?,
                            })
                        })
                        .collect::<Vec<_>>()
                };
                perform_mail_drop(
                    &app,
                    &choose_state,
                    &choose_runtime,
                    &MailDragPayload { items },
                    target.scope,
                    i32::try_from(target.account_id).unwrap_or(-1),
                    i32::try_from(target.folder_id).unwrap_or(-1),
                )
            }
            PickerMode::Copy => copy_mail(&choose_state, &ids, &target),
        };
        if let Err(error) = result {
            app.set_render_status(match mode {
                PickerMode::Move => UiMessage::detail("Could not move mail: {}", error),
                PickerMode::Copy => UiMessage::detail("Could not copy mail: {}", error),
            });
        }
    });
}

/// Copies the mail to the target: to another folder of a JMAP account, or
/// another account.
fn copy_mail(
    state: &Rc<RefCell<InboxState>>,
    ids: &[i32],
    target: &MoveTarget,
) -> Result<(), String> {
    let (core, operations) = {
        let state = state.borrow();
        let operations = ids
            .iter()
            .map(|id| {
                let message = state
                    .messages
                    .iter()
                    .find(|message| message.id == *id)
                    .ok_or_else(|| "message is no longer available".to_owned())?;
                let mail = MailTarget::of(message)
                    .ok_or_else(|| "message thread is unavailable".to_owned())?;
                Ok((
                    *id,
                    mail,
                    mail_work::Operation::Copy {
                        folder_id: target.folder_id,
                        other_account: message.account_id != target.account_id,
                    },
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        (
            state
                .core
                .clone()
                .ok_or_else(|| "mail core is unavailable".to_owned())?,
            operations,
        )
    };
    mail_work::enqueue(state, core, operations)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        account_id: i64,
        scope: Scope,
        folder_id: i64,
        parent: i64,
        depth: usize,
        label: &str,
    ) -> MailboxEntry {
        MailboxEntry {
            account_id,
            folder_id,
            parent_folder_id: parent,
            depth,
            has_children: false,
            is_standard: matches!(scope, Scope::Account { .. }),
            is_selectable: true,
            can_create_children: false,
            can_rename: false,
            can_delete: false,
            label: label.into(),
            scope: Some(scope),
            context: if account_id == 1 { "Work" } else { "Home" }.into(),
            detail: String::new(),
            avatar: String::new(),
            is_account: false,
            count: 0,
        }
    }

    fn account(account_id: i64, mailbox: StandardMailbox) -> Scope {
        Scope::Account {
            account_id,
            mailbox,
        }
    }

    /// Two accounts' sidebar: Work's standard mailboxes and a folder tree
    /// with a hierarchy container and two folders of the same name, and
    /// Home's Inbox and one folder.
    fn sidebar() -> Vec<MailboxEntry> {
        let mut container = entry(1, Scope::Folder(20), 20, -1, 0, "Projects");
        container.is_selectable = false;
        let mut heading = entry(1, account(1, StandardMailbox::Inbox), -1, -1, 0, "Work");
        heading.is_account = true;
        heading.scope = None;
        vec![
            heading,
            entry(1, account(1, StandardMailbox::Inbox), 10, -1, 0, "Inbox"),
            entry(
                1,
                account(1, StandardMailbox::Starred),
                -1,
                -1,
                0,
                "Starred",
            ),
            entry(1, account(1, StandardMailbox::Sent), 11, -1, 0, "Sent"),
            entry(
                1,
                account(1, StandardMailbox::Archive),
                12,
                -1,
                0,
                "Archive",
            ),
            entry(1, account(1, StandardMailbox::Spam), 13, -1, 0, "Spam"),
            entry(1, account(1, StandardMailbox::Trash), 14, -1, 0, "Trash"),
            entry(1, account(1, StandardMailbox::Drafts), 15, -1, 0, "Drafts"),
            container,
            entry(1, Scope::Folder(21), 21, 20, 1, "Work"),
            entry(1, Scope::Folder(22), 22, 21, 2, "2024"),
            entry(1, Scope::Folder(23), 23, 20, 1, "2024"),
            entry(2, account(2, StandardMailbox::Inbox), 40, -1, 0, "Inbox"),
            entry(2, Scope::Folder(41), 41, -1, 0, "Travel"),
        ]
    }

    /// Where each target is, as "Account: path / label".
    fn labels(targets: &[MoveTarget]) -> Vec<String> {
        targets
            .iter()
            .map(|target| {
                let name = match target.path.as_str() {
                    "" => target.label.clone(),
                    path => format!("{path} / {}", target.label),
                };
                match target.account.as_str() {
                    "" => name,
                    account => format!("{account}: {name}"),
                }
            })
            .collect()
    }

    fn not_jmap(_: i64) -> bool {
        false
    }

    #[test]
    fn mail_moves_to_its_accounts_folders_and_then_to_the_others() {
        let targets = move_targets(
            &sidebar(),
            &[1],
            account(1, StandardMailbox::Inbox),
            PickerMode::Move,
            &not_jmap,
        );
        assert_eq!(
            labels(&targets),
            [
                "Work: Archive",
                "Work: Spam",
                "Work: Trash",
                "Work: Projects / Work",
                "Work: Projects / Work / 2024",
                "Work: Projects / 2024",
                "Home: Inbox",
                "Home: Travel",
            ],
            "not the list's own mailbox, Starred, Sent, Drafts or a container"
        );
        assert_eq!(targets[4].scope, Scope::Folder(22));
        assert_eq!(targets[4].depth, 2);

        // A folder named like the one shown is still another folder.
        let from_folder = move_targets(
            &sidebar(),
            &[1],
            Scope::Folder(23),
            PickerMode::Move,
            &not_jmap,
        );
        assert!(from_folder.iter().any(|target| target.folder_id == 22));
        assert!(!from_folder.iter().any(|target| target.folder_id == 23));

        // Home's mail lists Home first, and Work's Inbox: it isn't in it.
        let home = move_targets(
            &sidebar(),
            &[2],
            Scope::Unified(StandardMailbox::Inbox),
            PickerMode::Move,
            &not_jmap,
        );
        assert_eq!(labels(&home)[0], "Home: Travel");
        assert!(home.iter().any(|target| target.folder_id == 10));
        assert!(!home.iter().any(|target| target.folder_id == 40));
    }

    #[test]
    fn copies_go_to_other_accounts_or_within_one_that_keeps_mail_in_several() {
        let copies = move_targets(
            &sidebar(),
            &[1],
            account(1, StandardMailbox::Inbox),
            PickerMode::Copy,
            &not_jmap,
        );
        assert_eq!(labels(&copies), ["Home: Inbox", "Home: Travel"]);
        // The rows say whose folders they are.
        assert!(names_accounts(&copies, &[1]));
        let jmap = |account_id: i64| account_id == 1;
        let within = move_targets(
            &sidebar(),
            &[1],
            account(1, StandardMailbox::Inbox),
            PickerMode::Copy,
            &jmap,
        );
        assert_eq!(within.len(), 8);
    }

    #[test]
    fn mail_of_several_accounts_moves_to_unified_mailboxes_too() {
        let targets = move_targets(
            &sidebar(),
            &[1, 2],
            Scope::Unified(StandardMailbox::Inbox),
            PickerMode::Move,
            &not_jmap,
        );
        assert_eq!(
            targets[..3]
                .iter()
                .map(|target| target.scope)
                .collect::<Vec<_>>(),
            [
                Scope::Unified(StandardMailbox::Archive),
                Scope::Unified(StandardMailbox::Spam),
                Scope::Unified(StandardMailbox::Trash),
            ]
        );
        // Each account's mail is in its own Inbox.
        assert!(!targets.iter().any(|target| target.kind == "inbox"));
        let important = move_targets(
            &sidebar(),
            &[1],
            Scope::Important,
            PickerMode::Move,
            &not_jmap,
        );
        assert!(!important.iter().any(|target| target.folder_id == 10));
    }

    #[test]
    fn recent_folders_come_first_and_search_finds_names_locations_and_accounts() {
        let targets = move_targets(
            &sidebar(),
            &[1],
            account(1, StandardMailbox::Inbox),
            PickerMode::Move,
            &not_jmap,
        );
        let named = names_accounts(&targets, &[1]);
        assert!(named, "Home's folders are listed too");
        let shown = |rows: Vec<PickerRow>| {
            slint_rows(&targets, rows, None, named)
                .into_iter()
                .map(|row| match (row.key, row.detail.as_str()) {
                    (-1, _) => format!("[{} {}]", row.kind, row.label).replace(" ]", "]"),
                    (_, "") => format!("{}{}", "  ".repeat(row.depth as usize), row.label),
                    (_, detail) => format!("{} ({detail})", row.label),
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            shown(picker_rows(&targets, &[], "", named)),
            [
                "[account Work]",
                "Archive",
                "Spam",
                "Trash",
                "  Work",
                "    2024",
                "  2024",
                "[account Home]",
                "Inbox",
                "Travel",
            ]
        );
        // Recent folders that are no destination now are left out: the
        // inbox shown, a folder deleted since.
        assert_eq!(
            shown(picker_rows(&targets, &[41, 23, 10, 99], "", named))[..4],
            [
                "[recent]",
                "Travel (Home)",
                "2024 (Projects · Work)",
                "[account Work]",
            ]
        );
        let many = [12, 13, 14, 21, 22, 23];
        assert_eq!(
            picker_rows(&targets, &many, "", named)
                .iter()
                .take_while(|row| !matches!(row, PickerRow::Account(_)))
                .count(),
            1 + RECENT_TARGETS
        );
        assert_eq!(
            shown(picker_rows(&targets, &[], " 2024 ", named)),
            ["2024 (Projects / Work · Work)", "2024 (Projects · Work)"]
        );
        assert_eq!(
            shown(picker_rows(&targets, &[], "home", named)),
            ["Inbox (Home)", "Travel (Home)"]
        );
        assert!(picker_rows(&targets, &[], "nothing like it", named).is_empty());

        // One account's folders need no names, but a heading for the rest.
        let one = move_targets(
            &sidebar()[..12],
            &[1],
            account(1, StandardMailbox::Inbox),
            PickerMode::Move,
            &not_jmap,
        );
        assert!(!names_accounts(&one, &[1]));
        let rows = slint_rows(&one, picker_rows(&one, &[23], "", false), None, false);
        assert_eq!(rows[0].kind, "recent");
        assert_eq!(rows[1].detail, "Projects");
        assert_eq!(rows[2].kind, "folders");
    }

    #[test]
    fn a_finished_transfer_says_what_became_of_it() {
        let report = |moved, copied, present, failed| TransferReport {
            moved,
            copied,
            present,
            failed,
            error: (failed > 0).then(|| "refused".to_owned()),
        };
        assert_eq!(
            transfer_message(&report(2, 0, 1, 0)).english(),
            "Moved 2 messages to the other account."
        );
        assert_eq!(
            transfer_message(&report(0, 1, 0, 0)).english(),
            "Copied 1 messages to the other account."
        );
        assert_eq!(
            transfer_message(&report(0, 0, 3, 0)).english(),
            "The other account had 3 messages already."
        );
        assert_eq!(
            transfer_message(&report(1, 1, 0, 2)).english(),
            "Could not move or copy 2 messages: refused"
        );
    }
}
