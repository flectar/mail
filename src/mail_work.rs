//! Bounded database work for interactive mail navigation and mutations.
use super::*;

#[derive(Clone)]
struct PageRequest {
    core: CoreMailSource,
    scope: String,
    query: String,
}

pub(super) struct MailWork {
    pages: tokio::sync::watch::Sender<Option<(u64, PageRequest)>>,
    actions: tokio::sync::mpsc::Sender<ActionRequest>,
    pub generation: u64,
    pub epoch: u64,
    pub loading: bool,
    replace: bool,
    moved: HashSet<i32>,
    pending: HashSet<i32>,
}

#[derive(Clone)]
pub(super) enum Operation {
    Action(String),
    Drop(MailDropDestination),
    Label(i64, bool),
}
impl Operation {
    fn moves(&self) -> bool {
        match self {
            Self::Action(a) => matches!(
                a.as_str(),
                "archive" | "spam" | "trash" | "not_spam" | "unarchive"
            ),
            Self::Drop(MailDropDestination::Folder(_)) => true,
            Self::Drop(MailDropDestination::Action(a)) => Self::Action((*a).into()).moves(),
            _ => false,
        }
    }
    async fn perform(&self, core: &CoreMailSource, thread: i64) -> Result<(), String> {
        match self {
            Self::Action(a) => core.perform_message_action(thread, a).await,
            Self::Label(id, add) => core.perform_label_action(thread, *id, *add).await,
            Self::Drop(MailDropDestination::Action(a)) => {
                core.perform_message_action(thread, a).await
            }
            Self::Drop(MailDropDestination::Folder(id)) => {
                core.move_thread_to_folder(thread, *id).await
            }
            Self::Drop(MailDropDestination::Label(id)) => {
                core.perform_label_action(thread, *id, true).await
            }
            Self::Drop(MailDropDestination::Route(tab)) => {
                core.route_thread_to_tab(thread, tab.clone()).await
            }
        }
    }
}
struct ActionRequest {
    core: CoreMailSource,
    epoch: u64,
    operations: Vec<(i32, i64, Operation)>,
}
struct ActionResult {
    epoch: u64,
    ids: Vec<i32>,
    completed: Vec<i32>,
    moved: Vec<i32>,
    error: Option<String>,
}

