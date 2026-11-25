//! The window a reader shows the open message in (#254). Each window has its
//! own reader globals, so the reader's rendering and input reach them through
//! its host rather than through the main window, and the same code drives
//! the main window's reader and a conversation's window of its own.

use super::*;

/// Which reader: the main window's, or a conversation window's. What a
/// module keeps for a reader, it keeps under this key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReaderKey {
    Main,
    #[cfg(desktop)]
    Window(u64),
}

impl ReaderKey {
    /// The reader's window, while it is open.
    pub(crate) fn host(self, app: &slint::Weak<AppWindow>) -> Option<ReaderHost> {
        match self {
            Self::Main => Some(ReaderHost::Main(app.clone())),
            #[cfg(desktop)]
            Self::Window(id) => crate::reader_window::host(id),
        }
    }
}

/// The conversation a reader shows, to change what it keeps of a message, as
/// a card read again. `None` once the reader's window is gone.
pub(crate) fn with_messages<R>(
    state: &RefCell<InboxState>,
    key: ReaderKey,
    f: impl FnOnce(&mut Vec<MailMessage>) -> R,
) -> Option<R> {
    match key {
        ReaderKey::Main => Some(f(&mut state.borrow_mut().conversation_messages)),
        #[cfg(desktop)]
        ReaderKey::Window(id) => crate::reader_window::with_messages(id, f),
    }
}

/// The list row of the conversation a reader shows, which names the thread
/// or message that actions act on.
pub(crate) fn row(state: &RefCell<InboxState>, key: ReaderKey) -> Option<MailMessage> {
    match key {
        ReaderKey::Main => {
            let state = state.borrow();
            state
                .selected_id
                .and_then(|id| state.messages.iter().find(|message| message.id == id))
                .cloned()
        }
        #[cfg(desktop)]
        ReaderKey::Window(id) => crate::reader_window::row(id),
    }
}

/// The mailbox a reader's conversation is in, as moving it counts it: the
/// list's, or the one a window's conversation was opened from.
pub(crate) fn scope(state: &RefCell<InboxState>, key: ReaderKey) -> Scope {
    match key {
        ReaderKey::Main => state.borrow().scope,
        #[cfg(desktop)]
        ReaderKey::Window(id) => {
            crate::reader_window::scope(id).unwrap_or_else(|| state.borrow().scope)
        }
    }
}

/// The message a reader shows: one of its conversation's, or the list row
/// while the conversation loads.
pub(crate) fn shown_message(state: &RefCell<InboxState>, key: ReaderKey) -> Option<MailMessage> {
    match key {
        ReaderKey::Main => {
            let state = state.borrow();
            let row = state
                .selected_id
                .and_then(|id| state.messages.iter().find(|message| message.id == id))?;
            let shown = if state.conversation_owner_id == Some(row.id) {
                state
                    .conversation_messages
                    .get(state.conversation_selected_index)
                    .unwrap_or(row)
            } else {
                row
            };
            Some(shown.clone())
        }
        #[cfg(desktop)]
        ReaderKey::Window(id) => crate::reader_window::shown_message(id),
    }
}

/// Every message of the conversations the readers keep, to change what they
/// keep of it, as whether its sender is blocked.
pub(crate) fn each_kept_message(state: &RefCell<InboxState>, f: &mut impl FnMut(&mut MailMessage)) {
    state
        .borrow_mut()
        .conversation_messages
        .iter_mut()
        .for_each(&mut *f);
    #[cfg(desktop)]
    crate::reader_window::each_kept_message(f);
}

/// The conversation `key` shows left the mailbox it was in, as moving or
/// snoozing it from the reader does: a conversation's window closes.
pub(crate) fn conversation_left(key: ReaderKey) {
    match key {
        ReaderKey::Main => {}
        #[cfg(desktop)]
        ReaderKey::Window(id) => crate::reader_window::conversation_left(id),
    }
}

/// Every reader: the main window's, then each conversation window's.
pub(crate) fn for_each_reader(app: &AppWindow, f: impl Fn(&ReaderRef)) {
    f(&ReaderRef::main(app));
    #[cfg(desktop)]
    crate::reader_window::for_each(&f);
}

