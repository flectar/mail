//! Messages forwarded as attachments: each original as it arrived, in a file of its own that
//! mail apps open as a message.

use super::*;
use flectar_mail_core::models::{ActionTarget, ForwardedMessage};

impl Composers {
    /// Forward the message the reader shows as an attachment.
    pub(super) fn forward_shown_as_attachment(self: &Rc<Self>, app: &AppWindow) {
        let (row, shown) = self.shown_message();
        let (target, latest) = match (shown, row) {
            (Some(id), _) => (ActionTarget::Messages(vec![id]), false),
            (None, Some(row)) => match (row.message_id, row.thread_id) {
                (Some(id), _) => (ActionTarget::Messages(vec![id]), false),
                // A conversation still loading forwards its latest message, as Forward does.
                (None, Some(thread)) => (ActionTarget::Threads(vec![thread]), true),
                (None, None) => {
                    app.set_render_status(UiMessage::plain("Choose a message first."));
                    return;
                }
            },
            (None, None) => {
                app.set_render_status(UiMessage::plain("Choose a message first."));
                return;
            }
        };
        let Some(mut messages) = self.forwarded_messages(app, vec![target]) else {
            return;
        };
        if latest {
            messages.drain(..messages.len().saturating_sub(1));
        }
        self.forward_messages(app, messages);
    }

    /// Forward the messages of `targets` as attachments of a new message.
    pub(crate) fn forward_as_attachment(
        self: &Rc<Self>,
        app: &AppWindow,
        targets: Vec<ActionTarget>,
    ) {
        if let Some(messages) = self.forwarded_messages(app, targets) {
            self.forward_messages(app, messages);
        }
    }

    /// The messages of `targets` a forward attaches, or none when they can't be read, which
    /// the main window says.
    fn forwarded_messages(
        &self,
        app: &AppWindow,
        targets: Vec<ActionTarget>,
    ) -> Option<Vec<ForwardedMessage>> {
        let Some(core) = self.core() else {
            app.set_render_status(UiMessage::plain(
                "Connect an account before replying or forwarding.",
            ));
            return None;
        };
        let mut messages = Vec::new();
        for target in targets {
            match self.runtime.block_on(core.forwarded_messages(target)) {
                Ok(found) => messages.extend(found),
                Err(error) => {
                    app.set_render_status(UiMessage::detail("Could not open composer: {}", error));
                    return None;
                }
            }
        }
        Some(messages)
    }

    /// Open a new message with `messages` as its attachments, which opens at once: their
    /// originals reach it as they're fetched.
    fn forward_messages(self: &Rc<Self>, app: &AppWindow, messages: Vec<ForwardedMessage>) {
        let Some(core) = self.core() else {
            return;
        };
        let Some(first) = messages.first() else {
            app.set_render_status(UiMessage::plain("Choose a message first."));
            return;
        };
        // Local mail is answered from an account.
        if self.state.borrow().connected_accounts.is_empty() {
            app.set_render_status(UiMessage::plain("Add an account to send mail."));
            return;
        }
        // From the account and the address the first message reached, as its forward is.
        let (account, sender_email) = match self.runtime.block_on(core.forward_sender(first)) {
            Ok(sender) => sender,
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not open composer: {}", error));
                return;
            }
        };
        let Ok(account_id) = i32::try_from(account.id) else {
            app.set_render_status(UiMessage::plain(
                "Could not open composer: invalid account identifier",
            ));
            return;
        };
        let subject = crate::compose_controller::prefixed_subject(&first.subject, "Fwd");
        let Some(session) = self.open(app) else {
            return;
        };
        let Some(host) = session.host() else {
            return;
        };
        let ui = host.ui();
        ui.set_account_id(account_id);
        ui.set_from_label(
            match account
                .display_name
                .as_deref()
                .filter(|name| !name.trim().is_empty() && *name != account.email)
            {
                Some(name) => format!("{name}  <{}>", account.email).into(),
                None => account.email.as_str().into(),
            },
        );
        ui.set_to("".into());
        ui.set_cc("".into());
        ui.set_bcc("".into());
        ui.set_subject(subject.into());
        *session.intent.borrow_mut() = ComposeIntent {
            mode: "forward".to_owned(),
            in_reply_to_message_id: None,
            in_reply_to_header: None,
            draft_id: None,
        };
        session.show("forward", Some(sender_email));
        let fallback = translated(app, &UiMessage::plain("Forwarded message")).to_string();
        session.attach_forwarded(&core, messages, &fallback);
    }

    /// The message row the reader shows, and the message of its conversation it shows when the
    /// row is a conversation.
    pub(super) fn shown_message(&self) -> (Option<MailMessage>, Option<i64>) {
        let state = self.state.borrow();
        let row = state
            .selected_id
            .and_then(|id| state.messages.iter().find(|message| message.id == id))
            .cloned();
        let shown = row.as_ref().and_then(|row| {
            (state.conversation_owner_id == Some(row.id))
                .then(|| {
                    state
                        .conversation_messages
                        .get(state.conversation_selected_index)
                        .map(|message| i64::from(message.id))
                })
                .flatten()
        });
        (row, shown)
    }
}

