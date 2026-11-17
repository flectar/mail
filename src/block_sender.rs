//! Blocking the sender of the open message (#227). A dialog asks first;
//! their mail in an inbox then moves to Spam, with Undo, and sync sends what
//! they mail later there too (`Core::block_sender`). Settings lists the
//! blocked addresses.

use super::*;
use std::future::Future;

struct Controller {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
}

thread_local! {
    static CONTROLLER: RefCell<Option<Rc<Controller>>> = const { RefCell::new(None) };
}

fn with_controller(f: impl FnOnce(&Rc<Controller>)) {
    let controller = CONTROLLER.with(|slot| slot.borrow().clone());
    if let Some(controller) = controller {
        f(&controller);
    }
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    CONTROLLER.with(|slot| {
        *slot.borrow_mut() = Some(Rc::new(Controller {
            app: app.as_weak(),
            state: Rc::clone(state),
            runtime: runtime.clone(),
        }));
    });
    let ui = app.global::<BlockSender>();
    ui.on_start(|| with_controller(|controller| controller.start()));
    ui.on_confirm(|| with_controller(|controller| controller.confirm()));
    ui.on_close(|| with_controller(|controller| controller.close()));
    ui.on_unblock(|address| {
        with_controller(|controller| controller.unblock(address.to_string(), false));
    });
}

/// Show the blocked addresses in Settings, once the core is there.
pub(super) fn refresh() {
    with_controller(|controller| controller.refresh_list());
}

/// Undo took a block back.
pub(super) fn unblocked(address: &str) {
    with_controller(|controller| controller.changed(address, false));
}

impl Controller {
    fn core(&self) -> Option<CoreMailSource> {
        self.state.borrow().core.clone()
    }

    /// The menu: ask before blocking the sender of the open message, or
    /// unblock them.
    fn start(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<BlockSender>();
        if !ui.get_stage().is_empty() || !ui.get_available() {
            return;
        }
        let address = app.get_selected_address().to_string();
        if ui.get_blocked() {
            self.unblock(address, true);
            return;
        }
        let Some(core) = self.core() else {
            return;
        };
        let name = app.get_selected_sender().to_string();
        self.spawn(
            async move { core.block_offer(&address).await },
            move |app, result| match result {
                Ok(Some(offer)) => {
                    let ui = app.global::<BlockSender>();
                    // A sender known only by address is named by it once.
                    let name = if name.eq_ignore_ascii_case(&offer.address) {
                        String::new()
                    } else {
                        name
                    };
                    ui.set_name(name.into());
                    ui.set_address(offer.address.into());
                    ui.set_inbox_count(i32::try_from(offer.inbox_count).unwrap_or(i32::MAX));
                    ui.set_stage("confirm".into());
                }
                Ok(None) => {}
                Err(error) => {
                    crate::undo::tell(&app, UiMessage::detail("Could not block: {}", error))
                }
            },
        );
    }

    /// The dialog's Block button.
    fn confirm(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<BlockSender>();
        if ui.get_stage() != "confirm" {
            return;
        }
        let Some(core) = self.core() else {
            return;
        };
        let address = ui.get_address().to_string();
        ui.set_stage("working".into());
        self.spawn(
            async move { core.block_sender(&address).await },
            |app, result| {
                app.global::<BlockSender>().set_stage("".into());
                match result {
                    Ok(blocked) => {
                        with_controller(|controller| controller.changed(&blocked.address, true));
                        crate::undo::blocked(&app, blocked.address, blocked.action_ids);
                    }
                    Err(error) => {
                        crate::undo::tell(&app, UiMessage::detail("Could not block: {}", error))
                    }
                }
            },
        );
    }

    fn close(&self) {
        if let Some(app) = self.app.upgrade() {
            let ui = app.global::<BlockSender>();
            if ui.get_stage() != "working" {
                ui.set_stage("".into());
            }
        }
    }

    /// Settings, or the menu of a blocked sender's message: stop blocking
    /// `address`, and say so when asked from a message.
    fn unblock(&self, address: String, tell: bool) {
        let Some(core) = self.core() else {
            return;
        };
        self.spawn(
            async move { core.unblock_sender(&address).await.map(|()| address) },
            move |app, result| match result {
                Ok(address) => {
                    with_controller(|controller| controller.changed(&address, false));
                    if tell {
                        crate::undo::tell(&app, UiMessage::detail("Unblocked {}.", address));
                    }
                }
                Err(error) => {
                    crate::undo::tell(&app, UiMessage::detail("Could not unblock: {}", error))
                }
            },
        );
    }

    /// The block list changed: the open message's menu, the loaded rows from
    /// that sender, and Settings follow.
    fn changed(&self, address: &str, blocked: bool) {
        if let Some(app) = self.app.upgrade()
            && app.get_selected_address().eq_ignore_ascii_case(address)
        {
            app.global::<BlockSender>().set_blocked(blocked);
        }
        for message in self.state.borrow_mut().messages.iter_mut() {
            if message.address.eq_ignore_ascii_case(address) {
                message.sender_blocked = blocked;
            }
        }
        self.refresh_list();
    }

    fn refresh_list(&self) {
        let Some(core) = self.core() else {
            return;
        };
        self.spawn(
            async move { core.blocked_addresses().await },
            |app, result| {
                if let Ok(addresses) = result {
                    let addresses = addresses
                        .into_iter()
                        .map(slint::SharedString::from)
                        .collect::<Vec<_>>();
                    app.global::<BlockSender>()
                        .set_addresses(ModelRc::new(VecModel::from(addresses)));
                }
            },
        );
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
