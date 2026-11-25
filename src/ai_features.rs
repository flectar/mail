//! The AI server (#241). Settings → AI is where the user sets it up, for the
//! app's AI features and translation by a model, and saves their own prompts.
//! The card above a conversation shows its summary, or the answer to a prompt
//! on text the user selected, and the composer's writing tools take their
//! prompts from here too. Nothing is preset, and nothing goes to the server
//! until the user uses a feature.

use super::*;
use flectar_mail_core::Core;
use flectar_mail_core::error::CoreError;
use flectar_mail_core::models::{AiPrompt, AiSettings};
use slint::SharedString;

struct Env {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
}

/// What the card asked the server for, to ask again.
#[derive(Clone)]
enum Request {
    Summary(i64),
    Answer { instruction: String, text: String },
}

thread_local! {
    static ENV: RefCell<Option<Env>> = const { RefCell::new(None) };
    /// The settings as last loaded.
    static SETTINGS: RefCell<AiSettings> = RefCell::new(AiSettings::default());
    /// The prompts Settings shows, as saved when they came in: a refresh
    /// keeps what the user is typing unless the saved ones changed.
    static SHOWN_PROMPTS: RefCell<Option<Vec<AiPrompt>>> = const { RefCell::new(None) };
    /// The models a server offered, in the order Settings lists them, and
    /// that server's address.
    static MODELS: RefCell<(String, Vec<String>)> = const { RefCell::new((String::new(), Vec::new())) };
    /// Each reader's card.
    static CARDS: PerReader<Card> = const { PerReader::new() };
    /// Counts what the cards asked for, to tell an answer from those asked
    /// for before.
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// What a reader's card is about.
#[derive(Clone, Default)]
struct Card {
    /// The conversation the reader shows, and what tells it from others: its
    /// thread, or the message when it has none.
    shown_thread: Option<i64>,
    shown_key: Option<i64>,
    /// The conversation the card is for, what it asked, and which of the
    /// cards' questions that was.
    key: Option<i64>,
    request: Option<Request>,
    generation: u64,
}

fn card(key: ReaderKey) -> Card {
    CARDS.with(|cards| cards.get(key)).unwrap_or_default()
}

fn update_card(key: ReaderKey, change: impl FnOnce(&mut Card)) {
    let mut card = card(key);
    change(&mut card);
    CARDS.with(|cards| cards.set(key, card));
}

fn next_generation() -> u64 {
    let generation = GENERATION.get() + 1;
    GENERATION.set(generation);
    generation
}

/// The reader `key`, while its window is open.
fn reader_of(key: ReaderKey) -> Option<ReaderRef> {
    ENV.with_borrow(|env| {
        env.as_ref()
            .and_then(|env| key.host(&env.app))
            .and_then(|host| host.upgrade())
    })
}

fn core(state: &Rc<RefCell<InboxState>>) -> Option<Arc<Core>> {
    state.borrow().core.as_ref().map(|core| core.file_core())
}

/// What went wrong with the server, as Settings names it, with what the
/// server said when that's all there is.
fn failure(error: &CoreError) -> (&'static str, String) {
    match error {
        CoreError::Offline => ("offline", String::new()),
        CoreError::AiNotConfigured => ("model", String::new()),
        CoreError::Other(message)
            if message.contains("require HTTPS")
                || message.contains("invalid AI endpoint URL")
                || message.contains("has no host")
                || message.contains("credentials or fragments") =>
        {
            ("address", String::new())
        }
        CoreError::Other(message)
            if message.starts_with("ai api error 401")
                || message.starts_with("ai api error 403") =>
        {
            ("key", String::new())
        }
        other => ("service", other.to_string()),
    }
}

/// What went wrong, for a notice outside Settings.
pub(super) fn failure_notice(error: &CoreError) -> UiMessage {
    if matches!(error, CoreError::AiNotConfigured) {
        return UiMessage::plain("Set up an AI server in Settings to use AI features.");
    }
    match failure(error) {
        ("offline", _) => UiMessage::plain(
            "The AI server can't be reached. Check its address in Settings and that it's running.",
        ),
        ("address", _) => UiMessage::plain(
            "The AI server's address in Settings has to start with https://, or with http://localhost for a server on this computer.",
        ),
        ("key", _) => UiMessage::plain("The AI server didn't accept the key in Settings."),
        (_, detail) => UiMessage::detail(
            "The AI server didn't answer as expected. It said: {}",
            detail,
        ),
    }
}

/// Runs `work` with the core on the runtime and `then` with its result on
/// the UI thread.
fn with_core<T: Send + 'static>(
    work: impl FnOnce(Arc<Core>) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>
    + 'static,
    then: impl FnOnce(&AppWindow, T) + Send + 'static,
) {
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(core) = core(&env.state) else { return };
        let weak = env.app.clone();
        let future = work(core);
        env.runtime.spawn(async move {
            let result = future.await;
            let _ = weak.upgrade_in_event_loop(move |app| then(&app, result));
        });
    });
}

