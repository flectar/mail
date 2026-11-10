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
    static LEAVING: RefCell<Option<Leaving>> = const { RefCell::new(None) };
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    opener: ExternalUrlOpener,
) {
    let ui = app.global::<Unsubscribe>();
    let controller = Rc::new(Controller {
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
        opener,
    });
    let this = Rc::clone(&controller);
    ui.on_start(move || this.start());
    let this = Rc::clone(&controller);
    ui.on_confirm(move || this.confirm());
    let this = Rc::clone(&controller);
    ui.on_review(move || this.review());
    let this = Rc::clone(&controller);
    ui.on_clean_up(move |action| this.clean_up(&action));
    let this = controller;
    ui.on_close(move || this.close());
}

struct Controller {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
    opener: ExternalUrlOpener,
}

impl Controller {
    /// The banner or the menu: find out how the open message's list takes a
    /// request, and ask.
    fn start(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        if !app.global::<Unsubscribe>().get_stage().is_empty() {
            return;
        }
        let (core, row) = {
            let state = self.state.borrow();
            let row = state
                .selected_id
                .and_then(|id| state.messages.iter().find(|message| message.id == id))
                .cloned();
            (state.core.clone(), row)
        };
        let (Some(core), Some(row)) = (core, row) else {
            return;
        };
        let Some(target) = MailTarget::of(&row) else {
            return;
        };
        let account_id = row.account_id;
        let sender = app.get_selected_sender().to_string();
        self.spawn(
            async move {
                let Some(message_id) = core.unsubscribe_message_of(target).await? else {
                    return Ok(None);
                };
                Ok(core
                    .unsubscribe_offer(message_id)
                    .await?
                    .map(|offer| (message_id, offer)))
            },
            move |app, result| match result {
                Ok(Some((message_id, offer))) => {
                    let ui = app.global::<Unsubscribe>();
                    ui.set_sender(sender.into());
                    show_offer(&ui, &offer);
                    ui.set_inbox_count(0);
                    ui.set_stage("confirm".into());
                    LEAVING.with(|slot| {
                        *slot.borrow_mut() = Some(Leaving {
                            message_id,
                            account_id,
                            offer,
                            inbox: Vec::new(),
                        });
                    });
                }
                Ok(None) => crate::undo::tell(
                    &app,
                    UiMessage::plain("This message names no way to unsubscribe."),
                ),
                Err(error) => {
                    crate::undo::tell(&app, UiMessage::detail("Could not unsubscribe: {}", error))
                }
            },
        );
    }

    /// The dialog's main button: make the request, or open the list's page.
    fn confirm(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let Some((message_id, offer)) = LEAVING.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|leaving| (leaving.message_id, leaving.offer.clone()))
        }) else {
            return;
        };
        if let UnsubscribeOffer::Website { url } = &offer {
            let url = url.clone();
            self.close();
            if let Err(error) = crate::mail_render_projection::open_email_link(&self.opener, &url) {
                crate::undo::tell(&app, UiMessage::detail("Could not open link: {}", error));
            }
            return;
        }
        let Some(core) = self.state.borrow().core.clone() else {
            return;
        };
        let ui = app.global::<Unsubscribe>();
        ui.set_stage("working".into());
        self.spawn(
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
            move |app, result| {
                // Closed while the request went out: the request still
                // counts, and the notice tells of it.
                let open = LEAVING.with(|slot| {
                    slot.borrow()
                        .as_ref()
                        .is_some_and(|leaving| leaving.message_id == message_id)
                });
                let ui = app.global::<Unsubscribe>();
                match result {
                    Ok((UnsubscribeOutcome::NeedsBrowser { url }, _)) if open => {
                        ui.set_host(host_of(&url).into());
                        ui.set_stage("website".into());
                        LEAVING.with(|slot| {
                            if let Some(leaving) = slot.borrow_mut().as_mut() {
                                leaving.offer = UnsubscribeOffer::Website { url };
                            }
                        });
                    }
                    Ok((UnsubscribeOutcome::NeedsBrowser { .. }, _)) => {}
                    Ok((outcome, inbox)) => {
                        let sender = ui.get_sender().to_string();
                        let to = ui.get_to().to_string();
                        if open && !inbox.is_empty() {
                            ui.set_inbox_count(i32::try_from(inbox.len()).unwrap_or(i32::MAX));
                            ui.set_stage("done".into());
                            LEAVING.with(|slot| {
                                if let Some(leaving) = slot.borrow_mut().as_mut() {
                                    leaving.inbox = inbox;
                                }
                            });
                            return;
                        }
                        close_dialog(&app);
                        crate::undo::tell(
                            &app,
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
                        &app,
                        UiMessage::detail("Could not unsubscribe: {}", error),
                    ),
                }
            },
        );
    }

    /// Open an emailed request in the composer, to be looked over and sent
    /// there (RFC 2369 appendix B.2.4).
    fn review(&self) {
        let Some(leaving) = LEAVING.with(|slot| slot.borrow_mut().take()) else {
            return;
        };
        self.close();
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
        });
    }

    /// Archive the list's messages in the inbox, or move them to Trash.
    fn clean_up(&self, action: &str) {
        let Some(inbox) = LEAVING.with(|slot| slot.borrow().as_ref().map(|l| l.inbox.clone()))
        else {
            return;
        };
        self.close();
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
            async move { core.act_on_messages(inbox, kind).await },
            move |app, result| match result {
                Ok(action_ids) if !action_ids.is_empty() => crate::undo::gesture(
                    &app,
                    action_ids,
                    UndoableBatch {
                        kind,
                        conversations: false,
                        count,
                    },
                ),
                Ok(_) => {}
                Err(error) => {
                    app.set_render_status(UiMessage::detail("Message action failed: {}", error))
                }
            },
        );
    }

    fn close(&self) {
        LEAVING.with(|slot| slot.borrow_mut().take());
        if let Some(app) = self.app.upgrade() {
            close_dialog(&app);
        }
    }

    /// Run `work` on the runtime and `apply` its result on the UI thread.
    fn spawn<T: Send + 'static>(
        &self,
        work: impl Future<Output = Result<T, String>> + Send + 'static,
        apply: impl FnOnce(AppWindow, Result<T, String>) + 'static,
    ) {
        let weak = self.app.clone();
        let task = self.runtime.spawn(work);
        let abort = task.abort_handle();
        if slint::spawn_local(async move {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            if let Some(app) = weak.upgrade() {
                apply(app, result);
            }
        })
        .is_err()
        {
            abort.abort();
        }
    }
}

fn close_dialog(app: &AppWindow) {
    app.global::<Unsubscribe>().set_stage("".into());
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
