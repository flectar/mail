//! Visible sidebar projection and batched, retained Slint model updates.
use super::*;

pub(super) type SidebarModel = crate::retained_model::RetainedModel<SidebarRow>;

impl SidebarModel {
    fn reconcile(&self, rows: Vec<SidebarRow>) {
        self.reconcile_by(rows, |row| row.key.clone(), same_row);
    }
}

fn collapse_new_account_label_sections(
    account_ids: impl IntoIterator<Item = i64>,
    initialized: &mut HashSet<i64>,
    collapsed: &mut HashSet<String>,
) {
    for account_id in account_ids {
        if initialized.insert(account_id) {
            collapsed.insert(format!("account-labels:{account_id}"));
        }
    }
}

fn collapse_new_folder_sections(
    folder_ids: impl IntoIterator<Item = i64>,
    initialized: &mut HashSet<i64>,
    collapsed: &mut HashSet<i64>,
) {
    for folder_id in folder_ids {
        if initialized.insert(folder_id) {
            collapsed.insert(folder_id);
        }
    }
}

// Slint image handles can compare unequal even when both are empty. Compare
// absent avatars semantically, otherwise every ordinary folder emits a change.
fn same_row(a: &SidebarRow, b: &SidebarRow) -> bool {
    let MailboxRow {
        account_id,
        folder_id,
        parent_folder_id,
        depth,
        has_children,
        expanded,
        is_standard,
        is_selectable,
        can_create_children,
        can_rename,
        can_delete,
        label_has_emoji,
        label,
        scope,
        context,
        detail,
        avatar,
        avatar_image,
        has_avatar,
        is_account,
        count,
        color,
        has_custom_color,
    } = &a.mailbox;
    let other = &b.mailbox;
    a.key == b.key
        && a.kind == b.kind
        && a.open == b.open
        && a.label == b.label
        && *account_id == other.account_id
        && *folder_id == other.folder_id
        && *parent_folder_id == other.parent_folder_id
        && *depth == other.depth
        && *has_children == other.has_children
        && *expanded == other.expanded
        && *is_standard == other.is_standard
        && *is_selectable == other.is_selectable
        && *can_create_children == other.can_create_children
        && *can_rename == other.can_rename
        && *can_delete == other.can_delete
        && *label_has_emoji == other.label_has_emoji
        && *label == other.label
        && *scope == other.scope
        && *context == other.context
        && *detail == other.detail
        && *avatar == other.avatar
        && *has_avatar == other.has_avatar
        && *is_account == other.is_account
        && *count == other.count
        && *color == other.color
        && *has_custom_color == other.has_custom_color
        && (!has_avatar || *avatar_image == other.avatar_image)
}

pub(super) fn refresh_sidebar(state: &Rc<RefCell<InboxState>>) {
    let mut state = state.borrow_mut();
    let account_ids = state
        .mailboxes
        .iter()
        .filter(|mailbox| mailbox.is_account)
        .map(|mailbox| mailbox.account_id)
        .collect::<Vec<_>>();
    let folder_ids = state
        .mailboxes
        .iter()
        .filter(|mailbox| !mailbox.is_account && mailbox.has_children && mailbox.folder_id >= 0)
        .map(|mailbox| mailbox.folder_id)
        .collect::<Vec<_>>();
    let InboxState {
        initialized_sidebar_accounts,
        initialized_sidebar_folders,
        collapsed_sidebar_sections,
        collapsed_folder_ids,
        ..
    } = &mut *state;
    collapse_new_account_label_sections(
        account_ids,
        initialized_sidebar_accounts,
        collapsed_sidebar_sections,
    );
    collapse_new_folder_sections(
        folder_ids,
        initialized_sidebar_folders,
        collapsed_folder_ids,
    );
    let query = state.folder_filter.trim();
    let filtering = !query.is_empty();
    let no_collapsed_folders = HashSet::new();
    let mailboxes = make_mailbox_rows(
        &state.mailboxes,
        &state.profile_avatar_images,
        &state.labels,
        if filtering {
            &no_collapsed_folders
        } else {
            &state.collapsed_folder_ids
        },
    );
    let account_names = mailboxes
        .iter()
        .filter(|mailbox| mailbox.is_account)
        .map(|mailbox| (mailbox.account_id, mailbox.label.clone()))
        .collect::<HashMap<_, _>>();
    let mut labels = make_label_rows(&state.labels, None, "");
    for label in &mut labels {
        if let Some(name) = account_names.get(&label.account_id) {
            label.account_name = name.clone();
        } else if label.is_global {
            label.account_name = "All accounts".into();
        }
    }
    let gmail_account_ids = state
        .connected_accounts
        .iter()
        .filter(|account| account.provider == Provider::Gmail)
        .filter_map(|account| i32::try_from(account.id).ok())
        .collect::<HashSet<_>>();
    let rows = if filtering {
        make_filtered_sidebar_rows(mailboxes, labels, query, &gmail_account_ids)
    } else {
        make_sidebar_rows(
            mailboxes,
            make_mailbox_rows(
                &state.unified_mailboxes,
                &state.profile_avatar_images,
                &state.labels,
                &no_collapsed_folders,
            ),
            labels,
            &gmail_account_ids,
            &state.collapsed_sidebar_sections,
        )
    };
    // Model notifications can synchronously inspect application state.
    let model = Rc::clone(&state.sidebar_rows);
    drop(state);
    model.reconcile(rows);
}

