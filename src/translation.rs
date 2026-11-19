//! Translating messages (#240): the bar above a message in a language the
//! user doesn't read, Translate in the message's menu, and Settings →
//! Translation. The core finds the language on the device and asks the
//! service (`flectar_mail_core::translate`). A translated message shows its
//! translation until the user goes back to the original, for as long as the
//! app runs.

use super::*;
use flectar_mail_core::Core;
use flectar_mail_core::error::CoreError;
use flectar_mail_core::models::TranslationSettings;
use flectar_mail_core::translate::{
    DEEPL, LIBRETRANSLATE, LanguageChoice, MODEL, MessageTranslation, ReaderLanguages,
    TranslationError,
};
use slint::SharedString;

mod names;

/// The services in Settings' order, after None.
const SERVICES: [&str; 3] = [DEEPL, LIBRETRANSLATE, MODEL];

/// What the bar knows of the open message.
#[derive(Default)]
struct Open {
    message_id: Option<i64>,
    /// Its language, as the device found it.
    language: Option<String>,
    /// The bar offers to translate it.
    offer: bool,
    /// Its language was asked for.
    asked: bool,
}

/// A message shown translated.
struct Shown {
    html: String,
    source: Option<String>,
    target: String,
}

struct Env {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
}

thread_local! {
    static ENV: RefCell<Option<Env>> = const { RefCell::new(None) };
    static OPEN: RefCell<Open> = RefCell::new(Open::default());
    /// Messages shown translated in this session.
    static SHOWN: RefCell<HashMap<i64, Shown>> = RefCell::new(HashMap::new());
    /// The app's language, as `apply_language` chose it, and whether the user
    /// chose it rather than the system.
    static APP_LANGUAGE: RefCell<(String, bool)> = RefCell::new(("en".to_owned(), false));
    /// Settings' Translate into choices: the language tags, "" first for the
    /// app's language.
    static TARGETS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// The settings as last loaded.
    static SETTINGS: RefCell<TranslationSettings> = RefCell::new(TranslationSettings::default());
    /// The composer's Translate dialog's languages, as language tags.
    static DRAFT_TARGETS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn core(state: &Rc<RefCell<InboxState>>) -> Option<Arc<Core>> {
    state.borrow().core.as_ref().map(|core| core.file_core())
}

/// The languages the user reads: the app's, and the system's, as Firefox
/// counts them. Translations go into the app's language when the user chose
/// it, else into the system's first language.
fn reader_languages() -> ReaderLanguages {
    let (app_language, chosen) = APP_LANGUAGE.with_borrow(Clone::clone);
    let system: Vec<String> = sys_locale::get_locales().collect();
    let fallback_target = if chosen {
        app_language.clone()
    } else {
        system
            .first()
            .cloned()
            .unwrap_or_else(|| app_language.clone())
    };
    let mut reading = vec![app_language];
    reading.extend(system);
    ReaderLanguages {
        reading,
        fallback_target: fallback_target.replace('_', "-"),
    }
}

fn app_language() -> String {
    APP_LANGUAGE.with_borrow(|(language, _)| language.clone())
}

/// What went wrong, as the bar and Settings name it, with what the service
/// said when that's all there is.
pub(super) fn failure(error: &CoreError) -> (&'static str, String) {
    match error {
        CoreError::Translation(error) => match error {
            TranslationError::NotSetUp => ("not-set-up", String::new()),
            TranslationError::KeyRefused => ("key", String::new()),
            TranslationError::QuotaUsed => ("quota", String::new()),
            TranslationError::TooMany => ("busy", String::new()),
            TranslationError::Unreachable => ("offline", String::new()),
            TranslationError::BadAddress => ("address", String::new()),
            TranslationError::TooLong => ("long", String::new()),
            TranslationError::SameLanguage => ("same", String::new()),
            TranslationError::Service(message) => ("service", message.clone()),
        },
        CoreError::Offline => ("offline", String::new()),
        other => ("service", other.to_string()),
    }
}

/// The app's language changed: names follow it.
pub(super) fn language_changed(app: &AppWindow, language: &str, chosen: bool) {
    APP_LANGUAGE.set((language.to_owned(), chosen));
    let open_language = OPEN.with_borrow(|open| open.language.clone());
    let ui = app.global::<Translation>();
    ui.set_language_name(
        open_language
            .map(|tag| names::name(language, &tag, ""))
            .unwrap_or_default()
            .into(),
    );
    let shown = OPEN
        .with_borrow(|open| open.message_id)
        .and_then(|id| SHOWN.with_borrow(|shown| shown.get(&id).map(|shown| shown.target.clone())));
    if let Some(target) = shown {
        ui.set_target_name(names::name(language, &target, "").into());
    }
    project_settings(app);
}

/// The translated body of a message shown translated, for the reader.
pub(super) fn shown_html(message_id: i64) -> Option<String> {
    SHOWN.with_borrow(|shown| shown.get(&message_id).map(|shown| shown.html.clone()))
}

/// The reader shows `message`: the bar follows it. Its language is found
/// once its body is here.
pub(super) fn project(app: &AppWindow, message: &MailMessage) {
    let id = i64::from(message.id);
    if OPEN.with_borrow(|open| open.message_id) == Some(id) {
        let ask = !message.body_pending && OPEN.with_borrow(|open| !open.asked);
        if ask {
            OPEN.with_borrow_mut(|open| open.asked = true);
            ask_offer(id);
        }
        return;
    }
    OPEN.set(Open {
        message_id: Some(id),
        asked: !message.body_pending,
        ..Open::default()
    });
    let ui = app.global::<Translation>();
    ui.set_language_name("".into());
    ui.set_always(false);
    match SHOWN.with_borrow(|shown| shown.get(&id).map(|shown| shown.target.clone())) {
        Some(target) => {
            ui.set_target_name(names::name(&app_language(), &target, "").into());
            ui.set_state("shown".into());
        }
        None => ui.set_state("".into()),
    }
    if message.body_pending {
        return;
    }
    ask_offer(id);
}

/// No message is open.
pub(super) fn clear(app: &AppWindow) {
    OPEN.set(Open::default());
    app.global::<Translation>().set_state("".into());
}

fn ask_offer(message_id: i64) {
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(core) = core(&env.state) else { return };
        let app = env.app.clone();
        let languages = reader_languages();
        env.runtime.spawn(async move {
            let offer = core.translation_offer(message_id, languages).await;
            let always = core
                .translation_settings()
                .await
                .map(|settings| settings.always)
                .unwrap_or_default();
            let _ = app.upgrade_in_event_loop(move |app| {
                let Ok(offer) = offer else { return };
                if OPEN.with_borrow(|open| open.message_id) != Some(message_id) {
                    return;
                }
                OPEN.with_borrow_mut(|open| {
                    open.language = offer.language.clone();
                    open.offer = offer.offer;
                });
                let ui = app.global::<Translation>();
                let name = offer
                    .language
                    .as_deref()
                    .map(|tag| names::name(&app_language(), tag, ""))
                    .unwrap_or_default();
                ui.set_language_name(name.into());
                ui.set_always(
                    offer
                        .language
                        .as_ref()
                        .is_some_and(|language| always.contains(language)),
                );
                if ui.get_state() != "" {
                    return;
                }
                if offer.automatic {
                    translate(&app, message_id);
                } else if offer.offer {
                    ui.set_state("offer".into());
                }
            });
        });
    });
}

