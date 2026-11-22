//! Special folders (#252): which folders an IMAP account's Sent, Drafts,
//! Trash, Spam and Archive are. The server suggests them, by the marks of
//! RFC 6154 or by their names, and Settings → Accounts can choose others.

use super::*;
use flectar_mail_core::models::AccountFolderRoles;
use slint::SharedString;

thread_local! {
    /// The folders each role's choices list after the server's suggestion,
    /// by id, for the account `FolderRoles` shows.
    static FOLDERS: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    let ui = app.global::<FolderRoles>();
    {
        let app = app.as_weak();
        let state = Rc::clone(state);
        let runtime = runtime.clone();
        ui.on_load(move |account_id| {
            let Some(app) = app.upgrade() else {
                return;
            };
            let ui = app.global::<FolderRoles>();
            ui.set_account_id(account_id);
            ui.set_loading(true);
            ui.set_error(SharedString::default());
            ui.set_roles(ModelRc::default());
            FOLDERS.with_borrow_mut(Vec::clear);
            if let Some(core) = state.borrow().core.clone() {
                read(&app, &runtime, core, account_id, None);
            }
        });
    }
    let app = app.as_weak();
    let state = Rc::clone(state);
    let runtime = runtime.clone();
    ui.on_choose(move |account_id, role, index| {
        let Some(app) = app.upgrade() else {
            return;
        };
        if app.global::<FolderRoles>().get_account_id() != account_id {
            return;
        }
        // The first choice is the server's suggestion.
        let folder_id = match usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
        {
            None => None,
            Some(index) => match FOLDERS.with_borrow(|folders| folders.get(index).copied()) {
                Some(folder_id) => Some(folder_id),
                None => return,
            },
        };
        if let Some(core) = state.borrow().core.clone() {
            read(
                &app,
                &runtime,
                core,
                account_id,
                Some((role.into(), folder_id)),
            );
        }
    });
}

/// Show account `account_id`'s special folders, first saving `choice`, a
/// role with the folder chosen for it or none for the server's, if any.
fn read(
    app: &AppWindow,
    runtime: &tokio::runtime::Handle,
    core: CoreMailSource,
    account_id: i32,
    choice: Option<(String, Option<i64>)>,
) {
    let app = app.as_weak();
    runtime.spawn(async move {
        let id = i64::from(account_id);
        let saved = match choice {
            Some((role, folder_id)) => core.set_account_folder_role(id, role, folder_id).await,
            None => Ok(()),
        };
        // Read either way, so each role shows where it goes.
        let listed = core.account_folder_roles(id).await;
        let _ = app.upgrade_in_event_loop(move |app| show(&app, account_id, saved, listed));
    });
}

fn show(
    app: &AppWindow,
    account_id: i32,
    saved: Result<(), String>,
    listed: Result<AccountFolderRoles, String>,
) {
    let ui = app.global::<FolderRoles>();
    // Closed, or opened for another account, since.
    if ui.get_account_id() != account_id {
        return;
    }
    ui.set_loading(false);
    let error = match (&saved, &listed) {
        (Err(error), _) => Some(UiMessage::detail("Could not choose the folder: {}", error)),
        (_, Err(error)) => Some(UiMessage::detail("Could not read the folders: {}", error)),
        _ => None,
    };
    ui.set_error(
        error
            .map(|error| translated(app, &error))
            .unwrap_or_default(),
    );
    let Ok(listed) = listed else {
        return;
    };
    let inbox = app.global::<MailboxWords>().invoke_name(MailboxRole::Inbox);
    let names = listed
        .folders
        .iter()
        .map(|folder| folder_name(&folder.names, &inbox))
        .collect::<Vec<_>>();
    let position = |id: i64| listed.folders.iter().position(|folder| folder.id == id);
    let rows = listed
        .roles
        .iter()
        .map(|role| {
            let suggestion = match role.suggested.and_then(position) {
                Some(index) => UiMessage::detail("Automatic ({})", &names[index]),
                None => UiMessage::plain("No folder"),
            };
            let choices = std::iter::once(translated(app, &suggestion))
                .chain(names.iter().map(|name| SharedString::from(name.as_str())))
                .collect::<Vec<_>>();
            FolderRoleRow {
                role: role.role.as_str().into(),
                choices: ModelRc::new(VecModel::from(choices)),
                selected: role
                    .chosen
                    .and_then(position)
                    .and_then(|index| i32::try_from(index + 1).ok())
                    .unwrap_or(0),
            }
        })
        .collect::<Vec<_>>();
    FOLDERS.set(listed.folders.iter().map(|folder| folder.id).collect());
    // New rows, as each select keeps the choice it shows once one is made.
    ui.set_roles(ModelRc::new(VecModel::from(rows)));
}

/// A folder's name after its parents' names, as "Archive / 2026". The Inbox
/// of servers that keep every folder in it goes by its name in `inbox`.
fn folder_name(names: &[String], inbox: &str) -> String {
    names
        .iter()
        .enumerate()
        .map(|(level, name)| {
            if level == 0 && name.eq_ignore_ascii_case("INBOX") {
                inbox
            } else {
                name.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

#[cfg(test)]
mod tests {
    use super::folder_name;

    #[test]
    fn folders_are_named_after_their_parents() {
        let names = |names: &[&str]| {
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            folder_name(&names(&["Archive", "2026"]), "Inbox"),
            "Archive / 2026"
        );
        assert_eq!(
            folder_name(&names(&["INBOX", "Sent"]), "Bandeja"),
            "Bandeja / Sent"
        );
        assert_eq!(
            folder_name(&names(&["Projects", "INBOX"]), "Inbox"),
            "Projects / INBOX"
        );
    }
}
