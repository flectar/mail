//! Searches kept in the sidebar (#243). Opening one runs its query as a
//! search of the accounts in view, so the list, its actions and Search on
//! server work as for any search; its row in the sidebar stays selected while
//! the list shows that search.

use super::*;

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    let ui = app.global::<SavedSearchUi>();
    let controller = Rc::new(Controller {
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
    });
    let this = Rc::clone(&controller);
    ui.on_open(move |id| this.open(i64::from(id)));
    let this = Rc::clone(&controller);
    ui.on_start_save(move || this.start_save());
    let this = Rc::clone(&controller);
    ui.on_start_edit(move |id| this.start_edit(i64::from(id)));
    let this = Rc::clone(&controller);
    ui.on_start_delete(move |id| this.start_delete(i64::from(id)));
    let this = Rc::clone(&controller);
    ui.on_move(move |id, step| this.move_by(i64::from(id), step));
    let this = Rc::clone(&controller);
    ui.on_submit(move || this.submit());
    let this = Rc::clone(&controller);
    ui.on_confirm_delete(move || this.confirm_delete());
    let this = controller;
    ui.on_close(move || this.close());
}

/// The saved search the list shows: its query, run in the accounts in view.
pub(super) fn selected(state: &InboxState) -> Option<&mail::SavedSearchEntry> {
    let query = state.query.trim();
    if query.is_empty() || state.scope != Scope::UNIFIED_INBOX {
        return None;
    }
    state
        .saved_searches
        .iter()
        .find(|saved| saved.query == query)
}

/// Marks the saved search the list shows, which names the list too
/// (`mail_view_model::list_title`).
pub(super) fn show(app: &AppWindow, state: &InboxState) {
    app.global::<SavedSearchUi>().set_selected_id(
        selected(state)
            .and_then(|saved| i32::try_from(saved.id).ok())
            .unwrap_or(-1),
    );
}

/// Puts the saved searches' rows after what the accounts share, before the
/// accounts or, with one account, before mail kept on this device.
pub(super) fn place(
    rows: &mut Vec<SidebarRow>,
    saved_searches: &[mail::SavedSearchEntry],
    collapsed: &HashSet<String>,
    local_mail: Option<i32>,
) {
    let saved = self::rows(saved_searches, collapsed);
    if saved.is_empty() {
        return;
    }
    let at = rows
        .iter()
        .position(|row| row.kind == SidebarRowKind::AccountsHeading)
        .or_else(|| {
            rows.iter().position(|row| {
                row.kind == SidebarRowKind::Account && Some(row.mailbox.account_id) == local_mail
            })
        })
        .unwrap_or(rows.len());
    rows.splice(at..at, saved);
}

