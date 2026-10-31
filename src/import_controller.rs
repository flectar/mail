//! Import flow for accounts from other mail applications and Flectar backups,
//! and for the mail other applications keep or that was saved to files.
//!
//! Core reads and plans the source off the UI thread; this controller keeps
//! the reviewed plan, applies the user's selection, and presents results.
//! Accounts from other applications, and mail, are written only after the
//! user confirms the review steps; a Flectar backup is a restore and applies
//! directly, as it always has. A stale result from an abandoned step or an
//! earlier opening never replaces the current one.

use super::*;
use crate::data_controller::{format_storage_size, restore_account_backup};
use flectar_mail_core::mail_import::{
    self, ChosenFile, Destination, FolderKind, MailImportProgress, MailImportRequest, MailSource,
    SourceFolder,
};
use flectar_mail_core::migration::{
    self, AccountImportReport, AccountPlan, MailStore, OtherEntryKind, PlanStatus, Severity,
    SignIn, Source, SourceError,
};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::watch;

/// Reads one selected source on a blocking worker.
type SourceReader = Box<dyn FnOnce() -> Result<Source, SourceError> + Send>;

/// The most files chosen at once that a phone copies.
const MAX_CHOSEN_FILES: usize = 1000;

#[derive(Default)]
struct ImportSession {
    plans: Vec<AccountPlan>,
    selected: Vec<bool>,
    source: Option<migration::SourceKind>,
    /// Mail the source keeps, by store, and the folders chosen from it.
    mail: Vec<MailChoice>,
    /// Where mail can go: this device, then accounts.
    destinations: Vec<Option<MailAccount>>,
    /// What each mail row shows: a store, or one of its folders.
    rows: Vec<(usize, Option<usize>)>,
    /// Addresses the source's accounts send from. Mail from them is filed
    /// as sent.
    own_addresses: Vec<String>,
    /// The chosen files. A phone's copy of one must outlive the mail import.
    documents: Vec<documents::ImportedDocument>,
    /// Stops the mail import that is running.
    stop: Option<Arc<AtomicBool>>,
    /// The first folder imported mail went into.
    imported_folder: Option<i64>,
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

    /// Forget the chosen source, as when going back to choose another.
    fn clear_source(&mut self) {
        self.plans.clear();
        self.selected.clear();
        self.source = None;
        self.mail.clear();
        self.destinations.clear();
        self.rows.clear();
        self.own_addresses.clear();
        self.documents.clear();
    }

    fn selected_drafts(&self) -> Vec<migration::AccountDraft> {
        self.plans
            .iter()
            .zip(&self.selected)
            .filter(|(_, selected)| **selected)
            .filter_map(|(plan, _)| plan.draft.clone())
            .collect()
    }
}

/// An account mail can go into, by address and protocol: one here, or one
/// chosen for import.
#[derive(Debug, Clone, PartialEq)]
struct MailAccount {
    email: String,
    protocol: MailProtocol,
}

impl MailAccount {
    /// Its mail is on its server, so mail imported into it is uploaded
    /// there. A POP3 account's exists only on this device.
    fn uploads(&self) -> bool {
        self.protocol != MailProtocol::Pop3
    }
}

/// One store of mail, the folders chosen from it, and where it goes.
struct MailChoice {
    store: MailStore,
    selected: Vec<bool>,
    /// An index into the session's destinations.
    destination: usize,
    /// The folder its mail goes in on an account's server; without one, the
    /// folder for messages chosen one by one.
    upload_folder: Option<String>,
}

impl MailChoice {
    /// A store of an app's profile, whose mail is named after it on a
    /// server.
    fn from_profile(store: MailStore) -> Self {
        let upload_folder = Some(store.label.clone());
        Self {
            upload_folder,
            ..Self::new(store)
        }
    }

    fn new(store: MailStore) -> Self {
        // Mail that was thrown away or was junk is left out unless chosen.
        let tops = store
            .mail
            .folders
            .iter()
            .filter(|folder| folder.path.len() == 1)
            .map(|folder| (folder.path[0].as_str(), folder.kind))
            .collect::<HashMap<_, _>>();
        let selected = store
            .mail
            .folders
            .iter()
            .map(|folder| {
                !folder.path.first().is_some_and(|top| {
                    matches!(
                        tops.get(top.as_str()),
                        Some(FolderKind::Trash | FolderKind::Junk)
                    )
                })
            })
            .collect();
        Self {
            store,
            selected,
            destination: 0,
            upload_folder: None,
        }
    }

    fn folders(&self) -> &[SourceFolder] {
        &self.store.mail.folders
    }

    /// The folders below folder `index`, which follow it in the list.
    fn subfolders(&self, index: usize) -> std::ops::Range<usize> {
        let folders = self.folders();
        let path = &folders[index].path;
        let end = if path.is_empty() {
            index + 1
        } else {
            folders[index + 1..]
                .iter()
                .position(|folder| {
                    folder.path.len() <= path.len() || !folder.path.starts_with(path)
                })
                .map_or(folders.len(), |offset| index + 1 + offset)
        };
        index + 1..end
    }