fn make_filtered_sidebar_rows(
    mailboxes: Vec<MailboxRow>,
    labels: Vec<MailLabelRow>,
    query: &str,
    gmail_account_ids: &HashSet<i32>,
) -> Vec<SidebarRow> {
    use SidebarRowKind as Kind;

    let query = query.to_lowercase();
    let mut accounts = Vec::new();
    let mut folders: HashMap<i32, Vec<MailboxRow>> = HashMap::new();
    for mailbox in mailboxes {
        if mailbox.is_account {
            accounts.push(mailbox);
        } else if mailbox.label.to_lowercase().contains(&query) {
            folders.entry(mailbox.account_id).or_default().push(mailbox);
        }
    }

    let mut account_labels: HashMap<i32, Vec<MailLabelRow>> = HashMap::new();
    let mut global_labels = Vec::new();
    let mut categories = Vec::new();
    for mut label in labels
        .into_iter()
        .filter(|label| label.name.to_lowercase().contains(&query))
    {
        label.display_name = label.name.clone();
        label.depth = 0;
        label.has_children = false;
        label.expanded = true;
        label.can_create_children = gmail_account_ids.contains(&label.account_id);
        if label.is_auto {
            categories.push(label);
        } else if label.is_global {
            global_labels.push(label);
        } else {
            account_labels
                .entry(label.account_id)
                .or_default()
                .push(label);
        }
    }

    let mut rows = Vec::new();
    if !categories.is_empty() {
        rows.push(section(
            Kind::CategoriesSection,
            "categories",
            &HashSet::new(),
        ));
        rows.extend(categories.into_iter().map(|label| SidebarRow {
            key: format!("category:{}", label.id).into(),
            label,
            ..row(Kind::Category, "")
        }));
    }
    if !global_labels.is_empty() {
        rows.push(section(
            Kind::GlobalLabelsSection,
            "global-labels",
            &HashSet::new(),
        ));
        rows.extend(global_labels.into_iter().map(|label| SidebarRow {
            key: format!("global-label:{}", label.id).into(),
            label,
            ..row(Kind::GlobalLabel, "")
        }));
    }
    for account in accounts {
        let account_id = account.account_id;
        let matches = folders.remove(&account_id).unwrap_or_default();
        let label_matches = account_labels.remove(&account_id).unwrap_or_default();
        if matches.is_empty() && label_matches.is_empty() {
            continue;
        }
        rows.push(SidebarRow {
            open: true,
            mailbox: account,
            ..row(Kind::Account, format!("account:{account_id}"))
        });
        rows.extend(matches.into_iter().map(|mut mailbox| {
            // Search results are a flat projection; their original hierarchy
            // remains untouched and returns when the filter is cleared.
            mailbox.depth = 0;
            mailbox.has_children = false;
            let key = format!("mailbox:{account_id}:{}", mailbox.scope);
            SidebarRow {
                mailbox,
                ..row(Kind::Folder, key)
            }
        }));
        if !label_matches.is_empty() {
            rows.push(SidebarRow {
                mailbox: MailboxRow {
                    account_id,
                    ..Default::default()
                },
                open: true,
                ..row(
                    Kind::AccountLabelsSection,
                    format!("account-labels:{account_id}"),
                )
            });
            rows.extend(label_matches.into_iter().map(|label| SidebarRow {
                key: format!("account-label:{account_id}:{}", label.id).into(),
                label,
                ..row(Kind::AccountLabel, "")
            }));
        }
    }
    rows
}

fn row(kind: SidebarRowKind, key: impl Into<slint::SharedString>) -> SidebarRow {
    SidebarRow {
        kind,
        key: key.into(),
        ..Default::default()
    }
}

fn section(kind: SidebarRowKind, key: &str, collapsed: &HashSet<String>) -> SidebarRow {
    SidebarRow {
        open: !collapsed.contains(key),
        ..row(kind, key)
    }
}

