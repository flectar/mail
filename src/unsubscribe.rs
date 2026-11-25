//! Leaving a mailing list from the reader (#227). The core works out how the
//! list takes a request; a dialog asks first, as RFC 2369 and RFC 8058 want;
//! and after the request, the list's other mail in the inbox can be
//! archived or moved to Trash at once, with Undo.

use super::*;
use crate::mail_work::UndoableBatch;
use flectar_mail_core::models::{Address, SaveDraftArgs, UnsubscribeOffer, UnsubscribeOutcome};
use std::future::Future;

/// The list the dialog is about.
struct Leaving {
    message_id: i64,
    account_id: i64,
    offer: UnsubscribeOffer,
    /// Its messages in the inbox, once the request is made.
    inbox: Vec<i64>,
}

thread_local! {
    static CONTROLLER: RefCell<Option<Rc<Controller>>> = const { RefCell::new(None) };
    /// The list each reader's dialog is about.
    static LEAVING: PerReader<Leaving> = const { PerReader::new() };
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    opener: ExternalUrlOpener,
) {
    CONTROLLER.with(|slot| {
        *slot.borrow_mut() = Some(Rc::new(Controller {
            state: Rc::clone(state),
            runtime: runtime.clone(),
            opener,
        }));
    });
    attach(&ReaderRef::main(app));
}

/// Leave the list of the message a reader shows, from its banner, menu and
/// dialog.
pub(super) fn attach(reader: &ReaderRef) {
    let Some(controller) = CONTROLLER.with(|slot| slot.borrow().clone()) else {
        return;
    };
    let ui = reader.global::<Unsubscribe>();
    let (this, host) = (Rc::clone(&controller), reader.host());
    ui.on_start(move || this.start(&host));
    let (this, host) = (Rc::clone(&controller), reader.host());
    ui.on_confirm(move || this.confirm(&host));
    let (this, host) = (Rc::clone(&controller), reader.host());
    ui.on_review(move || this.review(&host));
    let (this, host) = (Rc::clone(&controller), reader.host());
    ui.on_clean_up(move |action| this.clean_up(&host, &action));
    let host = reader.host();
    ui.on_close(move || controller.close(&host));
}

/// A reader's window closed.
#[cfg(desktop)]
pub(super) fn forget(key: ReaderKey) {
    LEAVING.with(|leaving| leaving.remove(key));
}

struct Controller {
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
    opener: ExternalUrlOpener,
}

impl Controller {
    /// The banner or the menu: find out how the open message's list takes a
    /// request, and ask.
    fn start(&self, host: &ReaderHost) {
        let Some(reader) = host.upgrade() else {
            return;
        };
        if !reader.global::<Unsubscribe>().get_stage().is_empty() {
            return;
        }
        let key = host.key();
        let row = crate::reader_host::row(&self.state, key);
        let core = self.state.borrow().core.clone();
        let (Some(core), Some(row)) = (core, row) else {
            return;
        };
        let Some(target) = MailTarget::of(&row) else {
            return;
        };
        let account_id = row.account_id;
        let sender = reader.ui().get_selected_sender().to_string();
        self.spawn(
            host,
            async move {
                let Some(message_id) = core.unsubscribe_message_of(target).await? else {
                    return Ok(None);
                };
                Ok(core
                    .unsubscribe_offer(message_id)
                    .await?
                    .map(|offer| (message_id, offer)))
            },
            move |reader, result| match result {
                Ok(Some((message_id, offer))) => {
                    let ui = reader.global::<Unsubscribe>();
                    ui.set_sender(sender.into());
                    show_offer(&ui, &offer);
                    ui.set_inbox_count(0);
                    ui.set_stage("confirm".into());
                    LEAVING.with(|leaving| {
                        leaving.set(
                            key,
                            Leaving {
                                message_id,
                                account_id,
                                offer,
                                inbox: Vec::new(),
                            },
                        );
                    });
                }
                Ok(None) => crate::undo::tell(
                    reader.app(),
                    UiMessage::plain("This message names no way to unsubscribe."),
                ),
                Err(error) => crate::undo::tell(
                    reader.app(),
                    UiMessage::detail("Could not unsubscribe: {}", error),
                ),
            },
        );
    }