    /// The chosen folders that hold messages themselves.
    fn chosen(&self) -> impl Iterator<Item = usize> {
        (0..self.folders().len())
            .filter(|index| self.selected[*index] && self.folders()[*index].has_messages())
    }

    /// Whether all, or some, of the folders in `range` that hold messages are
    /// chosen.
    fn state(&self, range: std::ops::Range<usize>) -> (bool, bool) {
        let mut all = true;
        let mut any = false;
        let folders = &self.folders()[range.clone()];
        for (folder, selected) in folders.iter().zip(&self.selected[range]) {
            if folder.has_messages() {
                all &= *selected;
                any |= *selected;
            }
        }
        (all && any, any && !all)
    }
}

/// Mail of one store to import.
struct MailJob {
    label: String,
    source: MailSource,
    folders: Vec<usize>,
    bytes: u64,
    destination: Option<MailAccount>,
    /// The folder the mail goes in on this device, if it goes there.
    parent: Option<String>,
    /// The folder it goes in on an account's server.
    upload_folder: Option<String>,
}

/// How far the whole import is, across stores.
#[derive(Debug, Clone, Default)]
struct Progress {
    /// Mail is being imported, after any accounts.
    mail: bool,
    done_bytes: u64,
    total_bytes: u64,
    imported: u64,
    /// The folder being imported.
    folder: String,
}

#[derive(Debug, Default)]
struct MailOutcome {
    imported: u64,
    already_present: u64,
    too_large: u64,
    /// Messages a server refused, and why it refused the first.
    refused: u64,
    refusal: Option<String>,
    stopped: bool,
    /// Folders not read to the end, by name, with the reason.
    unreadable: Vec<(String, String)>,
    folders: Vec<i64>,
    error: Option<String>,
}

struct ImportOutcome {
    accounts: Option<AccountImportReport>,
    mail: Option<MailOutcome>,
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
    // A phone copies what is chosen before it can be read, which takes a
    // while for a large file; the dialog behind its picker shows that.
    if cfg!(mobile) {
        app.global::<AccountImport>().set_stage("reading".into());
    }
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
    let ui = app.global::<AccountImport>();
    if cfg!(mobile) && ui.get_stage() == "reading" {
        ui.set_stage("choose".into());
    }
    can_choose(app, &session)
}