pub(super) fn register(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
) {
    let (pages, page_rx) = tokio::sync::watch::channel(None);
    let (page_tx, mut page_results) = bounded_ui_channel();
    let updates = UiSender::new(
        page_tx,
        UiWake::new(app.as_weak(), |app| app.invoke_drain_navigation_updates()),
    );
    runtime.spawn(latest_load::run(
        page_rx,
        |request: PageRequest| async move {
            request
                .core
                .load_page(
                    &request.scope,
                    &request.query,
                    None,
                    PAGE_SIZE as i64,
                    false,
                )
                .await
        },
        move |generation, result| {
            let updates = updates.clone();
            async move {
                let _ = updates.send((generation, result)).await;
            }
        },
    ));
    let (actions, mut action_rx) = tokio::sync::mpsc::channel::<ActionRequest>(8);
    let (action_tx, mut action_results) = bounded_ui_channel();
    let updates = UiSender::new(
        action_tx,
        UiWake::new(app.as_weak(), |app| app.invoke_drain_action_updates()),
    );
    runtime.spawn(async move {
        while let Some(request) = action_rx.recv().await {
            let core = request.core;
            let result = collect_results(
                request.epoch,
                request.operations,
                move |thread, operation| {
                    let core = core.clone();
                    async move { operation.perform(&core, thread).await }
                },
            )
            .await;
            if updates.send(result).await.is_err() {
                break;
            }
        }
    });
    state.borrow_mut().mail_work = Some(MailWork {
        pages,
        actions,
        generation: 0,
        epoch: 0,
        loading: false,
        replace: false,
        moved: HashSet::new(),
        pending: HashSet::new(),
    });
    let weak = app.as_weak();
    let page_state = state.clone();
    let page_runtime = runtime.clone();
    app.on_drain_navigation_updates(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        while let Ok((generation, result)) = page_results.try_recv() {
            let projection = {
                let mut state = page_state.borrow_mut();
                let work = state.mail_work.as_mut().unwrap();
                if generation != work.generation {
                    continue;
                }
                work.loading = false;
                // Keep move exclusions and replacement intent after an error so a retry
                // cannot resurrect an archived message in the retained pagination tail.
                result.is_ok().then(|| {
                    (
                        std::mem::take(&mut work.replace),
                        std::mem::take(&mut work.moved)
                            .into_iter()
                            .collect::<Vec<_>>(),
                    )
                })
            };
            app.set_mail_page_loading(false);
            let page = match result {
                Ok(page) => page,
                Err(error) => {
                    app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
                    continue;
                }
            };
            let (replace, moved) = projection.unwrap();
            if replace {
                let mut state = page_state.borrow_mut();
                state.messages = page.messages;
                state.labels = page.labels;
                state.total_count = state.messages.len();
                state.next_cursor = page.next_cursor;
                drop(state);
                if let Err(error) = render_current(&app, &page_state, &page_runtime) {
                    app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
                }
            } else {
                apply_background_mail_page(&app, &page_state, &page_runtime, page, &moved);
            }
            app.set_mail_list_revision(app.get_mail_list_revision().wrapping_add(1));
        }
    });
    let weak = app.as_weak();
    let action_state = state.clone();
    let action_runtime = runtime.clone();
    app.on_drain_action_updates(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        while let Ok(result) = action_results.try_recv() {
            let same_view = {
                let mut state = action_state.borrow_mut();
                let work = state.mail_work.as_mut().unwrap();
                for id in &result.ids {
                    work.pending.remove(id);
                }
                let same_view = work.epoch == result.epoch;
                if same_view {
                    for id in &result.completed {
                        state.checked_ids.remove(id);
                    }
                }
                same_view
            };
            if same_view && !result.completed.is_empty() {
                refresh_rows_only(&app, &action_state, &action_runtime);
            }
            if same_view
                && !result.completed.is_empty()
                && let Err(error) =
                    refresh_from_source(&app, &action_state, &action_runtime, true, &result.moved)
            {
                app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
                continue;
            }
            match result.error {
                Some(error) => {
                    app.set_render_status(UiMessage::detail("Message action failed: {}", error))
                }
                None => app.set_render_status(UiMessage::plain("Message action completed.")),
            }
        }
        // Core events accumulated during a batch need only one metadata/list refresh.
        app.invoke_drain_core_updates();
    });
}

pub(super) fn refresh(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    preserve: bool,
    moved: &[i32],
) -> Result<(), String> {
    {
        let mut state = state.borrow_mut();
        let request = PageRequest {
            core: state.core.clone().ok_or("mail core is unavailable")?,
            scope: state.scope.clone(),
            query: state.query.clone(),
        };
        let work = state
            .mail_work
            .as_mut()
            .ok_or("mail workers are unavailable")?;
        work.generation = work.generation.wrapping_add(1);
        if !preserve {
            work.epoch = work.epoch.wrapping_add(1);
            work.moved.clear();
            work.replace = true;
        }
        work.moved.extend(moved);
        work.loading = true;
        work.pages.send_replace(Some((work.generation, request)));
        if !preserve {
            state.messages.clear();
            state.selected_id = None;
            state.next_cursor = None;
            state.total_count = 0;
        }
    }
    if !preserve {
        app.set_mail_page_loading(true);
        render_current(app, state, runtime)?;
    }
    Ok(())
}

pub(super) fn enqueue(
    state: &Rc<RefCell<InboxState>>,
    core: CoreMailSource,
    operations: Vec<(i32, i64, Operation)>,
) -> Result<(), String> {
    if operations.is_empty() {
        return Err("no message is selected".into());
    }
    if operations.len() > 10_000 {
        return Err("select at most 10000 messages per action".into());
    }
    let mut state = state.borrow_mut();
    let work = state
        .mail_work
        .as_mut()
        .ok_or("mail workers are unavailable")?;
    let ids: Vec<_> = operations.iter().map(|(id, _, _)| *id).collect();
    enqueue_batch(
        &work.actions,
        &mut work.pending,
        &ids,
        ActionRequest {
            core,
            epoch: work.epoch,
            operations,
        },
    )
}