    /// The dialog's main button: make the request, or open the list's page.
    fn confirm(&self, host: &ReaderHost) {
        let Some(reader) = host.upgrade() else {
            return;
        };
        let key = host.key();
        let Some((message_id, offer)) = LEAVING.with(|leaving| {
            leaving.with(key, |leaving| {
                leaving.map(|leaving| (leaving.message_id, leaving.offer.clone()))
            })
        }) else {
            return;
        };
        if let UnsubscribeOffer::Website { url } = &offer {
            let url = url.clone();
            self.close(host);
            if let Err(error) = crate::mail_render_projection::open_email_link(&self.opener, &url) {
                crate::undo::tell(
                    reader.app(),
                    UiMessage::detail("Could not open link: {}", error),
                );
            }
            return;
        }
        let Some(core) = self.state.borrow().core.clone() else {
            return;
        };
        let ui = reader.global::<Unsubscribe>();
        ui.set_stage("working".into());
        self.spawn(
            host,
            async move {
                let outcome = core.unsubscribe(message_id).await?;
                let inbox = match outcome {
                    UnsubscribeOutcome::NeedsBrowser { .. } => Vec::new(),
                    // The request is made: a failure to count the list's
                    // mail only means not offering to clear it.
                    _ => core
                        .list_inbox_messages(message_id)
                        .await
                        .unwrap_or_default(),
                };
                Ok((outcome, inbox))
            },
            move |reader, result| {
                // Closed while the request went out: the request still
                // counts, and the notice tells of it.
                let open = LEAVING.with(|leaving| {
                    leaving.with(key, |leaving| {
                        leaving.is_some_and(|leaving| leaving.message_id == message_id)
                    })
                });
                let ui = reader.global::<Unsubscribe>();
                match result {
                    Ok((UnsubscribeOutcome::NeedsBrowser { url }, _)) if open => {
                        ui.set_host(host_of(&url).into());
                        ui.set_stage("website".into());
                        LEAVING.with(|leaving| {
                            leaving.with(key, |leaving| {
                                if let Some(leaving) = leaving {
                                    leaving.offer = UnsubscribeOffer::Website { url };
                                }
                            });
                        });
                    }
                    Ok((UnsubscribeOutcome::NeedsBrowser { .. }, _)) => {}
                    Ok((outcome, inbox)) => {
                        let sender = ui.get_sender().to_string();
                        let to = ui.get_to().to_string();
                        if open && !inbox.is_empty() {
                            ui.set_inbox_count(i32::try_from(inbox.len()).unwrap_or(i32::MAX));
                            ui.set_stage("done".into());
                            LEAVING.with(|leaving| {
                                leaving.with(key, |leaving| {
                                    if let Some(leaving) = leaving {
                                        leaving.inbox = inbox;
                                    }
                                });
                            });
                            return;
                        }
                        close_dialog(&reader);
                        crate::undo::tell(
                            reader.app(),
                            match outcome {
                                UnsubscribeOutcome::MailtoSent => {
                                    UiMessage::detail("Unsubscribe request sent to {}.", to)
                                }
                                _ => UiMessage::detail("Unsubscribed from {}.", sender),
                            },
                        );
                    }
                    Err(error) if open => {
                        ui.set_error(error.into());
                        ui.set_stage("error".into());
                    }
                    Err(error) => crate::undo::tell(
                        reader.app(),
                        UiMessage::detail("Could not unsubscribe: {}", error),
                    ),
                }
            },
        );
    }

    /// Open an emailed request in the composer, to be looked over and sent
    /// there (RFC 2369 appendix B.2.4).
    fn review(&self, host: &ReaderHost) {
        let Some(leaving) = LEAVING.with(|leaving| leaving.remove(host.key())) else {
            return;
        };
        self.close(host);
        let UnsubscribeOffer::Mailto {
            from,
            to,
            subject,
            body,
        } = leaving.offer
        else {
            return;
        };
        crate::composer::compose_written(SaveDraftArgs {
            draft_id: None,
            account_id: leaving.account_id,
            from: Some(Address {
                name: None,
                email: from,
            }),
            to: vec![Address {
                name: None,
                email: to,
            }],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject,
            body_text: body,
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            in_reply_to_header: None,
            attachments: Vec::new(),
            request_read_receipt: false,
            importance: Default::default(),
        });
    }

    /// Archive the list's messages in the inbox, or move them to Trash.
    fn clean_up(&self, host: &ReaderHost, action: &str) {
        let Some(inbox) = LEAVING
            .with(|leaving| leaving.with(host.key(), |leaving| leaving.map(|l| l.inbox.clone())))
        else {
            return;
        };
        self.close(host);
        let Some(core) = self.state.borrow().core.clone() else {
            return;
        };
        let kind: &'static str = if action == "trash" {
            "trash"
        } else {
            "archive"
        };
        let count = inbox.len();
        self.spawn(
            host,
            async move { core.act_on_messages(inbox, kind).await },
            move |reader, result| match result {
                Ok(action_ids) if !action_ids.is_empty() => crate::undo::gesture(
                    reader.app(),
                    action_ids,
                    UndoableBatch {
                        kind,
                        conversations: false,
                        count,
                    },
                ),
                Ok(_) => {}
                Err(error) => reader
                    .app()
                    .set_render_status(UiMessage::detail("Message action failed: {}", error)),
            },
        );
    }

    fn close(&self, host: &ReaderHost) {
        LEAVING.with(|leaving| leaving.remove(host.key()));
        if let Some(reader) = host.upgrade() {
            close_dialog(&reader);
        }
    }

    /// Run `work` on the runtime and `apply` its result to the reader on the
    /// UI thread, while its window is open.
    fn spawn<T: Send + 'static>(
        &self,
        host: &ReaderHost,
        work: impl Future<Output = Result<T, String>> + Send + 'static,
        apply: impl FnOnce(ReaderRef, Result<T, String>) + 'static,
    ) {
        let host = host.clone();
        let task = self.runtime.spawn(work);
        let abort = task.abort_handle();
        if slint::spawn_local(async move {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            if let Some(reader) = host.upgrade() {
                apply(reader, result);
            }
        })
        .is_err()
        {
            abort.abort();
        }
    }
}

fn close_dialog(reader: &ReaderRef) {
    reader.global::<Unsubscribe>().set_stage("".into());
}

/// What the dialog says about how the list takes the request.
fn show_offer(ui: &Unsubscribe, offer: &UnsubscribeOffer) {
    match offer {
        UnsubscribeOffer::OneClick { host } => {
            ui.set_kind("one-click".into());
            ui.set_host(host.into());
        }
        UnsubscribeOffer::Mailto { from, to, .. } => {
            ui.set_kind("mailto".into());
            ui.set_from(from.into());
            ui.set_to(to.into());
        }
        UnsubscribeOffer::Website { url } => {
            ui.set_kind("website".into());
            ui.set_host(host_of(url).into());
        }
    }
}

/// The host a web address names, or the address itself.
fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}
