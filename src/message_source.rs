//! Message source (#253): the open message as it arrived, every header field
//! included, as mail clients show a message's source. A conversation shows
//! its newest message, as the OpenPGP reader opens it.

use super::*;
use crate::mail::MailTarget;
use slint::SharedString;

/// The text the reader shows at most; a longer message saves as a file.
const SHOWN_BYTES: usize = 512 * 1024;

/// What a reader's source view shows.
#[derive(Clone, Default)]
struct Source {
    /// Bumped each time an original is asked for, so an older answer that
    /// comes late goes nowhere.
    asked: u64,
    /// The message the reader shows the original of, with its subject and
    /// its bytes, which Save writes.
    shown: Option<(i64, String, Vec<u8>)>,
}

struct Env {
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
    documents: Arc<dyn documents::DocumentProvider>,
}

thread_local! {
    static ENV: RefCell<Option<Rc<Env>>> = const { RefCell::new(None) };
    static SOURCES: PerReader<Source> = const { PerReader::new() };
}

fn source(key: ReaderKey) -> Source {
    SOURCES.with(|sources| sources.get(key)).unwrap_or_default()
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    documents: Arc<dyn documents::DocumentProvider>,
) {
    ENV.set(Some(Rc::new(Env {
        state: Rc::clone(state),
        runtime: runtime.clone(),
        documents,
    })));
    attach(&ReaderRef::main(app));
}

/// Show and save the original of the message a reader shows.
pub(super) fn attach(reader: &ReaderRef) {
    let Some(env) = ENV.with_borrow(Clone::clone) else {
        return;
    };
    let ui = reader.global::<MessageSource>();
    {
        let env = Rc::clone(&env);
        let host = reader.host();
        ui.on_show(move || {
            let Some(reader) = host.upgrade() else {
                return;
            };
            show(&reader, &env.state, &env.runtime);
        });
    }
    let host = reader.host();
    ui.on_save(move || {
        let Some(shown) = source(host.key()).shown else {
            return;
        };
        let Some(core) = env
            .state
            .borrow()
            .core
            .as_ref()
            .map(CoreMailSource::account_preferences_core)
        else {
            return;
        };
        let (window, documents) = (host.clone(), Arc::clone(&env.documents));
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
        documents::run_request(&env.runtime, request, move |saved| {
            if let (Err(error), Some(reader)) = (saved, window.upgrade()) {
                reader
                    .app()
                    .set_render_status(UiMessage::detail("Could not save the message: {}", error));
            }
        });
    });
}

/// A reader's window closed.
#[cfg(desktop)]
pub(super) fn forget(key: ReaderKey) {
    SOURCES.with(|sources| sources.remove(key));
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
fn show(reader: &ReaderRef, state: &Rc<RefCell<InboxState>>, runtime: &tokio::runtime::Handle) {
    let ui = reader.global::<MessageSource>();
    let key = reader.key();
    let selected = crate::reader_host::row(state, key)
        .and_then(|row| MailTarget::of(&row).map(|target| (row.id, target, row.subject.clone())));
    let core = state
        .borrow()
        .core
        .as_ref()
        .map(CoreMailSource::account_preferences_core);
    let (Some((selected_id, target, subject)), Some(core)) = (selected, core) else {
        return;
    };
    let asked = source(key).asked.wrapping_add(1);
    SOURCES.with(|sources| {
        sources.set(key, Source { asked, shown: None });
    });
    ui.set_text(SharedString::default());
    ui.set_error(SharedString::default());
    ui.set_partial(false);
    ui.set_loading(true);
    ui.set_shown(true);
    let host = reader.host();
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
        let Some(reader) = host.upgrade() else {
            return;
        };
        // Another message, or another ask, since.
        if source(key).asked != asked
            || crate::reader_host::row(&state, key).map(|row| row.id) != Some(selected_id)
        {
            return;
        }
        let ui = reader.global::<MessageSource>();
        ui.set_loading(false);
        match result {
            Ok((message_id, bytes)) => {
                let (text, partial) = shown_text(&bytes);
                ui.set_text(text.as_str().into());
                ui.set_partial(partial);
                SOURCES.with(|sources| {
                    sources.set(
                        key,
                        Source {
                            asked,
                            shown: Some((message_id, subject.to_string(), bytes)),
                        },
                    );
                });
            }
            Err(error) => ui.set_error(translated(
                reader.app(),
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
pub(super) fn hide(reader: &ReaderRef) {
    let key = reader.key();
    let asked = source(key).asked.wrapping_add(1);
    SOURCES.with(|sources| sources.set(key, Source { asked, shown: None }));
    let ui = reader.global::<MessageSource>();
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
