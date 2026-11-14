//! Messages written and sent as plain text only, with no formatting, pictures in the text, or
//! HTML part. Each account chooses how its messages start, and each message can change.

use super::*;
use flectar_mail_core::models::{AccountSettings, MessageFormat};

/// The edit that made a message plain text, which undo can take back.
pub(super) struct PlainEdit {
    mark: EditMark,
    /// The signature block before the edit and after it, for the next signature swap as the
    /// edit is undone and redone.
    signature: Option<(String, String)>,
}

impl ComposeSession {
    /// Start the message as its sending account's preference says, once, unless a draft or a
    /// message edited as new started it already.
    pub(super) fn choose_starting_format(&self, settings: &AccountSettings) {
        let Some(answers_plain_text) = self.format_pending.take() else {
            return;
        };
        let plain = match settings.message_format {
            MessageFormat::PlainText => true,
            MessageFormat::Formatted => answers_plain_text && settings.plain_text_replies,
        };
        if plain {
            self.set_plain_text(true);
        }
    }

    /// Start the message as plain text or formatted, as the draft or the message edited as new
    /// it opens from is.
    pub(super) fn start_format(&self, plain: bool) {
        self.format_pending.set(None);
        *self.plain_edit.borrow_mut() = None;
        self.plain_text.set(plain);
        self.show_format();
    }

    /// Have the message go out as plain text only, taking its formatting out in one edit that
    /// undo takes back, or formatted again, from the toolbar.
    pub(super) fn set_plain_text(&self, plain: bool) {
        self.format_pending.set(None);
        if plain == self.plain_text.get() {
            self.show_format();
            return;
        }
        *self.plain_edit.borrow_mut() = None;
        if plain {
            let signature = self.signature.borrow().clone();
            let mut made = None;
            self.edit(|document| {
                let plain_signature = signature.as_deref().and_then(|block| {
                    let start = document.text().find(block)?;
                    Some(document.plain_text_in(start..start + block.len()))
                });
                let Some(selection) = document.make_plain() else {
                    return document.selection();
                };
                made = document.mark_last_edit().map(|mark| PlainEdit {
                    mark,
                    signature: signature.clone().zip(plain_signature),
                });
                selection
            });
            if let Some(made) = made {
                if let Some((_, plain_signature)) = &made.signature {
                    *self.signature.borrow_mut() = Some(plain_signature.clone());
                }
                *self.plain_edit.borrow_mut() = Some(made);
                if let Some(host) = self.host() {
                    host.ui().set_notice(
                        UiMessage::plain("Formatting and pictures removed. Undo restores them."),
                        false,
                    );
                }
            }
        }
        self.plain_text.set(plain);
        self.show_format();
    }

    /// Undoing the edit that made the message plain text makes it formatted again, and redoing
    /// it plain text again.
    pub(super) fn follow_plain_edit(&self, document: &RichComposeDocument) {
        let state = self
            .plain_edit
            .borrow()
            .as_ref()
            .map(|made| (document.edit_state(made.mark), made.signature.clone()));
        let (plain, signature) = match state {
            Some((EditState::Applied, signature)) => (true, signature),
            Some((EditState::Undone, signature)) => (false, signature),
            Some((EditState::Gone, _)) => {
                *self.plain_edit.borrow_mut() = None;
                return;
            }
            None => return,
        };
        if plain == self.plain_text.get() {
            return;
        }
        if let Some((formatted, plain_signature)) = signature {
            *self.signature.borrow_mut() = Some(if plain { plain_signature } else { formatted });
        }
        self.plain_text.set(plain);
        self.show_format();
    }

    /// Show whether the message is plain text: its toolbar, and the original below the editor
    /// as the text that goes out.
    pub(super) fn show_format(&self) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        let plain = self.plain_text.get();
        if ui.get_plain_text() == plain {
            return;
        }
        ui.set_plain_text(plain);
        *self.quote_renderer.borrow_mut() = None;
        self.render_quote(&host);
    }
}

/// An original's plain text, below a message being written, as HTML the quote renderer shows.
pub(super) fn plain_quote_html(plain: &str) -> String {
    let mut html = format!(
        "<body style=\"margin:8px 0;font-size:{}px\"><div style=\"white-space:pre-wrap\">",
        crate::compose_editor::FONT_SIZE_LOGICAL
    );
    crate::email_document::append_escaped(&mut html, plain);
    html.push_str("</div></body>");
    html
}

#[cfg(all(test, desktop))]
mod tests {
    use super::super::tests::{Setup, setup};
    use super::*;
    use flectar_mail_core::models::{ConnectionSecurity, MailConnectionSettings};

    fn unformatted(session: &ComposeSession) -> bool {
        session
            .document
            .borrow()
            .style_runs()
            .iter()
            .all(|run| run.style.marks == 0 && run.style.color.is_none())
    }

    #[test]
    fn messages_switch_to_plain_text_and_back_with_undo() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        app.invoke_open_compose("".into());
        let session = composers.in_app().unwrap();
        let host = session.host().unwrap();
        let ui = host.ui();
        let text = "Hello world";
        session.edit(|document| {
            document.load(text, 0, 0);
            document.format("bold", text, 0, 5)
        });
        assert!(!unformatted(&session));

        ui.invoke_plain_text_chosen(true);
        assert!(ui.get_plain_text());
        assert!(unformatted(&session));
        // Formatting from the keyboard does nothing in plain text.
        ui.invoke_format("italic".into(), text.into(), 6, 11);
        assert!(unformatted(&session));

        // Undo brings the formatting back, and with it formatted text; redo takes it out again.
        session.history("undo");
        assert!(!ui.get_plain_text());
        assert!(!unformatted(&session));
        session.history("redo");
        assert!(ui.get_plain_text());
        assert!(unformatted(&session));

        // Back to formatted text, the text stays as it is.
        ui.invoke_plain_text_chosen(false);
        assert!(!ui.get_plain_text());
        assert!(unformatted(&session));
        ui.invoke_format("italic".into(), text.into(), 6, 11);
        assert!(!unformatted(&session));
    }

    #[test]
    fn accounts_choose_how_their_messages_start_once() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        app.invoke_open_compose("".into());
        let session = composers.in_app().unwrap();
        let mut settings = AccountSettings::new(MailConnectionSettings::new(
            ConnectionSecurity::Tls,
            ConnectionSecurity::Tls,
        ));
        // New messages start formatted, and replies to plain-text messages as plain text.
        session.format_pending.set(Some(false));
        session.choose_starting_format(&settings);
        assert!(!session.plain_text.get());
        session.format_pending.set(Some(true));
        session.choose_starting_format(&settings);
        assert!(session.plain_text.get());
        // The choice is made once: another account later doesn't change the message.
        settings.plain_text_replies = false;
        session.choose_starting_format(&settings);
        assert!(session.plain_text.get());

        session.start_format(false);
        settings.message_format = MessageFormat::PlainText;
        session.format_pending.set(Some(false));
        session.choose_starting_format(&settings);
        assert!(session.plain_text.get());
        // A draft or a message edited as new starts as it was saved or sent.
        session.start_format(false);
        session.choose_starting_format(&settings);
        assert!(!session.plain_text.get());
    }
}