fn account_label_rows(
    labels: &[MailLabelRow],
    account_id: i32,
    nested: bool,
    collapsed: &HashSet<String>,
) -> Vec<SidebarRow> {
    use SidebarRowKind as Kind;

    let account_labels = labels
        .iter()
        .filter(|label| label.account_id == account_id)
        .cloned()
        .collect::<Vec<_>>();
    if !nested {
        return account_labels
            .into_iter()
            .map(|mut label| {
                label.display_name = label.name.clone();
                SidebarRow {
                    key: format!("account-label:{account_id}:{}", label.id).into(),
                    label,
                    ..row(Kind::AccountLabel, "")
                }
            })
            .collect();
    }

    let ids_by_name = account_labels
        .iter()
        .map(|label| (label.name.to_string(), label.id))
        .collect::<HashMap<_, _>>();
    let mut children: HashMap<Option<i32>, Vec<MailLabelRow>> = HashMap::new();
    for label in account_labels {
        let parent_id = label
            .name
            .rsplit_once('/')
            .and_then(|(parent, _)| ids_by_name.get(parent).copied());
        children.entry(parent_id).or_default().push(label);
    }

    let mut stack = children
        .remove(&None)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .map(|label| (label, 0))
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    while let Some((mut label, depth)) = stack.pop() {
        let key = format!("gmail-label:{account_id}:{}", label.id);
        let descendants = children.remove(&Some(label.id)).unwrap_or_default();
        label.display_name = if depth > 0 {
            label
                .name
                .rsplit_once('/')
                .filter(|(_, leaf)| !leaf.is_empty())
                .map(|(_, leaf)| leaf.into())
                .unwrap_or_else(|| label.name.clone())
        } else {
            label.name.clone()
        };
        label.depth = depth;
        label.has_children = !descendants.is_empty();
        label.expanded = !collapsed.contains(&key);
        label.can_create_children = true;
        rows.push(SidebarRow {
            key: key.clone().into(),
            label,
            ..row(Kind::AccountLabel, "")
        });
        if !collapsed.contains(&key) {
            stack.extend(
                descendants
                    .into_iter()
                    .rev()
                    .map(|child| (child, depth.saturating_add(1))),
            );
        }
    }
    rows
}

