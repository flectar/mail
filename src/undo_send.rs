//! A sent message waits out the undo delay the user chose while a notice at
//! the foot of the main window can take it back, as mail apps let you
//! (#225). Without a delay the composer waits for the message itself
//! (src/composer.rs).

use super::*;
use flectar_mail_core::models::QueueSendResult;

/// How long "Message sent." stays.
const SENT_NOTICE_FOR: Duration = Duration::from_secs(4);
/// At exit, how long messages still in their delay may take to leave, so
/// that they don't wait for the next start.
const EXIT_SEND_LIMIT: Duration = Duration::from_secs(10);

/// A message on its way out this run.
struct Pending {
    action_id: i64,
    draft_id: i64,
    /// Taken back: how it ended is no news.
    undone: bool,
}

/// The message the notice tells of, and what its button does.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Shown {
    action_id: i64,
    draft_id: i64,
}

struct UndoSend {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
    pending: RefCell<Vec<Pending>>,
    shown: Cell<Option<Shown>>,
    hide: slint::Timer,
}

thread_local! {
    static UNDO_SEND: RefCell<Option<Rc<UndoSend>>> = const { RefCell::new(None) };
}

fn with_undo_send(f: impl FnOnce(&Rc<UndoSend>)) {
    UNDO_SEND.with(|slot| {
        if let Some(undo_send) = slot.borrow().as_ref() {
            f(undo_send);
        }
    });
}

/// Connect the notice to the main window.
pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    UNDO_SEND.with(|slot| {
        *slot.borrow_mut() = Some(Rc::new(UndoSend {
            app: app.as_weak(),
            state: Rc::clone(state),
            runtime: runtime.clone(),
            pending: RefCell::default(),
            shown: Cell::new(None),
            hide: slint::Timer::default(),
        }));
    });
    let notice = app.global::<Notice>();
    notice.on_act(|| with_undo_send(|undo_send| undo_send.act()));
    notice.on_dismiss(|| with_undo_send(|undo_send| undo_send.hide_notice()));
}

/// Whether a queued message waits out an undo delay before it leaves.
pub(super) fn holds(queued: &QueueSendResult) -> bool {
    queued.dispatch_at > chrono::Utc::now().timestamp_millis()
}

/// A message left its composer and waits out its delay.
pub(super) fn sending(app: &AppWindow, queued: QueueSendResult) {
    with_undo_send(|undo_send| {
        let Some(core) = undo_send.state.borrow().core.clone() else {
            return;
        };
        undo_send.pending.borrow_mut().push(Pending {
            action_id: queued.action_id,
            draft_id: queued.draft_id,
            undone: false,
        });
        undo_send.show(
            app,
            Shown {
                action_id: queued.action_id,
                draft_id: queued.draft_id,
            },
            UiMessage::plain("Sending message…"),
            "undo",
            None,
        );
        let action_id = queued.action_id;
        undo_send.runtime.spawn(async move {
            // A phone keeps the app running until the message is out.
            #[cfg(background_checks)]
            let _awake = crate::platform::stay_awake();
            let result = core.wait_for_send(action_id).await;
            let _ = slint::invoke_from_event_loop(move || {
                with_undo_send(|undo_send| undo_send.finished(action_id, result));
            });
        });
    });
}

/// The app is ending: messages still waiting out their delay leave now
/// rather than at the next start.
pub(super) fn send_before_exit(runtime: &tokio::runtime::Handle) {
    with_undo_send(|undo_send| {
        let waiting = undo_send
            .pending
            .borrow()
            .iter()
            .filter(|pending| !pending.undone)
            .map(|pending| pending.action_id)
            .collect::<Vec<_>>();
        let Some(core) = undo_send.state.borrow().core.clone() else {
            return;
        };
        if waiting.is_empty() {
            return;
        }
        // The time limit is set inside the runtime, whose timer it needs.
        let _ = runtime.block_on(async {
            tokio::time::timeout(EXIT_SEND_LIMIT, async {
                for &action_id in &waiting {
                    let _ = core.send_now(action_id).await;
                }
                for &action_id in &waiting {
                    let _ = core.wait_for_send(action_id).await;
                }
            })
            .await
        });
    });
}

