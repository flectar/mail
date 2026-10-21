//! Import flow for accounts from other mail applications and Flectar backups.
//!
//! Core reads and plans the source off the UI thread; this controller keeps
//! the reviewed plan, applies the user's selection, and presents results.
//! Accounts from other applications are written only after the user confirms
//! the review step; a Flectar backup is a restore and applies directly, as it
//! always has. A stale result from an abandoned step or an earlier opening
//! never replaces the current one.

use super::*;
use crate::data_controller::restore_account_backup;
use flectar_mail_core::migration::{
    self, AccountImportReport, AccountPlan, OtherEntryKind, PlanStatus, Severity, SignIn, Source,
    SourceError,
};

/// Reads one selected source on a blocking worker.
type SourceReader = Box<dyn FnOnce() -> Result<Source, SourceError> + Send>;

#[derive(Default)]
struct ImportSession {
    plans: Vec<AccountPlan>,
    selected: Vec<bool>,
    #[cfg(desktop)]
    profiles: Vec<migration::thunderbird::DetectedProfile>,
    /// Bumped whenever the dialog moves on, so late results are dropped.
    generation: u64,
    /// Bumped when the dialog opens or closes; profile discovery and pickers
    /// belong to one opening and stay valid across steps within it.
    opening: u64,
    /// A file or folder picker is open; a second one must not start.
    picking: bool,
}

impl ImportSession {
    fn advance(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }
}

/// Whether a source may be chosen now: the dialog is on its first step and
/// no picker is already open.
fn can_choose(app: &AppWindow, session: &ImportSession) -> bool {
    let ui = app.global::<AccountImport>();
    ui.get_open() && ui.get_stage() == "choose" && !session.picking
}

/// Marks a picker open and returns the opening its result belongs to.
fn start_picking(app: &AppWindow, session: &RefCell<ImportSession>) -> Option<u64> {
    let mut session = session.borrow_mut();
    if !can_choose(app, &session) {
        return None;
    }
    session.picking = true;
    Some(session.opening)
}

/// Ends a picker. Returns whether its result still applies: the user may
/// have closed the dialog, or reopened it, while the picker was up.
fn finish_picking(app: &AppWindow, session: &RefCell<ImportSession>, opening: u64) -> bool {
    let mut session = session.borrow_mut();
    if session.opening != opening {
        return false;
    }
    session.picking = false;
    can_choose(app, &session)
}

/// What reading a source produced, ready for the UI thread.
enum Inspected {
    Plans {
        source: migration::SourceKind,
        plans: Vec<AccountPlan>,
        other: Vec<OtherEntryKind>,
    },
    Backup(Vec<u8>),
}

fn source_error(error: SourceError) -> (&'static str, String) {
    match error {
        SourceError::Unreadable(detail) => ("unreadable", detail),
        SourceError::TooLarge => ("too-large", String::new()),
        SourceError::UnknownFormat => ("unknown-format", String::new()),
        SourceError::Malformed { reason, .. } => ("malformed", reason),
        SourceError::UnsupportedVersion(version) => ("unsupported-version", version),
        SourceError::NoAccounts => ("no-accounts", String::new()),
        SourceError::MultipleProfiles => ("multiple-profiles", String::new()),
        SourceError::NotAProfile => ("not-a-profile", String::new()),
        SourceError::NetworkProfile => ("network-profile", String::new()),
    }
}

fn show_error(app: &AppWindow, code: &str, detail: String) {
    let ui = app.global::<AccountImport>();
    ui.set_error_code(code.into());
    ui.set_error_detail(detail.into());
    ui.set_stage("error".into());
}

/// Core is missing only when startup could not open local storage.
fn show_storage_unavailable(app: &AppWindow) {
    let message = translated(
        app,
        &UiMessage::plain("Local data storage is unavailable. Retry startup."),
    );
    show_error(app, "failed", message.to_string());
}

