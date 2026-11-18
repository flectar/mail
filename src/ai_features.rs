//! The AI server (#241). Settings → AI is where the user sets it up, for the
//! app's AI features and translation by a model, and saves their own prompts.
//! Nothing is preset, and nothing goes to the server until the user uses a
//! feature.

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

// ---- For translation --------------------------------------------------------

/// An AI server and model are set up.
pub(super) fn ready() -> bool {
    SETTINGS.with_borrow(AiSettings::ready)
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
fn settings_changed(settings: AiSettings) {
    SETTINGS.set(settings);
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
            settings_changed(settings);
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
                settings_changed(saved);
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
}
