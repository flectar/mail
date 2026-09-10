//! Structured, reactive UI messages shared by Rust controllers and Slint.
//!
//! Rust supplies only a stable source key and substitution arguments. Slint
//! owns the wording and keeps the rendered text reactive when the bundled
//! language changes. This also prevents background workers from formatting
//! user-facing English before an update reaches the UI thread.

use crate::{AppWindow, I18n, LocalizedMessage};
use slint::{ComponentHandle, SharedString};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct UiMessage {
    key: &'static str,
    argument: String,
    second_argument: String,
    third_argument: String,
}

impl UiMessage {
    pub(crate) const EMPTY: Self = Self {
        key: "",
        argument: String::new(),
        second_argument: String::new(),
        third_argument: String::new(),
    };

    pub(crate) fn plain(key: &'static str) -> Self {
        Self { key, ..Self::EMPTY }
    }

    pub(crate) fn detail(key: &'static str, argument: impl ToString) -> Self {
        Self {
            key,
            argument: argument.to_string(),
            second_argument: String::new(),
            third_argument: String::new(),
        }
    }

    pub(crate) fn arguments(
        key: &'static str,
        argument: impl ToString,
        second_argument: impl ToString,
    ) -> Self {
        Self {
            key,
            argument: argument.to_string(),
            second_argument: second_argument.to_string(),
            third_argument: String::new(),
        }
    }

    pub(crate) fn three_arguments(
        key: &'static str,
        argument: impl ToString,
        second_argument: impl ToString,
        third_argument: impl ToString,
    ) -> Self {
        Self {
            key,
            argument: argument.to_string(),
            second_argument: second_argument.to_string(),
            third_argument: third_argument.to_string(),
        }
    }

    fn as_slint(&self) -> LocalizedMessage {
        LocalizedMessage {
            key: self.key.into(),
            argument: self.argument.as_str().into(),
            second_argument: self.second_argument.as_str().into(),
            third_argument: self.third_argument.as_str().into(),
        }
    }
}

pub(crate) fn translated(app: &AppWindow, message: &UiMessage) -> SharedString {
    let message = message.as_slint();
    app.global::<I18n>().invoke_ui_message(
        message.key,
        message.argument,
        message.second_argument,
        message.third_argument,
    )
}

pub(crate) trait AppWindowMessages {
    fn set_startup_error(&self, message: UiMessage);
    fn set_contact_save_status(&self, message: UiMessage);
    fn set_list_status(&self, message: UiMessage);
    fn set_render_status(&self, message: UiMessage);
    fn set_sync_status(&self, message: UiMessage);
    fn set_compose_notice(&self, message: UiMessage);
}

impl AppWindowMessages for AppWindow {
    fn set_startup_error(&self, message: UiMessage) {
        self.set_startup_message(message.as_slint());
    }

    fn set_contact_save_status(&self, message: UiMessage) {
        self.set_contact_save_message(message.as_slint());
    }

    fn set_list_status(&self, message: UiMessage) {
        self.set_list_message(message.as_slint());
    }

    fn set_render_status(&self, message: UiMessage) {
        self.set_render_message(message.as_slint());
    }

    fn set_sync_status(&self, message: UiMessage) {
        self.set_sync_message(message.as_slint());
    }

    fn set_compose_notice(&self, message: UiMessage) {
        self.set_compose_message(message.as_slint());
    }
}