/// Shows the message again, translated or not.
fn render_again(app: &AppWindow) {
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        env.state.borrow().email_renderer.borrow_mut().loaded_key = None;
        if let Err(error) = render_current(app, &env.state, &env.runtime) {
            app.set_render_status(UiMessage::detail("Could not show the message: {}", error));
        }
    });
}

fn translate(app: &AppWindow, message_id: i64) {
    let ui = app.global::<Translation>();
    if ui.get_state() == "translating" {
        return;
    }
    ui.set_state("translating".into());
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(core) = core(&env.state) else { return };
        let weak = env.app.clone();
        let languages = reader_languages();
        env.runtime.spawn(async move {
            let result = core.translate_message(message_id, languages).await;
            let _ = weak.upgrade_in_event_loop(move |app| shown(&app, message_id, result));
        });
    });
}

fn shown(app: &AppWindow, message_id: i64, result: Result<MessageTranslation, CoreError>) {
    let open = OPEN.with_borrow(|open| open.message_id) == Some(message_id);
    let ui = app.global::<Translation>();
    match result {
        Ok(translation) => {
            let html = match (translation.html, translation.text) {
                (Some(html), _) => html,
                (None, Some(text)) => {
                    crate::mail::text_to_html_in(&text, translation.right_to_left)
                }
                (None, None) => return,
            };
            if let Some(source) = &translation.source {
                OPEN.with_borrow_mut(|open| {
                    if open.message_id == Some(message_id) && open.language.is_none() {
                        open.language = Some(source.clone());
                    }
                });
            }
            SHOWN.with_borrow_mut(|shown| {
                shown.insert(
                    message_id,
                    Shown {
                        html,
                        source: translation.source.clone(),
                        target: translation.target.clone(),
                    },
                )
            });
            if !open {
                return;
            }
            let language = app_language();
            if let Some(source) = translation.source {
                ui.set_language_name(names::name(&language, &source, "").into());
            }
            ui.set_target_name(names::name(&language, &translation.target, "").into());
            ui.set_state("shown".into());
            render_again(app);
        }
        Err(error) => {
            if !open {
                return;
            }
            let (kind, detail) = failure(&error);
            ui.set_failure(kind.into());
            ui.set_detail(detail.into());
            ui.set_state("failed".into());
        }
    }
}