fn account_row(plan: &AccountPlan, selected: bool) -> ImportAccountRow {
    let findings = plan
        .findings
        .iter()
        .map(|finding| ImportFindingRow {
            code: finding.code.as_str().into(),
            count: finding.detail.parse().unwrap_or_default(),
            detail: finding.detail.as_str().into(),
            blocking: finding.severity() == Severity::Blocking,
        })
        .collect::<Vec<_>>();
    ImportAccountRow {
        email: plan.email.as_str().into(),
        label: plan.label.as_deref().unwrap_or_default().into(),
        server: plan
            .draft
            .as_ref()
            .map(|draft| draft.config.imap_host.as_str())
            .unwrap_or_default()
            .into(),
        status: match plan.status {
            PlanStatus::Ready => "ready",
            PlanStatus::AlreadyAdded => "already-added",
            PlanStatus::Unsupported => "unsupported",
        }
        .into(),
        sign_in: match plan.sign_in {
            SignIn::Password => "password",
            SignIn::Google => "google",
            SignIn::Microsoft => "microsoft",
        }
        .into(),
        selected,
        findings: ModelRc::new(VecModel::from(findings)),
    }
}

fn show_review(app: &AppWindow, session: &ImportSession, source: migration::SourceKind) {
    let ui = app.global::<AccountImport>();
    let rows = session
        .plans
        .iter()
        .zip(&session.selected)
        .map(|(plan, selected)| account_row(plan, *selected))
        .collect::<Vec<_>>();
    ui.set_source(source.as_str().into());
    ui.set_accounts(ModelRc::new(VecModel::from(rows)));
    ui.set_selected_count(
        session
            .selected
            .iter()
            .filter(|selected| **selected)
            .count() as i32,
    );
    ui.set_stage("review".into());
}

