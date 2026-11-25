//! Folders (#253): which of an account's folders show in the sidebar and the
//! Move to list, and for an IMAP account which ones this device syncs. A
//! hidden folder takes the folders in it along, and its mail still
//! downloads. The Inbox and the folders with a role always show, and the
//! Inbox, Sent and Drafts always sync.

use super::*;
use flectar_mail_core::models::AccountFolder;
use slint::SharedString;

/// At most this many folders are listed at once; a search finds the rest.
const MAX_ROWS: usize = 200;

thread_local! {
    /// The folders of the account `FolderVisibility` shows, as last read.
    static FOLDERS: RefCell<Vec<AccountFolder>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    let ui = app.global::<FolderVisibility>();
    {
        let app = app.as_weak();
        let state = Rc::clone(state);
        let runtime = runtime.clone();
        ui.on_load(move |account_id| {
            let Some(app) = app.upgrade() else {
                return;
            };
            let ui = app.global::<FolderVisibility>();
            ui.set_account_id(account_id);
            ui.set_loading(true);
            ui.set_error(SharedString::default());
            ui.set_query(SharedString::default());
            ui.set_rows(ModelRc::default());
            ui.set_matches(0);
            ui.set_total(0);
            FOLDERS.with_borrow_mut(Vec::clear);
            if let Some(core) = state.borrow().core.clone() {
                read(&app, &runtime, core, account_id, None);
            }
        });
    }
    {
        let app = app.as_weak();
        ui.on_search(move |_| {
            if let Some(app) = app.upgrade() {
                list(&app);
            }
        });
    }
    {
        let app = app.as_weak();
        let state = Rc::clone(state);
        let runtime = runtime.clone();
        ui.on_set_shown(move |account_id, folder_id, shown| {
            choose(
                &app,
                &state,
                &runtime,
                account_id,
                Choice::Shown(i64::from(folder_id), shown),
            );
        });
    }
    let app = app.as_weak();
    let state = Rc::clone(state);
    let runtime = runtime.clone();
    ui.on_set_synced(move |account_id, folder_id, synced| {
        choose(
            &app,
            &state,
            &runtime,
            account_id,
            Choice::Synced(i64::from(folder_id), synced),
        );
    });
}

/// What the user chose for a folder.
#[derive(Clone, Copy)]
enum Choice {
    Shown(i64, bool),
    Synced(i64, bool),
}

/// Save `choice` for a folder of account `account_id`, if the dialog still
/// shows that account, and list its folders again.
fn choose(
    app: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    account_id: i32,
    choice: Choice,
) {
    let Some(app) = app.upgrade() else {
        return;
    };
    if app.global::<FolderVisibility>().get_account_id() != account_id {
        return;
    }
    if let Some(core) = state.borrow().core.clone() {
        read(&app, runtime, core, account_id, Some(choice));
    }
}

/// List account `account_id`'s folders, first saving `choice`, if any.
fn read(
    app: &AppWindow,
    runtime: &tokio::runtime::Handle,
    core: CoreMailSource,
    account_id: i32,
    choice: Option<Choice>,
) {
    let app = app.as_weak();
    runtime.spawn(async move {
        let saved = match choice {
            Some(Choice::Shown(folder_id, shown)) => {
                core.set_folder_hidden(folder_id, !shown).await
            }
            Some(Choice::Synced(folder_id, synced)) => {
                core.set_folder_synced(folder_id, synced).await
            }
            None => Ok(()),
        };
        // Read either way, so each switch shows what's so.
        let listed = core.account_folders(i64::from(account_id)).await;
        let _ = app.upgrade_in_event_loop(move |app| show(&app, account_id, saved, listed));
    });
}

