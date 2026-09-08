//! Visible sidebar projection and batched, retained Slint model updates.
use super::*;

/// Keep one model for all responsive presentations. A splice emits range
/// notifications so expanding thousands of descendants moves the tail once,
/// without resetting the ListView or doing one Vec insertion per folder.
#[derive(Default)]
pub(super) struct SidebarModel {
    rows: RefCell<Vec<SidebarRow>>,
    notify: slint::ModelNotify,
}

impl Model for SidebarModel {
    type Data = SidebarRow;

    fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<SidebarRow> {
        self.rows.borrow().get(row).cloned()
    }

    fn model_tracker(&self) -> &dyn slint::ModelTracker {
        &self.notify
    }
}

impl SidebarModel {
    fn reconcile(&self, rows: Vec<SidebarRow>) {
        let current = self.rows.borrow();
        let prefix = current
            .iter()
            .zip(&rows)
            .take_while(|(a, b)| a.key == b.key)
            .count();
        let suffix = current[prefix..]
            .iter()
            .rev()
            .zip(rows[prefix..].iter().rev())
            .take_while(|(a, b)| a.key == b.key)
            .count();
        let removed = current.len() - prefix - suffix;
        let added = rows.len() - prefix - suffix;
        drop(current);
        if removed > 0 {
            self.rows.borrow_mut().drain(prefix..prefix + removed);
            self.notify.row_removed(prefix, removed);
        }
        if added > 0 {
            self.rows
                .borrow_mut()
                .splice(prefix..prefix, rows[prefix..prefix + added].iter().cloned());
            self.notify.row_added(prefix, added);
        }
        for (index, row) in rows.into_iter().enumerate() {
            if (prefix..prefix + added).contains(&index) {
                continue;
            }
            if !same_row(&self.rows.borrow()[index], &row) {
                self.rows.borrow_mut()[index] = row;
                self.notify.row_changed(index);
            }
        }
    }
}

// Slint 1.17.1 considers Image::default() unequal even to itself. Compare
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
    let state = state.borrow();
    let rows = make_sidebar_rows(
        make_mailbox_rows(
            &state.mailboxes,
            &state.profile_avatar_images,
            &state.labels,
            &state.collapsed_folder_ids,
        ),
        make_mailbox_rows(
            &state.unified_mailboxes,
            &state.profile_avatar_images,
            &state.labels,
            &HashSet::new(),
        ),
        make_label_rows(&state.labels, None, ""),
        &state.collapsed_sidebar_sections,
    );
    // Model notifications can synchronously inspect application state.
    let model = Rc::clone(&state.sidebar_rows);
    drop(state);
    model.reconcile(rows);
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

fn make_sidebar_rows(
    mailboxes: Vec<MailboxRow>,
    unified: Vec<MailboxRow>,
    labels: Vec<MailLabelRow>,
    collapsed: &HashSet<String>,
) -> Vec<SidebarRow> {
    use SidebarRowKind as Kind;
    let mut rows = vec![
        row(Kind::UnifiedHeading, "unified-heading"),
        section(Kind::UnifiedSection, "unified", collapsed),
    ];
    if !collapsed.contains("unified") {
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
        if labels.iter().any(|label| !label.is_auto) {
            rows.push(section(Kind::LabelsSection, "labels", collapsed));
            if !collapsed.contains("labels") {
                rows.extend(
                    labels
                        .into_iter()
                        .filter(|label| !label.is_auto)
                        .map(|label| SidebarRow {
                            key: format!("label:{}", label.id).into(),
                            label,
                            ..row(Kind::Label, "")
                        }),
                );
            }
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
            rows.push(SidebarRow {
                mailbox: MailboxRow {
                    account_id,
                    folder_id: -1,
                    ..Default::default()
                },
                ..row(Kind::NewFolder, format!("new-folder:{account_id}"))
            });
        }
    }
    rows.push(row(Kind::AddAccount, "add-account"));
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
        HashSet::from(["categories".into(), "labels".into()])
    }

    #[test]
    fn every_folder_occurs_once_and_stays_with_its_account() {
        for (account_count, folder_count) in [(1, 750), (5, 150), (50, 150)] {
            let rows = make_sidebar_rows(
                accounts(account_count, folder_count),
                vec![],
                vec![],
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
        let rows = make_sidebar_rows(interleaved, vec![], vec![], &defaults());
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
    fn section_state_survives_parent_collapse_refresh_and_reorder() {
        let labels = vec![
            MailLabelRow {
                id: 1,
                is_auto: true,
                ..Default::default()
            },
            MailLabelRow {
                id: 2,
                ..Default::default()
            },
        ];
        let mut collapsed = HashSet::from(["account:1".into()]);
        let open = make_sidebar_rows(accounts(2, 3), vec![], labels.clone(), &collapsed);
        assert!(open.iter().any(|row| row.kind == Kind::Category));
        assert!(open.iter().any(|row| row.kind == Kind::Label));
        assert!(
            !open.iter().any(|row| row.mailbox.account_id == 1
                && matches!(row.kind, Kind::Folder | Kind::NewFolder))
        );
        collapsed.insert("unified".into());
        let closed = make_sidebar_rows(accounts(2, 3), vec![], labels.clone(), &collapsed);
        assert!(!closed.iter().any(|row| matches!(
            row.kind,
            Kind::Category | Kind::Label | Kind::CategoriesSection | Kind::LabelsSection
        )));
        collapsed.remove("unified");
        assert_rows_equal(
            &open,
            &make_sidebar_rows(accounts(2, 3), vec![], labels, &collapsed),
        );
        let mut reordered = accounts(2, 3);
        reordered.rotate_left(4);
        let rows = make_sidebar_rows(reordered, vec![], vec![], &collapsed);
        assert!(!rows.iter().find(|row| row.key == "account:1").unwrap().open);
        assert!(rows.iter().find(|row| row.key == "account:2").unwrap().open);
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
            row(Kind::AddAccount, "tail"),
        ];
        model.reconcile(closed.clone());
        events.borrow_mut().clear();
        model.reconcile(closed.clone());
        assert!(events.borrow().is_empty());
        let mut expanded = vec![section(Kind::Account, "account:1", &HashSet::new())];
        expanded.extend((0..10_000).map(|id| row(Kind::Folder, format!("folder:{id}"))));
        expanded.push(row(Kind::AddAccount, "tail"));
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
    fn sidebar_sections_survive_scrolling_and_layout_recreation() {
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
            "labels".into(),
        ])));
        let model = Rc::new(SidebarModel::default());
        model.reconcile(make_sidebar_rows(
            mailboxes.clone(),
            vec![],
            vec![],
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
        click(140., 220.); // Account header below the empty unified section.
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
        click(140., 220.);
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
        click(140., 94.); // Unified header still has its host-owned state.
        assert!(collapsed.borrow().contains("unified"));
        app.window().set_size(slint::PhysicalSize::new(390, 844));
        draw();
        click(31., 29.); // Open the compact folder drawer.
        click(140., 94.);
        assert!(!collapsed.borrow().contains("unified"));
        click(340., 200.); // Close the drawer through its backdrop.
        click(31., 29.);
        click(140., 220.);
        assert!(collapsed.borrow().contains("account:1"));
        app.hide().unwrap();
    }
}