pub(super) fn accepts_background(state: &InboxState, generation: u64) -> bool {
    state
        .mail_work
        .as_ref()
        .is_none_or(|work| !work.loading && work.generation == generation)
}
pub(super) fn generation(state: &InboxState) -> u64 {
    state.mail_work.as_ref().map_or(0, |work| work.generation)
}

async fn collect_results<F, Fut>(
    epoch: u64,
    operations: Vec<(i32, i64, Operation)>,
    perform: F,
) -> ActionResult
where
    F: Fn(i64, Operation) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut result = ActionResult {
        epoch,
        ids: vec![],
        completed: vec![],
        moved: vec![],
        error: None,
    };
    for (id, thread, operation) in operations {
        result.ids.push(id);
        let moves = operation.moves();
        match perform(thread, operation).await {
            Ok(()) => {
                result.completed.push(id);
                if moves {
                    result.moved.push(id);
                }
            }
            Err(error) => {
                result.error.get_or_insert(error);
            }
        }
    }
    result
}

fn enqueue_batch<T>(
    sender: &tokio::sync::mpsc::Sender<T>,
    pending: &mut HashSet<i32>,
    ids: &[i32],
    request: T,
) -> Result<(), String> {
    if ids.iter().any(|id| pending.contains(id)) {
        return Err("a message action is already pending".into());
    }
    sender
        .try_send(request)
        .map_err(|_| "mail action queue is busy; retry shortly".to_owned())?;
    pending.extend(ids);
    Ok(())
}

pub(super) fn actions_pending(state: &InboxState) -> bool {
    state
        .mail_work
        .as_ref()
        .is_some_and(|work| !work.pending.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_queue_rejects_without_reserving_ids_and_duplicate_actions_are_rejected() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let mut pending = HashSet::new();
        enqueue_batch(&sender, &mut pending, &[1, 2], "first").unwrap();
        assert!(enqueue_batch(&sender, &mut pending, &[2], "duplicate").is_err());
        assert!(enqueue_batch(&sender, &mut pending, &[3], "full").is_err());
        assert_eq!(pending, HashSet::from([1, 2]));
        assert_eq!(receiver.try_recv().unwrap(), "first");
        enqueue_batch(&sender, &mut pending, &[3], "retry").unwrap();
        assert_eq!(receiver.try_recv().unwrap(), "retry");
    }

    #[tokio::test]
    async fn partial_failure_keeps_failed_ids_and_only_removes_successful_moves() {
        let calls = RefCell::new(Vec::new());
        let result = collect_results(
            7,
            vec![
                (1, 101, Operation::Action("archive".into())),
                (2, 102, Operation::Action("trash".into())),
                (3, 103, Operation::Action("star".into())),
            ],
            |thread, _| {
                calls.borrow_mut().push(thread);
                async move {
                    if thread == 102 {
                        Err("database busy".into())
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await;
        assert_eq!(result.epoch, 7);
        assert_eq!(result.ids, vec![1, 2, 3]);
        assert_eq!(result.completed, vec![1, 3]);
        assert_eq!(result.moved, vec![1]);
        assert_eq!(result.error.as_deref(), Some("database busy"));
        assert_eq!(*calls.borrow(), vec![101, 102, 103]);
    }

    #[tokio::test]
    async fn waiting_for_a_mutation_does_not_block_the_executor() {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let worker_gate = gate.clone();
        let worker = tokio::spawn(collect_results(
            0,
            vec![(1, 101, Operation::Action("archive".into()))],
            move |_, _| {
                let gate = worker_gate.clone();
                async move {
                    gate.acquire().await.unwrap().forget();
                    Ok(())
                }
            },
        ));
        tokio::task::yield_now().await;
        assert!(!worker.is_finished());
        gate.add_permits(1);
        assert_eq!(worker.await.unwrap().completed, vec![1]);
    }
}