fn show_report(app: &AppWindow, report: &AccountImportReport) {
    let ui = app.global::<AccountImport>();
    ui.set_imported(report.imported.len() as i32);
    ui.set_already_present(report.already_present.len() as i32);
    ui.set_restored_preferences(false);
    ui.set_failures(ModelRc::new(VecModel::from(
        report
            .failed
            .iter()
            .map(|failed| ImportFailureRow {
                email: failed.email.as_str().into(),
                error: failed.error.as_str().into(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_stage("done".into());
}

/// Reloads accounts after an import so the sidebar and settings list them
/// with their imported colors.
fn refresh_accounts(
    app: &AppWindow,
    core: &CoreMailSource,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
) {
    match runtime.block_on(core.load_settings()) {
        Ok(settings) => {
            state.borrow_mut().account_presentation =
                AccountPresentationSettings::from_settings(&settings);
        }
        Err(error) => tracing::warn!(%error, "reloading settings after import failed"),
    }
    match runtime.block_on(core.load_account_snapshot()) {
        Ok(snapshot) => {
            update_connected_accounts(app, state, runtime, snapshot.accounts, snapshot.configs);
            refresh_rows_only(app, state, runtime);
        }
        Err(error) => tracing::warn!(%error, "reloading accounts after import failed"),
    }
}

pub(super) fn register_import_callbacks(
    app: &AppWindow,
    documents: &std::sync::Arc<dyn documents::DocumentProvider>,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
) {
    let session = Rc::new(RefCell::new(ImportSession::default()));
    app.global::<AccountImport>()
        .set_folder_supported(cfg!(desktop));

    // Reads a file or folder off the UI thread, then plans it against the
    // accounts this device already has.
    let inspect: Rc<dyn Fn(SourceReader)> = {
        let app = app.as_weak();
        let session = Rc::clone(&session);
        let state = Rc::clone(state);
        let runtime = Rc::clone(runtime);
        Rc::new(move |read| {
            let Some(app) = app.upgrade() else {
                return;
            };
            let Some(core) = state.borrow().core.clone() else {
                show_storage_unavailable(&app);
                return;
            };
            let generation = session.borrow_mut().advance();
            app.global::<AccountImport>().set_stage("reading".into());
            let request: documents::DocumentFuture<Result<Inspected, (&'static str, String)>> =
                Box::pin(async move {
                    let source = tokio::task::spawn_blocking(read)
                        .await
                        .map_err(|error| error.to_string())?;
                    let inventory = match source {
                        Ok(Source::Accounts(inventory)) => inventory,
                        Ok(Source::FlectarBackup(bytes)) => {
                            return Ok(Ok(Inspected::Backup(bytes)));
                        }
                        Err(error) => return Ok(Err(source_error(error))),
                    };
                    let existing = core
                        .load_accounts()
                        .await?
                        .into_iter()
                        .map(|account| account.email)
                        .collect::<Vec<_>>();
                    Ok(Ok(Inspected::Plans {
                        source: inventory.source,
                        plans: migration::plan(&inventory, &existing),
                        other: inventory
                            .other_entries
                            .iter()
                            .map(|entry| entry.kind)
                            .collect(),
                    }))
                });
            let app_weak = app.as_weak();
            let session = Rc::clone(&session);
            let state = Rc::clone(&state);
            let runtime_for_restore = Rc::clone(&runtime);
            documents::run_request(&runtime, request, move |result| {
                let Some(app) = app_weak.upgrade() else {
                    return;
                };
                if session.borrow().generation != generation {
                    return;
                }
                let inspected = match result {
                    Ok(Ok(inspected)) => inspected,
                    Ok(Err((code, detail))) => return show_error(&app, code, detail),
                    Err(error) => return show_error(&app, "failed", error),
                };
                match inspected {
                    Inspected::Backup(bytes) => {
                        let Some(core) = state.borrow().core.clone() else {
                            return show_storage_unavailable(&app);
                        };
                        match restore_account_backup(
                            &app,
                            &bytes,
                            &core,
                            &state,
                            &runtime_for_restore,
                        ) {
                            Ok(restore) => {
                                let ui = app.global::<AccountImport>();
                                ui.set_imported(restore.imported as i32);
                                ui.set_already_present(restore.already_present as i32);
                                ui.set_restored_preferences(restore.preferences);
                                ui.set_failures(ModelRc::default());
                                ui.set_stage("done".into());
                            }
                            Err(error) => {
                                // A failure part way may still have added
                                // accounts; show whatever exists now.
                                refresh_accounts(&app, &core, &state, &runtime_for_restore);
                                show_error(&app, "restore-failed", error);
                            }
                        }
                    }
                    Inspected::Plans {
                        source,
                        plans,
                        other,
                    } => {
                        let ui = app.global::<AccountImport>();
                        let count =
                            |kind| other.iter().filter(|entry| **entry == kind).count() as i32;
                        ui.set_local_folders(count(OtherEntryKind::LocalFolders));
                        ui.set_feeds(count(OtherEntryKind::Feeds));
                        ui.set_newsgroups(count(OtherEntryKind::Newsgroups));
                        let mut session = session.borrow_mut();
                        // Every importable account starts selected, as in
                        // Thunderbird's and K-9's own import screens.
                        session.selected = plans
                            .iter()
                            .map(|plan| plan.status == PlanStatus::Ready)
                            .collect();
                        session.plans = plans;
                        show_review(&app, &session, source);
                    }
                }
            });
        })
    };

    let app_weak = app.as_weak();
    let open_session = Rc::clone(&session);
    let open_runtime = Rc::clone(runtime);
    app.on_import_accounts(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        // Focus can reach the entry points behind the dialog; reopening must
        // not discard an import in progress or its report.
        if app.global::<AccountImport>().get_open() {
            return;
        }
        let opening = {
            let mut session = open_session.borrow_mut();
            session.advance();
            session.opening += 1;
            session.opening
        };
        let ui = app.global::<AccountImport>();
        ui.set_stage("choose".into());
        ui.set_accounts(ModelRc::default());
        ui.set_profiles(ModelRc::default());
        ui.set_open(true);
        #[cfg(desktop)]
        {
            let app_weak = app.as_weak();
            let session = Rc::clone(&open_session);
            let request: documents::DocumentFuture<Vec<migration::thunderbird::DetectedProfile>> =
                Box::pin(async {
                    tokio::task::spawn_blocking(migration::thunderbird::discover_profiles)
                        .await
                        .map_err(|error| error.to_string())
                });
            documents::run_request(&open_runtime, request, move |profiles| {
                let (Some(app), Ok(profiles)) = (app_weak.upgrade(), profiles) else {
                    return;
                };
                let mut session = session.borrow_mut();
                if session.opening != opening {
                    return;
                }
                app.global::<AccountImport>()
                    .set_profiles(ModelRc::new(VecModel::from(
                        profiles
                            .iter()
                            .map(|profile| ImportProfileRow {
                                name: profile.name.as_str().into(),
                                path: profile.path.display().to_string().into(),
                                is_default: profile.is_default,
                            })
                            .collect::<Vec<_>>(),
                    )));
                session.profiles = profiles;
            });
        }
        #[cfg(not(desktop))]
        let _ = (&open_runtime, opening);
    });

    let app_weak = app.as_weak();
    let file_documents = documents.clone();
    let file_runtime = Rc::clone(runtime);
    let file_inspect = Rc::clone(&inspect);
    let file_session = Rc::clone(&session);
    app.global::<AccountImport>().on_choose_file(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(opening) = start_picking(&app, &file_session) else {
            return;
        };
        let dialog = documents::DocumentDialog {
            title: translated(&app, &UiMessage::plain("Import accounts")).to_string(),
            filter: Some((
                translated(&app, &UiMessage::plain("Account settings")).to_string(),
                &["k9s", "zip", "js", "json"],
            )),
            // Imports read files the user picks; the download location does
            // not apply here.
            directory: None,
        };
        let inspect = Rc::clone(&file_inspect);
        let session = Rc::clone(&file_session);
        let app_weak = app.as_weak();
        documents::run_request(
            &file_runtime,
            file_documents.import_with(dialog),
            move |picked| {
                let Some(app) = app_weak.upgrade() else {
                    return;
                };
                if !finish_picking(&app, &session, opening) {
                    return;
                }
                match picked {
                    // The document is moved into the reader so a temporary copy
                    // lives exactly as long as it is being read.
                    Ok(Some(document)) => {
                        inspect(Box::new(move || migration::open(document.path())))
                    }
                    Ok(None) => {}
                    Err(error) => show_error(&app, "unreadable", error),
                }
            },
        );
    });

    #[cfg(desktop)]
    {
        let app_weak = app.as_weak();
        let folder_runtime = Rc::clone(runtime);
        let folder_inspect = Rc::clone(&inspect);
        let folder_session = Rc::clone(&session);
        app.global::<AccountImport>().on_choose_folder(move || {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            let Some(opening) = start_picking(&app, &folder_session) else {
                return;
            };
            let title = translated(
                &app,
                &UiMessage::plain("Choose a Thunderbird profile folder"),
            );
            let request: documents::DocumentFuture<Option<PathBuf>> = Box::pin(async move {
                Ok(rfd::AsyncFileDialog::new()
                    .set_title(title.as_str())
                    .pick_folder()
                    .await
                    .map(|folder| folder.path().to_owned()))
            });
            let inspect = Rc::clone(&folder_inspect);
            let session = Rc::clone(&folder_session);
            let app_weak = app.as_weak();
            documents::run_request(&folder_runtime, request, move |picked| {
                let Some(app) = app_weak.upgrade() else {
                    return;
                };
                if !finish_picking(&app, &session, opening) {
                    return;
                }
                match picked {
                    Ok(Some(folder)) => inspect(Box::new(move || migration::open(&folder))),
                    Ok(None) => {}
                    Err(error) => show_error(&app, "unreadable", error),
                }
            });
        });

        let app_weak = app.as_weak();
        let profile_session = Rc::clone(&session);
        let profile_inspect = Rc::clone(&inspect);
        app.global::<AccountImport>()
            .on_choose_profile(move |index| {
                let Some(app) = app_weak.upgrade() else {
                    return;
                };
                let path = {
                    let session = profile_session.borrow();
                    if !can_choose(&app, &session) {
                        return;
                    }
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| session.profiles.get(index).cloned())
                        .map(|profile| profile.path)
                };
                if let Some(path) = path {
                    profile_inspect(Box::new(move || migration::open(&path)));
                }
            });
    }

    let app_weak = app.as_weak();
    let toggle_session = Rc::clone(&session);
    app.global::<AccountImport>()
        .on_toggle(move |index, checked| {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            let mut session = toggle_session.borrow_mut();
            let Some(index) = usize::try_from(index)
                .ok()
                .filter(|index| *index < session.plans.len())
            else {
                return;
            };
            let checked = checked && session.plans[index].status == PlanStatus::Ready;
            session.selected[index] = checked;
            let ui = app.global::<AccountImport>();
            let rows = ui.get_accounts();
            if let Some(mut row) = rows.row_data(index) {
                row.selected = checked;
                rows.set_row_data(index, row);
            }
            ui.set_selected_count(
                session
                    .selected
                    .iter()
                    .filter(|selected| **selected)
                    .count() as i32,
            );
        });

    let app_weak = app.as_weak();
    let confirm_session = Rc::clone(&session);
    let confirm_state = Rc::clone(state);
    let confirm_runtime = Rc::clone(runtime);
    app.global::<AccountImport>().on_confirm(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        // A second activation while importing must not import twice.
        if app.global::<AccountImport>().get_stage() != "review" {
            return;
        }
        let Some(core) = confirm_state.borrow().core.clone() else {
            return show_storage_unavailable(&app);
        };
        let drafts = {
            let session = confirm_session.borrow();
            session
                .plans
                .iter()
                .zip(&session.selected)
                .filter(|(_, selected)| **selected)
                .filter_map(|(plan, _)| plan.draft.clone())
                .collect::<Vec<_>>()
        };
        if drafts.is_empty() {
            return;
        }
        let generation = confirm_session.borrow_mut().advance();
        app.global::<AccountImport>().set_stage("importing".into());
        let import_core = core.clone();
        let request: documents::DocumentFuture<AccountImportReport> =
            Box::pin(async move { import_core.import_accounts(drafts).await });
        let app_weak = app.as_weak();
        let session = Rc::clone(&confirm_session);
        let state = Rc::clone(&confirm_state);
        let runtime = Rc::clone(&confirm_runtime);
        documents::run_request(&confirm_runtime, request, move |result| {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            // Accounts may exist even if the dialog was closed meanwhile.
            refresh_accounts(&app, &core, &state, &runtime);
            if session.borrow().generation != generation {
                return;
            }
            match result {
                Ok(report) => show_report(&app, &report),
                Err(error) => show_error(&app, "failed", error),
            }
        });
    });

    let app_weak = app.as_weak();
    let back_session = Rc::clone(&session);
    app.global::<AccountImport>().on_back(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        back_session.borrow_mut().advance();
        app.global::<AccountImport>().set_stage("choose".into());
    });

    let app_weak = app.as_weak();
    let close_session = Rc::clone(&session);
    app.global::<AccountImport>().on_close(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut session = close_session.borrow_mut();
        let generation = session.advance();
        *session = ImportSession {
            generation,
            opening: session.opening + 1,
            ..ImportSession::default()
        };
        let ui = app.global::<AccountImport>();
        ui.set_open(false);
        ui.set_accounts(ModelRc::default());
        ui.set_failures(ModelRc::default());
    });
}