fn show_original(app: &AppWindow) {
    let Some(message_id) = OPEN.with_borrow(|open| open.message_id) else {
        return;
    };
    let source = SHOWN.with_borrow_mut(|shown| shown.remove(&message_id));
    let offer = OPEN.with_borrow(|open| open.offer);
    let ui = app.global::<Translation>();
    // The way to translate it again stays where the translation was.
    ui.set_state(
        if offer || source.is_some_and(|shown| shown.source.is_some()) {
            "offer".into()
        } else {
            "".into()
        },
    );
    render_again(app);
}

fn choose(app: &AppWindow, choice: &str) {
    let Some((message_id, Some(language))) =
        OPEN.with_borrow(|open| open.message_id.map(|id| (id, open.language.clone())))
    else {
        return;
    };
    let choice = match choice {
        "always" => LanguageChoice::Always,
        "never" => LanguageChoice::Never,
        _ => LanguageChoice::Ask,
    };
    let ui = app.global::<Translation>();
    ui.set_always(choice == LanguageChoice::Always);
    match choice {
        // Firefox translates the page as its language goes on the list.
        LanguageChoice::Always if ui.get_state() == "offer" => translate(app, message_id),
        LanguageChoice::Never if ui.get_state() == "offer" => ui.set_state("".into()),
        _ => {}
    }
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(core) = core(&env.state) else { return };
        let weak = env.app.clone();
        env.runtime.spawn(async move {
            let result = core.set_language_choice(language, choice).await;
            let _ = weak.upgrade_in_event_loop(move |app| {
                if let Err(error) = result {
                    crate::undo::tell(
                        &app,
                        UiMessage::detail("Could not save the translation setting: {}", error),
                    );
                }
                refresh();
            });
        });
    });
}

// ---- The composer ---------------------------------------------------------

/// A translation service is set up, so the composer offers to translate: a
/// model needs the AI server set up too.
pub(super) fn ready() -> bool {
    SETTINGS.with_borrow(|settings| match settings.service.as_str() {
        "" => false,
        flectar_mail_core::translate::MODEL => crate::ai_features::ready(),
        _ => true,
    })
}

/// The composer's Translate dialog's choices: the service's languages by name,
/// with the one a message was last translated into chosen, else the language
/// of the message a reply answers.
pub(super) fn draft_choices(
    targets: Vec<(String, String)>,
    last: &str,
    answered: Option<&str>,
) -> (Vec<SharedString>, i32) {
    let language = app_language();
    let mut named: Vec<(String, String)> = targets
        .into_iter()
        .map(|(code, name)| {
            let shown = names::menu_name(&language, &code, &name);
            (code, shown)
        })
        .collect();
    named.sort_by(|a, b| a.1.cmp(&b.1));
    let position = |wanted: &str| {
        named
            .iter()
            .position(|(code, _)| code.eq_ignore_ascii_case(wanted))
            .or_else(|| {
                let primary = wanted.split(['-', '_']).next().unwrap_or_default();
                named.iter().position(|(code, _)| {
                    code.split(['-', '_'])
                        .next()
                        .is_some_and(|code| code.eq_ignore_ascii_case(primary))
                })
            })
    };
    let index = Some(last)
        .filter(|last| !last.is_empty())
        .and_then(position)
        .or_else(|| answered.and_then(position))
        .unwrap_or(0);
    let names = named
        .iter()
        .map(|(_, name)| SharedString::from(name.as_str()))
        .collect();
    DRAFT_TARGETS.set(named.into_iter().map(|(code, _)| code).collect());
    (names, i32::try_from(index).unwrap_or(0))
}

/// The language tag of the composer dialog's choice `index`.
pub(super) fn draft_target(index: i32) -> Option<String> {
    let index = usize::try_from(index).ok()?;
    DRAFT_TARGETS.with_borrow(|targets| targets.get(index).cloned())
}

/// Remembers the language a message was translated into, for the next one.
pub(super) fn remember_draft_target(target: &str) {
    if SETTINGS.with_borrow(|settings| settings.compose_target == target) {
        return;
    }
    let target = target.to_owned();
    save_settings_quietly(move |settings| settings.compose_target = target);
}