fn strings(list: impl IntoIterator<Item = impl AsRef<str>>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        list.into_iter()
            .map(|item| SharedString::from(item.as_ref()))
            .collect::<Vec<_>>(),
    ))
}

/// Opens Settings at the AI page.
pub(super) fn open_settings(app: &AppWindow) {
    app.set_settings_tab("AI".into());
    app.invoke_open_settings();
}

// ---- For translation, the card and the composer -----------------------------

/// An AI server and model are set up.
pub(super) fn ready() -> bool {
    SETTINGS.with_borrow(AiSettings::ready)
}

/// The names of the user's prompts, as menus list them.
pub(super) fn prompt_names() -> ModelRc<SharedString> {
    SETTINGS.with_borrow(|settings| strings(settings.prompts.iter().map(|prompt| &prompt.name)))
}

fn prompt(index: i32) -> Option<String> {
    let index = usize::try_from(index).ok()?;
    SETTINGS.with_borrow(|settings| {
        settings
            .prompts
            .get(index)
            .map(|prompt| prompt.instruction.clone())
    })
}

/// What Proofread, -1, or the user's prompt `index` asks of a draft.
pub(super) fn instruction(index: i32) -> Option<String> {
    if index < 0 {
        Some(flectar_mail_core::ai::PROOFREAD.to_owned())
    } else {
        prompt(index)
    }
}

// ---- The card above the conversation ---------------------------------------

/// The reader shows `message`: a card for another conversation goes.
pub(super) fn project(reader: &ReaderRef, message: &MailMessage) {
    let shown = message.thread_id.unwrap_or(-i64::from(message.id));
    let mut stale = false;
    update_card(reader.key(), |card| {
        card.shown_thread = message.thread_id;
        card.shown_key = Some(shown);
        stale = card.key.is_some_and(|key| key != shown);
    });
    if stale {
        close_card(reader);
    }
}

/// No message is shown.
pub(super) fn clear(reader: &ReaderRef) {
    update_card(reader.key(), |card| {
        card.shown_thread = None;
        card.shown_key = None;
    });
    close_card(reader);
}

/// A reader's window closed.
#[cfg(desktop)]
pub(super) fn forget(key: ReaderKey) {
    CARDS.with(|cards| cards.remove(key));
}

fn close_card(reader: &ReaderRef) {
    update_card(reader.key(), |card| {
        card.generation = next_generation();
        card.key = None;
        card.request = None;
    });
    let ui = reader.global::<AiSummary>();
    ui.set_state("".into());
    ui.set_replies_state("".into());
    ui.set_replies(ModelRc::default());
}

fn card_failed(reader: &ReaderRef, error: &CoreError) {
    let ui = reader.global::<AiSummary>();
    let (kind, detail) = match error {
        CoreError::AiNotConfigured => ("not-set-up", String::new()),
        other => failure(other),
    };
    ui.set_failure(kind.into());
    ui.set_detail(detail.into());
}

/// The reader `key` while it still waits for the answer to `generation`.
fn waiting(key: ReaderKey, generation: u64) -> Option<ReaderRef> {
    (card(key).generation == generation)
        .then(|| reader_of(key))
        .flatten()
}

