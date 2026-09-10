//! Native account signature and OpenPGP controls.
use super::*;
use flectar_mail_core::{
    Core,
    mail_security::{MailSecurity, OpenedMessage},
    models::{Settings, Signature, SignatureDefaults},
};
use slint::SharedString;
use std::{cell::Cell, future::Future};

/// Async work is owned by a profile/account generation. A removed account or
/// replaced core must never receive a late settings or decrypted-content result.
#[derive(Clone)]
struct UiWork {
    runtime: Rc<tokio::runtime::Runtime>,
    generation: Rc<Cell<u64>>,
}
impl UiWork {
    fn spawn<T: Send + 'static>(
        &self,
        app: &AppWindow,
        busy: bool,
        work: impl Future<Output = Result<T, String>> + Send + 'static,
        apply: impl FnOnce(AppWindow, Result<T, String>) + 'static,
    ) {
        if busy {
            app.global::<AccountMailPreferences>().set_busy(true);
        }
        let weak = app.as_weak();
        let generation = self.generation.clone();
        let expected = generation.get();
        let task = self.runtime.spawn(work);
        let abort = task.abort_handle();
        if let Err(error) = slint::spawn_local(async move {
            let result = task.await.unwrap_or_else(|e| Err(e.to_string()));
            if generation.get() != expected {
                return;
            }
            if let Some(app) = weak.upgrade() {
                if busy {
                    app.global::<AccountMailPreferences>().set_busy(false);
                }
                apply(app, result);
            }
        }) {
            abort.abort();
            let ui = app.global::<AccountMailPreferences>();
            if busy {
                ui.set_busy(false);
            }
            ui.set_status(error.to_string().into());
        }
    }
}

async fn preferences(core: &Core) -> Result<(Settings, Vec<flectar_mail_core::models::AccountConfig>), String> {
    let settings = core.get_settings().await.map_err(|e| e.to_string())?;
    let configs = core.list_account_configs().await.map_err(|e| e.to_string())?;
    Ok((settings, configs))
}