// ---- Settings → Translation -----------------------------------------------

/// The lists as Settings shows them, named in the app's language.
fn languages(tags: &[String]) -> ModelRc<TranslationLanguage> {
    let language = app_language();
    let mut list: Vec<TranslationLanguage> = tags
        .iter()
        .map(|tag| TranslationLanguage {
            code: tag.as_str().into(),
            name: names::menu_name(&language, tag, "").into(),
        })
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    ModelRc::new(VecModel::from(list))
}

/// Settings shows what the core has.
fn project_settings(app: &AppWindow) {
    let settings = SETTINGS.with_borrow(Clone::clone);
    let ui = app.global::<Translation>();
    let index = SERVICES
        .iter()
        .position(|service| *service == settings.service)
        .map_or(0, |index| index + 1);
    ui.set_service_index(i32::try_from(index).unwrap_or(0));
    ui.set_libretranslate_url(settings.libretranslate_url.as_str().into());
    ui.set_offer(settings.offer);
    ui.set_always_languages(languages(&settings.always));
    ui.set_never_languages(languages(&settings.never));
}

/// Settings' Translate into choices: the app's language first, then the
/// service's languages by name.
fn project_targets(app: &AppWindow, targets: Vec<(String, String)>) {
    let language = app_language();
    let fallback = reader_languages().fallback_target;
    let mut named: Vec<(String, String)> = targets
        .into_iter()
        .map(|(code, name)| {
            let shown = names::menu_name(&language, &code, &name);
            (code, shown)
        })
        .collect();
    named.sort_by(|a, b| a.1.cmp(&b.1));
    let first = translated(
        app,
        &UiMessage::detail(
            "Automatic ({})",
            names::menu_name(&language, &fallback, &fallback),
        ),
    );
    let chosen = SETTINGS.with_borrow(|settings| settings.target.clone());
    let index = named
        .iter()
        .position(|(code, _)| code.eq_ignore_ascii_case(&chosen))
        .map_or(0, |index| index + 1);
    let mut choices: Vec<SharedString> = vec![first];
    choices.extend(
        named
            .iter()
            .map(|(_, name)| SharedString::from(name.as_str())),
    );
    TARGETS.set(
        std::iter::once(String::new())
            .chain(named.into_iter().map(|(code, _)| code))
            .collect(),
    );
    let ui = app.global::<Translation>();
    ui.set_target_choices(ModelRc::new(VecModel::from(choices)));
    ui.set_target_index(i32::try_from(index).unwrap_or(0));
}

fn refresh() {
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(core) = core(&env.state) else { return };
        let weak = env.app.clone();
        env.runtime.spawn(async move {
            let Ok(settings) = core.translation_settings().await else {
                return;
            };
            let key_saved = if settings.service.is_empty() {
                false
            } else {
                core.translation_key_saved(settings.service.clone())
                    .await
                    .unwrap_or(false)
            };
            let targets = if settings.service.is_empty() {
                Vec::new()
            } else {
                core.translation_targets()
                    .await
                    .map(|targets| {
                        targets
                            .into_iter()
                            .map(|target| (target.code, target.name))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let _ = weak.upgrade_in_event_loop(move |app| {
                SETTINGS.set(settings);
                project_settings(&app);
                app.global::<Translation>().set_key_saved(key_saved);
                project_targets(&app, targets);
                crate::composer::services_changed();
            });
        });
    });
}

/// Saves the settings as `change` leaves them, without showing them again.
fn save_settings_quietly(change: impl FnOnce(&mut TranslationSettings)) {
    let mut settings = SETTINGS.with_borrow(Clone::clone);
    change(&mut settings);
    SETTINGS.set(settings.clone());
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(core) = core(&env.state) else { return };
        env.runtime.spawn(async move {
            if let Err(error) = core.set_translation_settings(settings).await {
                tracing::warn!(%error, "saving the translation settings failed");
            }
        });
    });
}

/// The core is ready: the settings come in, without asking any service.
pub(super) fn core_ready(core: Arc<Core>) {
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let weak = env.app.clone();
        env.runtime.spawn(async move {
            let Ok(settings) = core.translation_settings().await else {
                return;
            };
            let _ = weak.upgrade_in_event_loop(move |app| {
                SETTINGS.set(settings);
                project_settings(&app);
            });
        });
    });
}