/// Asks the server for `request`, in the card, in place of what it showed.
fn ask_server(reader: &ReaderRef, request: Request) {
    let key = reader.key();
    let generation = next_generation();
    update_card(key, |card| {
        card.generation = generation;
        card.key = card.shown_key;
        card.request = Some(request.clone());
    });
    let ui = reader.global::<AiSummary>();
    ui.set_mode(match request {
        Request::Summary(_) => "summary".into(),
        Request::Answer { .. } => "answer".into(),
    });
    ui.set_state("summarizing".into());
    ui.set_replies_state("".into());
    ui.set_replies(ModelRc::default());
    ui.set_unverified(ModelRc::default());
    match request {
        Request::Summary(thread_id) => with_core(
            move |core| Box::pin(async move { core.ai_summarize(thread_id).await }),
            move |_, result| {
                let Some(reader) = waiting(key, generation) else {
                    return;
                };
                let ui = reader.global::<AiSummary>();
                match result {
                    Ok(summary) => {
                        ui.set_key_points(strings(&summary.key_points));
                        ui.set_next_action(summary.next_action.unwrap_or_default().into());
                        ui.set_warning(summary.warning.unwrap_or_default().into());
                        ui.set_unverified(strings(&summary.unverified));
                        ui.set_omitted(i32::try_from(summary.omitted).unwrap_or(i32::MAX));
                        ui.set_proposed_reply(summary.proposed_reply.unwrap_or_default().into());
                        ui.set_state("shown".into());
                    }
                    Err(error) => {
                        card_failed(&reader, &error);
                        ui.set_state("failed".into());
                    }
                }
            },
        ),
        Request::Answer { instruction, text } => with_core(
            move |core| Box::pin(async move { core.ai_answer(text, instruction).await }),
            move |_, result| {
                let Some(reader) = waiting(key, generation) else {
                    return;
                };
                let ui = reader.global::<AiSummary>();
                match result {
                    Ok(answer) => {
                        ui.set_answer(answer.text.into());
                        ui.set_unverified(strings(&answer.unverified));
                        ui.set_state("shown".into());
                    }
                    Err(error) => {
                        card_failed(&reader, &error);
                        ui.set_state("failed".into());
                    }
                }
            },
        ),
    }
}

fn summarize(reader: &ReaderRef) {
    if let Some(thread_id) = card(reader.key()).shown_thread {
        ask_server(reader, Request::Summary(thread_id));
    }
}

/// Runs Explain, -1, or the user's prompt `index` on `text` the user selected
/// in a message.
fn ask(reader: &ReaderRef, index: i32, text: &str) {
    let text = text.trim();
    let instruction = if index < 0 {
        Some(flectar_mail_core::ai::EXPLAIN.to_owned())
    } else {
        prompt(index)
    };
    if let Some(instruction) = instruction.filter(|_| !text.is_empty()) {
        let text = text.to_owned();
        ask_server(reader, Request::Answer { instruction, text });
    }
}

fn retry(reader: &ReaderRef) {
    if let Some(request) = card(reader.key()).request {
        ask_server(reader, request);
    }
}

fn suggest_replies(reader: &ReaderRef) {
    let key = reader.key();
    let card = card(key);
    let Some(Request::Summary(thread_id)) = card.request else {
        return;
    };
    let generation = card.generation;
    reader
        .global::<AiSummary>()
        .set_replies_state("loading".into());
    with_core(
        move |core| Box::pin(async move { core.ai_quick_replies(thread_id).await }),
        move |_, result| {
            let Some(reader) = waiting(key, generation) else {
                return;
            };
            let ui = reader.global::<AiSummary>();
            match result {
                Ok(replies) => {
                    ui.set_replies(strings(&replies));
                    ui.set_replies_state("shown".into());
                }
                Err(error) => {
                    card_failed(&reader, &error);
                    ui.set_replies_state("failed".into());
                }
            }
        },
    );
}

/// The card's actions in a reader, and the prompts its selection menu lists.
pub(super) fn attach(reader: &ReaderRef) {
    let card = reader.global::<AiSummary>();
    card.set_prompts(prompt_names());
    let host = reader.host();
    card.on_summarize(move || {
        if let Some(reader) = host.upgrade() {
            summarize(&reader);
        }
    });
    let host = reader.host();
    card.on_ask(move |index, text| {
        if let Some(reader) = host.upgrade() {
            ask(&reader, index, &text);
        }
    });
    let host = reader.host();
    card.on_retry(move || {
        if let Some(reader) = host.upgrade() {
            retry(&reader);
        }
    });
    let host = reader.host();
    card.on_close(move || {
        if let Some(reader) = host.upgrade() {
            close_card(&reader);
        }
    });
    let host = reader.host();
    card.on_suggest_replies(move || {
        if let Some(reader) = host.upgrade() {
            suggest_replies(&reader);
        }
    });
    let host = reader.host();
    card.on_open_settings(move || {
        if let Some(reader) = host.upgrade() {
            open_settings(reader.app());
            reader.raise_main();
        }
    });
}