impl ComposeSession {
    /// Attach the originals of `messages` once they're fetched, unless the message was sent,
    /// discarded or replaced meanwhile. `fallback` names those without a subject.
    fn attach_forwarded(
        self: &Rc<Self>,
        core: &CoreMailSource,
        messages: Vec<ForwardedMessage>,
        fallback: &str,
    ) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        host.ui().set_notice(
            UiMessage::detail("Attaching {} messages…", messages.len()),
            false,
        );
        let names = file_names(&messages, fallback);
        let ids = messages.iter().map(|message| message.message_id).collect();
        let core = core.clone();
        let request: documents::DocumentFuture<_> =
            Box::pin(async move { Ok(core.forwarded_originals(ids).await) });
        let weak = Rc::downgrade(self);
        let started_in = self.generation.get();
        self.count_attaching(true);
        // Files that reach no message are removed as they're dropped.
        documents::run_request(&composers.runtime, request, move |fetched| {
            let Some(session) = weak.upgrade() else {
                return;
            };
            if session.generation.get() != started_in {
                return;
            }
            session.count_attaching(false);
            let mut attachments = Vec::new();
            let mut failure = None;
            for (id, file) in fetched.unwrap_or_else(|error| vec![(0, Err(error))]) {
                match file {
                    Ok(file) => {
                        let path = file.to_path_buf();
                        attachments.push(NewAttachment {
                            filename: names
                                .iter()
                                .find(|(message_id, _)| *message_id == id)
                                .map(|(_, name)| name.clone()),
                            // The file goes with the attachment.
                            import: Some(documents::ImportedDocument::temporary(
                                path.clone(),
                                move |_| {
                                    let _ = file.close();
                                },
                            )),
                            path,
                        });
                    }
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            }
            session.attach(attachments);
            if let Some(error) = failure
                && let Some(host) = session.host()
            {
                host.ui().set_notice(
                    UiMessage::detail("Could not attach a forwarded message: {}", error),
                    true,
                );
            }
        });
    }
}

/// The file each of `messages` goes in, by message: its subject as a file name, in mail apps'
/// `.eml` for a message, each name once.
fn file_names(messages: &[ForwardedMessage], fallback: &str) -> Vec<(i64, String)> {
    let mut names: Vec<(i64, String)> = Vec::with_capacity(messages.len());
    for message in messages {
        // The same name a received forward without one gets (#455).
        let base = flectar_mail_core::mime::message_file_stem(&message.subject);
        let base = if base.is_empty() { fallback } else { &base };
        let mut name = format!("{base}.eml");
        let mut number = 2;
        while names
            .iter()
            .any(|(_, taken)| taken.eq_ignore_ascii_case(&name))
        {
            name = format!("{base} ({number}).eml");
            number += 1;
        }
        names.push((message.message_id, name));
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(message_id: i64, subject: &str) -> ForwardedMessage {
        ForwardedMessage {
            message_id,
            account_id: 1,
            thread_id: Some(1),
            subject: subject.to_owned(),
        }
    }

    #[test]
    fn forwarded_messages_are_named_by_their_subjects() {
        let messages = [
            message(1, "Re: Q3 report / final?"),
            message(2, "Re: Q3 report / final?"),
            message(3, "  "),
            message(4, &"Very long subject ".repeat(8)),
            message(5, "...hidden"),
        ];
        let names = file_names(&messages, "Forwarded message");
        assert_eq!(
            names,
            [
                (1, "Re Q3 report final.eml".to_owned()),
                (2, "Re Q3 report final (2).eml".to_owned()),
                (3, "Forwarded message.eml".to_owned()),
                (
                    4,
                    "Very long subject Very long subject Very long subject Very long.eml"
                        .to_owned()
                ),
                (5, "hidden.eml".to_owned()),
            ]
        );
    }
}