/// Saves the settings as `change` leaves them, then shows them.
fn save_settings(change: impl FnOnce(&mut TranslationSettings)) {
    let mut settings = SETTINGS.with_borrow(Clone::clone);
    change(&mut settings);
    SETTINGS.set(settings.clone());
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(core) = core(&env.state) else { return };
        let weak = env.app.clone();
        env.runtime.spawn(async move {
            let result = core.set_translation_settings(settings).await;
            let _ = weak.upgrade_in_event_loop(move |app| {
                if let Err(error) = result {
                    crate::undo::tell(
                        &app,
                        UiMessage::detail("Could not save the translation setting: {}", error),
                    );
                }
                refresh();
            });
        });
    });
}

/// Saves the service's address and a newly typed key, then asks the
/// service for its languages, which tells whether it works.
fn save_service(app: &AppWindow, key: String) {
    let ui = app.global::<Translation>();
    let url = ui.get_libretranslate_url().trim().to_owned();
    let mut settings = SETTINGS.with_borrow(Clone::clone);
    settings.libretranslate_url = url;
    SETTINGS.set(settings.clone());
    ui.set_settings_state("saving".into());
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(core) = core(&env.state) else { return };
        let weak = env.app.clone();
        env.runtime.spawn(async move {
            let service = settings.service.clone();
            let mut result = core.set_translation_settings(settings).await;
            if result.is_ok() && !key.trim().is_empty() {
                result = core.set_translation_key(service, key).await;
            }
            let checked = match result {
                Ok(()) => core.translation_targets().await.map(|_| ()),
                Err(error) => Err(error),
            };
            let _ = weak.upgrade_in_event_loop(move |app| {
                let ui = app.global::<Translation>();
                match checked {
                    Ok(()) => {
                        ui.set_settings_state("ready".into());
                        ui.set_settings_failure("".into());
                        ui.set_settings_detail("".into());
                    }
                    Err(error) => {
                        let (kind, detail) = failure(&error);
                        ui.set_settings_state("failed".into());
                        ui.set_settings_failure(kind.into());
                        ui.set_settings_detail(detail.into());
                    }
                }
                refresh();
            });
        });
    });
}

fn remove_key() {
    let service = SETTINGS.with_borrow(|settings| settings.service.clone());
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(core) = core(&env.state) else { return };
        let weak = env.app.clone();
        env.runtime.spawn(async move {
            let result = core.set_translation_key(service, String::new()).await;
            let _ = weak.upgrade_in_event_loop(move |app| {
                if let Err(error) = result {
                    crate::undo::tell(
                        &app,
                        UiMessage::detail("Could not remove the key: {}", error),
                    );
                }
                app.global::<Translation>().set_settings_state("".into());
                refresh();
            });
        });
    });
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
    let ui = app.global::<Translation>();
    let weak = app.as_weak();
    ui.on_translate(move || {
        let Some(app) = weak.upgrade() else { return };
        if let Some(message_id) = OPEN.with_borrow(|open| open.message_id) {
            translate(&app, message_id);
        }
    });
    let weak = app.as_weak();
    ui.on_show_original(move || {
        if let Some(app) = weak.upgrade() {
            show_original(&app);
        }
    });
    let weak = app.as_weak();
    ui.on_choose(move |choice| {
        if let Some(app) = weak.upgrade() {
            choose(&app, &choice);
        }
    });
    let weak = app.as_weak();
    ui.on_open_settings(move || {
        if let Some(app) = weak.upgrade() {
            app.set_settings_tab("Translation".into());
            app.invoke_open_settings();
        }
    });
    ui.on_refresh(refresh);
    let weak = app.as_weak();
    ui.on_choose_service(move |index| {
        let service = usize::try_from(index - 1)
            .ok()
            .and_then(|index| SERVICES.get(index))
            .map_or("", |service| service);
        if let Some(app) = weak.upgrade() {
            app.global::<Translation>().set_settings_state("".into());
        }
        save_settings(|settings| settings.service = service.to_owned());
    });
    let weak = app.as_weak();
    ui.on_save(move |key| {
        if let Some(app) = weak.upgrade() {
            save_service(&app, key.to_string());
        }
    });
    ui.on_remove_key(remove_key);
    ui.on_choose_target(move |index| {
        let target = TARGETS.with_borrow(|targets| {
            usize::try_from(index)
                .ok()
                .and_then(|index| targets.get(index).cloned())
                .unwrap_or_default()
        });
        save_settings(|settings| settings.target = target);
    });
    ui.on_offer_toggled(|offer| save_settings(|settings| settings.offer = offer));
    ui.on_forget(|code| {
        let code = code.to_string();
        save_settings(|settings| {
            settings.always.retain(|tag| *tag != code);
            settings.never.retain(|tag| *tag != code);
        });
    });
}
