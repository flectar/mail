//! Read receipts (#233, RFC 8098). Settings → General decides what happens
//! when the open message asks for one: nothing, a bar above the message that
//! asks, or a receipt sent at once, except where the core says the user has
//! to agree first (`ReceiptRequest`). On a message the user sent, the
//! receipts that came back show above it.

use super::*;
use crate::mail::{MailMessage, ReceiptAsk};
use flectar_mail_core::models::{Disposition, ReadReceiptPolicy, ReceiptAnswer};

type Answer = Rc<dyn Fn(i64, ReceiptAnswer)>;

/// The receipts that came back for the message a reader shows, all of them,
/// and whether its list shows them all.
type Returned = (Option<i32>, Vec<ReturnedReceipt>, bool);

thread_local! {
    /// Answers a message's request through the core.
    static ANSWER: RefCell<Option<Answer>> = const { RefCell::new(None) };
    /// The message whose request each reader's bar shows.
    static OPEN: PerReader<i64> = const { PerReader::new() };
    /// Requests answered here, until the core's answer reaches the reader.
    static ANSWERED: RefCell<HashSet<i64>> = RefCell::new(HashSet::new());
    static RETURNED: PerReader<Returned> = const { PerReader::new() };
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
            // Every reader that asks about this message stops asking.
            for key in OPEN.with(PerReader::keys) {
                if OPEN.with(|open| open.get(key)) == Some(message_id)
                    && let Some(reader) = key.host(&app).and_then(|host| host.upgrade())
                {
                    reader.global::<ReadReceipts>().set_available(false);
                }
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
    ANSWER.set(Some(answer));
    attach(&ReaderRef::main(app));
}

/// Answer from the bar a reader shows, and open its list of receipts.
pub(super) fn attach(reader: &ReaderRef) {
    let ui = reader.global::<ReadReceipts>();
    let key = reader.key();
    ui.on_send(move || answer_open(key, ReceiptAnswer::Send { user_agreed: true }));
    ui.on_ignore(move || answer_open(key, ReceiptAnswer::Decline));
    let host = reader.host();
    ui.on_show_all_returned(move || {
        RETURNED.with(|returned| {
            returned.with(host.key(), |returned| {
                if let Some(returned) = returned {
                    returned.2 = true;
                }
            });
        });
        if let Some(reader) = host.upgrade() {
            show_returned(&reader);
        }
    });
}

/// Answer the request the reader's bar shows.
fn answer_open(key: ReaderKey, answer: ReceiptAnswer) {
    let Some(message_id) = OPEN.with(|open| open.get(key)) else {
        return;
    };
    if let Some(send) = ANSWER.with_borrow(Clone::clone) {
        send(message_id, answer);
    }
}

/// The open message's read receipts: its request, by the bar that asks or,
/// with Always, a receipt sent now when it needs no agreement; and the
/// receipts that came back for it.
pub(super) fn project(reader: &ReaderRef, email: &MailMessage) {
    project_returned(reader, email);
    // The policy is the user's, in the main window's Settings.
    let policy = ReadReceiptPolicy::parse(&reader.app().global::<ReadReceipts>().get_policy())
        .unwrap_or_default();
    let ask = email.receipt.as_ref().filter(|ask| {
        policy != ReadReceiptPolicy::Never
            && !ANSWERED.with_borrow(|answered| answered.contains(&ask.message_id))
    });
    let Some(ReceiptAsk {
        message_id,
        request,
    }) = ask
    else {
        clear_request(reader);
        return;
    };
    if policy == ReadReceiptPolicy::Always && !request.needs_confirmation {
        clear_request(reader);
        let message_id = *message_id;
        if let Some(answer) = ANSWER.with_borrow(Clone::clone) {
            answer(message_id, ReceiptAnswer::Send { user_agreed: false });
        }
        return;
    }
    OPEN.with(|open| open.set(reader.key(), *message_id));
    let ui = reader.global::<ReadReceipts>();
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

fn clear_request(reader: &ReaderRef) {
    OPEN.with(|open| open.remove(reader.key()));
    reader.global::<ReadReceipts>().set_available(false);
}

pub(super) fn clear(reader: &ReaderRef) {
    clear_request(reader);
    RETURNED.with(|returned| returned.remove(reader.key()));
    show_returned(reader);
}

/// A reader's window closed.
#[cfg(desktop)]
pub(super) fn forget(key: ReaderKey) {
    OPEN.with(|open| open.remove(key));
    RETURNED.with(|returned| returned.remove(key));
}

/// What the receipts that came back for the open message say. A list that
/// was opened in full stays so while the same message is open.
fn project_returned(reader: &ReaderRef, email: &MailMessage) {
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
    RETURNED.with(|returned| {
        let all = returned.with(reader.key(), |returned| {
            returned.is_some_and(|(id, _, all)| *id == Some(email.id) && *all)
        });
        returned.set(reader.key(), (Some(email.id), rows, all));
    });
    show_returned(reader);
}

fn show_returned(reader: &ReaderRef) {
    let ui = reader.global::<ReadReceipts>();
    let (rows, all) = RETURNED
        .with(|returned| returned.get(reader.key()))
        .map(|(_, rows, all)| (rows, all))
        .unwrap_or_default();
    let shown = if rows.len() > RETURNED_SHOWN && !all {
        RETURNED_SHOWN - 1
    } else {
        rows.len()
    };
    let current = ui.get_returned();
    // An unchanged list keeps its rows rather than rebuilding them.
    if current.row_count() != shown || current.iter().zip(&rows).any(|(row, new)| row != *new) {
        ui.set_returned(ModelRc::new(VecModel::from(rows[..shown].to_vec())));
    }
    ui.set_more_returned(i32::try_from(rows.len() - shown).unwrap_or(i32::MAX));
}
