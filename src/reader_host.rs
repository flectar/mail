//! The window a reader shows the open message in (#254). Each window has its
//! own reader globals, so the reader's rendering and input reach them through
//! its host rather than through the main window, and the same code can drive
//! a reader in a window of its own.

use super::*;

/// A reader's window, as callbacks and timers that may outlive it hold it.
#[derive(Clone)]
pub(crate) enum ReaderHost {
    Main(slint::Weak<AppWindow>),
}

impl ReaderHost {
    pub(crate) fn upgrade(&self) -> Option<ReaderRef> {
        match self {
            Self::Main(app) => app.upgrade().map(ReaderRef::Main),
        }
    }
}

/// A reader's window while it is in use.
pub(crate) enum ReaderRef {
    Main(AppWindow),
}

impl ReaderRef {
    /// The main window's reader.
    pub(crate) fn main(app: &AppWindow) -> Self {
        Self::Main(app.clone_strong())
    }

    pub(crate) fn host(&self) -> ReaderHost {
        match self {
            Self::Main(app) => ReaderHost::Main(app.as_weak()),
        }
    }

    /// The main window, which shows what is outside the reader, such as the
    /// notices of what was done to the message.
    pub(crate) fn app(&self) -> &AppWindow {
        match self {
            Self::Main(app) => app,
        }
    }

    pub(crate) fn window(&self) -> &slint::Window {
        match self {
            Self::Main(app) => app.window(),
        }
    }

    pub(crate) fn ui(&self) -> ReaderUi<'_> {
        match self {
            Self::Main(app) => app.global::<ReaderUi>(),
        }
    }

    pub(crate) fn email(&self) -> EmailReader<'_> {
        match self {
            Self::Main(app) => app.global::<EmailReader>(),
        }
    }

    pub(crate) fn colors(&self) -> MessageColors<'_> {
        match self {
            Self::Main(app) => app.global::<MessageColors>(),
        }
    }

    pub(crate) fn send_later(&self) -> SendLaterUi<'_> {
        match self {
            Self::Main(app) => app.global::<SendLaterUi>(),
        }
    }

    pub(crate) fn reminder(&self) -> ReminderUi<'_> {
        match self {
            Self::Main(app) => app.global::<ReminderUi>(),
        }
    }

    pub(crate) fn unsubscribe(&self) -> Unsubscribe<'_> {
        match self {
            Self::Main(app) => app.global::<Unsubscribe>(),
        }
    }

    pub(crate) fn block_sender(&self) -> BlockSender<'_> {
        match self {
            Self::Main(app) => app.global::<BlockSender>(),
        }
    }

    /// The OpenPGP message the reader shows over itself.
    pub(crate) fn preferences(&self) -> AccountMailPreferences<'_> {
        match self {
            Self::Main(app) => app.global::<AccountMailPreferences>(),
        }
    }
}