/// Show the reader's message again, as after it was translated.
pub(crate) fn show_again(
    reader: &ReaderRef,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    match reader {
        ReaderRef::Main(app) => {
            state.borrow().email_renderer.borrow_mut().loaded_key = None;
            if let Err(error) = render_current(app, state, runtime) {
                app.set_render_status(UiMessage::detail("Could not show the message: {}", error));
            }
        }
        #[cfg(desktop)]
        ReaderRef::Window { id, .. } => crate::reader_window::show_again(*id),
    }
}

/// The message `id` of the conversation a reader shows, as it keeps it.
pub(crate) fn kept_message(
    state: &RefCell<InboxState>,
    key: ReaderKey,
    id: i32,
) -> Option<MailMessage> {
    match key {
        ReaderKey::Main => state
            .borrow()
            .conversation_messages
            .iter()
            .find(|message| message.id == id)
            .cloned(),
        #[cfg(desktop)]
        ReaderKey::Window(window) => crate::reader_window::kept_message(window, id),
    }
}

/// A reader's window, as callbacks and timers that may outlive it hold it.
#[derive(Clone)]
pub(crate) enum ReaderHost {
    Main(slint::Weak<AppWindow>),
    #[cfg(desktop)]
    Window {
        window: slint::Weak<ReaderWindow>,
        app: slint::Weak<AppWindow>,
        id: u64,
    },
}

impl ReaderHost {
    pub(crate) fn upgrade(&self) -> Option<ReaderRef> {
        match self {
            Self::Main(app) => app.upgrade().map(ReaderRef::Main),
            #[cfg(desktop)]
            Self::Window { window, app, id } => Some(ReaderRef::Window {
                window: window.upgrade()?,
                app: app.upgrade()?,
                id: *id,
            }),
        }
    }

    pub(crate) fn key(&self) -> ReaderKey {
        match self {
            Self::Main(_) => ReaderKey::Main,
            #[cfg(desktop)]
            Self::Window { id, .. } => ReaderKey::Window(*id),
        }
    }

    /// Run `f` with the reader on the UI thread, from any thread, while its
    /// window is open.
    pub(crate) fn in_event_loop(self, f: impl FnOnce(ReaderRef) + Send + 'static) {
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(reader) = self.upgrade() {
                f(reader);
            }
        });
    }
}

/// A reader's window while it is in use.
pub(crate) enum ReaderRef {
    Main(AppWindow),
    #[cfg(desktop)]
    Window {
        window: ReaderWindow,
        app: AppWindow,
        id: u64,
    },
}

impl ReaderRef {
    /// The main window's reader.
    pub(crate) fn main(app: &AppWindow) -> Self {
        Self::Main(app.clone_strong())
    }

    pub(crate) fn host(&self) -> ReaderHost {
        match self {
            Self::Main(app) => ReaderHost::Main(app.as_weak()),
            #[cfg(desktop)]
            Self::Window { window, app, id } => ReaderHost::Window {
                window: window.as_weak(),
                app: app.as_weak(),
                id: *id,
            },
        }
    }

    pub(crate) fn key(&self) -> ReaderKey {
        match self {
            Self::Main(_) => ReaderKey::Main,
            #[cfg(desktop)]
            Self::Window { id, .. } => ReaderKey::Window(*id),
        }
    }

    /// The main window, which shows what is outside the reader, such as the
    /// notices of what was done to the message.
    pub(crate) fn app(&self) -> &AppWindow {
        match self {
            Self::Main(app) => app,
            #[cfg(desktop)]
            Self::Window { app, .. } => app,
        }
    }

    pub(crate) fn window(&self) -> &slint::Window {
        match self {
            Self::Main(app) => app.window(),
            #[cfg(desktop)]
            Self::Window { window, .. } => window.window(),
        }
    }

    /// Open a link of the message, as clicking it does.
    pub(crate) fn open_link(&self, url: slint::SharedString) {
        match self {
            Self::Main(app) => app.invoke_open_email_link(url),
            #[cfg(desktop)]
            Self::Window { window, .. } => window.invoke_open_email_link(url),
        }
    }

    /// What clicking a link of the message does in this reader.
    pub(crate) fn on_open_link(&self, open: impl Fn(slint::SharedString) + 'static) {
        match self {
            Self::Main(app) => app.on_open_email_link(open),
            #[cfg(desktop)]
            Self::Window { window, .. } => window.on_open_email_link(open),
        }
    }