/// What reading a source produced, ready for the UI thread.
enum Inspected {
    Plans {
        source: migration::SourceKind,
        plans: Vec<AccountPlan>,
        other: Vec<OtherEntryKind>,
        mail: Vec<MailStore>,
        own_addresses: Vec<String>,
    },
    Backup(Vec<u8>),
    Mail(MailSource),
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
        SourceError::NoMail => ("no-mail", String::new()),
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
            .map(|draft| draft.config.incoming_host.as_str())
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

fn show_review(app: &AppWindow, session: &ImportSession) {
    let ui = app.global::<AccountImport>();
    let rows = session
        .plans
        .iter()
        .zip(&session.selected)
        .map(|(plan, selected)| account_row(plan, *selected))
        .collect::<Vec<_>>();
    ui.set_source(
        session
            .source
            .map(migration::SourceKind::as_str)
            .unwrap_or_default()
            .into(),
    );
    ui.set_accounts(ModelRc::new(VecModel::from(rows)));
    ui.set_selected_count(
        session
            .selected
            .iter()
            .filter(|selected| **selected)
            .count() as i32,
    );
    ui.set_has_mail(!session.mail.is_empty());
    ui.set_stage("review".into());
}

/// The accounts mail can go into: those here, and the POP3 accounts chosen
/// for import. A POP3 account's mail, like local mail, exists only on this
/// device; mail for another account is uploaded to its server, so it needs
/// one already signed in.
fn mail_destinations(session: &ImportSession, accounts: &[Account]) -> Vec<Option<MailAccount>> {
    let mut destinations: Vec<Option<MailAccount>> = vec![None];
    let here = accounts.iter().map(|account| MailAccount {
        email: account.email.clone(),
        protocol: account.mail_protocol,
    });
    let chosen = session
        .plans
        .iter()
        .zip(&session.selected)
        .filter(|(_, selected)| **selected)
        .filter_map(|(plan, _)| plan.draft.as_ref())
        .filter(|draft| draft.config.mail_protocol == MailProtocol::Pop3)
        .map(|draft| MailAccount {
            email: draft.config.email.clone(),
            protocol: MailProtocol::Pop3,
        });
    for account in here.chain(chosen) {
        if !destinations.iter().flatten().any(|known| {
            known.protocol == account.protocol && known.email.eq_ignore_ascii_case(&account.email)
        }) {
            destinations.push(Some(account));
        }
    }
    destinations
}

/// Where a store's mail goes unless the user says otherwise: a POP3
/// account's mail into that account, everything else onto this device.
fn default_destination(
    session: &ImportSession,
    destinations: &[Option<MailAccount>],
    store: &MailStore,
) -> usize {
    let Some(email) = store.account.as_ref().and_then(|account| {
        session
            .plans
            .iter()
            .find(|plan| plan.source_id == *account)
            .map(|plan| plan.email.as_str())
    }) else {
        return 0;
    };
    destinations
        .iter()
        .position(|destination| {
            destination
                .as_ref()
                .is_some_and(|known| !known.uploads() && known.email.eq_ignore_ascii_case(email))
        })
        .unwrap_or(0)
}

/// How the destination menu names a destination: by address, with the
/// protocol where an address has more than one account.
fn destination_name(destinations: &[Option<MailAccount>], index: usize, device: &str) -> String {
    let Some(account) = &destinations[index] else {
        return device.to_owned();
    };
    let shared = destinations.iter().flatten().any(|other| {
        other.protocol != account.protocol && other.email.eq_ignore_ascii_case(&account.email)
    });
    if !shared {
        return account.email.clone();
    }
    let protocol = match account.protocol {
        MailProtocol::Imap => "IMAP",
        MailProtocol::Jmap => "JMAP",
        MailProtocol::Gmail => "Gmail",
        MailProtocol::Pop3 => "POP3",
        MailProtocol::Local => "",
    };
    format!("{} ({protocol})", account.email)
}

/// Present the mail step for the accounts chosen before it.
fn show_mail(app: &AppWindow, session: &mut ImportSession, accounts: &[Account]) {
    let destinations = mail_destinations(session, accounts);
    let previous = std::mem::take(&mut session.destinations);
    let chosen = session
        .mail
        .iter()
        .map(|choice| {
            // A destination chosen before is kept while it is still offered.
            previous
                .get(choice.destination)
                .filter(|_| choice.destination > 0)
                .and_then(|chosen| destinations.iter().position(|offered| offered == chosen))
                .unwrap_or_else(|| default_destination(session, &destinations, &choice.store))
        })
        .collect::<Vec<_>>();
    for (choice, destination) in session.mail.iter_mut().zip(chosen) {
        choice.destination = destination;
    }
    session.destinations = destinations;
    let device = translated(app, &UiMessage::plain("On this device")).to_string();
    let ui = app.global::<AccountImport>();
    ui.set_destinations(ModelRc::new(VecModel::from(
        (0..session.destinations.len())
            .map(|index| destination_name(&session.destinations, index, &device).into())
            .collect::<Vec<slint::SharedString>>(),
    )));
    show_mail_rows(app, session);
    ui.set_stage("mail".into());
}

fn show_mail_rows(app: &AppWindow, session: &mut ImportSession) {
    let loose = translated(app, &UiMessage::plain("Imported messages")).to_string();
    let mut rows = Vec::new();
    let mut models = Vec::new();
    let mut chosen = 0;
    let mut chosen_bytes = 0;
    for (store_index, choice) in session.mail.iter().enumerate() {
        let (selected, partial) = choice.state(0..choice.folders().len());
        let uploads = session
            .destinations
            .get(choice.destination)
            .and_then(Option::as_ref)
            .is_some_and(MailAccount::uploads);
        rows.push((store_index, None));
        models.push(ImportMailRow {
            is_store: true,
            name: choice.store.label.as_str().into(),
            depth: 0,
            size: format_storage_size(choice.store.mail.size()).into(),
            selected,
            partial,
            destination: choice.destination as i32,
            folder: if choice.store.account.is_some() {
                choice.store.label.as_str().into()
            } else {
                slint::SharedString::default()
            },
            upload_folder: if uploads {
                choice
                    .upload_folder
                    .as_deref()
                    .unwrap_or(loose.as_str())
                    .into()
            } else {
                slint::SharedString::default()
            },
        });
        for (index, folder) in choice.folders().iter().enumerate() {
            let (selected, partial) = if !folder.has_messages() {
                choice.state(choice.subfolders(index))
            } else {
                (choice.selected[index], false)
            };
            if selected && folder.has_messages() {
                chosen += 1;
                chosen_bytes += folder.size;
            }
            rows.push((store_index, Some(index)));
            models.push(ImportMailRow {
                is_store: false,
                name: folder
                    .path
                    .last()
                    .map_or(loose.as_str(), String::as_str)
                    .into(),
                depth: folder.path.len().saturating_sub(1) as i32,
                size: if !folder.has_messages() {
                    slint::SharedString::default()
                } else {
                    format_storage_size(folder.size).into()
                },
                selected,
                partial,
                destination: 0,
                folder: slint::SharedString::default(),
                upload_folder: slint::SharedString::default(),
            });
        }
    }
    session.rows = rows;
    let ui = app.global::<AccountImport>();
    ui.set_mail_rows(ModelRc::new(VecModel::from(models)));
    ui.set_mail_selected_count(chosen);
    ui.set_mail_selected_size(format_storage_size(chosen_bytes).into());
}

/// The chosen mail, store by store.
fn mail_jobs(session: &ImportSession) -> Vec<MailJob> {
    session
        .mail
        .iter()
        .filter_map(|choice| {
            let folders = choice.chosen().collect::<Vec<_>>();
            if folders.is_empty() {
                return None;
            }
            Some(MailJob {
                label: choice.store.label.clone(),
                bytes: folders
                    .iter()
                    .map(|index| choice.folders()[*index].size)
                    .sum(),
                source: choice.store.mail.clone(),
                folders,
                destination: session
                    .destinations
                    .get(choice.destination)
                    .cloned()
                    .flatten(),
                // An account's mail stays apart from local mail's own.
                parent: choice
                    .store
                    .account
                    .is_some()
                    .then(|| choice.store.label.clone()),
                upload_folder: choice.upload_folder.clone(),
            })
        })
        .collect()
}

/// Import each store's chosen mail in turn, reporting progress across all
/// of them.
async fn import_mail(
    core: CoreMailSource,
    jobs: Vec<MailJob>,
    own_addresses: Vec<String>,
    loose_folder_name: String,
    progress: watch::Sender<Progress>,
    stop: Arc<AtomicBool>,
) -> MailOutcome {
    let mut outcome = MailOutcome::default();
    let accounts = match core.load_accounts().await {
        Ok(accounts) => accounts,
        Err(error) => {
            outcome.error = Some(error);
            return outcome;
        }
    };
    let total_bytes: u64 = jobs.iter().map(|job| job.bytes).sum();
    let several = jobs.len() > 1;
    let mut done_bytes = 0;
    progress.send_replace(Progress {
        mail: true,
        done_bytes,
        total_bytes,
        ..Progress::default()
    });
    for job in jobs {
        if stop.load(Ordering::Relaxed) {
            outcome.stopped = true;
            break;
        }
        let account = job.destination.as_ref().and_then(|destination| {
            accounts.iter().find(|account| {
                account.mail_protocol == destination.protocol
                    && account.email.eq_ignore_ascii_case(&destination.email)
            })
        });
        let (destination, parent) = match account {
            Some(account) if account.mail_protocol == MailProtocol::Pop3 => {
                (Destination::Account(account.id), None)
            }
            Some(account) => (Destination::Account(account.id), job.upload_folder.clone()),
            // An account that couldn't be added leaves its mail on this
            // device, apart from the rest.
            None => (
                Destination::Local,
                job.parent.clone().or_else(|| {
                    job.destination
                        .as_ref()
                        .map(|destination| destination.email.clone())
                }),
            ),
        };
        // How progress names each of the job's folders.
        let names = job
            .source
            .folders
            .iter()
            .map(|folder| {
                let path = if folder.path.is_empty() {
                    loose_folder_name.clone()
                } else {
                    folder.path.join("/")
                };
                if several {
                    format!("{}/{path}", job.label)
                } else {
                    path
                }
            })
            .collect::<Vec<_>>();
        let request = MailImportRequest {
            source: job.source,
            folders: job.folders,
            destination,
            parent_folder: parent,
            loose_folder_name: loose_folder_name.clone(),
            own_addresses: own_addresses.clone(),
        };
        let (job_progress, mut job_receiver) = watch::channel(MailImportProgress::default());
        let task = {
            let core = core.clone();
            let stop = Arc::clone(&stop);
            tokio::spawn(async move { core.import_mail(request, job_progress, stop).await })
        };
        // The import drops its sender when it ends.
        let imported_before = outcome.imported;
        while job_receiver.changed().await.is_ok() {
            let job_progress = *job_receiver.borrow_and_update();
            progress.send_replace(Progress {
                mail: true,
                done_bytes: done_bytes + job_progress.done_bytes,
                total_bytes,
                imported: imported_before + job_progress.imported,
                folder: job_progress
                    .folder
                    .and_then(|index| names.get(index).cloned())
                    .unwrap_or_default(),
            });
        }
        done_bytes += job.bytes;
        let report = match task.await {
            Ok(Ok(report)) => report,
            Ok(Err(error)) => {
                outcome.error = Some(error);
                break;
            }
            Err(error) => {
                outcome.error = Some(error.to_string());
                break;
            }
        };
        outcome.imported += report.imported;
        outcome.already_present += report.already_present;
        outcome.too_large += report.too_large;
        outcome.refused += report.refused;
        if outcome.refusal.is_none() {
            outcome.refusal = report.refusal;
        }
        outcome.folders.extend(report.folders);
        for (path, error) in report.unreadable {
            let mut name = path.join("/");
            if several {
                name = if name.is_empty() {
                    job.label.clone()
                } else {
                    format!("{}/{name}", job.label)
                };
            }
            outcome.unreadable.push((name, error));
        }
        if report.stopped {
            outcome.stopped = true;
            break;
        }
    }
    outcome
}

fn show_progress(app: &AppWindow, progress: Progress) {
    let ui = app.global::<AccountImport>();
    if ui.get_stage() != "importing" || !progress.mail {
        return;
    }
    ui.set_mail_progress(if progress.total_bytes == 0 {
        0.0
    } else {
        (progress.done_bytes as f64 / progress.total_bytes as f64) as f32
    });
    ui.set_mail_progress_count(i32::try_from(progress.imported).unwrap_or(i32::MAX));
    ui.set_mail_progress_done(format_storage_size(progress.done_bytes).into());
    ui.set_mail_progress_total(format_storage_size(progress.total_bytes).into());
    ui.set_mail_progress_folder(progress.folder.into());
}

fn ui_count(value: u64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn show_mail_report(app: &AppWindow, outcome: &MailOutcome) {
    let ui = app.global::<AccountImport>();
    ui.set_mail_results(true);
    ui.set_mail_imported(ui_count(outcome.imported));
    ui.set_mail_already_present(ui_count(outcome.already_present));
    ui.set_mail_too_large(ui_count(outcome.too_large));
    ui.set_mail_refused(ui_count(outcome.refused));
    ui.set_mail_refusal(outcome.refusal.clone().unwrap_or_default().into());
    ui.set_mail_stopped(outcome.stopped);
    ui.set_mail_failures(ModelRc::new(VecModel::from(
        outcome
            .unreadable
            .iter()
            .map(|(folder, error)| ImportFailureRow {
                email: folder.as_str().into(),
                error: error.as_str().into(),
            })
            .collect::<Vec<_>>(),
    )));
}

fn show_report(app: &AppWindow, report: &AccountImportReport) {
    let ui = app.global::<AccountImport>();
    ui.set_account_results(true);
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
}

/// Clear the results of an earlier import from the dialog.
fn clear_results(app: &AppWindow) {
    let ui = app.global::<AccountImport>();
    ui.set_account_results(false);
    ui.set_imported(0);
    ui.set_already_present(0);
    ui.set_restored_preferences(false);
    ui.set_failures(ModelRc::default());
    ui.set_mail_results(false);
    ui.set_mail_imported(0);
    ui.set_mail_already_present(0);
    ui.set_mail_too_large(0);
    ui.set_mail_refused(0);
    ui.set_mail_refusal(slint::SharedString::default());
    ui.set_mail_stopped(false);
    ui.set_mail_failures(ModelRc::default());
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

/// The name of a chosen file or folder, for the mail it holds. Files
/// chosen together are named by their folder when they sit in it under their
/// own names. A phone's copies, each in a folder of its own or under a
/// generated name, name nothing.
fn source_label(files: &[ChosenFile]) -> String {
    let in_place =
        |file: &ChosenFile| file.path.file_name() == Some(std::ffi::OsStr::new(&file.name));
    match files {
        [file] => file.name.clone(),
        [first, rest @ ..] if files.iter().all(in_place) => first
            .path
            .parent()
            .filter(|parent| rest.iter().all(|file| file.path.parent() == Some(*parent)))
            .and_then(|parent| parent.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        _ => String::new(),
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
    // accounts this device already has. `label` names mail found without
    // accounts; `documents` are kept while their mail may be imported.
    type Inspect = dyn Fn(SourceReader, String, Vec<documents::ImportedDocument>);
    let inspect: Rc<Inspect> = {
        let app = app.as_weak();
        let session = Rc::clone(&session);
        let state = Rc::clone(state);
        let runtime = Rc::clone(runtime);
        Rc::new(
            move |read: SourceReader,
                  label: String,
                  documents: Vec<documents::ImportedDocument>| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                let Some(core) = state.borrow().core.clone() else {
                    show_storage_unavailable(&app);
                    return;
                };
                let generation = {
                    let mut session = session.borrow_mut();
                    session.clear_source();
                    session.documents = documents;
                    session.advance()
                };
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
                            Ok(Source::Mail(mail)) => return Ok(Ok(Inspected::Mail(mail))),
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
                            own_addresses: inventory
                                .accounts
                                .iter()
                                .flat_map(|account| &account.identities)
                                .map(|identity| identity.email.clone())
                                .collect(),
                            mail: inventory.mail,
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
                    let ui = app.global::<AccountImport>();
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
                                    clear_results(&app);
                                    ui.set_account_results(true);
                                    ui.set_imported(restore.imported as i32);
                                    ui.set_already_present(restore.already_present as i32);
                                    ui.set_restored_preferences(restore.preferences);
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
                            mail,
                            own_addresses,
                        } => {
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
                            session.source = Some(source);
                            session.mail = mail.into_iter().map(MailChoice::from_profile).collect();
                            session.own_addresses = own_addresses;
                            if session.plans.is_empty() {
                                let accounts = state.borrow().connected_accounts.clone();
                                show_mail(&app, &mut session, &accounts);
                            } else {
                                show_review(&app, &session);
                            }
                        }
                        Inspected::Mail(mail) => {
                            ui.set_local_folders(0);
                            ui.set_feeds(0);
                            ui.set_newsgroups(0);
                            ui.set_selected_count(0);
                            let mut session = session.borrow_mut();
                            session.mail = vec![MailChoice::new(MailStore {
                                account: None,
                                label,
                                mail,
                            })];
                            let accounts = state.borrow().connected_accounts.clone();
                            show_mail(&app, &mut session, &accounts);
                        }
                    }
                });
            },
        )
    };