fn show(
    app: &AppWindow,
    account_id: i32,
    saved: Result<(), String>,
    listed: Result<Vec<AccountFolder>, String>,
) {
    let ui = app.global::<FolderVisibility>();
    // Closed, or opened for another account, since.
    if ui.get_account_id() != account_id {
        return;
    }
    ui.set_loading(false);
    let error = match (&saved, &listed) {
        (Err(error), _) => Some(UiMessage::detail("Could not change the folder: {}", error)),
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
    ui.set_total(i32::try_from(listed.len()).unwrap_or(i32::MAX));
    FOLDERS.set(listed);
    list(app);
}

/// Show the folders the search matches: the whole tree without one, and
/// each match named after its parents with one.
fn list(app: &AppWindow) {
    let ui = app.global::<FolderVisibility>();
    let inbox = app.global::<MailboxWords>().invoke_name(MailboxRole::Inbox);
    let query = ui.get_query().trim().to_lowercase();
    let (rows, matches) = FOLDERS.with_borrow(|folders| rows(folders, &query, &inbox));
    ui.set_matches(i32::try_from(matches).unwrap_or(i32::MAX));
    // New rows, as each switch keeps the state it was flipped to.
    ui.set_rows(ModelRc::new(VecModel::from(rows)));
}

/// The rows for `folders` that `query`, in lowercase, matches, at most
/// [`MAX_ROWS`], and how many match in all.
fn rows(folders: &[AccountFolder], query: &str, inbox: &str) -> (Vec<FolderShownRow>, usize) {
    let mut matches = 0;
    let mut rows = Vec::new();
    for folder in folders {
        let path = super::folder_roles::folder_name(&folder.names, inbox);
        if !query.is_empty() && !path.to_lowercase().contains(query) {
            continue;
        }
        matches += 1;
        if rows.len() == MAX_ROWS {
            continue;
        }
        let name = if query.is_empty() {
            match folder.names.as_slice() {
                [only] if only.eq_ignore_ascii_case("INBOX") => inbox.to_owned(),
                names => names.last().cloned().unwrap_or_default(),
            }
        } else {
            path
        };
        rows.push(FolderShownRow {
            id: i32::try_from(folder.id).unwrap_or(-1),
            name: name.into(),
            depth: i32::try_from(folder.names.len().saturating_sub(1)).unwrap_or(0),
            shown: !folder.hidden,
            always_shown: folder.always_shown,
            inside_hidden: folder.inside_hidden,
            synced: folder.synced,
            always_synced: folder.always_synced,
            syncable: folder.syncable,
        });
    }
    (rows, matches)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, names: &[&str], hidden: bool, inside_hidden: bool) -> AccountFolder {
        AccountFolder {
            id,
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            always_shown: names == ["INBOX"],
            hidden,
            inside_hidden,
            synced: true,
            always_synced: names == ["INBOX"],
            syncable: true,
        }
    }

    /// The tree lists each folder by its own name at its depth, and a search
    /// lists the matches by their paths, at most a page of them.
    #[test]
    fn folders_list_as_a_tree_or_as_matches() {
        let folders = [
            folder(1, &["INBOX"], false, false),
            folder(2, &["Notes"], true, false),
            folder(3, &["Notes", "Ideas"], false, true),
        ];
        let (tree, matches) = rows(&folders, "", "Bandeja");
        assert_eq!(matches, 3);
        assert_eq!(
            tree.iter()
                .map(|row| (row.name.to_string(), row.depth, row.shown, row.inside_hidden))
                .collect::<Vec<_>>(),
            [
                ("Bandeja".to_owned(), 0, true, false),
                ("Notes".to_owned(), 0, false, false),
                ("Ideas".to_owned(), 1, true, true),
            ]
        );
        assert!(tree[0].always_shown);

        let (found, matches) = rows(&folders, "ide", "Inbox");
        assert_eq!(matches, 1);
        assert_eq!(found[0].name.as_str(), "Notes / Ideas");

        let many = (0..MAX_ROWS as i64 + 5)
            .map(|id| folder(id, &["Archive", &id.to_string()], false, false))
            .collect::<Vec<_>>();
        let (page, matches) = rows(&many, "archive", "Inbox");
        assert_eq!((page.len(), matches), (MAX_ROWS, MAX_ROWS + 5));
    }
}