    /// Reply to, forward or edit the message the reader shows, as its menu
    /// does.
    pub(crate) fn compose_message_action(&self, action: slint::SharedString) {
        match self {
            Self::Main(app) => app.invoke_compose_message_action(action),
            #[cfg(desktop)]
            Self::Window { window, .. } => window.invoke_compose_message_action(action),
        }
    }

    /// Bring the main window forward, for what a conversation's window
    /// shows there, such as the calendar or Settings.
    pub(crate) fn raise_main(&self) {
        match self {
            Self::Main(_) => {}
            #[cfg(desktop)]
            Self::Window { app, .. } => crate::window_controller::activate(app, None),
        }
    }

    /// One of the reader's window's globals.
    pub(crate) fn global<'a, T>(&'a self) -> T
    where
        T: slint::Global<'a, AppWindow> + slint::Global<'a, ReaderWindow>,
    {
        match self {
            Self::Main(app) => app.global::<T>(),
            #[cfg(desktop)]
            Self::Window { window, .. } => window.global::<T>(),
        }
    }

    pub(crate) fn ui(&self) -> ReaderUi<'_> {
        self.global()
    }

    pub(crate) fn email(&self) -> EmailReader<'_> {
        self.global()
    }

    pub(crate) fn colors(&self) -> MessageColors<'_> {
        self.global()
    }

    pub(crate) fn send_later(&self) -> SendLaterUi<'_> {
        self.global()
    }

    pub(crate) fn reminder(&self) -> ReminderUi<'_> {
        self.global()
    }

    pub(crate) fn unsubscribe(&self) -> Unsubscribe<'_> {
        self.global()
    }

    pub(crate) fn block_sender(&self) -> BlockSender<'_> {
        self.global()
    }

    /// The OpenPGP message the reader shows over itself.
    pub(crate) fn preferences(&self) -> AccountMailPreferences<'_> {
        self.global()
    }
}

/// What a module keeps for each reader, such as the message whose card a
/// reader shows. A window's entry goes when the window closes.
pub(crate) struct PerReader<T> {
    entries: RefCell<Vec<(ReaderKey, T)>>,
}

impl<T> PerReader<T> {
    pub(crate) const fn new() -> Self {
        Self {
            entries: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn set(&self, key: ReaderKey, value: T) {
        let mut entries = self.entries.borrow_mut();
        match entries.iter_mut().find(|(entry, _)| *entry == key) {
            Some((_, slot)) => *slot = value,
            None => entries.push((key, value)),
        }
    }

    pub(crate) fn remove(&self, key: ReaderKey) -> Option<T> {
        let mut entries = self.entries.borrow_mut();
        let index = entries.iter().position(|(entry, _)| *entry == key)?;
        Some(entries.swap_remove(index).1)
    }

    pub(crate) fn with<R>(&self, key: ReaderKey, f: impl FnOnce(Option<&mut T>) -> R) -> R {
        let mut entries = self.entries.borrow_mut();
        f(entries
            .iter_mut()
            .find(|(entry, _)| *entry == key)
            .map(|(_, value)| value))
    }

    /// The readers that have an entry.
    pub(crate) fn keys(&self) -> Vec<ReaderKey> {
        self.entries.borrow().iter().map(|(key, _)| *key).collect()
    }
}

impl<T: Clone> PerReader<T> {
    pub(crate) fn get(&self, key: ReaderKey) -> Option<T> {
        self.with(key, |value| value.cloned())
    }
}

#[cfg(all(test, desktop))]
mod tests {
    use super::*;

    #[test]
    fn each_reader_keeps_its_own_state_until_its_window_goes() {
        let open = PerReader::new();
        open.set(ReaderKey::Main, 1);
        open.set(ReaderKey::Window(7), 2);
        // A reader's new value takes the place of its old one.
        open.set(ReaderKey::Window(7), 3);
        assert_eq!(open.get(ReaderKey::Main), Some(1));
        assert_eq!(open.get(ReaderKey::Window(7)), Some(3));
        assert_eq!(open.get(ReaderKey::Window(8)), None);
        assert_eq!(open.keys().len(), 2);
        open.with(ReaderKey::Main, |value| {
            if let Some(value) = value {
                *value += 10;
            }
        });
        assert_eq!(open.get(ReaderKey::Main), Some(11));
        assert_eq!(open.remove(ReaderKey::Window(7)), Some(3));
        assert_eq!(open.remove(ReaderKey::Window(7)), None);
        assert_eq!(open.keys(), [ReaderKey::Main]);
    }
}