    let app_weak = app.as_weak();
    let open_session = Rc::clone(&session);
    let open_runtime = Rc::clone(runtime);
    app.on_import_accounts(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        // Focus can reach the entry points behind the dialog; reopening must
        // not discard an import in progress or its report. One going on in
        // the background is shown again.
        let ui = app.global::<AccountImport>();
        if ui.get_open() {
            return;
        }
        if ui.get_in_background() {
            ui.set_open(true);
            return;
        }
        let opening = {
            let mut session = open_session.borrow_mut();
            session.advance();
            session.opening += 1;
            session.opening
        };
        ui.set_stage("choose".into());
        ui.set_accounts(ModelRc::default());
        ui.set_profiles(ModelRc::default());
        ui.set_has_mail(false);
        ui.set_mail_rows(ModelRc::default());
        clear_results(&app);
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
        // Saved mail often has no extension, so every file is offered.
        let dialog = documents::DocumentDialog::titled(
            translated(&app, &UiMessage::plain("Import accounts and mail")).to_string(),
        );
        let inspect = Rc::clone(&file_inspect);
        let session = Rc::clone(&file_session);
        let app_weak = app.as_weak();
        documents::run_request(
            &file_runtime,
            file_documents.import_many(MAX_CHOSEN_FILES, dialog),
            move |picked| {
                let Some(app) = app_weak.upgrade() else {
                    return;
                };
                if !finish_picking(&app, &session, opening) {
                    return;
                }
                match picked {
                    Ok(documents) if documents.is_empty() => {}
                    // Several files can only be mail.
                    Ok(documents) => {
                        let files = documents
                            .iter()
                            .map(|document| ChosenFile {
                                path: document.path().to_owned(),
                                name: document.name().unwrap_or_default(),
                            })
                            .collect::<Vec<_>>();
                        let mut label = source_label(&files);
                        if label.is_empty() {
                            label = translated(&app, &UiMessage::plain("Imported messages"))
                                .to_string();
                        }
                        let read: SourceReader = if files.len() == 1 {
                            let file = files[0].clone();
                            Box::new(move || migration::open_chosen(&file))
                        } else {
                            Box::new(move || mail_import::scan_files(&files).map(Source::Mail))
                        };
                        inspect(read, label, documents);
                    }
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
                &UiMessage::plain("Choose a Thunderbird profile or a folder of mail"),
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
                    Ok(Some(folder)) => {
                        let label = source_label(&[ChosenFile::at(folder.clone())]);
                        inspect(
                            Box::new(move || migration::open(&folder)),
                            label,
                            Vec::new(),
                        );
                    }
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
                let profile = {
                    let session = profile_session.borrow();
                    if !can_choose(&app, &session) {
                        return;
                    }
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| session.profiles.get(index).cloned())
                };
                if let Some(profile) = profile {
                    let path = profile.path;
                    profile_inspect(
                        Box::new(move || migration::open(&path)),
                        profile.name,
                        Vec::new(),
                    );
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
    let next_session = Rc::clone(&session);
    let next_state = Rc::clone(state);
    app.global::<AccountImport>().on_next(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut session = next_session.borrow_mut();
        if app.global::<AccountImport>().get_stage() != "review" || session.mail.is_empty() {
            return;
        }
        let accounts = next_state.borrow().connected_accounts.clone();
        show_mail(&app, &mut session, &accounts);
    });

    let app_weak = app.as_weak();
    let mail_toggle_session = Rc::clone(&session);
    app.global::<AccountImport>()
        .on_toggle_mail(move |row, checked| {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            let mut session = mail_toggle_session.borrow_mut();
            let Some(&(store, folder)) = usize::try_from(row)
                .ok()
                .and_then(|row| session.rows.get(row))
            else {
                return;
            };
            let choice = &mut session.mail[store];
            // A folder is chosen with its subfolders.
            let range = match folder {
                Some(folder) => folder..choice.subfolders(folder).end,
                None => 0..choice.folders().len(),
            };
            choice.selected[range].fill(checked);
            show_mail_rows(&app, &mut session);
        });

    let app_weak = app.as_weak();
    let destination_session = Rc::clone(&session);
    app.global::<AccountImport>()
        .on_choose_destination(move |row, destination| {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            let mut session = destination_session.borrow_mut();
            let Some(destination) = usize::try_from(destination)
                .ok()
                .filter(|destination| *destination < session.destinations.len())
            else {
                return;
            };
            let Some(&(store, None)) = usize::try_from(row)
                .ok()
                .and_then(|row| session.rows.get(row))
            else {
                return;
            };
            session.mail[store].destination = destination;
            show_mail_rows(&app, &mut session);
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
        let stage = app.global::<AccountImport>().get_stage();
        if stage != "review" && stage != "mail" {
            return;
        }
        let Some(core) = confirm_state.borrow().core.clone() else {
            return show_storage_unavailable(&app);
        };
        let (drafts, jobs, own_addresses, stop, generation) = {
            let mut session = confirm_session.borrow_mut();
            let drafts = session.selected_drafts();
            // Mail is chosen on its own step, after the accounts.
            let jobs = if stage == "mail" {
                mail_jobs(&session)
            } else {
                Vec::new()
            };
            if drafts.is_empty() && jobs.is_empty() {
                return;
            }
            let stop = Arc::new(AtomicBool::new(false));
            session.stop = Some(Arc::clone(&stop));
            session.imported_folder = None;
            (
                drafts,
                jobs,
                session.own_addresses.clone(),
                stop,
                session.advance(),
            )
        };
        let has_accounts = !drafts.is_empty();
        let has_mail = !jobs.is_empty();
        clear_results(&app);
        let ui = app.global::<AccountImport>();
        ui.set_stopping(false);
        ui.set_mail_progress(if has_accounts { -1.0 } else { 0.0 });
        ui.set_mail_progress_count(0);
        ui.set_mail_progress_folder(slint::SharedString::default());
        ui.set_mail_progress_done(format_storage_size(0).into());
        ui.set_mail_progress_total(
            format_storage_size(jobs.iter().map(|job| job.bytes).sum()).into(),
        );
        ui.set_stage("importing".into());

        // Progress reaches the dialog at most ten times a second.
        let (progress, mut progress_receiver) = watch::channel(Progress::default());
        let progress_app = app.as_weak();
        confirm_runtime.spawn(async move {
            while progress_receiver.changed().await.is_ok() {
                let progress = progress_receiver.borrow_and_update().clone();
                let _ =
                    progress_app.upgrade_in_event_loop(move |app| show_progress(&app, progress));
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        });

        let loose_folder_name =
            translated(&app, &UiMessage::plain("Imported messages")).to_string();
        let import_core = core.clone();
        let request: documents::DocumentFuture<ImportOutcome> = Box::pin(async move {
            let accounts = if drafts.is_empty() {
                None
            } else {
                Some(import_core.import_accounts(drafts).await?)
            };
            let mail = if jobs.is_empty() {
                None
            } else {
                Some(
                    import_mail(
                        import_core,
                        jobs,
                        own_addresses,
                        loose_folder_name,
                        progress,
                        stop,
                    )
                    .await,
                )
            };
            Ok(ImportOutcome { accounts, mail })
        });
        let app_weak = app.as_weak();
        let session = Rc::clone(&confirm_session);
        let state = Rc::clone(&confirm_state);
        let runtime = Rc::clone(&confirm_runtime);
        documents::run_request(&confirm_runtime, request, move |result| {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            // Accounts and mail may exist even if the import failed part way.
            if has_accounts {
                refresh_accounts(&app, &core, &state, &runtime);
            }
            if has_mail {
                refresh_local_mail(&app, &state, &runtime);
            }
            let mut session = session.borrow_mut();
            session.stop = None;
            if session.generation != generation {
                return;
            }
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => return show_error(&app, "failed", error),
            };
            if let Some(report) = &outcome.accounts {
                show_report(&app, report);
            }
            if let Some(mail) = &outcome.mail {
                session.imported_folder = mail.folders.first().copied();
                if let Some(error) = &mail.error {
                    return show_error(&app, "mail-failed", error.clone());
                }
                show_mail_report(&app, mail);
            }
            app.global::<AccountImport>().set_stage("done".into());
        });
    });

    let app_weak = app.as_weak();
    let stop_session = Rc::clone(&session);
    app.global::<AccountImport>().on_stop(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if let Some(stop) = &stop_session.borrow().stop {
            stop.store(true, Ordering::Relaxed);
            app.global::<AccountImport>().set_stopping(true);
        }
    });

    let app_weak = app.as_weak();
    let show_session = Rc::clone(&session);
    app.global::<AccountImport>().on_show_mail(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let folder = show_session.borrow().imported_folder;
        app.global::<AccountImport>().invoke_close();
        if let Some(folder) = folder {
            app.invoke_select_scope(Scope::Folder(folder).to_string().into());
        }
    });

    let app_weak = app.as_weak();
    let back_session = Rc::clone(&session);
    app.global::<AccountImport>().on_back(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut session = back_session.borrow_mut();
        session.advance();
        let ui = app.global::<AccountImport>();
        // From the mail step, back to the accounts it follows.
        if ui.get_stage() == "mail" && !session.plans.is_empty() {
            show_review(&app, &session);
            return;
        }
        session.clear_source();
        ui.set_has_mail(false);
        ui.set_mail_rows(ModelRc::default());
        ui.set_stage("choose".into());
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
        ui.set_in_background(false);
        ui.set_accounts(ModelRc::default());
        ui.set_mail_rows(ModelRc::default());
        ui.set_destinations(ModelRc::default());
        ui.set_failures(ModelRc::default());
        ui.set_mail_failures(ModelRc::default());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use flectar_mail_core::models::{AuthKind, Provider};

    const MESSAGE: &str = "From a@example.org Mon Jan  1 00:00:00 2024\n\
        From: a@example.org\nSubject: Hi\n\nHello\n";

    /// A Thunderbird store whose Archives holds only a subfolder.
    fn store(account: Option<&str>) -> (tempfile::TempDir, MailStore) {
        let directory = tempfile::tempdir().unwrap();
        for (name, contents) in [
            ("Inbox", MESSAGE),
            ("Archives", ""),
            ("Archives.sbd/2023", MESSAGE),
            ("Junk", MESSAGE),
            ("Trash", MESSAGE),
        ] {
            let path = directory.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        let mail = mail_import::scan_folder(directory.path()).unwrap();
        let store = MailStore {
            account: account.map(str::to_owned),
            label: "Local Folders".into(),
            mail,
        };
        (directory, store)
    }

    fn account(id: i64, email: &str, mail_protocol: MailProtocol) -> Account {
        Account {
            id,
            email: email.into(),
            display_name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol,
            sync_state: "idle".into(),
            sync_error: None,
            can_create_top_level_mailbox: true,
            server_keeps_mail_days: None,
        }
    }

    #[test]
    fn folders_are_chosen_with_their_subfolders_but_not_trash_or_junk() {
        let (_directory, store) = store(None);
        let mut choice = MailChoice::new(store);
        let names = choice
            .folders()
            .iter()
            .map(|folder| folder.path.join("/"))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["Archives", "Archives/2023", "Inbox", "Junk", "Trash"]
        );
        assert_eq!(choice.chosen().collect::<Vec<_>>(), [1, 2]);
        // Some of the store's folders are chosen, not all.
        assert_eq!(choice.state(0..5), (false, true));
        // Archives has no messages of its own; its subfolder speaks for it.
        assert_eq!(choice.subfolders(0), 1..2);
        assert_eq!(choice.state(choice.subfolders(0)), (true, false));
        assert_eq!(choice.subfolders(2), 3..3);
        assert_eq!(choice.subfolders(4), 5..5);

        let archives = 0..choice.subfolders(0).end;
        choice.selected[archives].fill(false);
        assert_eq!(choice.chosen().collect::<Vec<_>>(), [2]);
        assert_eq!(choice.state(choice.subfolders(0)), (false, false));
        choice.selected.fill(true);
        assert_eq!(choice.state(0..5), (true, false));
    }

    #[test]
    fn chosen_mail_goes_where_its_store_says() {
        let (_local_directory, local) = store(None);
        let (_pop_directory, pop) = store(Some("account1"));
        let accounts = [
            account(1, "pop@example.com", MailProtocol::Pop3),
            account(2, "imap@example.com", MailProtocol::Imap),
        ];
        let mut session = ImportSession {
            mail: vec![MailChoice::new(local), MailChoice::from_profile(pop)],
            ..ImportSession::default()
        };
        session.destinations = mail_destinations(&session, &accounts);
        let pop3 = MailAccount {
            email: "pop@example.com".into(),
            protocol: MailProtocol::Pop3,
        };
        let imap = MailAccount {
            email: "imap@example.com".into(),
            protocol: MailProtocol::Imap,
        };
        assert_eq!(
            session.destinations,
            [None, Some(pop3.clone()), Some(imap.clone())]
        );
        // Only an IMAP account's mail is uploaded to its server.
        assert!(!pop3.uploads() && imap.uploads());
        // Without a plan for its account, a store stays on this device.
        assert_eq!(
            default_destination(&session, &session.destinations, &session.mail[1].store),
            0
        );
        session.mail[1].destination = 1;

        let jobs = mail_jobs(&session);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].folders, [1, 2]);
        let folders = session.mail[0].folders();
        assert_eq!(jobs[0].bytes, folders[1].size + folders[2].size);
        assert_eq!(jobs[0].destination, None);
        assert_eq!(jobs[0].parent, None);
        assert_eq!(jobs[0].upload_folder, None);
        // An account's mail goes in a folder of its own if it stays here.
        assert_eq!(jobs[1].destination, Some(pop3));
        assert_eq!(jobs[1].parent.as_deref(), Some("Local Folders"));
        // On a server, a profile's mail goes in a folder named after it.
        session.mail[1].destination = 2;
        let jobs = mail_jobs(&session);
        assert_eq!(jobs[1].destination, Some(imap));
        assert_eq!(jobs[1].upload_folder.as_deref(), Some("Local Folders"));

        // A store with nothing chosen is left out.
        session.mail[0].selected.fill(false);
        assert_eq!(mail_jobs(&session).len(), 1);
    }

