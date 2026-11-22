//! Message source (#253): the open message as it arrived, every header field
//! included, as mail clients show a message's source. A conversation shows
//! its newest message, as the OpenPGP reader opens it.

use super::*;
use crate::mail::MailTarget;
use slint::SharedString;

/// The text the reader shows at most; a longer message saves as a file.
const SHOWN_BYTES: usize = 512 * 1024;

thread_local! {
    /// The message the reader shows the original of, with its subject and
    /// its bytes, which Save writes.
    static SHOWN: RefCell<Option<(i64, String, Vec<u8>)>> = const { RefCell::new(None) };
    /// Bumped each time an original is asked for, so an older answer that
    /// comes late goes nowhere.
    static ASKED: Cell<u64> = const { Cell::new(0) };
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    documents: Arc<dyn documents::DocumentProvider>,
) {
    let ui = app.global::<MessageSource>();
    {
        let app = app.as_weak();
        let state = Rc::clone(state);
        let runtime = runtime.clone();
        ui.on_show(move || {
            let Some(app) = app.upgrade() else {
                return;
            };
            show(&app, &state, &runtime);
        });
    }
    let app = app.as_weak();
    let state = Rc::clone(state);
    let runtime = runtime.clone();
    ui.on_save(move || {
        let Some(shown) = SHOWN.with_borrow(Clone::clone) else {
            return;
        };
        let Some(core) = state
            .borrow()
            .core
            .as_ref()
            .map(CoreMailSource::account_preferences_core)
        else {
            return;
        };
        let (app, documents) = (app.clone(), Arc::clone(&documents));
        let request: documents::DocumentFuture<bool> = Box::pin(async move {
            let (_, subject, bytes) = shown;
            let file = tempfile::Builder::new()
                .prefix("original-")
                .suffix(".eml")
                .tempfile()
                .map_err(|error| error.to_string())?;
            tokio::fs::write(file.path(), &bytes)
                .await
                .map_err(|error| error.to_string())?;
            let path = file.into_temp_path();
            let name = format!("{}.eml", documents::safe_name(file_stem(&subject)));
            // Where downloads go, as attachments save.
            let dialog = documents::download_dialog_for(&core).await;
            let saved = documents
                .export_with(path.to_path_buf(), name, dialog)
                .await;
            drop(path);
            saved
        });
        documents::run_request(&runtime, request, move |saved| {
            if let (Err(error), Some(app)) = (saved, app.upgrade()) {
                app.set_render_status(UiMessage::detail("Could not save the message: {}", error));
            }
        });
    });
}

/// The name a saved message's file starts with.
fn file_stem(subject: &str) -> &str {
    let subject = subject.trim();
    if subject.is_empty() {
        "message"
    } else {
        subject
    }
}

/// Show the open message's original in place of the message.
fn show(app: &AppWindow, state: &Rc<RefCell<InboxState>>, runtime: &tokio::runtime::Handle) {
    let ui = app.global::<MessageSource>();
    let (selected, core) = {
        let state = state.borrow();
        let selected = state.selected_id.and_then(|id| {
            state
                .messages
                .iter()
                .find(|message| message.id == id)
                .and_then(|message| {
                    MailTarget::of(message).map(|target| (id, target, message.subject.clone()))
                })
        });
        (
            selected,
            state
                .core
                .as_ref()
                .map(CoreMailSource::account_preferences_core),
        )
    };
    let (Some((selected_id, target, subject)), Some(core)) = (selected, core) else {
        return;
    };
    ASKED.set(ASKED.get().wrapping_add(1));
    let asked = ASKED.get();
    SHOWN.set(None);
    ui.set_text(SharedString::default());
    ui.set_error(SharedString::default());
    ui.set_partial(false);
    ui.set_loading(true);
    ui.set_shown(true);
    let app = app.as_weak();
    let state = Rc::clone(state);
    let task = runtime.spawn(async move {
        let message_id = match target {
            MailTarget::Message { id, .. } => id,
            MailTarget::Thread(thread_id) => {
                core.get_latest_thread_body(thread_id)
                    .await
                    .map_err(|error| error.to_string())?
                    .id
            }
        };
        let bytes = core
            .message_original(message_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((message_id, bytes))
    });
    if slint::spawn_local(async move {
        let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
        let Some(app) = app.upgrade() else {
            return;
        };
        // Another message, or another ask, since.
        if ASKED.get() != asked || state.borrow().selected_id != Some(selected_id) {
            return;
        }
        let ui = app.global::<MessageSource>();
        ui.set_loading(false);
        match result {
            Ok((message_id, bytes)) => {
                let (text, partial) = shown_text(&bytes);
                ui.set_text(text.as_str().into());
                ui.set_partial(partial);
                SHOWN.set(Some((message_id, subject.to_string(), bytes)));
            }
            Err(error) => ui.set_error(translated(
                &app,
                &UiMessage::detail("Could not read the message source: {}", error),
            )),
        }
    })
    .is_err()
    {
        reset(&ui);
    }
}

fn reset(ui: &MessageSource<'_>) {
    ui.set_loading(false);
    ui.set_shown(false);
}

/// Stop showing an original, as when another message opens.
pub(super) fn hide(app: &AppWindow) {
    ASKED.set(ASKED.get().wrapping_add(1));
    SHOWN.set(None);
    let ui = app.global::<MessageSource>();
    reset(&ui);
    ui.set_text(SharedString::default());
    ui.set_error(SharedString::default());
}

/// A message's bytes as the reader shows them: as text, any bytes that
/// aren't UTF-8 replaced, and only the start of a long message. Returns
/// whether it's only the start.
fn shown_text(bytes: &[u8]) -> (String, bool) {
    let partial = bytes.len() > SHOWN_BYTES;
    let shown = &bytes[..bytes.len().min(SHOWN_BYTES)];
    (String::from_utf8_lossy(shown).into_owned(), partial)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_message_shows_its_start() {
        assert_eq!(
            shown_text(b"Subject: Hi\r\n\r\nBody"),
            ("Subject: Hi\r\n\r\nBody".into(), false)
        );
        let long = vec![b'a'; SHOWN_BYTES + 1];
        let (text, partial) = shown_text(&long);
        assert!(partial);
        assert_eq!(text.len(), SHOWN_BYTES);
        // Bytes that aren't UTF-8 show as replacement characters.
        assert_eq!(shown_text(b"caf\xe9").0, "caf\u{fffd}");
    }

    #[test]
    fn a_message_without_a_subject_saves_as_message() {
        assert_eq!(file_stem("  "), "message");
        assert_eq!(file_stem(" Invoice "), "Invoice");
    }
}
