//! Searching the servers past the mail this device keeps, from the end of a
//! search's results (#242). The core keeps what the servers find and shows
//! it in the same search of the mail list from then on, so the list only
//! needs reading again when a search of the servers is done.

use super::*;
use flectar_mail_core::models::ServerSearchCursor;

/// How many messages a search of the servers brings at a time from each
/// account, besides the matches here already that it passes.
pub(super) const PAGE: usize = 25;

/// A search of the servers for one list's search.
#[derive(Default)]
pub(super) struct ServerSearch {
    /// The search it is for: the list's scope and query.
    list: Option<(Scope, String)>,
    /// Each account it searches, and where its search goes on from.
    accounts: Vec<(i64, Progress)>,
    /// The accounts the last search failed for, with why.
    failed: Vec<(i64, String)>,
    searching: bool,
    /// How many rows the searches added to the list.
    found: usize,
    /// Counts searches, so that an answer for an earlier one is dropped.
    generation: u64,
}

#[derive(Clone)]
enum Progress {
    Start,
    From(ServerSearchCursor),
    Done,
}

impl ServerSearch {
    /// Takes in the answer to a search of the accounts in `asked`, and says
    /// how many of the rows it found aren't among the list's `known` rows.
    /// An account that failed goes on from where it was when tried again.
    fn take(
        &mut self,
        asked: &[(i64, Option<ServerSearchCursor>)],
        result: Result<mail::ServerFound, String>,
        known: &HashSet<i32>,
    ) -> usize {
        self.searching = false;
        let found = match result {
            Ok(found) => found,
            Err(error) => {
                self.failed = asked
                    .iter()
                    .map(|(account_id, _)| (*account_id, error.clone()))
                    .collect();
                return 0;
            }
        };
        for (account_id, progress) in &mut self.accounts {
            if !asked.iter().any(|(asked, _)| asked == account_id) {
                continue;
            }
            if let Some((_, error)) = found.failed.iter().find(|(failed, _)| failed == account_id) {
                self.failed.push((*account_id, error.clone()));
            } else if let Some((_, cursor)) = found
                .cursors
                .iter()
                .find(|(searched, _)| searched == account_id)
            {
                *progress = Progress::From(cursor.clone());
            } else {
                *progress = Progress::Done;
            }
        }
        let new_rows = found.rows.iter().filter(|row| !known.contains(row)).count();
        self.found += new_rows;
        new_rows
    }

    /// Where the search of `list` is, for the UI: "offer" until it starts,
    /// then "searching", "failed", "more" or "done".
    fn stage(&self, list: &(Scope, String)) -> &'static str {
        if self.list.as_ref() != Some(list) {
            "offer"
        } else if self.searching {
            "searching"
        } else if !self.failed.is_empty() {
            "failed"
        } else if self
            .accounts
            .iter()
            .any(|(_, progress)| !matches!(progress, Progress::Done))
        {
            "more"
        } else {
            "done"
        }
    }
}

/// The list's search, when its rows are all here and an account it covers
/// has a server to search.
fn searchable_list(state: &InboxState) -> Option<(Scope, String)> {
    let query = state.query.trim();
    (state.using_core
        && !query.is_empty()
        && state.next_cursor.is_none()
        && !searchable_accounts(state).is_empty())
    .then(|| (state.scope, query.to_owned()))
}

/// The accounts in view whose servers can be searched: not POP3, and not the
/// mail kept only on this device.
fn searchable_accounts(state: &InboxState) -> Vec<i64> {
    let in_view = state.accounts_in_view();
    state
        .connected_accounts
        .iter()
        .filter(|account| {
            matches!(
                account.mail_protocol,
                MailProtocol::Imap | MailProtocol::Jmap | MailProtocol::Gmail
            )
        })
        .filter(|account| in_view.as_ref().is_none_or(|ids| ids.contains(&account.id)))
        .map(|account| account.id)
        .collect()
}

/// Ends a list of search results with the search of the servers, when the
/// list has rows: an empty list shows it under its own message.
pub(super) fn add_row(entries: &mut Vec<MailListEntry>, state: &InboxState) {
    if !entries.is_empty() && searchable_list(state).is_some() {
        entries.push(mail_groups::server_search_entry());
    }
}

/// Tells the UI where the search of the servers is for the list's search.
pub(super) fn show(app: &AppWindow, state: &InboxState) {
    let ui = app.global::<ServerSearchUi>();
    let Some(list) = searchable_list(state) else {
        ui.set_stage("".into());
        return;
    };
    let search = &state.server_search;
    ui.set_stage(search.stage(&list).into());
    let current = search.list.as_ref() == Some(&list);
    ui.set_found(if current {
        i32::try_from(search.found).unwrap_or(i32::MAX)
    } else {
        0
    });
    let failed = if current {
        search.failed.as_slice()
    } else {
        &[]
    };
    ui.set_failed_count(i32::try_from(failed.len()).unwrap_or(i32::MAX));
    let (account, error) = failed
        .first()
        .map(|(account_id, error)| {
            let name = state
                .connected_accounts
                .iter()
                .find(|account| account.id == *account_id)
                .map(|account| account.email.clone())
                .unwrap_or_default();
            (name, error.clone())
        })
        .unwrap_or_default();
    ui.set_failed_account(account.into());
    ui.set_error(error.into());
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    let weak = app.as_weak();
    let state = Rc::clone(state);
    let runtime = runtime.clone();
    app.global::<ServerSearchUi>().on_search(move || {
        if let Some(app) = weak.upgrade() {
            search(&app, &state, &runtime);
        }
    });
}