fn core(state: &Rc<RefCell<InboxState>>) -> Option<Arc<Core>> {
    state
        .borrow()
        .core
        .as_ref()
        .map(|s| s.account_preferences_core())
}
fn status(app: &AppWindow, result: Result<(), String>, success: &str) {
    app.global::<AccountMailPreferences>()
        .set_status(match result {
            Ok(()) => success.into(),
            Err(e) => e.into(),
        });
}
fn choices(settings: &Settings, account_id: i64) -> Vec<Signature> {
    settings
        .signature_list
        .iter()
        .filter(|s| s.account_id == account_id)
        .cloned()
        .collect()
}
fn project(app: &AppWindow, settings: &Settings, account_id: i64) {
    let ui = app.global::<AccountMailPreferences>();
    let signatures = choices(settings, account_id);
    ui.set_signatures(ModelRc::new(VecModel::from(
        signatures
            .iter()
            .map(|s| SignatureChoice {
                id: s.id.clone().into(),
                name: s.name.clone().into(),
                text: flectar_mail_core::signatures::plain_text(s).into(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_signature_names(ModelRc::new(VecModel::from(
        signatures
            .iter()
            .map(|s| SharedString::from(&s.name))
            .collect::<Vec<_>>(),
    )));
    let mut names = vec![SharedString::from("No signature")];
    names.extend(signatures.iter().map(|s| SharedString::from(&s.name)));
    ui.set_default_names(ModelRc::new(VecModel::from(names)));
    let defaults = settings
        .signature_defaults
        .get(&account_id.to_string())
        .cloned()
        .unwrap_or_default();
    let index = |id: Option<String>| {
        signatures
            .iter()
            .position(|s| Some(&s.id) == id.as_ref())
            .map_or(0, |i| i as i32 + 1)
    };
    ui.set_new_index(index(defaults.new_id));
    ui.set_reply_index(index(defaults.reply_id));
}

pub(super) fn register(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
    document: &Rc<RefCell<RichComposeDocument>>,
    editor: &Rc<RefCell<CosmicComposeEditor>>,
) {
    let ui = app.global::<AccountMailPreferences>();
    ui.set_desktop_supported(!cfg!(any(target_os = "android", target_os = "ios")));
    let tasks = UiWork { runtime: runtime.clone(), generation: Rc::new(Cell::new(0)) };
    let context_state = state.clone();
    let context_generation = tasks.generation.clone();
    let context = Rc::new(RefCell::new((core(state), state.borrow().connected_accounts.iter().map(|a| a.id).collect::<Vec<_>>())));
    let weak = app.as_weak();
    ui.on_context_changed(move || {
        let current_core = core(&context_state);
        let accounts = context_state.borrow().connected_accounts.iter().map(|a| a.id).collect::<Vec<_>>();
        let mut previous = context.borrow_mut();
        let same_core = match (&previous.0, &current_core) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same_core && previous.1 == accounts { return; }
        *previous = (current_core, accounts);
        context_generation.set(context_generation.get().wrapping_add(1));
        let Some(app) = weak.upgrade() else { return; };
        let ui = app.global::<AccountMailPreferences>();
        ui.set_busy(false);
        ui.set_account_id(-1);
        ui.set_account_label("".into());
        ui.set_signatures(ModelRc::default());
        ui.set_signature_names(ModelRc::default());
        ui.set_default_names(ModelRc::default());
        ui.invoke_edit_signature(-1);
        ui.set_fingerprint("".into());
        ui.set_recipient_keys("".into());
        ui.set_key_inventory("".into());
        ui.set_status("".into());
        ui.invoke_close_reader();
        ui.invoke_composer_reset();
    });
    let weak = app.as_weak();
    let state_load = state.clone();
    let work = tasks.clone();
    ui.on_load(move |id| {
        let Some(app) = weak.upgrade() else { return };
        let Some(core) = core(&state_load) else {
            return;
        };
        let ui = app.global::<AccountMailPreferences>();
        if ui.get_busy() {
            return;
        }
        work.spawn(&app, true, async move { preferences(&core).await }, move |app, result| {
        let ui = app.global::<AccountMailPreferences>();
        match result {
            Ok((settings, configs)) => {
                if let Some(config) = configs.iter().find(|c| c.id == i64::from(id)) {
                    ui.set_account_id(id);
                    ui.set_account_label(config.email.clone().into());
                    ui.set_status("".into());
                    ui.set_key_inventory("".into());
                    project(&app, &settings, config.id);
                    ui.invoke_edit_signature(-1);
                    let p = &config.settings.security;
                    ui.set_fingerprint(p.signing_fingerprint.clone().into());
                    ui.set_sign(p.sign_by_default);
                    ui.set_encrypt(p.require_encryption);
                    ui.set_recipient_keys(
                        p.recipient_keys
                            .iter()
                            .map(|(e, k)| format!("{e}={k}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                            .into(),
                    );
                }
            }
            Err(e) => ui.set_status(e.into()),
        }
        });
    });
    let weak = app.as_weak();
    ui.on_edit_signature(move |index| {
        let Some(app) = weak.upgrade() else { return };
        let ui = app.global::<AccountMailPreferences>();
        let signature = usize::try_from(index)
            .ok()
            .and_then(|i| ui.get_signatures().row_data(i));
        ui.set_editing_index(index);
        ui.set_editing_id(signature.as_ref().map(|s| s.id.clone()).unwrap_or_default());
        ui.set_signature_name(
            signature
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_default(),
        );
        ui.set_signature_text(signature.map(|s| s.text).unwrap_or_default());
    });
    let weak = app.as_weak();
    let state_save = state.clone();
    let work = tasks.clone();
    ui.on_save_signature(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(core) = core(&state_save) else {
            return;
        };
        let ui = app.global::<AccountMailPreferences>();
        if ui.get_busy() { return; }
        let signature = Signature {
            id: ui.get_editing_id().to_string(),
            account_id: i64::from(ui.get_account_id()),
            name: ui.get_signature_name().to_string(),
            html: flectar_mail_core::signatures::text_html(ui.get_signature_text().as_str()),
        };
        work.spawn(&app, true, async move {
            let saved = core.save_signature(signature).await.map_err(|e| e.to_string())?;
            let settings = core.get_settings().await.map_err(|e| e.to_string())?;
            Ok((saved, settings))
        }, |app, result| {
            let ui = app.global::<AccountMailPreferences>();
            match result {
                Ok((saved, settings)) => {
                    project(&app, &settings, saved.account_id);
                    let index = choices(&settings, saved.account_id).iter().position(|s| s.id == saved.id).unwrap_or(0);
                    ui.invoke_edit_signature(index as i32);
                    ui.set_status("Signature saved.".into());
                }
                Err(e) => ui.set_status(e.into()),
            }
        });
    });
    let weak = app.as_weak();
    let state_delete = state.clone();
    let work = tasks.clone();
    ui.on_delete_signature(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(core) = core(&state_delete) else {
            return;
        };
        let ui = app.global::<AccountMailPreferences>();
        if ui.get_busy() { return; }
        let id = i64::from(ui.get_account_id());
        let signature = ui.get_editing_id().to_string();
        work.spawn(&app, true, async move {
            core.delete_signature(id, signature).await.map_err(|e| e.to_string())?;
            core.get_settings().await.map_err(|e| e.to_string())
        }, move |app, result| {
            match result {
                Ok(settings) => {
                    project(&app, &settings, id);
                    app.global::<AccountMailPreferences>().invoke_edit_signature(-1);
                    status(&app, Ok(()), "Signature deleted; affected defaults cleared.");
                }
                Err(e) => status(&app, Err(e), ""),
            }
        });
    });
    let weak = app.as_weak();
    let state_defaults = state.clone();
    let work = tasks.clone();
    ui.on_save_defaults(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(core) = core(&state_defaults) else {
            return;
        };
        let ui = app.global::<AccountMailPreferences>();
        if ui.get_busy() { return; }
        let id = |index: i32| {
            usize::try_from(index - 1)
                .ok()
                .and_then(|i| ui.get_signatures().row_data(i))
                .map(|s| s.id.to_string())
        };
        let account = i64::from(ui.get_account_id());
        let defaults = SignatureDefaults { new_id: id(ui.get_new_index()), reply_id: id(ui.get_reply_index()) };
        work.spawn(&app, true, async move {
            core.set_signature_defaults(account, defaults).await.map_err(|e| e.to_string())
        }, |app, result| status(&app, result, "Signature defaults saved."));
    });
    let weak = app.as_weak();
    let state_security = state.clone();
    let work = tasks.clone();
    ui.on_save_security(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(core) = core(&state_security) else {
            return;
        };
        let ui = app.global::<AccountMailPreferences>();
        if ui.get_busy() {
            return;
        }
        let mut policy = MailSecurity {
            signing_fingerprint: ui.get_fingerprint().to_string(),
            sign_by_default: ui.get_sign(),
            require_encryption: ui.get_encrypt(),
            ..Default::default()
        };
        for line in ui
            .get_recipient_keys()
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            let Some((email, key)) = line.split_once('=') else {
                ui.set_status("Use one email=full-fingerprint per line.".into());
                return;
            };
            if policy
                .recipient_keys
                .insert(email.trim().to_lowercase(), key.trim().to_owned())
                .is_some()
            {
                ui.set_status("Remove duplicate recipient addresses.".into());
                return;
            }
        }
        let id = i64::from(ui.get_account_id());
        work.spawn(&app, true, async move {
            core.set_mail_security(id, policy).await.map_err(|e| e.to_string())
        }, |app, result| status(&app, result, "OpenPGP settings saved."));
    });
    register_keys_and_reader(app, state, &tasks);
    register_composer(app, state, &tasks, document, editor);
}

fn show_message(app: &AppWindow, message: OpenedMessage, memory: &Arc<std::sync::Mutex<Vec<u8>>>) {
    *memory.lock().unwrap() = message.mime;
    let ui = app.global::<AccountMailPreferences>();
    ui.set_reader_text(message.text.into());
    ui.set_reader_status(message.status.into());
    ui.set_reader_open(true);
}
fn register_keys_and_reader(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    tasks: &UiWork,
) {
    let memory = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let reader_generation = Rc::new(Cell::new(0_u64));
    let ui = app.global::<AccountMailPreferences>();
    let weak = app.as_weak();
    let work = tasks.clone();
    let opened = memory.clone();
    let generation = reader_generation.clone();
    ui.on_key_action(move |action| {
        let Some(app) = weak.upgrade() else { return };
        let ui = app.global::<AccountMailPreferences>();
        if ui.get_busy() || ui.get_account_id() < 0 { return; }
        let key = ui.get_fingerprint().to_string();
        let email = ui.get_account_label().to_string();
        let opened = opened.clone();
        let generation = generation.clone();
        let expected = generation.get();
        work.spawn(&app, true, async move { key_action(action.as_str(), &key, &email).await }, move |app, result| {
            let ui = app.global::<AccountMailPreferences>();
            match result {
                Ok(KeyResult::Generated(key)) => { ui.set_fingerprint(key.into()); ui.set_status("Key created. Back up your private key and revocation certificate in GnuPG, then save OpenPGP settings.".into()); }
                Ok(KeyResult::Inventory(text)) => {
                    ui.set_key_inventory(text.into());
                    ui.set_status("Keys loaded. Verify fingerprints before trusting recipient keys.".into());
                }
                Ok(KeyResult::Message(message)) if generation.get() == expected => show_message(&app, message, &opened),
                Ok(KeyResult::Message(_)) => {},
                Ok(KeyResult::Done(text)) => ui.set_status(text.into()),
                Err(e) => ui.set_status(e.into()),
            }
        });
    });
    let weak = app.as_weak();
    let work = tasks.clone();
    let state = state.clone();
    let opened = memory.clone();
    let generation = reader_generation.clone();
    ui.on_read_message(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(core) = core(&state) else { return };
        let Some((selected_id, thread_id)) = ({
            let state = state.borrow();
            state.selected_id.and_then(|id| state.messages.iter().find(|m| m.id == id))
                .and_then(|m| m.thread_id.map(|thread| (m.id, thread)))
        }) else { return; };
        let ui = app.global::<AccountMailPreferences>();
        if ui.get_busy() { return; }
        ui.set_reader_text("".into());
        ui.set_reader_status("Opening OpenPGP message…".into());
        ui.set_reader_open(true);
        *opened.lock().unwrap() = Vec::new();
        let opened = opened.clone();
        let state = state.clone();
        let generation = generation.clone();
        let expected = generation.get();
        work.spawn(&app, true, async move {
            let detail = core.get_latest_thread_body(thread_id).await.map_err(|e| e.to_string())?;
            core.open_openpgp_message(detail.id).await.map_err(|e| e.to_string())
        }, move |app, result| {
            if generation.get() != expected || state.borrow().selected_id != Some(selected_id) {
                return;
            }
            match result {
                Ok(message) => show_message(&app, message, &opened),
                Err(e) => app.global::<AccountMailPreferences>().set_reader_status(e.into()),
            }
        });
    });
    let memory_close = memory.clone();
    let weak = app.as_weak();
    let generation = reader_generation.clone();
    ui.on_close_reader(move || {
        generation.set(generation.get().wrapping_add(1));
        *memory_close.lock().unwrap() = Vec::new();
        if let Some(app) = weak.upgrade() {
            let ui = app.global::<AccountMailPreferences>();
            ui.set_reader_open(false);
            ui.set_reader_text("".into());
            ui.set_reader_status("".into());
        }
    });
    let weak = app.as_weak();
    let work = tasks.clone();
    ui.on_export_message(move || {
        let Some(app) = weak.upgrade() else { return };
        if app.global::<AccountMailPreferences>().get_busy() { return; }
        let data = memory.lock().unwrap().clone();
        if data.is_empty() { return; }
        let generation = reader_generation.clone();
        let expected = generation.get();
        work.spawn(&app, true, export_message(data), move |app, result| {
            if generation.get() == expected && let Err(e) = result {
                app.global::<AccountMailPreferences>().set_reader_status(e.into());
            }
        });
    });
}

// Mobile keeps the shared UI result interface, but key actions return an
// unsupported-platform error until a native OpenPGP backend is available.
#[cfg_attr(any(target_os = "android", target_os = "ios"), allow(dead_code))]
enum KeyResult {
    Generated(String),
    Inventory(String),
    Message(OpenedMessage),
    Done(String),
}
#[cfg(not(any(target_os = "android", target_os = "ios")))]
async fn key_action(action: &str, fingerprint: &str, email: &str) -> Result<KeyResult, String> {
    let gpg = flectar_mail_core::mail_security::Gpg::default();
    match action {
        "generate" => gpg
            .generate_key(email)
            .await
            .map(KeyResult::Generated)
            .map_err(|e| e.to_string()),
        "list" => {
            let secret = gpg.list_keys(true).await.map_err(|e| e.to_string())?;
            let public = gpg.list_keys(false).await.map_err(|e| e.to_string())?;
            let display = |listing: &str| {
                listing
                    .lines()
                    .filter_map(|line| {
                        let f: Vec<_> = line.split(':').collect();
                        match f.first().copied()? {
                            "pub" => Some("Public key".to_owned()),
                            "sec" => Some("Private key".to_owned()),
                            "fpr" => f.get(9).map(|s| format!("  {s}")),
                            "uid" => f.get(9).map(|s| {
                                format!("  {}", flectar_mail_core::mail_security::decode_colon(s))
                            }),
                            _ => None,
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(KeyResult::Inventory(format!(
                "{}\n\n{}",
                display(&secret),
                display(&public)
            )))
        }
        "import" | "open" => {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(if action == "import" {
                    "Import OpenPGP key"
                } else {
                    "Open OpenPGP message"
                })
                .pick_file()
                .await
            else {
                return Ok(KeyResult::Done("Cancelled.".into()));
            };
            let limit = if action == "import" {
                1024 * 1024
            } else {
                100 * 1024 * 1024
            };
            let input = tokio::fs::File::open(file.path())
                .await
                .map_err(|e| e.to_string())?;
            use tokio::io::AsyncReadExt;
            let mut data = Vec::new();
            input
                .take(limit + 1)
                .read_to_end(&mut data)
                .await
                .map_err(|e| e.to_string())?;
            if data.len() as u64 > limit {
                return Err("File exceeds the size limit.".into());
            }
            if action == "import" {
                gpg.import_key(&data).await.map_err(|e| e.to_string())?;
                Ok(KeyResult::Done(
                    "Key imported. Verify its fingerprint before trusting it.".into(),
                ))
            } else {
                gpg.open_message(&data, None)
                    .await
                    .map(KeyResult::Message)
                    .map_err(|e| e.to_string())
            }
        }
        "export" => {
            let data = gpg
                .export_public_key(fingerprint)
                .await
                .map_err(|e| e.to_string())?;
            if let Some(file) = rfd::AsyncFileDialog::new()
                .set_file_name("public-key.asc")
                .save_file()
                .await
            {
                file.write(&data).await.map_err(|e| e.to_string())?;
                Ok(KeyResult::Done("Public key exported.".into()))
            } else {
                Ok(KeyResult::Done("Cancelled.".into()))
            }
        }
        _ => Err("Unknown key action.".into()),
    }
}
#[cfg(any(target_os = "android", target_os = "ios"))]
async fn key_action(_: &str, _: &str, _: &str) -> Result<KeyResult, String> {
    Err("OpenPGP requires desktop GnuPG.".into())
}
#[cfg(not(any(target_os = "android", target_os = "ios")))]
async fn export_message(data: Vec<u8>) -> Result<(), String> {
    if let Some(file) = rfd::AsyncFileDialog::new()
        .set_title("Save decrypted message (contains private content)")
        .set_file_name("decrypted-message.eml")
        .save_file()
        .await
    {
        file.write(&data).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[cfg(any(target_os = "android", target_os = "ios"))]
async fn export_message(_: Vec<u8>) -> Result<(), String> {
    Err("Message export is unavailable on this platform.".into())
}

fn register_composer(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    tasks: &UiWork,
    document: &Rc<RefCell<RichComposeDocument>>,
    editor: &Rc<RefCell<CosmicComposeEditor>>,
) {
    // Only replace the exact unedited block we inserted. User edits are never
    // discarded when switching identity or selecting a different signature.
    let inserted = Rc::new(RefCell::new(None::<String>));
    let available = Rc::new(RefCell::new(Vec::<Signature>::new()));
    let ui = app.global::<AccountMailPreferences>();
    let reset = inserted.clone();
    let generation = Rc::new(Cell::new(0_u64));
    let reset_generation = generation.clone();
    let reset_list = available.clone();
    let weak = app.as_weak();
    ui.on_composer_reset(move || {
        reset_generation.set(reset_generation.get().wrapping_add(1));
        *reset.borrow_mut() = None;
        reset_list.borrow_mut().clear();
        if let Some(app) = weak.upgrade() {
            let ui = app.global::<AccountMailPreferences>();
            ui.set_composer_signature_names(ModelRc::default());
            ui.set_composer_signature_index(0);
            ui.set_composer_security("".into());
            ui.set_composer_preferences_ready(false);
        }
    });
    let weak = app.as_weak();
    let state = state.clone();
    let work = tasks.clone();
    let list = available.clone();
    let managed = inserted.clone();
    ui.on_composer_account_changed(move |account_id| {
        let Some(app) = weak.upgrade() else { return };
        let ui = app.global::<AccountMailPreferences>();
        ui.set_composer_preferences_ready(false);
        ui.set_composer_signature_names(ModelRc::default());
        ui.set_composer_security("".into());
        let Some(core) = core(&state) else { return };
        generation.set(generation.get().wrapping_add(1));
        let generation = generation.clone();
        let expected = generation.get();
        let list = list.clone();
        let managed = managed.clone();
        work.spawn(&app, false, async move { preferences(&core).await }, move |app, result| {
        if generation.get() != expected || app.get_compose_account_id() != account_id { return; }
        let ui = app.global::<AccountMailPreferences>();
        match result {
            Ok((settings, configs)) => {
                if !configs.iter().any(|c| c.id == i64::from(account_id)) { return; }
                ui.set_composer_preferences_ready(true);
                let signatures = choices(&settings, i64::from(account_id));
                let mut names = vec![SharedString::from("No signature")];
                names.extend(signatures.iter().map(|s| SharedString::from(&s.name)));
                ui.set_composer_signature_names(ModelRc::new(VecModel::from(names)));
                *list.borrow_mut() = signatures.clone();
                if let Some(config) = configs.iter().find(|c| c.id == i64::from(account_id)) {
                    let p = &config.settings.security;
                    ui.set_composer_security(
                        if p.require_encryption {
                            if p.sign_by_default {
                                "OpenPGP: encryption required · digitally signed"
                            } else {
                                "OpenPGP: encryption required"
                            }
                        } else if p.sign_by_default {
                            "OpenPGP: digitally signed · not encrypted"
                        } else {
                            "Message is not end-to-end encrypted"
                        }
                        .into(),
                    );
                }
                if app.get_compose_mode() == "draft" {
                    *managed.borrow_mut() = None;
                    ui.set_composer_signature_index(0);
                    return;
                }
                // A cleared composer starts a fresh insertion lifecycle.
                if app.get_compose_body().is_empty() {
                    *managed.borrow_mut() = None;
                }
                let defaults = settings
                    .signature_defaults
                    .get(&account_id.to_string())
                    .cloned()
                    .unwrap_or_default();
                let id = if app.get_compose_mode() == "new" {
                    defaults.new_id
                } else {
                    defaults.reply_id
                };
                let index = signatures
                    .iter()
                    .position(|s| Some(&s.id) == id.as_ref())
                    .map_or(0, |i| i as i32 + 1);
                ui.invoke_composer_signature(index);
            }
            Err(e) => {
                app.set_compose_notice(UiMessage::detail(
                    "Could not load account preferences: {}",
                    e,
                ));
                app.set_compose_notice_is_error(true);
            }
        }
        });
    });
    let weak = app.as_weak();
    let document = document.clone();
    let editor = editor.clone();
    ui.on_composer_signature(move |index| {
        let Some(app) = weak.upgrade() else { return }; let mut document = document.borrow_mut();
        let current = document.text().to_owned();
        let old = inserted.borrow().clone();
        let body = match remove_managed_signature(&current, old.as_deref()) {
            Ok(body) => body,
            Err(()) if index == 0 => {
                *inserted.borrow_mut() = None;
                app.global::<AccountMailPreferences>().set_composer_signature_index(0);
                return;
            }
            Err(()) => {
                app.set_compose_notice(UiMessage::plain("Your edited signature was preserved. Choose No signature to keep it as message text before inserting another."));
                app.set_compose_notice_is_error(true); return;
            }
        };
        let signature = usize::try_from(index - 1).ok().and_then(|i| available.borrow().get(i).cloned());
        let text = signature.as_ref().map(flectar_mail_core::signatures::plain_text).unwrap_or_default();
        let new_body = flectar_mail_core::signatures::insert(&body, &text);
        *inserted.borrow_mut() = if text.is_empty() { None } else { Some(format!("\n\n-- \n{}", text.trim())) };
        let selection = document.synchronize(&new_body, 0, 0);
        apply_rich_compose(&app, &document, selection, &mut editor.borrow_mut());
        app.global::<AccountMailPreferences>().set_composer_signature_index(index);
    });
}

fn remove_managed_signature(body: &str, managed: Option<&str>) -> Result<String, ()> {
    match managed {
        None => Ok(body.to_owned()),
        Some(block) if body.matches(block).count() == 1 => Ok(body.replacen(block, "", 1)),
        Some(_) if body.is_empty() => Ok(String::new()),
        Some(_) => Err(()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asynchronous_results_are_discarded_after_account_context_changes() {
        use slint::platform::{EventLoopProxy, Platform, WindowAdapter};
        use std::sync::mpsc;
        struct Proxy(mpsc::Sender<Box<dyn FnOnce() + Send>>);
        impl EventLoopProxy for Proxy {
            fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> { Ok(()) }
            fn invoke_from_event_loop(&self, event: Box<dyn FnOnce() + Send>) -> Result<(), slint::EventLoopError> {
                self.0.send(event).map_err(|_| slint::EventLoopError::EventLoopTerminated)
            }
        }
        struct Headless(mpsc::Sender<Box<dyn FnOnce() + Send>>);
        impl Platform for Headless {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(slint::platform::software_renderer::MinimalSoftwareWindow::new(
                    slint::platform::software_renderer::RepaintBufferType::ReusedBuffer))
            }
            fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> { Some(Box::new(Proxy(self.0.clone()))) }
        }
        struct Delivered(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Delivered {
            fn drop(&mut self) { self.0.store(true, std::sync::atomic::Ordering::SeqCst); }
        }
        let (events, receiver) = mpsc::channel();
        slint::platform::set_platform(Box::new(Headless(events))).unwrap();
        let app = AppWindow::new().unwrap();
        let tasks = UiWork {
            runtime: Rc::new(tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap()),
            generation: Rc::new(Cell::new(0)),
        };
        let (release, waiting) = tokio::sync::oneshot::channel();
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let payload = Delivered(dropped.clone());
        tasks.spawn(&app, true, async move {
            waiting.await.unwrap();
            Ok(payload)
        }, |_, _| panic!("removed account received private content"));
        assert!(app.global::<AccountMailPreferences>().get_busy());
        tasks.generation.set(1);
        release.send(()).unwrap();
        while !dropped.load(std::sync::atomic::Ordering::SeqCst) {
            receiver.recv_timeout(std::time::Duration::from_secs(5)).unwrap()();
        }
        // A stale result must not release a newer operation's busy indicator.
        assert!(app.global::<AccountMailPreferences>().get_busy());
        let applied = Rc::new(Cell::new(false));
        let applied2 = applied.clone();
        tasks.spawn(&app, true, async { Ok(()) }, move |app, result| {
            result.unwrap();
            assert!(!app.global::<AccountMailPreferences>().get_busy());
            applied2.set(true);
        });
        while !applied.get() {
            receiver.recv_timeout(std::time::Duration::from_secs(5)).unwrap()();
        }
    }

    #[test]
    fn switching_signature_preserves_edits_and_rejects_ambiguous_replacement() {
        let block = "\n\n-- \nZoë";
        assert_eq!(
            remove_managed_signature(&format!("Reply{block}\n\n> Quote"), Some(block)),
            Ok("Reply\n\n> Quote".into())
        );
        assert!(remove_managed_signature("Reply\n\n-- \nEdited", Some(block)).is_err());
        assert!(remove_managed_signature(&format!("{block}{block}"), Some(block)).is_err());
    }
}
