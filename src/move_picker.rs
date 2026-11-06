//! Move to: moving mail into a folder without dragging it (WCAG 2.5.7).
//!
//! The picker lists the folders the mail could be dropped on in the sidebar,
//! and a choice is carried out as that drop, so both always agree.
use super::*;

/// Folders listed under Recent, at most.
const RECENT_TARGETS: usize = 5;

/// A folder the picker offers: the sidebar row the mail would be dropped on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MoveTarget {
    pub(crate) scope: Scope,
    pub(crate) account_id: i64,
    pub(crate) folder_id: i64,
    pub(crate) kind: &'static str,
    pub(crate) label: String,
    /// The folders it is in, outermost first.
    pub(crate) path: String,
    pub(crate) depth: usize,
}

/// One row of the picker: a target by its index, or a heading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerRow {
    Heading(&'static str),
    Target {
        index: usize,
        /// Whether the row shows where the folder is, rather than its
        /// indentation in the folder tree.
        located: bool,
    },
}

/// Where mail of these accounts can go, in sidebar order: one account's
/// mailboxes and folders, or for several accounts the unified mailboxes.
/// These are the sidebar's drop targets that move mail, less the one the
/// list shows it in.
pub(crate) fn move_targets(
    mailboxes: &[MailboxEntry],
    account_ids: &[i64],
    viewed: Scope,
) -> Vec<MoveTarget> {
    let kind = |mailbox| match mailbox {
        StandardMailbox::Inbox => Some("inbox"),
        StandardMailbox::Archive => Some("archive"),
        StandardMailbox::Spam => Some("spam"),
        StandardMailbox::Trash => Some("trash"),
        StandardMailbox::Starred | StandardMailbox::Sent | StandardMailbox::Drafts => None,
    };
    if let [account_id] = account_ids {
        let labels = mailboxes
            .iter()
            .filter(|mailbox| mailbox.account_id == *account_id && mailbox.folder_id >= 0)
            .map(|mailbox| {
                (
                    mailbox.folder_id,
                    (mailbox.parent_folder_id, mailbox.label.as_str()),
                )
            })
            .collect::<HashMap<_, _>>();
        return mailboxes
            .iter()
            .filter(|mailbox| {
                mailbox.account_id == *account_id
                    && !mailbox.is_account
                    && mailbox.is_selectable
                    && mailbox.folder_id >= 0
            })
            .filter_map(|mailbox| {
                let scope = mailbox.scope?;
                let kind = match scope {
                    Scope::Account { mailbox, .. } => kind(mailbox)?,
                    Scope::Folder(_) => "folder",
                    _ => return None,
                };
                if viewed.shows_mail_in(scope) {
                    return None;
                }
                Some(MoveTarget {
                    scope,
                    account_id: *account_id,
                    folder_id: mailbox.folder_id,
                    kind,
                    label: mailbox.label.clone(),
                    path: folder_path(&labels, mailbox.parent_folder_id),
                    depth: mailbox.depth,
                })
            })
            .collect();
    }
    StandardMailbox::ALL
        .into_iter()
        .filter_map(|mailbox| Some((Scope::Unified(mailbox), kind(mailbox)?)))
        .filter(|(scope, _)| !viewed.shows_mail_in(*scope))
        .map(|(scope, kind)| MoveTarget {
            scope,
            account_id: -1,
            folder_id: -1,
            kind,
            label: String::new(),
            path: String::new(),
            depth: 0,
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

/// The picker's rows: with no search, the recent folders and then every
/// folder as a tree; with one, the folders whose name or location contains
/// it.
pub(crate) fn picker_rows(targets: &[MoveTarget], recent: &[i64], query: &str) -> Vec<PickerRow> {
    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        return targets
            .iter()
            .enumerate()
            .filter(|(_, target)| {
                target.label.to_lowercase().contains(&query)
                    || target.path.to_lowercase().contains(&query)
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
    let mut rows = Vec::with_capacity(targets.len() + recent.len() + 2);
    if !recent.is_empty() {
        rows.push(PickerRow::Heading("recent"));
        rows.extend(recent.into_iter().map(|index| PickerRow::Target {
            index,
            located: true,
        }));
        rows.push(PickerRow::Heading("folders"));
    }
    rows.extend((0..targets.len()).map(|index| PickerRow::Target {
        index,
        located: false,
    }));
    rows
}

fn slint_rows(targets: &[MoveTarget], rows: Vec<PickerRow>) -> Vec<MoveTargetRow> {
    rows.into_iter()
        .filter_map(|row| match row {
            PickerRow::Heading(kind) => Some(MoveTargetRow {
                key: -1,
                kind: kind.into(),
                ..Default::default()
            }),
            PickerRow::Target { index, located } => {
                let target = targets.get(index)?;
                Some(MoveTargetRow {
                    key: i32::try_from(index).ok()?,
                    kind: target.kind.into(),
                    label: target.label.clone().into(),
                    detail: if located {
                        target.path.clone().into()
                    } else {
                        Default::default()
                    },
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

/// The open picker: the mail it moves and where to.
#[derive(Default)]
struct MovePicker {
    ids: Vec<i32>,
    targets: Vec<MoveTarget>,
    recent: Vec<i64>,
}

/// The mail Move to acts on: a list row and the rows checked along with it,
/// the checked rows, or the open conversation.
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
    ui.on_request(move |source, id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let (ids, targets, core, account_ids) = {
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
            let targets = move_targets(&state.mailboxes, &account_ids, state.scope);
            (ids, targets, state.core.clone(), account_ids)
        };
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
        let Some(core) = core else {
            return;
        };
        if ids.is_empty() || account_ids.is_empty() {
            return;
        }
        if targets.is_empty() {
            app.set_render_status(UiMessage::plain("No other folder to move this mail to."));
            return;
        }
        // Recent folders are one account's.
        let recent = match account_ids.as_slice() {
            [account_id] => request_runtime
                .block_on(core.recent_move_targets(*account_id, 2 * RECENT_TARGETS))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let rows = slint_rows(&targets, picker_rows(&targets, &recent, ""));
        *request_picker.borrow_mut() = MovePicker {
            ids,
            targets,
            recent,
        };
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
            picker_rows(&picker.targets, &picker.recent, &query),
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
        let MovePicker { ids, targets, .. } = std::mem::take(&mut *picker.borrow_mut());
        let Some(target) = usize::try_from(key)
            .ok()
            .and_then(|index| targets.into_iter().nth(index))
        else {
            return;
        };
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
        if let Err(error) = perform_mail_drop(
            &app,
            &choose_state,
            &choose_runtime,
            &MailDragPayload { items },
            target.scope,
            i32::try_from(target.account_id).unwrap_or(-1),
            i32::try_from(target.folder_id).unwrap_or(-1),
        ) {
            app.set_render_status(UiMessage::detail("Could not move mail: {}", error));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(scope: Scope, folder_id: i64, parent: i64, depth: usize, label: &str) -> MailboxEntry {
        MailboxEntry {
            account_id: 1,
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
            context: String::new(),
            detail: String::new(),
            avatar: String::new(),
            is_account: false,
            count: 0,
        }
    }

    fn account(mailbox: StandardMailbox) -> Scope {
        Scope::Account {
            account_id: 1,
            mailbox,
        }
    }

    /// An account's sidebar: standard mailboxes, then a folder tree with a
    /// hierarchy container and two folders of the same name.
    fn sidebar() -> Vec<MailboxEntry> {
        let mut container = entry(Scope::Folder(20), 20, -1, 0, "Projects");
        container.is_selectable = false;
        let mut heading = entry(account(StandardMailbox::Inbox), -1, -1, 0, "me@example.com");
        heading.is_account = true;
        heading.scope = None;
        let mut other_account = entry(Scope::Folder(40), 40, -1, 0, "Elsewhere");
        other_account.account_id = 2;
        vec![
            heading,
            entry(account(StandardMailbox::Inbox), 10, -1, 0, "Inbox"),
            entry(account(StandardMailbox::Starred), -1, -1, 0, "Starred"),
            entry(account(StandardMailbox::Sent), 11, -1, 0, "Sent"),
            entry(account(StandardMailbox::Archive), 12, -1, 0, "Archive"),
            entry(account(StandardMailbox::Spam), 13, -1, 0, "Spam"),
            entry(account(StandardMailbox::Trash), 14, -1, 0, "Trash"),
            entry(account(StandardMailbox::Drafts), 15, -1, 0, "Drafts"),
            container,
            entry(Scope::Folder(21), 21, 20, 1, "Work"),
            entry(Scope::Folder(22), 22, 21, 2, "2024"),
            entry(Scope::Folder(23), 23, 20, 1, "2024"),
            other_account,
        ]
    }

    fn labels(targets: &[MoveTarget]) -> Vec<String> {
        targets
            .iter()
            .map(|target| match target.path.as_str() {
                "" => target.label.clone(),
                path => format!("{path} / {}", target.label),
            })
            .collect()
    }

    #[test]
    fn mail_moves_to_the_folders_it_can_be_dropped_on() {
        let targets = move_targets(&sidebar(), &[1], account(StandardMailbox::Inbox));
        assert_eq!(
            labels(&targets),
            [
                "Archive",
                "Spam",
                "Trash",
                "Projects / Work",
                "Projects / Work / 2024",
                "Projects / 2024",
            ],
            "not the list's own mailbox, Starred, Sent, Drafts, a container or another account's folder"
        );
        assert_eq!(
            targets.iter().map(|target| target.kind).collect::<Vec<_>>(),
            ["archive", "spam", "trash", "folder", "folder", "folder"]
        );
        assert_eq!(targets[4].scope, Scope::Folder(22));
        assert_eq!(targets[4].depth, 2);

        // A folder named like the one shown is still another folder.
        let from_folder = move_targets(&sidebar(), &[1], Scope::Folder(23));
        assert!(from_folder.iter().any(|target| target.folder_id == 22));
        assert!(!from_folder.iter().any(|target| target.folder_id == 23));
        assert!(from_folder.iter().any(|target| target.kind == "inbox"));
    }

    #[test]
    fn mail_of_several_accounts_moves_to_unified_mailboxes() {
        let targets = move_targets(&sidebar(), &[1, 2], Scope::Unified(StandardMailbox::Inbox));
        assert_eq!(
            targets
                .iter()
                .map(|target| target.scope)
                .collect::<Vec<_>>(),
            [
                Scope::Unified(StandardMailbox::Archive),
                Scope::Unified(StandardMailbox::Spam),
                Scope::Unified(StandardMailbox::Trash),
            ]
        );
        // One account's mail in a unified list is in that account's mailbox.
        let one = move_targets(&sidebar(), &[1], Scope::Unified(StandardMailbox::Archive));
        assert!(one.iter().any(|target| target.kind == "inbox"));
        assert!(!one.iter().any(|target| target.kind == "archive"));
        let important = move_targets(&sidebar(), &[1], Scope::Important);
        assert!(!important.iter().any(|target| target.kind == "inbox"));
    }

    #[test]
    fn recent_folders_come_first_and_search_finds_names_and_locations() {
        let targets = move_targets(&sidebar(), &[1], account(StandardMailbox::Inbox));
        let shown = |rows: Vec<PickerRow>| {
            rows.into_iter()
                .map(|row| match row {
                    PickerRow::Heading(kind) => format!("[{kind}]"),
                    PickerRow::Target { index, located } => {
                        let target = &targets[index];
                        if located && !target.path.is_empty() {
                            format!("{} ({})", target.label, target.path)
                        } else {
                            format!("{}{}", "  ".repeat(target.depth), target.label)
                        }
                    }
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            shown(picker_rows(&targets, &[], "")),
            ["Archive", "Spam", "Trash", "  Work", "    2024", "  2024"]
        );
        // Recent folders that are no destination now are left out: the
        // inbox shown, a folder deleted since.
        assert_eq!(
            shown(picker_rows(&targets, &[23, 10, 99, 12], "")),
            [
                "[recent]",
                "2024 (Projects)",
                "Archive",
                "[folders]",
                "Archive",
                "Spam",
                "Trash",
                "  Work",
                "    2024",
                "  2024",
            ]
        );
        let many = [12, 13, 14, 21, 22, 23];
        assert_eq!(
            picker_rows(&targets, &many, "")
                .iter()
                .take_while(|row| **row != PickerRow::Heading("folders"))
                .count(),
            1 + RECENT_TARGETS
        );
        assert_eq!(
            shown(picker_rows(&targets, &[23], " 2024 ")),
            ["2024 (Projects / Work)", "2024 (Projects)"]
        );
        assert_eq!(
            shown(picker_rows(&targets, &[], "work")),
            ["Work (Projects)", "2024 (Projects / Work)"]
        );
        assert!(picker_rows(&targets, &[], "nothing like it").is_empty());
    }
}