/// Searches the servers for the list's search, or goes on where the last
/// search stopped.
fn search(app: &AppWindow, state: &Rc<RefCell<InboxState>>, runtime: &tokio::runtime::Handle) {
    let (core, list, accounts, generation) = {
        let mut state = state.borrow_mut();
        let Some(core) = state.core.clone() else {
            return;
        };
        let Some(list) = searchable_list(&state) else {
            return;
        };
        let searchable = searchable_accounts(&state);
        let search = &mut state.server_search;
        if search.list.as_ref() != Some(&list) {
            *search = ServerSearch {
                list: Some(list.clone()),
                accounts: searchable
                    .into_iter()
                    .map(|account_id| (account_id, Progress::Start))
                    .collect(),
                generation: search.generation,
                ..ServerSearch::default()
            };
        }
        if search.searching {
            return;
        }
        let accounts = search
            .accounts
            .iter()
            .filter_map(|(account_id, progress)| match progress {
                Progress::Start => Some((*account_id, None)),
                Progress::From(cursor) => Some((*account_id, Some(cursor.clone()))),
                Progress::Done => None,
            })
            .collect::<Vec<_>>();
        if accounts.is_empty() {
            return;
        }
        search.searching = true;
        search.failed.clear();
        search.generation = search.generation.wrapping_add(1);
        (core, list, accounts, search.generation)
    };
    show(app, &state.borrow());

    let ids = accounts.iter().map(|(account_id, _)| *account_id).collect();
    let cursors = accounts
        .iter()
        .filter_map(|(account_id, cursor)| Some((*account_id, cursor.clone()?)))
        .collect();
    let task = runtime.spawn({
        let query = list.1.clone();
        async move { core.search_server(query, ids, cursors).await }
    });
    let abort = task.abort_handle();
    let weak = app.as_weak();
    let state = Rc::clone(state);
    let runtime = runtime.clone();
    let spawned = slint::spawn_local(async move {
        let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
        if let Some(app) = weak.upgrade() {
            finish(&app, &state, &runtime, generation, &accounts, result);
        }
    });
    if spawned.is_err() {
        abort.abort();
    }
}

/// Takes in what a search of the servers found, and reads the list again so
/// that it shows it.
fn finish(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    generation: u64,
    asked: &[(i64, Option<ServerSearchCursor>)],
    result: Result<mail::ServerFound, String>,
) {
    let new_rows = {
        let mut state = state.borrow_mut();
        if state.server_search.generation != generation {
            return;
        }
        let known = state
            .messages
            .iter()
            .map(|message| message.id)
            .collect::<HashSet<_>>();
        state.server_search.take(asked, result, &known)
    };
    show(app, &state.borrow());
    if new_rows > 0 {
        let rows = state.borrow().messages.len() + new_rows;
        if let Err(error) = mail_work::refresh_rows(app, state, runtime, rows, &[]) {
            app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> (Scope, String) {
        (Scope::UNIFIED_INBOX, "invoice".to_owned())
    }

    fn cursor(position: u64) -> ServerSearchCursor {
        ServerSearchCursor {
            position,
            ..ServerSearchCursor::default()
        }
    }

    /// A search of accounts 1 and 2 that has started.
    fn started() -> ServerSearch {
        ServerSearch {
            list: Some(list()),
            accounts: vec![(1, Progress::Start), (2, Progress::Start)],
            searching: true,
            ..ServerSearch::default()
        }
    }

    fn asked() -> Vec<(i64, Option<ServerSearchCursor>)> {
        vec![(1, None), (2, None)]
    }

    #[test]
    fn another_lists_search_is_only_offered() {
        let search = started();
        assert_eq!(search.stage(&list()), "searching");
        let other = (Scope::UNIFIED_INBOX, "receipt".to_owned());
        assert_eq!(search.stage(&other), "offer");
    }

    #[test]
    fn an_answer_counts_the_new_rows_and_says_whether_more_is_left() {
        let mut search = started();
        let found = mail::ServerFound {
            rows: vec![10, 11, 12],
            cursors: vec![(1, cursor(25))],
            failed: Vec::new(),
        };
        // 10 is in the list already.
        let new_rows = search.take(&asked(), Ok(found), &HashSet::from([10]));
        assert_eq!((new_rows, search.found), (2, 2));
        assert_eq!(search.stage(&list()), "more");
        assert!(matches!(&search.accounts[0].1, Progress::From(next) if next.position == 25));
        assert!(matches!(search.accounts[1].1, Progress::Done));

        // The next page of account 1 has nothing past it.
        search.searching = true;
        let found = mail::ServerFound {
            rows: vec![13],
            cursors: Vec::new(),
            failed: Vec::new(),
        };
        let new_rows = search.take(&[(1, Some(cursor(25)))], Ok(found), &HashSet::new());
        assert_eq!((new_rows, search.found), (1, 3));
        assert_eq!(search.stage(&list()), "done");
    }

    #[test]
    fn a_failed_account_is_tried_again_from_where_it_was() {
        let mut search = started();
        let found = mail::ServerFound {
            rows: Vec::new(),
            cursors: Vec::new(),
            failed: vec![(2, "The account isn't connected.".to_owned())],
        };
        search.take(&asked(), Ok(found), &HashSet::new());
        assert_eq!(search.stage(&list()), "failed");
        assert!(matches!(search.accounts[0].1, Progress::Done));
        assert!(matches!(search.accounts[1].1, Progress::Start));

        // Nothing came back at all: every account asked failed.
        search.failed.clear();
        search.take(&[(2, None)], Err("offline".to_owned()), &HashSet::new());
        assert_eq!(search.failed, [(2, "offline".to_owned())]);
        assert!(matches!(search.accounts[1].1, Progress::Start));
    }
}