/// The sidebar's rows for the saved searches, under their own section.
fn rows(saved_searches: &[mail::SavedSearchEntry], collapsed: &HashSet<String>) -> Vec<SidebarRow> {
    if saved_searches.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![SidebarRow {
        key: "saved-searches".into(),
        kind: SidebarRowKind::SavedSearchesSection,
        open: !collapsed.contains("saved-searches"),
        show_row: true,
        ..Default::default()
    }];
    if collapsed.contains("saved-searches") {
        return rows;
    }
    rows.extend(saved_searches.iter().filter_map(|saved| {
        Some(SidebarRow {
            key: format!("saved-search:{}", saved.id).into(),
            kind: SidebarRowKind::SavedSearch,
            show_row: true,
            label: MailLabelRow {
                id: i32::try_from(saved.id).ok()?,
                name: saved.name.as_str().into(),
                display_name: saved.name.as_str().into(),
                name_has_emoji: contains_emoji(&saved.name),
                count: if saved.unread > 0 {
                    saved.unread.to_string().into()
                } else {
                    Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        })
    }));
    rows
}

struct Controller {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
}

impl Controller {
    fn open(&self, id: i64) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let Some(query) = self
            .state
            .borrow()
            .saved_searches
            .iter()
            .find(|saved| saved.id == id)
            .map(|saved| saved.query.clone())
        else {
            return;
        };
        app.set_search_query(query.as_str().into());
        app.invoke_search_changed(query.into());
        if self.state.borrow().scope != Scope::UNIFIED_INBOX {
            app.invoke_select_scope(Scope::UNIFIED_INBOX.into());
        }
    }

    /// Opens the dialog for keeping the list's search in the sidebar.
    fn start_save(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let (query, active) = {
            let state = self.state.borrow();
            (
                state.query.trim().to_owned(),
                state.account_presentation.active_profile.clone(),
            )
        };
        if query.is_empty() {
            return;
        }
        self.fill(&app, "save", -1, &query, &query, active.as_deref());
    }

    fn start_edit(&self, id: i64) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let Some(saved) = self.saved(id) else {
            return;
        };
        self.fill(
            &app,
            "edit",
            id,
            &saved.name,
            &saved.query,
            saved.profile_id.as_deref(),
        );
    }

    fn start_delete(&self, id: i64) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let Some(saved) = self.saved(id) else {
            return;
        };
        self.fill(&app, "delete", id, &saved.name, &saved.query, None);
    }

    fn saved(&self, id: i64) -> Option<mail::SavedSearchEntry> {
        self.state
            .borrow()
            .saved_searches
            .iter()
            .find(|saved| saved.id == id)
            .cloned()
    }

    /// Opens the dialog at `stage`, with "Show in" offering every profile
    /// and each one, on `profile`.
    fn fill(
        &self,
        app: &AppWindow,
        stage: &str,
        id: i64,
        name: &str,
        query: &str,
        profile: Option<&str>,
    ) {
        let profiles = self.state.borrow().account_presentation.profiles.clone();
        let ui = app.global::<SavedSearchUi>();
        let choices = if profiles.is_empty() {
            Vec::new()
        } else {
            std::iter::once(translated(app, &UiMessage::plain("All profiles")))
                .chain(profiles.iter().map(|profile| profile.name.as_str().into()))
                .collect()
        };
        let index = profile
            .and_then(|profile_id| profiles.iter().position(|profile| profile.id == profile_id))
            .map_or(0, |index| index + 1);
        ui.set_profiles(slint::ModelRc::new(slint::VecModel::from(choices)));
        ui.set_profile_index(i32::try_from(index).unwrap_or(0));
        ui.set_id(i32::try_from(id).unwrap_or(-1));
        ui.set_name(name.into());
        ui.set_query(query.into());
        ui.set_error("".into());
        ui.set_stage(stage.into());
    }

    fn submit(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<SavedSearchUi>();
        let id = (ui.get_stage() == "edit").then(|| i64::from(ui.get_id()));
        let name = ui.get_name().trim().to_owned();
        let query = ui.get_query().to_string();
        let taken = self
            .state
            .borrow()
            .saved_searches
            .iter()
            .any(|saved| Some(saved.id) != id && saved.name.eq_ignore_ascii_case(&name));
        if taken {
            let message = UiMessage::detail("A saved search named “{}” already exists.", &name);
            ui.set_error(translated(&app, &message));
            return;
        }
        let profile_id = usize::try_from(ui.get_profile_index())
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| {
                self.state
                    .borrow()
                    .account_presentation
                    .profiles
                    .get(index)
                    .map(|profile| profile.id.clone())
            });
        self.change(move |core| async move { core.save_search(id, name, query, profile_id).await });
    }

    fn confirm_delete(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let id = i64::from(app.global::<SavedSearchUi>().get_id());
        self.change(move |core| async move { core.delete_saved_search(id).await });
    }

    /// Moves a saved search a place up or down the sidebar.
    fn move_by(&self, id: i64, step: i32) {
        let Some(index) = self
            .state
            .borrow()
            .saved_searches
            .iter()
            .position(|saved| saved.id == id)
        else {
            return;
        };
        let Some(index) = index.checked_add_signed(step as isize) else {
            return;
        };
        self.change(move |core| async move { core.move_saved_search(id, index).await });
    }

    fn close(&self) {
        if let Some(app) = self.app.upgrade() {
            app.global::<SavedSearchUi>().set_stage("".into());
        }
    }

    /// Runs `work` against the core, then shows the saved searches as they
    /// are now. A failure keeps the dialog open with why.
    fn change<F, Fut>(&self, work: F)
    where
        F: FnOnce(CoreMailSource) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let Some(core) = self.state.borrow().core.clone() else {
            return;
        };
        let task = self.runtime.spawn(async move {
            work(core.clone()).await?;
            core.saved_searches().await
        });
        let abort = task.abort_handle();
        let app = self.app.clone();
        let state = Rc::clone(&self.state);
        let runtime = self.runtime.clone();
        let spawned = slint::spawn_local(async move {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let Some(app) = app.upgrade() else {
                return;
            };
            let ui = app.global::<SavedSearchUi>();
            match result {
                Ok(saved_searches) => {
                    ui.set_stage("".into());
                    state.borrow_mut().saved_searches = saved_searches;
                    refresh_sidebar(&state);
                    if let Err(error) = render_current(&app, &state, &runtime) {
                        app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
                    }
                }
                Err(error) => {
                    let message = UiMessage::detail("Couldn't save the search: {}", error);
                    ui.set_error(translated(&app, &message));
                }
            }
        });
        if spawned.is_err() {
            abort.abort();
        }
    }
}
