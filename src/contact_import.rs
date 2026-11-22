//! Importing contacts from a vCard file, as other address books, phones and
//! Google Contacts export them, and exporting every saved contact to one.
//! The dialog tells what a file holds before anything is saved
//! (ui/dialogs/contact-import-dialog.slint).

use super::*;
use flectar_mail_core::carddav::import::MAX_FILE_BYTES;
use flectar_mail_core::models::{ContactFileProblem, ContactImportPreview, ImportedDuplicates};
use std::path::PathBuf;
use std::sync::Arc;

thread_local! {
    static CONTROLLER: RefCell<Option<Rc<Controller>>> = const { RefCell::new(None) };
}

struct Controller {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    contacts: Rc<RefCell<ContactDirectoryState>>,
    runtime: tokio::runtime::Handle,
    documents: Arc<dyn documents::DocumentProvider>,
    staging: PathBuf,
    /// The file the dialog is about, read once, and the account whose
    /// address book its new contacts go to.
    reviewed: RefCell<Option<(Vec<u8>, Option<i64>)>>,
}

fn with_controller(f: impl FnOnce(&Rc<Controller>)) {
    let controller = CONTROLLER.with(|slot| slot.borrow().clone());
    if let Some(controller) = controller {
        f(&controller);
    }
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    contacts: &Rc<RefCell<ContactDirectoryState>>,
    runtime: &tokio::runtime::Handle,
    documents: &Arc<dyn documents::DocumentProvider>,
    staging: PathBuf,
) {
    CONTROLLER.with(|slot| {
        *slot.borrow_mut() = Some(Rc::new(Controller {
            app: app.as_weak(),
            state: Rc::clone(state),
            contacts: Rc::clone(contacts),
            runtime: runtime.clone(),
            documents: Arc::clone(documents),
            staging,
            reviewed: RefCell::default(),
        }));
    });
    let ui = app.global::<ContactImport>();
    ui.on_choose_file(|| with_controller(|controller| controller.choose_file()));
    ui.on_confirm(|choice| with_controller(|controller| controller.confirm(choice)));
    ui.on_close(|| with_controller(|controller| controller.close()));
    ui.on_export(|| with_controller(|controller| controller.export()));
}

/// A picked file, what importing it would do, and the address book its new
/// contacts would go to.
struct ReadFile {
    file: Vec<u8>,
    preview: ContactImportPreview,
    destination: Option<String>,
}

/// What importing does with contacts already saved, as the dialog's choice
/// numbers it.
fn duplicates(choice: i32) -> ImportedDuplicates {
    match choice {
        1 => ImportedDuplicates::Merge,
        2 => ImportedDuplicates::Add,
        _ => ImportedDuplicates::Skip,
    }
}

/// What a reviewed file leads to: the dialog, or a notice instead of it.
fn review_notice(preview: &ContactImportPreview) -> Option<UiMessage> {
    match preview.problem {
        Some(ContactFileProblem::TooLarge) => Some(UiMessage::plain("This file is larger than 64 MB.")),
        Some(ContactFileProblem::TooManyContacts) => Some(UiMessage::plain(
            "This file has more than 25,000 contacts.",
        )),
        None if preview.new == 0 && preview.duplicates == 0 => {
            Some(UiMessage::plain("No contacts found in this file."))
        }
        None => None,
    }
}

/// The file's bytes, up to one more than an import takes, so that the core
/// can tell a file too large from one that just fits.
async fn read_file(path: &std::path::Path) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    Ok(bytes)
}

impl Controller {
    fn choose_file(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<ContactImport>();
        if !ui.get_stage().is_empty() {
            return;
        }
        let Some(core) = self.state.borrow().core.clone() else {
            undo::tell(&app, UiMessage::plain("Local contact storage is still starting…"));
            return;
        };
        // New contacts go where a new contact would: the address book of
        // the account in view.
        let account = self
            .contacts
            .borrow()
            .scope
            .strip_prefix("Account:")
            .and_then(|id| id.parse::<i64>().ok());
        let import = self.documents.import_with(documents::DocumentDialog {
            title: translated(&app, &UiMessage::plain("Import contacts")).to_string(),
            filter: Some((
                translated(&app, &UiMessage::plain("vCard contacts")).to_string(),
                &["vcf", "vcard"],
            )),
            directory: None,
        });
        let request: documents::DocumentFuture<Option<ReadFile>> = Box::pin(async move {
            // Read before the document is dropped: a host's staged copy
            // lives only as long as the ImportedDocument.
            let Some(document) = import.await? else {
                return Ok(None);
            };
            let file = read_file(document.path()).await?;
            let preview = core.preview_contact_import(file.clone()).await?;
            let destination = match account {
                Some(account) => core.contact_destination(account).await?,
                None => None,
            };
            Ok(Some(ReadFile {
                file,
                preview,
                destination,
            }))
        });
        let this = Rc::clone(self);
        documents::run_request(&self.runtime, request, move |result| {
            let Some(app) = this.app.upgrade() else {
                return;
            };
            let ReadFile {
                file,
                preview,
                destination,
            } = match result {
                Ok(Some(read)) => read,
                Ok(None) => return,
                Err(error) => {
                    undo::tell(
                        &app,
                        UiMessage::detail("Could not read the contacts file: {}", error),
                    );
                    return;
                }
            };
            if let Some(notice) = review_notice(&preview) {
                undo::tell(&app, notice);
                return;
            }
            let ui = app.global::<ContactImport>();
            ui.set_new_count(i32::try_from(preview.new).unwrap_or(i32::MAX));
            ui.set_duplicate_count(i32::try_from(preview.duplicates).unwrap_or(i32::MAX));
            ui.set_unreadable_count(i32::try_from(preview.unreadable).unwrap_or(i32::MAX));
            ui.set_destination(destination.clone().unwrap_or_default().into());
            // Without an address book that takes them, they stay here.
            let account = account.filter(|_| destination.is_some());
            *this.reviewed.borrow_mut() = Some((file, account));
            ui.set_stage("review".into());
        });
    }