// ---- Settings → AI ---------------------------------------------------------

/// Settings and the reader show what the core has.
fn project_settings(app: &AppWindow, settings: &AiSettings, key_saved: bool) {
    let ui = app.global::<Ai>();
    ui.set_url(settings.url.as_str().into());
    ui.set_model(settings.model.as_str().into());
    ui.set_ready(settings.ready());
    ui.set_key_saved(key_saved);
}

fn project_prompts(app: &AppWindow, prompts: &[AiPrompt]) {
    let rows: Vec<AiPromptRow> = prompts
        .iter()
        .map(|prompt| AiPromptRow {
            name: prompt.name.as_str().into(),
            instruction: prompt.instruction.as_str().into(),
        })
        .collect();
    app.global::<Ai>()
        .set_prompts(ModelRc::new(VecModel::from(rows)));
    SHOWN_PROMPTS.set(Some(prompts.to_vec()));
}

/// The saved settings changed: the reader's and composers' menus follow.
fn settings_changed(app: &AppWindow, settings: AiSettings) {
    SETTINGS.set(settings);
    crate::reader_host::for_each_reader(app, |reader| {
        reader.global::<AiSummary>().set_prompts(prompt_names());
    });
    crate::composer::services_changed();
}

/// Settings lists the models of the server at `url` in place of the model's
/// name, with the one chosen, else the first. False when the chosen one isn't
/// there.
fn show_models(app: &AppWindow, url: String, models: Vec<String>) -> bool {
    let ui = app.global::<Ai>();
    let current = ui.get_model().to_string();
    let found = models.iter().position(|model| *model == current);
    ui.set_models(strings(&models));
    ui.set_model_index(
        found
            .and_then(|index| i32::try_from(index).ok())
            .unwrap_or(0),
    );
    if found.is_none()
        && let Some(first) = models.first()
    {
        ui.set_model(first.as_str().into());
    }
    MODELS.set((url, models));
    found.is_some()
}

fn refresh() {
    with_core(
        |core| {
            Box::pin(async move {
                let settings = core.ai_settings().await;
                let status = core.ai_status().await;
                (settings, status)
            })
        },
        |app, (settings, status)| {
            let (Ok(settings), Ok(status)) = (settings, status) else {
                return;
            };
            project_settings(app, &settings, status.key_saved);
            // The models listed stay for the server saved, and go with
            // another.
            let (url, models) = MODELS.take();
            if url == settings.url && !models.is_empty() {
                show_models(app, url, models);
            } else {
                app.global::<Ai>().set_models(ModelRc::default());
            }
            if SHOWN_PROMPTS.with_borrow(|shown| shown.as_ref() != Some(&settings.prompts)) {
                project_prompts(app, &settings.prompts);
            }
            settings_changed(app, settings);
        },
    );
}

/// The core is ready: the settings come in, so the composer and translation
/// know whether a server is set up, without asking the server anything.
pub(super) fn core_ready() {
    refresh();
}

fn save(app: &AppWindow, key: String) {
    let ui = app.global::<Ai>();
    let mut settings = SETTINGS.with_borrow(Clone::clone);
    settings.url = ui.get_url().trim().to_owned();
    settings.model = ui.get_model().trim().to_owned();
    ui.set_state("checking".into());
    let url = settings.url.clone();
    let saved_url = url.clone();
    let model = settings.model.clone();
    with_core(
        move |core| {
            Box::pin(async move {
                core.set_ai_settings(settings).await?;
                if !key.trim().is_empty() {
                    core.set_ai_key(key).await?;
                }
                core.ai_list_models(url).await
            })
        },
        move |app, result: Result<Vec<String>, CoreError>| {
            let ui = app.global::<Ai>();
            match result {
                // A server that lists models has to have the one saved.
                Ok(models) => {
                    let listed = !models.is_empty();
                    if show_models(app, saved_url, models) || !listed {
                        ui.set_state("ready".into());
                    } else {
                        ui.set_state("failed".into());
                        ui.set_failure("missing".into());
                        ui.set_detail(model.into());
                    }
                }
                Err(error) => {
                    let (kind, detail) = failure(&error);
                    ui.set_state("failed".into());
                    ui.set_failure(kind.into());
                    ui.set_detail(detail.into());
                }
            }
            refresh();
        },
    );
}