    #[test]
    fn destinations_are_named_by_address() {
        let destinations = [
            None,
            Some(MailAccount {
                email: "ana@example.com".into(),
                protocol: MailProtocol::Pop3,
            }),
            Some(MailAccount {
                email: "Ana@example.com".into(),
                protocol: MailProtocol::Imap,
            }),
            Some(MailAccount {
                email: "bo@example.com".into(),
                protocol: MailProtocol::Jmap,
            }),
        ];
        let names = (0..destinations.len())
            .map(|index| destination_name(&destinations, index, "On this device"))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "On this device",
                "ana@example.com (POP3)",
                "Ana@example.com (IMAP)",
                "bo@example.com"
            ]
        );
    }

    #[test]
    fn mail_without_accounts_is_named_after_what_was_chosen() {
        let chosen = |path: &str| ChosenFile::at(PathBuf::from(path));
        assert_eq!(
            source_label(&[chosen("/home/me/Mail/Inbox.mbox")]),
            "Inbox.mbox"
        );
        assert_eq!(
            source_label(&[
                chosen("/home/me/Saved/a.eml"),
                chosen("/home/me/Saved/b.eml")
            ]),
            "Saved"
        );
        // Phones copy picked files into one folder under generated names, and
        // keep the picked ones.
        let picked = |path: &str, name: &str| ChosenFile {
            path: PathBuf::from(path),
            name: name.into(),
        };
        assert_eq!(
            source_label(&[picked("/cache/file-imports/0b7c", "Work.mbox")]),
            "Work.mbox"
        );
        assert_eq!(
            source_label(&[
                picked("/cache/file-imports/0b7c", "a.eml"),
                picked("/cache/file-imports/9f21", "b.eml"),
            ]),
            ""
        );
        assert_eq!(source_label(&[]), "");
    }
}