    fn confirm(self: &Rc<Self>, choice: i32) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<ContactImport>();
        if ui.get_stage() != "review" {
            return;
        }
        let Some((file, account)) = self.reviewed.borrow_mut().take() else {
            ui.set_stage("".into());
            return;
        };
        let Some(core) = self.state.borrow().core.clone() else {
            ui.set_stage("".into());
            return;
        };
        ui.set_stage("working".into());
        let request: documents::DocumentFuture<_> = Box::pin(async move {
            core.import_contacts(file, account, duplicates(choice)).await
        });
        let weak = self.app.clone();
        documents::run_request(&self.runtime, request, move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            app.global::<ContactImport>().set_stage("".into());
            let message = match result {
                Ok(outcome) if outcome.added + outcome.merged > 0 => {
                    UiMessage::detail("Imported {} contacts.", outcome.added + outcome.merged)
                }
                Ok(_) => UiMessage::plain("No new contacts to import."),
                Err(error) => UiMessage::detail("Could not import contacts: {}", error),
            };
            undo::tell(&app, message);
        });
    }

    fn close(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<ContactImport>();
        if ui.get_stage() == "review" {
            self.reviewed.borrow_mut().take();
            ui.set_stage("".into());
        }
    }

    fn export(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let Some(core) = self.state.borrow().core.clone() else {
            undo::tell(&app, UiMessage::plain("Local contact storage is still starting…"));
            return;
        };
        let dialog = documents::DocumentDialog {
            title: translated(&app, &UiMessage::plain("Export contacts")).to_string(),
            filter: Some((
                translated(&app, &UiMessage::plain("vCard contacts")).to_string(),
                &["vcf"],
            )),
            directory: None,
        };
        let documents = Arc::clone(&self.documents);
        let staging = self.staging.clone();
        let request: documents::DocumentFuture<Option<usize>> = Box::pin(async move {
            let (file, count) = core.export_contacts().await?;
            if count == 0 {
                return Ok(Some(0));
            }
            let saved = documents::export_bytes(
                documents,
                staging,
                file.into_bytes(),
                "Contacts.vcf".into(),
                dialog,
            )
            .await?;
            Ok(saved.then_some(count))
        });
        let weak = self.app.clone();
        documents::run_request(&self.runtime, request, move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(Some(0)) => undo::tell(&app, UiMessage::plain("No saved contacts to export.")),
                Ok(Some(count)) => undo::tell(&app, UiMessage::detail("Exported {} contacts.", count)),
                Ok(None) => {}
                Err(error) => undo::tell(&app, UiMessage::detail("Could not export contacts: {}", error)),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_choice_names_what_happens_to_saved_contacts() {
        assert_eq!(duplicates(0), ImportedDuplicates::Skip);
        assert_eq!(duplicates(1), ImportedDuplicates::Merge);
        assert_eq!(duplicates(2), ImportedDuplicates::Add);
        assert_eq!(duplicates(-1), ImportedDuplicates::Skip);
    }

    #[test]
    fn files_that_cannot_be_imported_end_in_a_notice() {
        let preview = |new, duplicates, problem| ContactImportPreview {
            new,
            duplicates,
            unreadable: 0,
            problem,
        };
        assert!(review_notice(&preview(0, 0, Some(ContactFileProblem::TooLarge))).is_some());
        assert!(review_notice(&preview(0, 0, Some(ContactFileProblem::TooManyContacts))).is_some());
        assert!(review_notice(&preview(0, 0, None)).is_some());
        assert!(review_notice(&preview(0, 3, None)).is_none());
        assert!(review_notice(&preview(2, 0, None)).is_none());
    }
}
