//! Read receipts (#233, RFC 8098). Settings → General decides what happens
//! when the open message asks for one: nothing, a bar above the message that
//! asks, or a receipt sent at once, except where the core says the user has
//! to agree first (`ReceiptRequest`). On a message the user sent, the
//! receipts that came back show above it.

use super::*;
use crate::mail::{MailMessage, ReceiptAsk};
use flectar_mail_core::models::{Disposition, ReadReceiptPolicy, ReceiptAnswer};

type Answer = Rc<dyn Fn(i64, ReceiptAnswer)>;

thread_local! {
    /// Answers a message's request through the core.
    static ANSWER: RefCell<Option<Answer>> = const { RefCell::new(None) };
    /// The message whose request the bar shows.
    static OPEN: Cell<Option<i64>> = const { Cell::new(None) };
    /// Requests answered here, until the core's answer reaches the reader.
    static ANSWERED: RefCell<HashSet<i64>> = RefCell::new(HashSet::new());
    /// The open message's returned receipts, all of them, and whether the
    /// list shows them all.
    static RETURNED: RefCell<(Option<i32>, Vec<ReturnedReceipt>, bool)> =
        const { RefCell::new((None, Vec::new(), false)) };
}

/// A list this long shows its first two and a button for the rest.
const RETURNED_SHOWN: usize = 3;

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
    let app_weak = app.as_weak();
    ui.on_show_all_returned(move || {
        RETURNED.with_borrow_mut(|returned| returned.2 = true);
        if let Some(app) = app_weak.upgrade() {
            show_returned(&app);
        }
    });
}

/// The open message's read receipts: its request, by the bar that asks or,
/// with Always, a receipt sent now when it needs no agreement; and the
/// receipts that came back for it.
pub(super) fn project(app: &AppWindow, email: &MailMessage) {
    project_returned(app, email);
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
        clear_request(app);
        return;
    };
    if policy == ReadReceiptPolicy::Always && !request.needs_confirmation {
        clear_request(app);
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

fn clear_request(app: &AppWindow) {
    OPEN.set(None);
    app.global::<ReadReceipts>().set_available(false);
}

pub(super) fn clear(app: &AppWindow) {
    clear_request(app);
    RETURNED.set((None, Vec::new(), false));
    show_returned(app);
}

/// What the receipts that came back for the open message say. A list that
/// was opened in full stays so while the same message is open.
fn project_returned(app: &AppWindow, email: &MailMessage) {
    let now = Local::now();
    let rows = email
        .returned_receipts
        .iter()
        .map(|receipt| ReturnedReceipt {
            kind: match receipt.disposition {
                Disposition::Displayed => "read",
                Disposition::Deleted => "deleted",
                Disposition::Dispatched | Disposition::Processed => "received",
            }
            .into(),
            name: receipt
                .recipient
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| receipt.recipient.email.clone())
                .into(),
            time: crate::mail_view_model::message_time(receipt.reported_at, &now).into(),
        })
        .collect();
    RETURNED.with_borrow_mut(|returned| {
        let all = returned.0 == Some(email.id) && returned.2;
        *returned = (Some(email.id), rows, all);
    });
    show_returned(app);
}

fn show_returned(app: &AppWindow) {
    let ui = app.global::<ReadReceipts>();
    RETURNED.with_borrow(|(_, rows, all)| {
        let shown = if rows.len() > RETURNED_SHOWN && !all {
            RETURNED_SHOWN - 1
        } else {
            rows.len()
        };
        let current = ui.get_returned();
        // An unchanged list keeps its rows rather than rebuilding them.
        if current.row_count() != shown || current.iter().zip(rows).any(|(row, new)| row != *new) {
            ui.set_returned(ModelRc::new(VecModel::from(rows[..shown].to_vec())));
        }
        ui.set_more_returned(i32::try_from(rows.len() - shown).unwrap_or(i32::MAX));
    });
}