fn list_models(app: &AppWindow) {
    let ui = app.global::<Ai>();
    let url = ui.get_url().trim().to_owned();
    let listed_url = url.clone();
    ui.set_state("checking".into());
    with_core(
        move |core| Box::pin(async move { core.ai_list_models(url).await }),
        move |app, result: Result<Vec<String>, CoreError>| {
            let ui = app.global::<Ai>();
            match result {
                Ok(models) => {
                    ui.set_state("".into());
                    show_models(app, listed_url, models);
                }
                Err(error) => {
                    let (kind, detail) = failure(&error);
                    ui.set_state("failed".into());
                    ui.set_failure(kind.into());
                    ui.set_detail(detail.into());
                }
            }
        },
    );
}

fn remove_key() {
    with_core(
        |core| Box::pin(async move { core.set_ai_key(String::new()).await }),
        |app, result: Result<(), CoreError>| {
            if let Err(error) = result {
                crate::undo::tell(
                    app,
                    UiMessage::detail("Could not remove the key: {}", error),
                );
            }
            app.global::<Ai>().set_state("".into());
            refresh();
        },
    );
}

/// The prompt rows as Settings shows them, with `change` made to them.
fn edit_prompts(app: &AppWindow, change: impl FnOnce(&mut Vec<AiPromptRow>)) {
    let ui = app.global::<Ai>();
    let mut rows: Vec<AiPromptRow> = ui.get_prompts().iter().collect();
    change(&mut rows);
    ui.set_prompts(ModelRc::new(VecModel::from(rows)));
    ui.set_prompts_state("".into());
}

fn save_prompts(app: &AppWindow) {
    let mut settings = SETTINGS.with_borrow(Clone::clone);
    settings.prompts = app
        .global::<Ai>()
        .get_prompts()
        .iter()
        .map(|row| AiPrompt {
            name: row.name.trim().to_owned(),
            instruction: row.instruction.trim().to_owned(),
        })
        .collect();
    with_core(
        move |core| {
            Box::pin(async move {
                core.set_ai_settings(settings).await?;
                core.ai_settings().await
            })
        },
        |app, result: Result<AiSettings, CoreError>| match result {
            Ok(saved) => {
                project_prompts(app, &saved.prompts);
                settings_changed(app, saved);
                app.global::<Ai>().set_prompts_state("saved".into());
            }
            Err(error) => crate::undo::tell(
                app,
                UiMessage::detail("Could not save the prompts: {}", error),
            ),
        },
    );
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    ENV.set(Some(Env {
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
    }));
    let ui = app.global::<Ai>();
    ui.on_refresh(refresh);
    let weak = app.as_weak();
    ui.on_save(move |key| {
        if let Some(app) = weak.upgrade() {
            save(&app, key.to_string());
        }
    });
    ui.on_remove_key(remove_key);
    let weak = app.as_weak();
    ui.on_list_models(move || {
        if let Some(app) = weak.upgrade() {
            list_models(&app);
        }
    });
    let weak = app.as_weak();
    ui.on_model_chosen(move |index| {
        let Some(app) = weak.upgrade() else { return };
        let model = MODELS.with_borrow(|(_, models)| {
            usize::try_from(index)
                .ok()
                .and_then(|index| models.get(index).cloned())
        });
        if let Some(model) = model {
            app.global::<Ai>().set_model(model.into());
        }
    });
    let weak = app.as_weak();
    ui.on_add_prompt(move || {
        if let Some(app) = weak.upgrade() {
            edit_prompts(&app, |rows| rows.push(AiPromptRow::default()));
        }
    });
    let weak = app.as_weak();
    ui.on_remove_prompt(move |index| {
        let Some(app) = weak.upgrade() else { return };
        edit_prompts(&app, |rows| {
            if let Ok(index) = usize::try_from(index)
                && index < rows.len()
            {
                rows.remove(index);
            }
        });
    });
    let weak = app.as_weak();
    ui.on_save_prompts(move || {
        if let Some(app) = weak.upgrade() {
            save_prompts(&app);
        }
    });

    attach(&ReaderRef::main(app));
}
