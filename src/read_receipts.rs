//! Answering read receipt requests (#233, RFC 8098). Settings → General
//! decides what happens when the open message asks for one: nothing, a bar
//! above the message that asks, or a receipt sent at once, except where the
//! core says the user has to agree first (`ReceiptRequest`).

use super::*;
use crate::mail::{MailMessage, ReceiptAsk};
use flectar_mail_core::models::{ReadReceiptPolicy, ReceiptAnswer};

type Answer = Rc<dyn Fn(i64, ReceiptAnswer)>;

thread_local! {
    /// Answers a message's request through the core.
    static ANSWER: RefCell<Option<Answer>> = const { RefCell::new(None) };
    /// The message whose request the bar shows.
    static OPEN: Cell<Option<i64>> = const { Cell::new(None) };
    /// Requests answered here, until the core's answer reaches the reader.
    static ANSWERED: RefCell<HashSet<i64>> = RefCell::new(HashSet::new());
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    let answer: Answer = {
        let app = app.as_weak();
        let state = Rc::clone(state);
        let runtime = runtime.clone();
        Rc::new(move |message_id, answer| {
            if !ANSWERED.with_borrow_mut(|answered| answered.insert(message_id)) {
                return;
            }
            if let Some(app) = app.upgrade()
                && OPEN.get() == Some(message_id)
            {
                app.global::<ReadReceipts>().set_available(false);
            }
            let Some(core) = state.borrow().core.clone() else {
                return;
            };
            let app = app.clone();
            runtime.spawn(async move {
                if let Err(error) = core.answer_read_receipt(message_id, answer).await {
                    let _ = app.upgrade_in_event_loop(move |app| {
                        crate::undo::tell(
                            &app,
                            UiMessage::detail(
                                "Could not answer the read receipt request: {}",
                                error,
                            ),
                        );
                    });
                }
            });
        })
    };
    ANSWER.set(Some(Rc::clone(&answer)));
    let ui = app.global::<ReadReceipts>();
    let send = Rc::clone(&answer);
    ui.on_send(move || {
        if let Some(message_id) = OPEN.get() {
            send(message_id, ReceiptAnswer::Send { user_agreed: true });
        }
    });
    ui.on_ignore(move || {
        if let Some(message_id) = OPEN.get() {
            answer(message_id, ReceiptAnswer::Decline);
        }
    });
}

/// The open message's request: the bar that asks, or with Always send a
/// receipt sent now when it needs no agreement.
pub(super) fn project(app: &AppWindow, email: &MailMessage) {
    let ui = app.global::<ReadReceipts>();
    let policy = ReadReceiptPolicy::parse(&ui.get_policy()).unwrap_or_default();
    let ask = email.receipt.as_ref().filter(|ask| {
        policy != ReadReceiptPolicy::Never
            && !ANSWERED.with_borrow(|answered| answered.contains(&ask.message_id))
    });
    let Some(ReceiptAsk {
        message_id,
        request,
    }) = ask
    else {
        clear(app);
        return;
    };
    if policy == ReadReceiptPolicy::Always && !request.needs_confirmation {
        clear(app);
        let message_id = *message_id;
        if let Some(answer) = ANSWER.with_borrow(Clone::clone) {
            answer(message_id, ReceiptAnswer::Send { user_agreed: false });
        }
        return;
    }
    OPEN.set(Some(*message_id));
    ui.set_sender(email.sender.clone().into());
    // The bar names the receipt's address only when it isn't the sender's.
    let elsewhere = request
        .to
        .iter()
        .filter(|to| !to.email.eq_ignore_ascii_case(&email.address))
        .map(|to| to.email.as_str())
        .collect::<Vec<_>>();
    let address = if elsewhere.is_empty() {
        String::new()
    } else {
        request
            .to
            .iter()
            .map(|to| to.email.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    ui.set_address(address.into());
    ui.set_available(true);
}

pub(super) fn clear(app: &AppWindow) {
    OPEN.set(None);
    app.global::<ReadReceipts>().set_available(false);
}