impl UndoSend {
    /// Show `message` about one send; `action` is what its button does.
    fn show(
        self: &Rc<Self>,
        app: &AppWindow,
        shown: Shown,
        message: UiMessage,
        action: &str,
        hide_after: Option<Duration>,
    ) {
        let notice = app.global::<Notice>();
        notice.set_text(message);
        notice.set_action(action.into());
        notice.set_error(action == "open-draft");
        notice.set_shown(true);
        self.shown.set(Some(shown));
        match hide_after {
            Some(after) => {
                let weak = Rc::downgrade(self);
                self.hide
                    .start(slint::TimerMode::SingleShot, after, move || {
                        if let Some(undo_send) = weak.upgrade()
                            && undo_send.shown.get() == Some(shown)
                        {
                            undo_send.hide_notice();
                        }
                    });
            }
            None => self.hide.stop(),
        }
    }

    fn hide_notice(&self) {
        self.hide.stop();
        self.shown.set(None);
        if let Some(app) = self.app.upgrade() {
            app.global::<Notice>().set_shown(false);
        }
    }

    /// The notice's button: take the message back, or open the draft of
    /// one that could not be sent.
    fn act(self: &Rc<Self>) {
        let (Some(app), Some(shown)) = (self.app.upgrade(), self.shown.get()) else {
            return;
        };
        match app.global::<Notice>().get_action().as_str() {
            "undo" => self.undo(&app, shown),
            "open-draft" => {
                self.hide_notice();
                composer::reopen_draft(shown.draft_id);
            }
            _ => {}
        }
    }

    fn undo(self: &Rc<Self>, app: &AppWindow, shown: Shown) {
        let Some(core) = self.state.borrow().core.clone() else {
            return;
        };
        match self.runtime.block_on(core.cancel_send(shown.action_id)) {
            Ok(true) => {
                if let Some(pending) = self
                    .pending
                    .borrow_mut()
                    .iter_mut()
                    .find(|pending| pending.action_id == shown.action_id)
                {
                    pending.undone = true;
                }
                self.hide_notice();
                composer::reopen_draft(shown.draft_id);
            }
            // Already on its way: its end comes as usual.
            Ok(false) => self.show(
                app,
                shown,
                UiMessage::plain("This message has already been sent."),
                "",
                None,
            ),
            Err(error) => self.show(
                app,
                shown,
                UiMessage::detail("Could not take the message back: {}", error),
                "",
                None,
            ),
        }
    }

    /// A message left, or could not.
    fn finished(self: &Rc<Self>, action_id: i64, result: Result<(), String>) {
        let ended = {
            let mut pending = self.pending.borrow_mut();
            pending
                .iter()
                .position(|pending| pending.action_id == action_id)
                .map(|index| pending.remove(index))
        };
        let (Some(ended), Some(app)) = (ended, self.app.upgrade()) else {
            return;
        };
        if ended.undone {
            return;
        }
        let _ = refresh_from_source(&app, &self.state, &self.runtime, true, &[]);
        let shown = Shown {
            action_id,
            draft_id: ended.draft_id,
        };
        match result {
            Ok(()) if self.shown.get() == Some(shown) => self.show(
                &app,
                shown,
                UiMessage::plain("Message sent."),
                "",
                Some(SENT_NOTICE_FOR),
            ),
            Ok(()) => {}
            // The draft stays in Drafts to fix and send again.
            Err(error) => self.show(
                &app,
                shown,
                UiMessage::detail("Message was not sent: {} Your draft is preserved.", error),
                "open-draft",
                None,
            ),
        }
    }
}