fn make_sidebar_rows(
    mailboxes: Vec<MailboxRow>,
    unified: Vec<MailboxRow>,
    labels: Vec<MailLabelRow>,
    gmail_account_ids: &HashSet<i32>,
    collapsed: &HashSet<String>,
) -> Vec<SidebarRow> {
    use SidebarRowKind as Kind;
    let mut rows = vec![row(Kind::UnifiedHeading, "unified-heading")];
    rows.push(row(Kind::UnifiedInbox, "unified-inbox"));
    rows.extend(unified.into_iter().map(|mailbox| SidebarRow {
        key: format!("unified:{}", mailbox.scope).into(),
        mailbox,
        ..row(Kind::UnifiedMailbox, "")
    }));
    rows.push(section(Kind::CategoriesSection, "categories", collapsed));
    if !collapsed.contains("categories") {
        rows.push(row(Kind::Important, "important"));
        rows.push(row(Kind::Other, "other"));
        rows.extend(
            labels
                .iter()
                .filter(|label| label.is_auto)
                .map(|label| SidebarRow {
                    label: label.clone(),
                    ..row(Kind::Category, format!("category:{}", label.id))
                }),
        );
    }
    if labels.iter().any(|label| label.is_global) {
        rows.push(section(
            Kind::GlobalLabelsSection,
            "global-labels",
            collapsed,
        ));
        if !collapsed.contains("global-labels") {
            rows.extend(
                labels
                    .iter()
                    .filter(|label| label.is_global)
                    .map(|label| SidebarRow {
                        key: format!("global-label:{}", label.id).into(),
                        label: label.clone(),
                        ..row(Kind::GlobalLabel, "")
                    }),
            );
        }
    }
    rows.push(row(Kind::AccountsHeading, "accounts-heading"));
    // Group once by stable account id, not display text. Preserve account and
    // folder order even when providers return interleaved mailbox entries.
    let mut accounts = Vec::new();
    let mut folders: HashMap<i32, Vec<MailboxRow>> = HashMap::new();
    for mailbox in mailboxes {
        if mailbox.is_account {
            accounts.push(mailbox);
        } else {
            folders.entry(mailbox.account_id).or_default().push(mailbox);
        }
    }
    for account in accounts {
        let account_id = account.account_id;
        let account_name = account.label.clone();
        let can_create_top_level_mailbox = account.can_create_children;
        let key = format!("account:{account_id}");
        let open = !collapsed.contains(&key);
        rows.push(SidebarRow {
            mailbox: account,
            ..section(Kind::Account, &key, collapsed)
        });
        if open {
            rows.extend(
                folders
                    .remove(&account_id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mailbox| SidebarRow {
                        key: format!("mailbox:{account_id}:{}", mailbox.scope).into(),
                        mailbox,
                        ..row(Kind::Folder, "")
                    }),
            );
            if !gmail_account_ids.contains(&account_id) && can_create_top_level_mailbox {
                rows.push(SidebarRow {
                    mailbox: MailboxRow {
                        account_id,
                        folder_id: -1,
                        ..Default::default()
                    },
                    ..row(Kind::NewFolder, format!("new-folder:{account_id}"))
                });
            }
            let label_key = format!("account-labels:{account_id}");
            if gmail_account_ids.contains(&account_id)
                || labels.iter().any(|label| label.account_id == account_id)
            {
                rows.push(SidebarRow {
                    mailbox: MailboxRow {
                        account_id,
                        ..Default::default()
                    },
                    ..section(Kind::AccountLabelsSection, &label_key, collapsed)
                });
                if !collapsed.contains(&label_key) {
                    rows.extend(account_label_rows(
                        &labels,
                        account_id,
                        gmail_account_ids.contains(&account_id),
                        collapsed,
                    ));
                    if gmail_account_ids.contains(&account_id) {
                        rows.push(SidebarRow {
                            mailbox: MailboxRow {
                                account_id,
                                context: account_name.clone(),
                                ..Default::default()
                            },
                            ..row(Kind::NewLabel, format!("new-label:{account_id}"))
                        });
                    }
                }
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use SidebarRowKind as Kind;
    use slint::private_unstable_api::re_exports::{
        ModelChangeListener, ModelChangeListenerContainer,
    };
    use std::pin::Pin;

    fn assert_rows_equal(actual: &[SidebarRow], expected: &[SidebarRow]) {
        assert_eq!(actual.len(), expected.len());
        for (a, b) in actual.iter().zip(expected) {
            assert!(same_row(a, b), "{a:?} != {b:?}");
        }
    }

    fn accounts(count: i32, folders: i32) -> Vec<MailboxRow> {
        (1..=count)
            .flat_map(|account_id| {
                let account = MailboxRow {
                    account_id,
                    is_account: true,
                    can_create_children: true,
                    label: "Same display name".into(),
                    context: "Same display name".into(),
                    ..Default::default()
                };
                std::iter::once(account).chain((0..folders).map(move |folder| MailboxRow {
                    account_id,
                    folder_id: account_id * folders + folder,
                    scope: format!("Folder:{}", account_id * folders + folder).into(),
                    label: format!("Folder {folder}").into(),
                    context: "Same display name".into(),
                    depth: folder % 3,
                    has_children: folder % 3 == 0,
                    expanded: true,
                    ..Default::default()
                }))
            })
            .collect()
    }

    fn defaults() -> HashSet<String> {
        HashSet::from(["categories".into(), "global-labels".into()])
    }

    #[test]
    fn new_account_labels_start_collapsed_without_collapsing_accounts() {
        let mut initialized = HashSet::new();
        let mut collapsed = defaults();
        collapse_new_account_label_sections([1, 2], &mut initialized, &mut collapsed);
        assert!(!collapsed.contains("account:1"));
        assert!(collapsed.contains("account-labels:1"));
        assert!(!collapsed.contains("account:2"));
        assert!(collapsed.contains("account-labels:2"));

        collapsed.remove("account-labels:1");
        collapse_new_account_label_sections([1, 2, 3], &mut initialized, &mut collapsed);
        assert!(!collapsed.contains("account:1"));
        assert!(!collapsed.contains("account-labels:1"));
        assert!(!collapsed.contains("account:3"));
        assert!(collapsed.contains("account-labels:3"));
    }

    #[test]
    fn new_folder_branches_start_collapsed_without_overriding_user_expansion() {
        let mut initialized = HashSet::new();
        let mut collapsed = HashSet::new();
        collapse_new_folder_sections([10, 20], &mut initialized, &mut collapsed);
        assert_eq!(collapsed, HashSet::from([10, 20]));

        collapsed.remove(&10);
        collapse_new_folder_sections([10, 20, 30], &mut initialized, &mut collapsed);
        assert_eq!(collapsed, HashSet::from([20, 30]));
    }

    #[test]
    fn every_folder_occurs_once_and_stays_with_its_account() {
        for (account_count, folder_count) in [(1, 750), (5, 150), (50, 150)] {
            let rows = make_sidebar_rows(
                accounts(account_count, folder_count),
                vec![],
                vec![],
                &HashSet::new(),
                &defaults(),
            );
            assert_eq!(
                rows.iter().filter(|row| row.kind == Kind::Folder).count(),
                (account_count * folder_count) as usize
            );
            assert_eq!(
                rows.iter()
                    .map(|row| &row.key)
                    .collect::<HashSet<_>>()
                    .len(),
                rows.len()
            );
            let mut account = 0;
            for entry in rows {
                match entry.kind {
                    Kind::Account => account = entry.mailbox.account_id,
                    Kind::Folder | Kind::NewFolder => assert_eq!(entry.mailbox.account_id, account),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn account_order_and_interleaved_folders_use_ids_not_display_names() {
        let data = accounts(2, 2);
        let interleaved = [3, 1, 4, 0, 5, 2].map(|i| data[i].clone()).to_vec();
        let rows = make_sidebar_rows(interleaved, vec![], vec![], &HashSet::new(), &defaults());
        let ids: Vec<_> = rows
            .iter()
            .filter(|row| row.kind == Kind::Account || row.kind == Kind::Folder)
            .map(|row| (row.kind, row.mailbox.account_id, row.mailbox.folder_id))
            .collect();
        assert_eq!(
            ids,
            vec![
                (Kind::Account, 2, 0),
                (Kind::Folder, 2, 4),
                (Kind::Folder, 2, 5),
                (Kind::Account, 1, 0),
                (Kind::Folder, 1, 2),
                (Kind::Folder, 1, 3)
            ]
        );
    }

    #[test]
    fn same_named_labels_remain_distinct_under_their_accounts() {
        let labels = vec![
            MailLabelRow {
                id: 11,
                account_id: 1,
                name: "Travel".into(),
                ..Default::default()
            },
            MailLabelRow {
                id: 22,
                account_id: 2,
                name: "Travel".into(),
                ..Default::default()
            },
        ];
        let rows = make_sidebar_rows(
            accounts(2, 1),
            vec![],
            labels,
            &HashSet::new(),
            &HashSet::new(),
        );
        let account_labels = rows
            .iter()
            .filter(|row| row.kind == Kind::AccountLabel)
            .map(|row| (row.label.account_id, row.label.id, row.label.name.clone()))
            .collect::<Vec<_>>();

        assert_eq!(account_labels.len(), 2);
        assert_eq!(account_labels[0].0, 1);
        assert_eq!(account_labels[0].1, 11);
        assert_eq!(account_labels[0].2, "Travel");
        assert_eq!(account_labels[1].0, 2);
        assert_eq!(account_labels[1].1, 22);
        assert_eq!(account_labels[1].2, "Travel");
    }

    #[test]
    fn gmail_creates_labels_while_other_accounts_create_folders() {
        let rows = make_sidebar_rows(
            accounts(2, 0),
            vec![],
            vec![],
            &HashSet::from([1]),
            &HashSet::new(),
        );
        assert!(
            rows.iter()
                .any(|row| { row.kind == Kind::NewLabel && row.mailbox.account_id == 1 })
        );
        assert!(
            !rows
                .iter()
                .any(|row| { row.kind == Kind::NewFolder && row.mailbox.account_id == 1 })
        );
        assert!(
            rows.iter()
                .any(|row| { row.kind == Kind::NewFolder && row.mailbox.account_id == 2 })
        );
        assert!(
            !rows
                .iter()
                .any(|row| { row.kind == Kind::NewLabel && row.mailbox.account_id == 2 })
        );
    }

    #[test]
    fn account_without_root_creation_right_has_no_new_folder_action() {
        let mut rows_input = accounts(1, 0);
        rows_input[0].can_create_children = false;
        let rows = make_sidebar_rows(
            rows_input,
            vec![],
            vec![],
            &HashSet::new(),
            &HashSet::new(),
        );
        assert!(!rows.iter().any(|row| row.kind == Kind::NewFolder));
    }

    #[test]
    fn gmail_labels_form_a_collapsible_tree_without_inventing_parents() {
        let label = |id, name: &str| MailLabelRow {
            id,
            account_id: 1,
            name: name.into(),
            display_name: name.into(),
            ..Default::default()
        };
        let labels = vec![
            label(1, "Projects"),
            label(2, "Projects/Launch"),
            label(3, "Projects/Launch/Design"),
            label(4, "Orphan/Child"),
        ];
        let rows = make_sidebar_rows(
            accounts(1, 0),
            vec![],
            labels.clone(),
            &HashSet::from([1]),
            &HashSet::new(),
        );
        let visible = rows
            .iter()
            .filter(|row| row.kind == Kind::AccountLabel)
            .map(|row| {
                (
                    row.label.id,
                    row.label.display_name.to_string(),
                    row.label.depth,
                    row.label.has_children,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            visible,
            [
                (1, "Projects".into(), 0, true),
                (2, "Launch".into(), 1, true),
                (3, "Design".into(), 2, false),
                (4, "Orphan/Child".into(), 0, false),
            ]
        );

        let collapsed = make_sidebar_rows(
            accounts(1, 0),
            vec![],
            labels,
            &HashSet::from([1]),
            &HashSet::from(["gmail-label:1:1".into()]),
        );
        assert_eq!(
            collapsed
                .iter()
                .filter(|row| row.kind == Kind::AccountLabel)
                .map(|row| row.label.id)
                .collect::<Vec<_>>(),
            [1, 4]
        );
        assert!(!collapsed.iter().find(|row| row.label.id == 1).unwrap().label.expanded);
    }

    #[test]
    fn sidebar_filter_finds_account_and_global_labels() {
        let labels = vec![
            MailLabelRow {
                id: 11,
                account_id: 1,
                name: "Travel".into(),
                ..Default::default()
            },
            MailLabelRow {
                id: 22,
                name: "Travel plans".into(),
                is_global: true,
                ..Default::default()
            },
        ];
        let rows = make_filtered_sidebar_rows(accounts(2, 0), labels, "travel", &HashSet::new());
        assert!(
            rows.iter()
                .any(|row| row.kind == Kind::AccountLabel && row.label.id == 11)
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == Kind::GlobalLabel && row.label.id == 22)
        );
    }

    #[test]
    fn section_state_survives_parent_collapse_refresh_and_reorder() {
        let labels = vec![
            MailLabelRow {
                id: 1,
                is_auto: true,
                ..Default::default()
            },
            MailLabelRow {
                id: 2,
                is_global: true,
                ..Default::default()
            },
        ];
        let mut collapsed = HashSet::from(["account:1".into()]);
        let open = make_sidebar_rows(
            accounts(2, 3),
            vec![],
            labels.clone(),
            &HashSet::new(),
            &collapsed,
        );
        assert!(open.iter().any(|row| row.kind == Kind::Category));
        assert!(open.iter().any(|row| row.kind == Kind::GlobalLabel));
        assert!(
            !open.iter().any(|row| row.mailbox.account_id == 1
                && matches!(row.kind, Kind::Folder | Kind::NewFolder))
        );
        collapsed.insert("categories".into());
        collapsed.insert("global-labels".into());
        let closed = make_sidebar_rows(
            accounts(2, 3),
            vec![],
            labels.clone(),
            &HashSet::new(),
            &collapsed,
        );
        assert!(
            !closed
                .iter()
                .any(|row| matches!(row.kind, Kind::Category | Kind::GlobalLabel))
        );
        collapsed.remove("categories");
        collapsed.remove("global-labels");
        assert_rows_equal(
            &open,
            &make_sidebar_rows(accounts(2, 3), vec![], labels, &HashSet::new(), &collapsed),
        );
        let mut reordered = accounts(2, 3);
        reordered.rotate_left(4);
        let rows = make_sidebar_rows(reordered, vec![], vec![], &HashSet::new(), &collapsed);
        assert!(!rows.iter().find(|row| row.key == "account:1").unwrap().open);
        assert!(rows.iter().find(|row| row.key == "account:2").unwrap().open);
    }

    #[test]
    fn folder_filter_is_case_insensitive_and_keeps_account_context() {
        let mut data = accounts(2, 3);
        data[2].label = "Receipts".into();
        data[5].label = "RECEIPTS 2025".into();

        let rows = make_filtered_sidebar_rows(data, vec![], "receipts", &HashSet::new());
        assert_eq!(
            rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
            vec![Kind::Account, Kind::Folder, Kind::Account, Kind::Folder]
        );
        assert!(
            rows.iter()
                .filter(|row| row.kind == Kind::Account)
                .all(|row| row.open)
        );
        assert!(
            rows.iter()
                .filter(|row| row.kind == Kind::Folder)
                .all(|row| { row.mailbox.depth == 0 && !row.mailbox.has_children })
        );
    }

    #[derive(Debug, PartialEq)]
    enum Event {
        Added(usize, usize),
        Removed(usize, usize),
        Changed(usize),
    }
    struct Observer {
        model: Rc<SidebarModel>,
        events: Rc<RefCell<Vec<Event>>>,
    }
    impl Observer {
        fn record(&self, event: Event) {
            // Notifications must arrive after mutation with no outstanding borrow.
            for i in 0..self.model.row_count() {
                assert!(self.model.row_data(i).is_some());
            }
            self.events.borrow_mut().push(event);
        }
    }
    impl ModelChangeListener for Observer {
        fn row_changed(self: Pin<&Self>, row: usize) {
            self.record(Event::Changed(row));
        }
        fn row_added(self: Pin<&Self>, index: usize, count: usize) {
            self.record(Event::Added(index, count));
        }
        fn row_removed(self: Pin<&Self>, index: usize, count: usize) {
            self.record(Event::Removed(index, count));
        }
        fn reset(self: Pin<&Self>) {
            panic!("sidebar must not reset its model");
        }
    }

    #[test]
    fn refresh_is_silent_and_large_expansion_uses_one_range_notification() {
        let model = Rc::new(SidebarModel::default());
        let events = Rc::new(RefCell::new(Vec::new()));
        let observer = Box::pin(ModelChangeListenerContainer::new(Observer {
            model: model.clone(),
            events: events.clone(),
        }));
        model
            .model_tracker()
            .attach_peer(observer.as_ref().model_peer());
        let closed = vec![
            section(
                Kind::Account,
                "account:1",
                &HashSet::from(["account:1".into()]),
            ),
            row(Kind::Folder, "tail"),
        ];
        model.reconcile(closed.clone());
        events.borrow_mut().clear();
        model.reconcile(closed.clone());
        assert!(events.borrow().is_empty());
        let mut expanded = vec![section(Kind::Account, "account:1", &HashSet::new())];
        expanded.extend((0..10_000).map(|id| row(Kind::Folder, format!("folder:{id}"))));
        expanded.push(row(Kind::Folder, "tail"));
        model.reconcile(expanded.clone());
        assert_eq!(
            *events.borrow(),
            vec![Event::Added(1, 10_000), Event::Changed(0)]
        );
        events.borrow_mut().clear();
        expanded[500].mailbox.count = "99".into();
        model.reconcile(expanded.clone());
        assert_eq!(*events.borrow(), vec![Event::Changed(500)]);
        events.borrow_mut().clear();
        model.reconcile(closed.clone());
        assert_eq!(
            *events.borrow(),
            vec![Event::Removed(1, 10_000), Event::Changed(0)]
        );
        assert_rows_equal(&model.iter().collect::<Vec<_>>(), &closed);
    }

    #[test]
    fn reconciliation_handles_empty_replacement_and_reordering() {
        let model = SidebarModel::default();
        for keys in [
            vec![],
            vec!["a", "b", "c"],
            vec!["c", "a", "b"],
            vec!["x", "y"],
            vec![],
            vec!["tail"],
        ] {
            let rows: Vec<_> = keys.into_iter().map(|key| row(Kind::Folder, key)).collect();
            model.reconcile(rows.clone());
            assert_rows_equal(&model.iter().collect::<Vec<_>>(), &rows);
        }
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};

    struct Headless(Rc<MinimalSoftwareWindow>, Rc<Cell<Duration>>);
    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
        fn duration_since_start(&self) -> Duration {
            self.1.get()
        }
    }

    #[test]
    fn workspace_sidebars_survive_scrolling_and_layout_recreation() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let clock = Rc::new(Cell::new(Duration::ZERO));
        slint::platform::set_platform(Box::new(Headless(window.clone(), clock.clone()))).unwrap();
        let app = AppWindow::new().unwrap();
        app.set_startup_ready(true);
        app.set_startup_hydrated(true);
        app.set_connected_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
            id: 1,
            ..Default::default()
        }])));
        app.window().set_size(slint::PhysicalSize::new(1320, 800));
        let mut mailboxes = vec![MailboxRow {
            account_id: 1,
            label: "Large account".into(),
            is_account: true,
            ..Default::default()
        }];
        mailboxes.extend((0..1_000).map(|id| MailboxRow {
            account_id: 1,
            folder_id: id,
            scope: format!("Folder:{id}").into(),
            label: format!("Folder {id}").into(),
            expanded: true,
            ..Default::default()
        }));
        let collapsed = Rc::new(RefCell::new(HashSet::from([
            "categories".into(),
            "global-labels".into(),
        ])));
        let model = Rc::new(SidebarModel::default());
        model.reconcile(make_sidebar_rows(
            mailboxes.clone(),
            vec![],
            vec![],
            &HashSet::new(),
            &collapsed.borrow(),
        ));
        app.set_sidebar_rows(model.clone().into());
        let model_for_click = model.clone();
        let collapsed_for_click = collapsed.clone();
        app.on_toggle_sidebar_section(move |key, open| {
            if open {
                collapsed_for_click.borrow_mut().remove(key.as_str());
            } else {
                collapsed_for_click.borrow_mut().insert(key.to_string());
            }
            model_for_click.reconcile(make_sidebar_rows(
                mailboxes.clone(),
                vec![],
                vec![],
                &HashSet::new(),
                &collapsed_for_click.borrow(),
            ));
        });
        app.show().unwrap();
        let draw = || {
            // Finish smooth wheel/width animations without wall-clock sleeps.
            clock.set(clock.get() + Duration::from_secs(1));
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            window.draw_if_needed(|renderer| {
                let size = window.size();
                let mut pixels =
                    vec![slint::Rgb8Pixel::default(); (size.width * size.height) as usize];
                renderer.render(&mut pixels, size.width as usize);
                if let Some(directory) = std::env::var_os("FLECTAR_SIDEBAR_TEST_SCREENSHOTS") {
                    let directory = PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                    image::save_buffer(
                        directory.join(format!("frame-{:02}.png", clock.get().as_secs())),
                        &bytes,
                        size.width,
                        size.height,
                        image::ColorType::Rgb8,
                    )
                    .unwrap();
                }
            });
        };
        let click = |x, y| {
            let position = slint::LogicalPosition::new(x, y);
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
            draw();
        };
        draw();
        assert!(model.row_count() > 1_000);
        const CATEGORY_HEADER_CENTER_Y: f32 = 169.;
        const ACCOUNT_HEADER_CENTER_Y: f32 = 232.;
        click(140., ACCOUNT_HEADER_CENTER_Y);
        assert!(collapsed.borrow().contains("account:1"));
        assert!(model.row_count() < 10);
        // Hiding/showing the whole sidebar and switching desktop layouts
        // destroys delegates, but must retain the same account expansion state.
        app.set_mail_sidebar_collapsed(true);
        draw();
        app.set_mail_sidebar_collapsed(false);
        app.set_workspace_layout("minimal".into());
        draw();
        app.set_workspace_layout("default".into());
        draw();
        assert!(collapsed.borrow().contains("account:1"));
        click(140., ACCOUNT_HEADER_CENTER_Y);
        assert!(!collapsed.borrow().contains("account:1"));
        assert!(model.row_count() > 1_000);
        // Virtualized rows are recreated after scrolling in both directions.
        // Variable-height ListView estimates can change while scrolling. Use
        // a delta larger than the whole tree when returning to the top.
        for delta_y in [-10_000., 1_000_000.] {
            app.window().dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(140., 500.),
                delta_x: 0.,
                delta_y,
            });
            draw();
        }
        click(140., CATEGORY_HEADER_CENTER_Y);
        assert!(!collapsed.borrow().contains("categories"));
        app.window().set_size(slint::PhysicalSize::new(390, 844));
        draw();
        click(31., 29.); // Open the compact folder drawer.
        click(140., CATEGORY_HEADER_CENTER_Y);
        assert!(collapsed.borrow().contains("categories"));
        click(340., 200.); // Close the drawer through its backdrop.
        click(31., 29.);
        click(140., ACCOUNT_HEADER_CENTER_Y);
        assert!(collapsed.borrow().contains("account:1"));
        // Contacts uses the same retained update machinery and shared row controls.
        app.window().set_size(slint::PhysicalSize::new(1320, 800));
        app.set_active_view("contacts".into());
        app.set_connected_accounts(ModelRc::new(VecModel::from(
            (1..=750)
                .map(|id| AccountRow {
                    id,
                    name: format!("Account {id}").into(),
                    email: format!("account{id}@example.com").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        let contacts = Rc::new(RefCell::new(contacts::ContactDirectoryState::new(
            vec![],
            false,
        )));
        app.set_contact_sidebar_rows(contacts.borrow().sidebar_rows.clone().into());
        contacts::apply_contact_rows(&app, &contacts);
        let contacts_for_click = contacts.clone();
        let weak = app.as_weak();
        app.on_toggle_contact_section(move |key, open| {
            if open {
                contacts_for_click
                    .borrow_mut()
                    .collapsed_sections
                    .remove(key.as_str());
            } else {
                contacts_for_click
                    .borrow_mut()
                    .collapsed_sections
                    .insert(key.to_string());
            }
            contacts::refresh_contact_sidebar(&weak.unwrap(), &contacts_for_click);
        });
        let selected_scope = Rc::new(RefCell::new(String::new()));
        let selected_for_click = selected_scope.clone();
        app.on_select_contact_scope(move |scope| {
            *selected_for_click.borrow_mut() = scope.to_string()
        });
        draw();
        const CONTACT_FAVORITES_CENTER_Y: f32 = 182.;
        // The account disclosure follows the unified heading/section, three
        // unified scopes, and the accounts heading. Keep the click centered
        // in the 45 px Account 1 row rather than on the heading above it.
        const CONTACT_ACCOUNT_SECTION_CENTER_Y: f32 = 303.;
        click(140., CONTACT_FAVORITES_CENTER_Y);
        assert_eq!(*selected_scope.borrow(), "Favorites");
        click(140., CONTACT_ACCOUNT_SECTION_CENTER_Y);
        assert!(contacts.borrow().collapsed_sections.contains("account:1"));
        for delta_y in [-10_000., 1_000_000.] {
            app.window().dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(140., 500.),
                delta_x: 0.,
                delta_y,
            });
            draw();
        }
        app.set_contacts_sidebar_collapsed(true);
        draw();
        app.set_contacts_sidebar_collapsed(false);
        draw();
        assert!(contacts.borrow().collapsed_sections.contains("account:1"));
        click(140., CONTACT_ACCOUNT_SECTION_CENTER_Y);
        assert!(!contacts.borrow().collapsed_sections.contains("account:1"));

        // Calendar source toggles survive recycling and unrelated date changes.
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        let mut calendar = calendar::LocalCalendarState::new(today);
        calendar.sources = (1..=750)
            .map(|id| calendar::LocalCalendarSource {
                id,
                account_id: 1,
                name: format!("Calendar {id}"),
                color: String::new(),
                read_only: false,
                enabled: true,
                is_default: id == 1,
                last_synced_at: None,
            })
            .collect();
        let calendar = Rc::new(RefCell::new(calendar));
        app.set_calendar_sources(calendar.borrow().source_rows.clone().into());
        calendar::apply_calendar(&app, &calendar.borrow(), today);
        app.set_active_view("calendar".into());
        let calendar_for_click = calendar.clone();
        let weak = app.as_weak();
        app.on_calendar_set_source_enabled(move |id, enabled| {
            let mut state = calendar_for_click.borrow_mut();
            state
                .sources
                .iter_mut()
                .find(|source| source.id == i64::from(id))
                .unwrap()
                .enabled = enabled;
            calendar::apply_calendar(&weak.unwrap(), &state, today);
        });
        draw();
        click(82., 372.);
        assert!(!calendar.borrow().sources[0].enabled);
        for delta_y in [-10_000., 1_000_000.] {
            app.window().dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(140., 500.),
                delta_x: 0.,
                delta_y,
            });
            draw();
        }
        calendar.borrow_mut().selected_date += chrono::Duration::days(1);
        calendar::apply_calendar(&app, &calendar.borrow(), today);
        app.set_calendar_sidebar_collapsed(true);
        draw();
        app.set_calendar_sidebar_collapsed(false);
        draw();
        app.set_active_view("contacts".into());
        draw();
        app.set_active_view("calendar".into());
        draw();
        assert!(!calendar.borrow().source_rows.row_data(0).unwrap().enabled);
        click(82., 372.);
        assert!(calendar.borrow().sources[0].enabled);
        app.hide().unwrap();
    }
}
