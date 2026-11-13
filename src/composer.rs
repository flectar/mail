//! Messages being written. Each composer is a session that owns its
//! document, attachments and send state. A message opens inside the main
//! window; on the desktop it can move into a window of its own, leaving the
//! main window free to read mail or start another message.

use super::*;
use crate::rich_compose::{EditMark, EditState};
use flectar_mail_core::models::{
    Address, AttachmentMeta, SaveDraftArgs, SenderIdentity, Signature,
};
use slint::SharedString;
#[cfg(desktop)]
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use std::rc::Weak;

mod message_spelling;
pub(crate) use message_spelling::spelling_changed;
use message_spelling::{MessageSpelling, apply_spelling_state};

thread_local! {
    static COMPOSERS: RefCell<Option<Rc<Composers>>> = const { RefCell::new(None) };
}

/// How long a message's own window may take to appear. Winit drops a window
/// it could not create and reports that only when the app ends.
#[cfg(desktop)]
const WINDOW_OPEN_TIMEOUT: Duration = Duration::from_secs(3);

/// What every composer shares: the main window, the mail state and the
/// user's templates.
pub(super) struct Composers {
    /// Messages can move into windows of their own, as on the desktop.
    pop_out: bool,
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
    documents: Arc<dyn documents::DocumentProvider>,
    templates: RefCell<Vec<Snippet>>,
    send_updates: UiSender<ComposeSendUpdate>,
    sessions: RefCell<Vec<Rc<ComposeSession>>>,
    /// Wakes the composers when a quoted message's image finished loading.
    quote_wake: UiWake,
    quote_poll_pending: Cell<bool>,
}

/// An original moved into the message's text by Edit.
struct EditedQuote {
    quote: ComposeQuote,
    expanded: bool,
    mark: EditMark,
}

/// One message being written.
pub(super) struct ComposeSession {
    /// The session itself, for timers and tasks that finish after a callback returns.
    this: Weak<ComposeSession>,
    composers: Weak<Composers>,
    host: RefCell<Host>,
    /// Bumped when the message moves to another window, so the callbacks
    /// left on the one it moved from no longer reach it.
    host_id: Cell<u64>,
    files: RefCell<Vec<ComposeFile>>,
    /// Bumped when the message is cleared, so a file picker or a download of
    /// the original's files that finishes later attaches to nothing.
    generation: Cell<u64>,
    pub(super) document: RefCell<RichComposeDocument>,
    pub(super) editor: RefCell<LazyComposeEditor>,
    contacts: RefCell<Vec<Address>>,
    pub(super) intent: RefCell<ComposeIntent>,
    /// The original a reply or forward carries below the editor.
    quote: RefCell<Option<ComposeQuote>>,
    /// Draws the original read-only while it is shown, as the reader does.
    quote_renderer: RefCell<Option<GpuEmailRenderer>>,
    /// The original could not be drawn, even as text; it is not tried again
    /// until it changes.
    quote_unavailable: Cell<bool>,
    /// An original the user left out, which they can still put back.
    left_out_quote: RefCell<Option<ComposeQuote>>,
    /// An original moved into the text, while undoing that brings it back.
    edited_quote: RefCell<Option<EditedQuote>>,
    send_action: Cell<Option<i64>>,
    picking: Cell<bool>,
    /// The signature block this composer inserted, while the user has not
    /// edited it, and the signatures of its sending account.
    signature: RefCell<Option<String>>,
    signatures: RefCell<Vec<Signature>>,
    /// Bumped for each sending-account change; older results are dropped.
    preferences_generation: Cell<u64>,
    spelling: MessageSpelling,
    /// Closed, and waiting for the event loop to let its window go.
    disposed: Cell<bool>,
}

enum Host {
    /// The composer inside the main window.
    Main(slint::Weak<AppWindow>),
    #[cfg(desktop)]
    Window(ComposerWindow),
}

/// A session, as a window's callbacks reach it while it is shown there.
struct SessionRef {
    session: Weak<ComposeSession>,
    host_id: u64,
}

impl SessionRef {
    fn upgrade(&self) -> Option<Rc<ComposeSession>> {
        self.session
            .upgrade()
            .filter(|session| session.host_id.get() == self.host_id)
    }
}

/// A composer's window, held for the duration of one update.
pub(super) enum HostRef {
    Main(AppWindow),
    #[cfg(desktop)]
    Window(ComposerWindow),
}

impl Host {
    fn get(&self) -> Option<HostRef> {
        match self {
            Host::Main(app) => app.upgrade().map(HostRef::Main),
            #[cfg(desktop)]
            Host::Window(window) => Some(HostRef::Window(window.clone_strong())),
        }
    }
}

impl HostRef {
    pub(super) fn ui(&self) -> ComposerUi<'_> {
        match self {
            HostRef::Main(app) => app.global::<ComposerUi>(),
            #[cfg(desktop)]
            HostRef::Window(window) => window.global::<ComposerUi>(),
        }
    }

    pub(super) fn preferences(&self) -> AccountMailPreferences<'_> {
        match self {
            HostRef::Main(app) => app.global::<AccountMailPreferences>(),
            #[cfg(desktop)]
            HostRef::Window(window) => window.global::<AccountMailPreferences>(),
        }
    }

    fn window(&self) -> &slint::Window {
        match self {
            HostRef::Main(app) => app.window(),
            #[cfg(desktop)]
            HostRef::Window(window) => window.window(),
        }
    }

    pub(super) fn view(&self) -> ComposeView<'_> {
        ComposeView {
            ui: self.ui(),
            window: self.window(),
            suspended: match self {
                HostRef::Main(app) => app.get_render_suspended(),
                #[cfg(desktop)]
                HostRef::Window(window) => window.get_render_suspended(),
            },
        }
    }
}

/// Connect composing to the main window. `pop_out` lets messages move into
/// windows of their own, as on the desktop. Mail links and the window
/// lifecycle reach the composers through [`with_composers`].
pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    documents: Arc<dyn documents::DocumentProvider>,
    pop_out: bool,
) -> Rc<Composers> {
    let (send_tx, send_rx) = bounded_ui_channel::<ComposeSendUpdate>();
    let composers = Rc::new(Composers {
        pop_out,
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
        documents,
        templates: RefCell::default(),
        send_updates: UiSender::new(
            send_tx,
            UiWake::new(app.as_weak(), |app| app.invoke_drain_compose_send_updates()),
        ),
        sessions: RefCell::default(),
        quote_wake: UiWake::new(app.as_weak(), |app| app.invoke_refresh_composer_quotes()),
        quote_poll_pending: Cell::new(false),
    });
    COMPOSERS.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&composers)));

    let weak = Rc::downgrade(&composers);
    app.on_open_compose(move |to| {
        if let Some(composers) = weak.upgrade()
            && let Some(app) = composers.app.upgrade()
        {
            composers.open_new(&app, to.as_str());
        }
    });
    let weak = Rc::downgrade(&composers);
    app.on_compose_message_action(move |action| {
        if let Some(composers) = weak.upgrade()
            && let Some(app) = composers.app.upgrade()
        {
            composers.open_from_message(&app, action.as_str());
        }
    });
    let weak = Rc::downgrade(&composers);
    app.on_refresh_composer_quotes(move || {
        let Some(composers) = weak.upgrade() else {
            return;
        };
        if composers.quote_poll_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(&composers);
        // Coalesce a burst of decoded images into one repaint.
        Timer::single_shot(Duration::from_millis(50), move || {
            let Some(composers) = weak.upgrade() else {
                return;
            };
            composers.quote_poll_pending.set(false);
            let sessions = composers.sessions.borrow().clone();
            for session in sessions {
                session.poll_quote();
            }
        });
    });
    let weak = Rc::downgrade(&composers);
    let send_rx = RefCell::new(send_rx);
    app.on_drain_compose_send_updates(move || {
        let Some(composers) = weak.upgrade() else {
            return;
        };
        while let Ok(update) = send_rx.borrow_mut().try_recv() {
            composers.send_finished(update);
        }
    });
    #[cfg(desktop)]
    {
        let weak = Rc::downgrade(&composers);
        app.on_appearance_changed(move || {
            if let Some(composers) = weak.upgrade()
                && let Some(app) = composers.app.upgrade()
            {
                for session in composers.sessions.borrow().iter() {
                    if let Host::Window(window) = &*session.host.borrow() {
                        copy_appearance(&app, window);
                    }
                }
            }
        });
    }
    composers
}

/// The composers, once installed.
pub(super) fn with_composers<R>(f: impl FnOnce(&Rc<Composers>) -> R) -> Option<R> {
    COMPOSERS.with(|slot| slot.borrow().as_ref().map(f))
}

/// Composer windows still open on the desktop.
pub(super) fn open_windows() -> usize {
    with_composers(|composers| {
        composers
            .sessions
            .borrow()
            .iter()
            .filter(|session| !session.disposed.get() && !session.is_in_app())
            .count()
    })
    .unwrap_or_default()
}

/// Composers list the accounts the main window lists. A message whose
/// account was removed moves to the one new mail starts from.
pub(super) fn accounts_changed(app: &AppWindow) {
    with_composers(|composers| {
        let sessions = composers.sessions.borrow().clone();
        for session in sessions {
            let Some(host) = session.host() else {
                continue;
            };
            let ui = host.ui();
            let accounts = app.get_connected_accounts();
            let kept = (0..accounts.row_count())
                .filter_map(|index| accounts.row_data(index))
                .any(|account| account.id == ui.get_account_id());
            ui.set_accounts(accounts);
            if kept || session.disposed.get() {
                continue;
            }
            let account_id = app.get_compose_account_id();
            ui.set_account_id(account_id);
            ui.set_from_label(app.get_compose_from_label());
            if account_id > 0 {
                host.preferences()
                    .invoke_composer_account_changed(account_id);
            } else {
                session.reset_preferences();
            }
        }
    });
}

/// Open the draft `draft_id` again, as when its sending was taken back.
pub(super) fn reopen_draft(draft_id: i64) {
    with_composers(|composers| {
        let (Some(app), Some(core)) = (composers.app.upgrade(), composers.core()) else {
            return;
        };
        match composers.runtime.block_on(core.get_draft(draft_id)) {
            Ok(draft) => composers.show_draft(&app, draft),
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not open draft: {}", error));
            }
        }
    });
}

/// Open a new message already written, for the user to look over and send,
/// such as an emailed unsubscribe request (RFC 2369 appendix B.2.4).
pub(super) fn compose_written(message: SaveDraftArgs) {
    with_composers(|composers| {
        if let Some(app) = composers.app.upgrade() {
            composers.show_written(&app, message, "new");
        }
    });
}

/// The application is ending. A message someone was writing, in the main
/// window or one of its own, goes to Drafts: nothing else would keep it.
pub(super) fn save_open_messages() {
    with_composers(|composers| {
        let sessions = composers.sessions.borrow().clone();
        for session in sessions {
            if session.disposed.get() || session.send_action.get().is_some() {
                continue;
            }
            let Some(host) = session.host() else {
                continue;
            };
            let ui = host.ui();
            let body = ui.get_body();
            if session.only_inserted_signature(&body) {
                continue;
            }
            session.save(
                ui.get_account_id(),
                &ui.get_to(),
                &ui.get_cc(),
                &ui.get_bcc(),
                &ui.get_subject(),
                &body,
                false,
            );
        }
    });
}

/// The message colors changed, with the theme or the reader's choice: an
/// original already drawn below a composer takes them on too.
pub(super) fn message_colors_changed(app: &AppWindow) {
    let colors = crate::message_appearance::EmailAppearance::from_app(app);
    with_composers(|composers| {
        let sessions = composers.sessions.borrow().clone();
        for session in sessions {
            let changed = session
                .quote_renderer
                .borrow_mut()
                .as_mut()
                .is_some_and(|renderer| renderer.set_appearance(colors));
            if changed && let Some(host) = session.host() {
                session.render_quote(&host);
            }
        }
    });
}

/// The main window stops or resumes painting. Its own composer lets go of
/// its pixels and fonts while hidden.
pub(super) fn main_window_suspended(suspended: bool) {
    with_composers(|composers| {
        for session in composers.sessions.borrow().iter() {
            if !session.is_in_app() {
                continue;
            }
            let Some(host) = session.host() else {
                continue;
            };
            if suspended {
                host.ui().set_editor_tiles(ModelRc::default());
                session.editor.borrow_mut().release();
                // A hidden window keeps no quoted pixels either.
                session.render_quote(&host);
            } else if host.ui().get_open() {
                session.apply_document(&host);
            }
        }
    });
}

#[cfg(desktop)]
fn new_window(app: &AppWindow) -> Option<Host> {
    let window = match crate::window_chrome::composer_window(ComposerWindow::new) {
        Ok(window) => window,
        Err(error) => {
            app.set_render_status(UiMessage::detail("Could not open composer: {}", error));
            return None;
        }
    };
    copy_appearance(app, &window);
    window.set_attachment_picker_supported(app.get_document_apis_supported());
    let weak = window.as_weak();
    window.window().on_close_requested(move || {
        if let Some(window) = weak.upgrade() {
            window.invoke_request_close();
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    Some(Host::Window(window))
}

/// Phones keep every message in the main window: Slint's mobile backends
/// have one window.
#[cfg(not(desktop))]
fn new_window(_app: &AppWindow) -> Option<Host> {
    None
}

/// Everything a composer shows, for a message moving to another window.
fn copy_composer(from: &ComposerUi<'_>, to: &ComposerUi<'_>) {
    to.set_mode(from.get_mode());
    to.set_to(from.get_to());
    to.set_cc(from.get_cc());
    to.set_bcc(from.get_bcc());
    to.set_subject(from.get_subject());
    to.set_body(from.get_body());
    to.set_sending(from.get_sending());
    to.set_accounts(from.get_accounts());
    to.set_account_id(from.get_account_id());
    to.set_from_label(from.get_from_label());
    to.set_attachments(from.get_attachments());
    to.set_attachment_summary(from.get_attachment_summary());
    to.set_templates(from.get_templates());
    to.set_message(from.get_message());
    to.set_notice_is_error(from.get_notice_is_error());
    to.set_selection_start(from.get_selection_start());
    to.set_selection_end(from.get_selection_end());
    to.set_bold_active(from.get_bold_active());
    to.set_italic_active(from.get_italic_active());
    to.set_underline_active(from.get_underline_active());
    to.set_strike_active(from.get_strike_active());
    to.set_code_active(from.get_code_active());
    to.set_active_link_url(from.get_active_link_url());
    to.set_active_color(from.get_active_color());
    to.set_active_highlight(from.get_active_highlight());
    to.set_active_size(from.get_active_size());
    to.set_active_family(from.get_active_family());
    to.set_active_align(from.get_active_align());
    to.set_active_image_size(from.get_active_image_size());
    to.set_active_image_alt(from.get_active_image_alt());
    to.set_active_image_width(from.get_active_image_width());
    to.set_active_image_shown_width(from.get_active_image_shown_width());
    to.set_quote(from.get_quote());
    to.set_quote_left_out(from.get_quote_left_out());
    to.set_quote_text(from.get_quote_text());
    to.set_quote_expanded(from.get_quote_expanded());
    to.set_quote_height(from.get_quote_height());
}

/// The sender and signature a message chose, for it moving to another window.
fn copy_preferences(from: &AccountMailPreferences<'_>, to: &AccountMailPreferences<'_>) {
    to.set_composer_sender_identities(from.get_composer_sender_identities());
    to.set_composer_sender_names(from.get_composer_sender_names());
    to.set_composer_sender_index(from.get_composer_sender_index());
    to.set_composer_sender_email(from.get_composer_sender_email());
    to.set_composer_signature_names(from.get_composer_signature_names());
    to.set_composer_signature_index(from.get_composer_signature_index());
    to.set_composer_security(from.get_composer_security());
    to.set_composer_preferences_ready(from.get_composer_preferences_ready());
}

#[cfg(desktop)]
fn copy_appearance(app: &AppWindow, window: &ComposerWindow) {
    let from = app.global::<AppTheme>();
    let to = window.global::<AppTheme>();
    to.set_preset(from.get_preset());
    to.set_custom_light_primary(from.get_custom_light_primary());
    to.set_custom_light_page_bg(from.get_custom_light_page_bg());
    to.set_custom_light_surface(from.get_custom_light_surface());
    to.set_custom_light_text(from.get_custom_light_text());
    to.set_custom_light_border(from.get_custom_light_border());
    to.set_custom_dark_primary(from.get_custom_dark_primary());
    to.set_custom_dark_page_bg(from.get_custom_dark_page_bg());
    to.set_custom_dark_surface(from.get_custom_dark_surface());
    to.set_custom_dark_text(from.get_custom_dark_text());
    to.set_custom_dark_border(from.get_custom_dark_border());
    window
        .global::<AppIcon>()
        .set_variant(app.global::<AppIcon>().get_variant());
    window.set_theme(app.get_theme_mode());
}

impl Composers {
    fn core(&self) -> Option<CoreMailSource> {
        self.state.borrow().core.clone()
    }

    /// The message the main window's composer shows.
    pub(super) fn in_app(&self) -> Option<Rc<ComposeSession>> {
        self.sessions
            .borrow()
            .iter()
            .find(|session| !session.disposed.get() && session.is_in_app())
            .cloned()
    }

    /// The open composer editing the saved draft `draft_id`.
    fn holding_draft(&self, draft_id: i64) -> Option<Rc<ComposeSession>> {
        self.sessions
            .borrow()
            .iter()
            .find(|session| {
                !session.disposed.get() && session.intent.borrow().draft_id == Some(draft_id)
            })
            .cloned()
    }

    /// Whether any account can send a message.
    pub(super) fn can_send(&self) -> bool {
        !self.state.borrow().connected_accounts.is_empty()
    }

    /// The stored message a reply link answers, by its Message-ID.
    pub(super) fn message_by_header_id(&self, account_id: i64, header_id: &str) -> Option<i64> {
        let core = self.core()?;
        self.runtime
            .block_on(core.message_by_header_id(account_id, header_id))
            .ok()
            .flatten()
    }

    /// A blank composer in the main window, which replaces whatever the
    /// main window's composer held.
    fn open(self: &Rc<Self>, app: &AppWindow) -> Option<Rc<ComposeSession>> {
        if let Some(previous) = self.in_app() {
            // A message being sent keeps its composer until the send ends, so
            // the result still reaches it.
            if previous.send_action.get().is_some() {
                app.set_render_status(UiMessage::plain("Sending message…"));
                return None;
            }
            previous.dispose();
        }
        let session = Rc::new_cyclic(|this| ComposeSession {
            this: this.clone(),
            composers: Rc::downgrade(self),
            host: RefCell::new(Host::Main(app.as_weak())),
            host_id: Cell::new(0),
            files: RefCell::default(),
            generation: Cell::new(0),
            document: RefCell::default(),
            editor: RefCell::default(),
            contacts: RefCell::default(),
            intent: RefCell::default(),
            quote: RefCell::default(),
            quote_renderer: RefCell::default(),
            quote_unavailable: Cell::new(false),
            left_out_quote: RefCell::default(),
            edited_quote: RefCell::default(),
            send_action: Cell::new(None),
            picking: Cell::new(false),
            signature: RefCell::default(),
            signatures: RefCell::default(),
            preferences_generation: Cell::new(0),
            spelling: MessageSpelling::default(),
            disposed: Cell::new(false),
        });
        session.connect();
        // What's written starts in the default text style from Settings.
        session.document.borrow_mut().set_base_style(
            crate::rich_compose::FontFamily::from_name(&app.get_compose_font()).unwrap_or_default(),
            crate::rich_compose::TextSize::from_name(&app.get_compose_text_size())
                .unwrap_or_default(),
        );
        let host = session.host()?;
        let ui = host.ui();
        ui.set_can_pop_out(self.pop_out);
        ui.set_accounts(app.get_connected_accounts());
        ui.set_account_id(app.get_compose_account_id());
        ui.set_from_label(app.get_compose_from_label());
        apply_email_templates(&ui, &self.templates.borrow());
        session.clear_fields(&host);
        self.sessions.borrow_mut().push(Rc::clone(&session));
        Some(session)
    }

    /// A new message to `to`, which may be empty. A message the main window
    /// already shows stays.
    pub(super) fn open_new(
        self: &Rc<Self>,
        app: &AppWindow,
        to: &str,
    ) -> Option<Rc<ComposeSession>> {
        if let Some(open) = self.in_app() {
            return Some(open);
        }
        // Only local mail is here: nothing could send it.
        if self.state.borrow().connected_accounts.is_empty() {
            app.set_render_status(UiMessage::plain("Add an account to send mail."));
            return None;
        }
        let session = self.open(app)?;
        if let Some(host) = session.host() {
            host.ui().set_to(to.into());
        }
        session.show("new", None);
        Some(session)
    }

    /// Reply, reply all, forward, edit as new, or edit the selected draft.
    fn open_from_message(self: &Rc<Self>, app: &AppWindow, action: &str) {
        let (core, row, conversation_message_id) = {
            let state = self.state.borrow();
            let row = state
                .selected_id
                .and_then(|id| state.messages.iter().find(|message| message.id == id))
                .cloned();
            let conversation_message_id = row.as_ref().and_then(|row| {
                (state.conversation_owner_id == Some(row.id))
                    .then(|| {
                        state
                            .conversation_messages
                            .get(state.conversation_selected_index)
                            .map(|message| i64::from(message.id))
                    })
                    .flatten()
            });
            (state.core.clone(), row, conversation_message_id)
        };
        let Some(core) = core else {
            app.set_render_status(UiMessage::plain(
                "Connect an account before replying or forwarding.",
            ));
            return;
        };
        let Some(row) = row else {
            app.set_render_status(UiMessage::plain("Choose a message first."));
            return;
        };

        if action == "edit_draft" {
            self.open_draft(app, &core, &row);
            return;
        }

        // Local mail is answered from an account.
        if self.state.borrow().connected_accounts.is_empty() {
            app.set_render_status(UiMessage::plain("Add an account to send mail."));
            return;
        }
        let source = match self
            .runtime
            .block_on(core.load_compose_source(&row, conversation_message_id))
        {
            Ok(source) => source,
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not open composer: {}", error));
                return;
            }
        };
        let account_name = self
            .state
            .borrow()
            .connected_accounts
            .iter()
            .find(|account| account.id == source.account_id)
            .map(|account| {
                account
                    .display_name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| account.email.clone())
            });
        let prepared = match prepare_message_compose(
            &source,
            action,
            app.get_selected_plain_text().as_str(),
            |message| translated(app, &message).into(),
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not open composer: {}", error));
                return;
            }
        };
        let Ok(account_id) = i32::try_from(prepared.account_id) else {
            app.set_render_status(UiMessage::plain(
                "Could not open composer: invalid account identifier",
            ));
            return;
        };

        let Some(session) = self.open(app) else {
            return;
        };
        let Some(host) = session.host() else {
            return;
        };
        let ui = host.ui();
        ui.set_account_id(account_id);
        let display_name = account_name.unwrap_or_else(|| source.account_email.clone());
        ui.set_from_label(if display_name == source.account_email {
            source.account_email.clone().into()
        } else {
            format!("{display_name}  <{}>", source.account_email).into()
        });
        ui.set_to(prepared.to.into());
        ui.set_cc(prepared.cc.into());
        ui.set_bcc("".into());
        ui.set_subject(prepared.subject.into());
        let quote = {
            let mut document = session.document.borrow_mut();
            // A message edited as new keeps the formatting, and the original
            // a reply quoted, that it was sent with.
            let saved = prepared.saved.as_ref();
            let split = saved.and_then(|saved| {
                ComposeQuote::split_saved(&saved.html, &saved.text, |own| {
                    if own.trim().is_empty() {
                        document.load("", 0, 0);
                        return Some(0);
                    }
                    document.load_html_start(own, &saved.text)
                })
            });
            if split.is_none()
                && saved
                    .and_then(|saved| document.load_html(&saved.html, &saved.text))
                    .is_none()
            {
                document.load(&prepared.body, 0, 0);
            }
            split
        };
        session.set_quote(&ui, prepared.quote.or(quote));
        *session.intent.borrow_mut() = prepared.intent;
        session.show(action, Some(prepared.sender_email));
        // A forward, and a message edited as new, carry the original's files.
        if matches!(action, "forward" | "edit_as_new") {
            session.attach_original_files(&core, source.attachments);
        }
    }

    fn open_draft(self: &Rc<Self>, app: &AppWindow, core: &CoreMailSource, row: &MailMessage) {
        if row.folder != "Drafts" && row.label != "DRAFT" {
            app.set_render_status(UiMessage::plain("Only drafts can be reopened for editing."));
            return;
        }
        match self.runtime.block_on(core.load_draft(row)) {
            Ok(draft) => self.show_draft(app, draft),
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not open draft: {}", error));
            }
        }
    }

    /// Open a saved draft in a composer.
    fn show_draft(self: &Rc<Self>, app: &AppWindow, draft: SaveDraftArgs) {
        self.show_written(app, draft, "draft");
    }

    /// Open a written message in a composer: a saved draft (`mode` "draft"),
    /// or a new message the user has yet to look over ("new").
    fn show_written(self: &Rc<Self>, app: &AppWindow, draft: SaveDraftArgs, mode: &str) {
        // A draft already open in a composer comes forward instead of opening
        // twice, where each copy's saves would overwrite the other's.
        if let Some(open) = draft.draft_id.and_then(|id| self.holding_draft(id)) {
            open.bring_forward();
            return;
        }
        let Ok(account_id) = i32::try_from(draft.account_id) else {
            app.set_render_status(UiMessage::plain(
                "Could not open draft: invalid account identifier",
            ));
            return;
        };
        let account_label = self
            .state
            .borrow()
            .connected_accounts
            .iter()
            .find(|account| account.id == draft.account_id)
            .map(|account| {
                let name = account
                    .display_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty());
                match name {
                    Some(name) if name != account.email => {
                        format!("{name}  <{}>", account.email)
                    }
                    _ => account.email.clone(),
                }
            })
            .unwrap_or_else(|| translated(app, &UiMessage::plain("Connected account")).to_string());
        let draft_sender_email = draft
            .from
            .as_ref()
            .map(|address| address.email.clone())
            .unwrap_or_default();

        let Some(session) = self.open(app) else {
            return;
        };
        let Some(host) = session.host() else {
            return;
        };
        let ui = host.ui();
        ui.set_account_id(account_id);
        ui.set_from_label(account_label.into());
        ui.set_to(compose_addresses(&draft.to).into());
        ui.set_cc(compose_addresses(&draft.cc).into());
        ui.set_bcc(compose_addresses(&draft.bcc).into());
        ui.set_subject(draft.subject.clone().into());

        *session.files.borrow_mut() = draft
            .attachments
            .iter()
            .map(|attachment| {
                let path = PathBuf::from(&attachment.file_path);
                let size = std::fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or_default();
                ComposeFile {
                    path,
                    filename: attachment.filename.clone(),
                    size,
                    _import: None,
                }
            })
            .collect();
        apply_compose_files(&ui, &session.files.borrow());
        let quote = {
            let mut document = session.document.borrow_mut();
            let html = draft.body_html.as_deref().unwrap_or_default();
            // The HTML keeps the draft's formatting and the original it
            // quotes; HTML that holds other text than the draft's opens as
            // the plain text.
            let split = ComposeQuote::split_saved(html, &draft.body_text, |own| {
                if own.trim().is_empty() {
                    document.load("", 0, 0);
                    return Some(0);
                }
                document.load_html_start(own, &draft.body_text)
            });
            if split.is_none() && document.load_html(html, &draft.body_text).is_none() {
                // Images the HTML no longer holds leave no mark in the text.
                let text = draft
                    .body_text
                    .replace(crate::rich_compose::IMAGE_PLACEHOLDER, "");
                let cursor = i32::try_from(text.len()).unwrap_or(i32::MAX);
                document.load(&text, cursor, cursor);
            }
            split
        };
        session.set_quote(&ui, quote);
        *session.intent.borrow_mut() = ComposeIntent {
            mode: draft.mode,
            in_reply_to_message_id: draft.in_reply_to_message_id,
            in_reply_to_header: draft.in_reply_to_header,
            draft_id: draft.draft_id,
        };
        session.show(mode, Some(draft_sender_email));
    }

    /// A send reached its end: a sent message closes its composer, a failed
    /// one stays open with its draft for another try.
    fn send_finished(&self, update: ComposeSendUpdate) {
        let Some(session) = self
            .sessions
            .borrow()
            .iter()
            .find(|session| session.send_action.get() == Some(update.action_id))
            .cloned()
        else {
            return;
        };
        let Some(app) = self.app.upgrade() else {
            return;
        };
        session.send_action.set(None);
        if let Some(host) = session.host() {
            host.ui().set_sending(false);
        }
        match update.result {
            Ok(()) => {
                session.dispose();
                app.set_sync_status(UiMessage::plain("Message sent."));
                let _ = refresh_from_source(&app, &self.state, &self.runtime, true, &[]);
                mailto_links::open_pending_soon(&app);
            }
            Err(error) => {
                let message =
                    UiMessage::detail("Message was not sent: {} Your draft is preserved.", error);
                if let Some(host) = session.host() {
                    host.ui().set_notice(message.clone(), true);
                }
                app.set_sync_status(message);
            }
        }
    }

    /// Show the user's templates in every composer.
    fn apply_templates(&self) {
        let templates = self.templates.borrow();
        for session in self.sessions.borrow().iter() {
            if let Some(host) = session.host() {
                apply_email_templates(&host.ui(), &templates);
            }
        }
    }

    fn sort_templates(&self) {
        self.templates.borrow_mut().sort_by(|left, right| {
            right
                .usage_count
                .cmp(&left.usage_count)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
    }
}

impl ComposeSession {
    pub(super) fn host(&self) -> Option<HostRef> {
        self.host.borrow().get()
    }

    fn is_in_app(&self) -> bool {
        matches!(*self.host.borrow(), Host::Main(_))
    }

    /// Put this message in front: its own window raised and focused. The
    /// main window's composer is a dialog, already in front of the mail.
    fn bring_forward(&self) {
        #[cfg(desktop)]
        if let Some(HostRef::Window(window)) = self.host() {
            let native = window.window();
            if native.is_minimized() {
                native.set_minimized(false);
            }
            let weak = window.as_weak();
            let _ = slint::spawn_local(async move {
                if let Some(window) = weak.upgrade()
                    && let Ok(native) = window.window().winit_window().await
                {
                    native.focus_window();
                }
            });
        }
    }

    /// How a window's callbacks reach this session while it is shown there.
    fn reference(self: &Rc<Self>) -> SessionRef {
        SessionRef {
            session: Rc::downgrade(self),
            host_id: self.host_id.get(),
        }
    }

    fn composers(&self) -> Option<Rc<Composers>> {
        self.composers.upgrade()
    }

    /// Fill the editor and the formatting state from the document.
    fn apply_document(&self, host: &HostRef) {
        {
            let document = self.document.borrow();
            let mut editor = self.editor.borrow_mut();
            self.underline_found(&document, &mut editor);
            apply_rich_compose(&host.view(), &document, document.selection(), &mut editor);
        }
        self.render_quote(host);
        self.check_spelling_soon();
    }

    /// Draw the visible part of the original below the editor, while it is
    /// shown. A collapsed or removed original keeps no layout or pixels.
    fn render_quote(&self, host: &HostRef) {
        let view = host.view();
        let ui = &view.ui;
        let quote = self.quote.borrow();
        let Some(quote) = quote
            .as_ref()
            .filter(|_| ui.get_quote_expanded() && !view.suspended)
        else {
            if self.quote_renderer.borrow_mut().take().is_some() {
                ui.set_quote_tiles(ModelRc::default());
            }
            return;
        };
        // A window that hasn't laid its composer out yet would have the quote
        // laid out at a pixel's width, only to do it again.
        if self.quote_unavailable.get()
            || ui.get_editor_width() <= 1.0
            || ui.get_editor_viewport_height() <= 1.0
        {
            return;
        }
        let Some(composers) = self.composers() else {
            return;
        };
        let Some(app) = composers.app.upgrade() else {
            return;
        };
        let width = ui.get_editor_width().ceil();
        let viewport = ui.get_editor_viewport_height().ceil();
        let scale = view.window.scale_factor();
        let mut slot = self.quote_renderer.borrow_mut();
        if slot.is_none() {
            let mut renderer = GpuEmailRenderer::default();
            renderer.set_appearance(crate::message_appearance::EmailAppearance::from_app(&app));
            let wake = composers.quote_wake.clone();
            renderer.set_resource_notifier(Arc::new(move || wake.wake()));
            // Remote images stay off: embedded ones are part of the quote.
            if let Err(error) = renderer.configure_resources(composers.runtime.clone(), false) {
                tracing::warn!(%error, "quoted message images unavailable");
            }
            // In physical pixels, as the reader prepares a message.
            renderer.preparation_viewport = (
                (width * scale).ceil() as u32,
                (viewport * scale).ceil() as u32,
                scale,
            );
            // Line the quote up with the reply above it, and give unstyled
            // text such as the attribution line the reply's size rather than
            // the reader's 16px default.
            let html = format!(
                "<body style=\"margin:8px 0;font-size:{}px\">{}</body>",
                crate::compose_editor::FONT_SIZE_LOGICAL,
                quote.html
            );
            // An original the renderer can't lay out shows as its text, as
            // the reader shows such a message; what is sent stays the same.
            let prepared = renderer.prepare_email_html(&html, false).or_else(|error| {
                tracing::warn!(%error, "quoted message shown as text");
                let mut text = format!(
                    "<body style=\"margin:8px 0;font-size:{}px\"><div style=\"white-space:pre-wrap\">",
                    crate::compose_editor::FONT_SIZE_LOGICAL
                );
                crate::email_document::append_escaped(&mut text, &quote.plain);
                text.push_str("</div></body>");
                renderer.prepare_email_html(&text, false)
            });
            match prepared {
                Ok(prepared) => {
                    renderer.set_auto_fit(true);
                    renderer.set_email(prepared);
                }
                Err(error) => {
                    self.quote_unavailable.set(true);
                    ui.set_notice(
                        UiMessage::detail("Could not show the quoted message: {}", error),
                        true,
                    );
                    return;
                }
            }
            *slot = Some(renderer);
        }
        let Some(renderer) = slot.as_mut() else {
            return;
        };
        // The quote shows at the editor's width. Content laid out wider, even
        // zoomed out, is scaled down to it whole rather than squeezed
        // sideways, so the part in view reaches further down it.
        let fit = width / renderer.layout_width.max(width);
        renderer.set_visible_region(
            ((ui.get_editor_scroll_y() - ui.get_quote_top()) / fit).max(0.0),
            viewport / fit,
        );
        match renderer.render_cpu_if_needed(width as u32, viewport as u32, scale) {
            Ok(Some(rendered)) => {
                // Tiles come normalized to the laid-out width.
                let tiles = rendered
                    .tiles
                    .into_iter()
                    .map(|tile| ComposeEditorTile {
                        image: tile.image,
                        y: tile.y * width,
                        height: tile.height * width,
                    })
                    .collect::<Vec<_>>();
                let retained = ui.get_quote_tiles();
                if let Some(model) = retained
                    .as_any()
                    .downcast_ref::<VecModel<ComposeEditorTile>>()
                {
                    crate::reconcile_model_rows_by(
                        model,
                        tiles,
                        |tile| tile.y.to_bits(),
                        PartialEq::eq,
                    );
                } else {
                    ui.set_quote_tiles(ModelRc::new(VecModel::from(tiles)));
                }
                ui.set_quote_height(width * rendered.height as f32 / rendered.width.max(1) as f32);
            }
            Ok(None) => {}
            Err(error) => ui.set_notice(
                UiMessage::detail("Could not show the quoted message: {}", error),
                true,
            ),
        }
    }

    /// Fold in images the quoted message finished loading.
    fn poll_quote(&self) {
        let polled = self
            .quote_renderer
            .borrow_mut()
            .as_mut()
            .is_some_and(GpuEmailRenderer::poll_resources);
        if polled && let Some(host) = self.host() {
            self.render_quote(&host);
        }
    }

    /// Empty fields; the window's account and template list stay.
    fn clear_fields(&self, host: &HostRef) {
        let ui = host.ui();
        host.preferences().invoke_composer_reset();
        ui.set_editor_tiles(ModelRc::default());
        apply_compose_files(&ui, &[]);
        apply_compose_contacts(&ui, &[]);
        ui.set_sending(false);
        ui.set_mode("new".into());
        ui.set_to("".into());
        ui.set_cc("".into());
        ui.set_bcc("".into());
        ui.set_subject("".into());
        ui.set_body("".into());
        ui.set_quote("".into());
        ui.set_quote_left_out("".into());
        ui.set_quote_text("".into());
        ui.set_quote_expanded(false);
        ui.set_quote_tiles(ModelRc::default());
        ui.set_quote_height(0.0);
        ui.set_notice(UiMessage::EMPTY, false);
        apply_rich_compose_state(&ui, &self.document.borrow(), ComposeSelection::default());
    }

    /// Open the prepared message, choosing its sender once the account's
    /// identities are loaded.
    fn show(&self, mode: &str, sender_email: Option<String>) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        ui.set_mode(mode.into());
        let preferences = host.preferences();
        preferences.invoke_composer_reset();
        if let Some(sender_email) = sender_email {
            preferences.set_composer_sender_email(sender_email.into());
        }
        preferences.invoke_composer_account_changed(ui.get_account_id());
        self.editor.borrow_mut().reset();
        {
            let document = self.document.borrow();
            ui.set_body(document.text().into());
            apply_rich_compose_state(&ui, &document, document.selection());
        }
        // Opening last focuses the first field and lays the editor out.
        ui.set_open(true);
        self.apply_document(&host);
    }

    /// Move this message from the main window into a window of its own.
    /// It keeps everything it holds, its chosen sender and signature
    /// included, and the main window's composer is free for another.
    fn pop_out(self: &Rc<Self>) {
        let Some(composers) = self.composers() else {
            return;
        };
        if !composers.pop_out || !self.is_in_app() || self.send_action.get().is_some() {
            return;
        }
        let (Some(app), Some(main)) = (composers.app.upgrade(), self.host()) else {
            return;
        };
        let Some(window_host) = new_window(&app) else {
            return;
        };
        let Some(window) = window_host.get() else {
            return;
        };
        copy_composer(&main.ui(), &window.ui());
        copy_preferences(&main.preferences(), &window.preferences());
        window.ui().set_can_pop_out(false);
        self.host_id.set(self.host_id.get().wrapping_add(1));
        *self.host.borrow_mut() = window_host;
        self.connect();
        #[cfg(desktop)]
        if let HostRef::Window(handle) = &window {
            self.watch_window(handle);
        }
        self.clear_fields(&main);
        main.ui().set_open(false);

        #[cfg(desktop)]
        if let HostRef::Window(handle) = &window
            && let Err(error) = handle.show()
        {
            window.ui().set_notice(
                UiMessage::detail("Could not open composer: {}", error),
                true,
            );
        }
        // A window that never appears would keep the message out of reach.
        #[cfg(desktop)]
        if app.window().has_winit_window() {
            let session = self.reference();
            Timer::single_shot(WINDOW_OPEN_TIMEOUT, move || {
                if let Some(session) = session.upgrade() {
                    session.window_never_opened();
                }
            });
        }
        // The surfaces draw again at the new window's scale factor.
        self.editor.borrow_mut().reset();
        *self.quote_renderer.borrow_mut() = None;
        window.ui().set_open(true);
        self.apply_document(&window);
    }

    /// The message's own window never appeared. The message comes back to
    /// the main window, or goes to Drafts while another message is open there.
    #[cfg(desktop)]
    fn window_never_opened(self: &Rc<Self>) {
        let Some(composers) = self.composers() else {
            return;
        };
        let (Some(app), Some(HostRef::Window(handle))) = (composers.app.upgrade(), self.host())
        else {
            return;
        };
        if self.disposed.get() || handle.window().has_winit_window() {
            return;
        }
        // One that is only late stays closed rather than opening empty.
        let _ = handle.hide();
        let window = HostRef::Window(handle);
        if composers.in_app().is_some() {
            let ui = window.ui();
            self.save(
                ui.get_account_id(),
                &ui.get_to(),
                &ui.get_cc(),
                &ui.get_bcc(),
                &ui.get_subject(),
                &ui.get_body(),
                false,
            );
            app.set_sync_status(if self.disposed.get() {
                UiMessage::plain(
                    "Could not open the message in its own window, so it was saved to Drafts.",
                )
            } else {
                UiMessage::plain("Could not open the message in its own window.")
            });
            return;
        }
        let main = HostRef::Main(app.clone_strong());
        copy_composer(&window.ui(), &main.ui());
        copy_preferences(&window.preferences(), &main.preferences());
        self.host_id.set(self.host_id.get().wrapping_add(1));
        *self.host.borrow_mut() = Host::Main(app.as_weak());
        self.connect();
        self.editor.borrow_mut().reset();
        *self.quote_renderer.borrow_mut() = None;
        main.ui().set_open(true);
        self.apply_document(&main);
        main.ui().set_notice(
            UiMessage::plain("Could not open the message in its own window."),
            true,
        );
        // A main window closed meanwhile may now end the app, which saves
        // the message to Drafts.
        window_controller::composer_closed();
    }

    /// A message's own window returns the memory input leaves behind once
    /// it goes still, as the main window does, keeps no pixels while it is
    /// minimized, and quits the app for Control+Q as the main window does.
    #[cfg(desktop)]
    fn watch_window(self: &Rc<Self>, window: &ComposerWindow) {
        let session = self.reference();
        #[cfg(linux_desktop)]
        let modifiers = std::cell::Cell::new(winit::keyboard::ModifiersState::empty());
        window.window().on_winit_window_event(move |native, event| {
            use winit::event::WindowEvent;
            if !matches!(event, WindowEvent::RedrawRequested) {
                crate::memory::release_free_heap_when_idle();
            }
            #[cfg(linux_desktop)]
            match event {
                WindowEvent::ModifiersChanged(state) => modifiers.set(state.state()),
                WindowEvent::KeyboardInput { event, .. } => {
                    let app = session
                        .upgrade()
                        .and_then(|session| session.composers())
                        .and_then(|composers| composers.app.upgrade());
                    if let Some(app) = app
                        && crate::window_chrome::quit_key(&app, event, modifiers.get())
                    {
                        return EventResult::PreventDefault;
                    }
                }
                _ => {}
            }
            if matches!(
                event,
                WindowEvent::Resized(_) | WindowEvent::Focused(_) | WindowEvent::Occluded(_)
            ) && let Some(session) = session.upgrade()
            {
                let minimized = native
                    .with_winit_window(|window| window.is_minimized())
                    .flatten()
                    .unwrap_or(false);
                session.window_minimized(minimized);
            }
            EventResult::Propagate
        });
    }

    /// The message's own window was minimized, or shown again.
    #[cfg(desktop)]
    fn window_minimized(&self, minimized: bool) {
        let Some(host) = self.host() else {
            return;
        };
        let HostRef::Window(window) = &host else {
            return;
        };
        if window.get_render_suspended() == minimized {
            return;
        }
        window.set_render_suspended(minimized);
        if minimized {
            host.ui().set_editor_tiles(ModelRc::default());
            self.editor.borrow_mut().release();
            self.render_quote(&host);
        } else if host.ui().get_open() {
            self.apply_document(&host);
        }
    }

    /// Clear the message and let its window go. A message being sent stays
    /// until the send finishes.
    pub(super) fn dispose(self: &Rc<Self>) {
        if self.disposed.replace(true) {
            return;
        }
        self.files.borrow_mut().clear();
        // A picker still open for this composer must not attach to another.
        self.generation.set(self.generation.get().wrapping_add(1));
        self.preferences_generation
            .set(self.preferences_generation.get().wrapping_add(1));
        self.clear_spelling();
        self.document.borrow_mut().reset();
        self.editor.borrow_mut().release();
        self.contacts.borrow_mut().clear();
        *self.intent.borrow_mut() = ComposeIntent::default();
        *self.quote.borrow_mut() = None;
        *self.quote_renderer.borrow_mut() = None;
        *self.left_out_quote.borrow_mut() = None;
        *self.edited_quote.borrow_mut() = None;
        *self.signature.borrow_mut() = None;
        if let Some(host) = self.host() {
            match &host {
                HostRef::Main(_) => {
                    self.clear_fields(&host);
                    host.ui().set_open(false);
                }
                #[cfg(desktop)]
                HostRef::Window(window) => {
                    host.ui().set_open(false);
                    let _ = window.hide();
                }
            }
        }
        // What the composer held is freed; return it once the app goes
        // still, even when nothing is touched in the main window.
        crate::memory::release_free_heap_when_idle();
        let Some(composers) = self.composers() else {
            return;
        };
        // The window is dropped on the next turn of the event loop, outside
        // the callback that closed it.
        let session = Rc::clone(self);
        Timer::single_shot(Duration::ZERO, move || {
            composers
                .sessions
                .borrow_mut()
                .retain(|open| !Rc::ptr_eq(open, &session));
            drop(session);
            window_controller::composer_closed();
        });
    }

    /// Nothing the user wrote: empty fields, no attachments, and at most the
    /// signature the composer inserted.
    pub(super) fn is_blank(&self) -> bool {
        let Some(host) = self.host() else {
            return true;
        };
        let ui = host.ui();
        [ui.get_to(), ui.get_cc(), ui.get_bcc(), ui.get_subject()]
            .iter()
            .all(|text| text.trim().is_empty())
            && self.files.borrow().is_empty()
            && self.quote.borrow().is_none()
            && self.only_inserted_signature(&ui.get_body())
    }

    fn only_inserted_signature(&self, body: &str) -> bool {
        account_mail_preferences::remove_managed_signature(body, self.signature.borrow().as_deref())
            .is_ok_and(|rest| rest.trim().is_empty())
    }

    /// Connect this composer's window to the session.
    fn connect(self: &Rc<Self>) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        let weak = self.reference();
        ui.on_pop_out(move || {
            if let Some(session) = weak.upgrade() {
                session.pop_out();
            }
        });
        let weak = self.reference();
        ui.on_close(move || {
            if let Some(session) = weak.upgrade() {
                session.close();
            }
        });
        let weak = self.reference();
        ui.on_save(move |account_id, to, cc, bcc, subject, body, send| {
            if let Some(session) = weak.upgrade() {
                session.save(account_id, &to, &cc, &bcc, &subject, &body, send);
            }
        });
        let weak = self.reference();
        ui.on_pick_attachments(move || {
            if let Some(session) = weak.upgrade() {
                session.pick_attachments();
            }
        });
        let weak = self.reference();
        ui.on_insert_images(move || {
            if let Some(session) = weak.upgrade() {
                session.pick_images();
            }
        });
        let weak = self.reference();
        ui.on_remove_attachment(move |index| {
            if let Some(session) = weak.upgrade() {
                session.remove_attachment(index);
            }
        });
        let weak = self.reference();
        ui.on_edit_body(move |body, anchor, cursor, key_text| {
            if let Some(session) = weak.upgrade() {
                session.edit(|document| {
                    document.synchronize_key_edit(&body, anchor, cursor, &key_text)
                });
            }
        });
        let weak = self.reference();
        ui.on_update_selection(move |body, anchor, cursor| {
            if let Some(session) = weak.upgrade() {
                session.update_selection(&body, anchor, cursor);
            }
        });
        let weak = self.reference();
        ui.on_format(move |kind, body, anchor, cursor| {
            if let Some(session) = weak.upgrade() {
                session.edit(|document| document.format(&kind, &body, anchor, cursor));
            }
        });
        let weak = self.reference();
        ui.on_paste_formatted(move |body, anchor, cursor| {
            weak.upgrade()
                .is_some_and(|session| session.paste_formatted(&body, anchor, cursor))
        });
        let weak = self.reference();
        ui.on_copy_formatted(move |cut, body, anchor, cursor| {
            weak.upgrade()
                .is_some_and(|session| session.copy_formatted(cut, &body, anchor, cursor))
        });
        let weak = self.reference();
        ui.on_link(move |url, body, anchor, cursor| {
            if let Some(session) = weak.upgrade() {
                session.edit(|document| document.set_link(&url, &body, anchor, cursor));
            }
        });
        let weak = self.reference();
        ui.on_history(move |direction| {
            if let Some(session) = weak.upgrade() {
                session.history(&direction);
            }
        });
        let weak = self.reference();
        ui.on_editor_pointer_event(move |x, y, kind, _control, shift, _alt, _meta| {
            if let Some(session) = weak.upgrade() {
                session.pointer(x, y, &kind, shift);
            }
        });
        let weak = self.reference();
        ui.on_editor_key_event(move |key, control, shift, alt, meta| {
            weak.upgrade()
                .is_some_and(|session| session.navigate(&key, control, shift, alt, meta))
        });
        let weak = self.reference();
        ui.on_editor_layout_changed(move || {
            if let Some(session) = weak.upgrade() {
                session.layout_changed();
            }
        });
        let weak = self.reference();
        ui.on_search_contacts(move |recipients, account_id| {
            if let Some(session) = weak.upgrade() {
                session.search_contacts(&recipients, account_id);
            }
        });
        let weak = self.reference();
        ui.on_select_contact(move |index, recipients| match weak.upgrade() {
            Some(session) => session.select_contact(index, recipients),
            None => recipients,
        });
        let weak = self.reference();
        ui.on_refresh_templates(move || {
            if let Some(session) = weak.upgrade() {
                session.refresh_templates();
            }
        });
        let weak = self.reference();
        ui.on_save_template(move |id, name, shortcut, subject, body| {
            weak.upgrade()
                .is_some_and(|session| session.save_template(id, &name, &shortcut, &subject, &body))
        });
        let weak = self.reference();
        ui.on_use_template(move |id| {
            weak.upgrade()
                .is_some_and(|session| session.use_template(id))
        });
        let weak = self.reference();
        ui.on_edit_quote(move || {
            if let Some(session) = weak.upgrade() {
                session.edit_quote();
            }
        });
        let weak = self.reference();
        ui.on_remove_quote(move || {
            if let Some(session) = weak.upgrade() {
                session.remove_quote();
            }
        });
        let weak = self.reference();
        ui.on_restore_quote(move || {
            if let Some(session) = weak.upgrade() {
                session.restore_quote();
            }
        });
        let weak = self.reference();
        ui.on_delete_template(move |id| {
            weak.upgrade()
                .is_some_and(|session| session.delete_template(id))
        });
        apply_spelling_state(&ui);
        let weak = self.reference();
        ui.on_editor_menu_requested(move |x, y| {
            if let Some(session) = weak.upgrade() {
                session.open_editor_menu(Some((x, y)), false);
            }
        });
        let weak = self.reference();
        ui.on_editor_caret_menu_requested(move || {
            if let Some(session) = weak.upgrade() {
                session.open_editor_menu(None, false);
            }
        });
        let weak = self.reference();
        ui.on_editor_tapped(move |x, y| {
            if let Some(session) = weak.upgrade() {
                session.word_tapped(x, y);
            }
        });
        let weak = self.reference();
        ui.on_spelling_correct(move |index| {
            if let Some(session) = weak.upgrade() {
                session.correct_spelling(index);
            }
        });
        let weak = self.reference();
        ui.on_spelling_ignore(move || {
            if let Some(session) = weak.upgrade() {
                session.ignore_spelling();
            }
        });
        let weak = self.reference();
        ui.on_spelling_learn(move || {
            if let Some(session) = weak.upgrade() {
                session.learn_spelling();
            }
        });
        let weak = self.reference();
        ui.on_spelling_toggle(move || {
            if let Some(session) = weak.upgrade()
                && let Some(spelling) = crate::spelling::service()
            {
                session.save_spelling(!spelling.enabled(), spelling.chosen_languages());
            }
        });
        let weak = self.reference();
        ui.on_spelling_toggle_language(move |index| {
            if let Some(session) = weak.upgrade()
                && let Some(spelling) = crate::spelling::service()
                && let Some(languages) = session.toggled_spelling_language(index)
            {
                session.save_spelling(spelling.enabled(), Some(languages));
            }
        });

        let preferences = host.preferences();
        let weak = self.reference();
        preferences.on_composer_reset(move || {
            if let Some(session) = weak.upgrade() {
                session.reset_preferences();
            }
        });
        let weak = self.reference();
        preferences.on_composer_account_changed(move |account_id| {
            if let Some(session) = weak.upgrade() {
                session.account_chosen(account_id);
            }
        });
        let weak = self.reference();
        preferences.on_composer_sender(move |index| {
            if let Some(session) = weak.upgrade() {
                session.choose_sender(index);
            }
        });
        let weak = self.reference();
        preferences.on_composer_signature(move |index| {
            if let Some(session) = weak.upgrade() {
                session.choose_signature(index);
            }
        });
    }

    fn close(self: &Rc<Self>) {
        if self.send_action.get().is_some() {
            return;
        }
        self.dispose();
        if let Some(app) = self
            .composers()
            .and_then(|composers| composers.app.upgrade())
        {
            mailto_links::open_pending_soon(&app);
        }
    }

    /// The original this reply or forward carries, if any.
    fn set_quote(&self, ui: &ComposerUi<'_>, quote: Option<ComposeQuote>) {
        ui.set_quote(match &quote {
            Some(quote) if quote.forward => "forward".into(),
            Some(_) => "reply".into(),
            None => "".into(),
        });
        ui.set_quote_text(
            quote
                .as_ref()
                .map_or_else(SharedString::new, |quote| quote.plain.as_str().into()),
        );
        // A reply's original stays out of the way; a forward is what the
        // message is about, so it starts open.
        ui.set_quote_expanded(quote.as_ref().is_some_and(|quote| quote.forward));
        ui.set_quote_tiles(ModelRc::default());
        ui.set_quote_height(0.0);
        ui.set_quote_left_out("".into());
        *self.quote_renderer.borrow_mut() = None;
        self.quote_unavailable.set(false);
        *self.left_out_quote.borrow_mut() = None;
        *self.quote.borrow_mut() = quote;
    }

    /// Move the original into the editor as text, a reply's in a quote
    /// block, which the user can then edit like the rest of the message.
    fn edit_quote(&self) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        let Some(quote) = self.quote.borrow().clone() else {
            return;
        };
        let expanded = ui.get_quote_expanded();
        let quoted = quote.editable_text();
        self.set_quote(&ui, None);
        let mut mark = None;
        self.edit(|document| {
            let text = document.text();
            let written = text.trim_end_matches('\n').len();
            let end = text.len();
            let selection =
                document.replace_unformatted(written..end, &format!("\n\n{quoted}"), written);
            mark = document.mark_last_edit();
            selection
        });
        *self.edited_quote.borrow_mut() = mark.map(|mark| EditedQuote {
            quote,
            expanded,
            mark,
        });
    }

    /// Leave the original out of this message, until the user puts it back.
    fn remove_quote(&self) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        let quote = self.quote.borrow_mut().take();
        self.set_quote(&ui, None);
        if let Some(quote) = quote {
            ui.set_quote_left_out(if quote.forward { "forward" } else { "reply" }.into());
            *self.left_out_quote.borrow_mut() = Some(quote);
        }
    }

    /// Put a left-out original back below the message.
    fn restore_quote(&self) {
        let Some(host) = self.host() else {
            return;
        };
        let Some(quote) = self.left_out_quote.borrow_mut().take() else {
            return;
        };
        self.set_quote(&host.ui(), Some(quote));
        self.render_quote(&host);
    }

    /// Start the message with `text`, as a mail link's body.
    pub(super) fn load_text(&self, text: &str) {
        self.edit(|document| document.load(text, 0, 0));
    }

    /// Apply an edit to the document and show the result.
    fn edit(&self, change: impl FnOnce(&mut RichComposeDocument) -> ComposeSelection) {
        let Some(host) = self.host() else {
            return;
        };
        {
            let mut document = self.document.borrow_mut();
            let selection = change(&mut document);
            apply_rich_compose(
                &host.view(),
                &document,
                selection,
                &mut self.editor.borrow_mut(),
            );
        }
        self.check_spelling_soon();
    }

    /// Pastes the HTML on the clipboard over the selection, with the formatting the composer
    /// keeps, or else a picture on it. False when the clipboard holds neither, for the plain
    /// text to go in.
    fn paste_formatted(&self, body: &str, anchor: i32, cursor: i32) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        {
            let mut document = self.document.borrow_mut();
            if document.text() != body {
                document.synchronize(body, anchor, cursor);
            } else {
                document.update_selection(anchor, cursor);
            }
        }
        if let Some(html) = crate::reader_clipboard::html()
            && self.paste_html(&host, &html)
        {
            return true;
        }
        // A picture copied on its own, such as a screenshot, goes in as an image.
        let image = crate::reader_clipboard::image_png()
            .and_then(|png| crate::rich_compose::InlineImage::from_file_bytes(png, ""));
        match image {
            Some(image) => {
                self.insert_image(image);
                true
            }
            None => false,
        }
    }

    /// Pastes `html` over the selection, unless it holds no text, or images that would take the
    /// message's images and attachments past what a message carries. Returns whether it went in.
    fn paste_html(&self, host: &HostRef, html: &str) -> bool {
        {
            let files = self
                .files
                .borrow()
                .iter()
                .map(|file| file.size)
                .sum::<u64>();
            let mut document = self.document.borrow_mut();
            let carried = |document: &RichComposeDocument| -> u64 {
                files.saturating_add(image_bytes(document))
            };
            let before = carried(&document);
            let Some(mut selection) = document.paste_html(html) else {
                return false;
            };
            if carried(&document) > before.max(MAX_COMPOSE_ATTACHMENT_BYTES) {
                if let Some(undone) = document.history("undo") {
                    selection = undone;
                }
                host.ui().set_notice(
                    UiMessage::plain("Images and attachments can take up to 25 MB in all."),
                    true,
                );
            }
            apply_rich_compose(
                &host.view(),
                &document,
                selection,
                &mut self.editor.borrow_mut(),
            );
        }
        self.check_spelling_soon();
        true
    }

    /// Puts the selection on the clipboard as HTML and plain text, so that it pastes back with
    /// its formatting, and for a cut takes it out. False when the plain text is left to copy.
    fn copy_formatted(&self, cut: bool, body: &str, anchor: i32, cursor: i32) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        {
            let mut document = self.document.borrow_mut();
            if document.text() != body {
                document.synchronize(body, anchor, cursor);
            } else {
                document.update_selection(anchor, cursor);
            }
            let Some((html, text)) = document.selection_html() else {
                return false;
            };
            if !crate::reader_clipboard::set_html(&html, &text) {
                return false;
            }
            if !cut {
                return true;
            }
            let selection = document.insert_text("");
            apply_rich_compose(
                &host.view(),
                &document,
                selection,
                &mut self.editor.borrow_mut(),
            );
        }
        self.check_spelling_soon();
        true
    }

    fn update_selection(&self, body: &str, anchor: i32, cursor: i32) {
        let Some(host) = self.host() else {
            return;
        };
        let mut document = self.document.borrow_mut();
        // Slint emits the new caret position before `edited`. The document
        // still has the old text at that point, so wait for the edit callback
        // to update both text and selection together.
        if body != document.text() {
            return;
        }
        let selection = document.update_selection(anchor, cursor);
        let view = host.view();
        apply_rich_compose_state(&view.ui, &document, selection);
        apply_compose_editor_surface(&view, &document, selection, &mut self.editor.borrow_mut());
    }

    fn history(&self, direction: &str) {
        let Some(host) = self.host() else {
            return;
        };
        let edit = {
            let mut document = self.document.borrow_mut();
            let Some(selection) = document.history(direction) else {
                return;
            };
            apply_rich_compose(
                &host.view(),
                &document,
                selection,
                &mut self.editor.borrow_mut(),
            );
            self.edited_quote
                .borrow()
                .as_ref()
                .map(|edited| document.edit_state(edited.mark))
        };
        self.check_spelling_soon();
        // Undoing Edit puts the drawn original back, and redoing it moves the
        // original into the text again.
        let ui = host.ui();
        let shown = self.quote.borrow().is_some() || self.left_out_quote.borrow().is_some();
        match edit {
            Some(EditState::Undone) if !shown => {
                let Some((quote, expanded)) = self
                    .edited_quote
                    .borrow()
                    .as_ref()
                    .map(|edited| (edited.quote.clone(), edited.expanded))
                else {
                    return;
                };
                self.set_quote(&ui, Some(quote));
                ui.set_quote_expanded(expanded);
                self.render_quote(&host);
            }
            Some(EditState::Applied) if shown => self.set_quote(&ui, None),
            Some(EditState::Gone) => *self.edited_quote.borrow_mut() = None,
            _ => {}
        }
    }

    /// Move the caret or selection from the surface's own hit testing or
    /// keyboard navigation.
    fn select_from_surface(
        &self,
        host: &HostRef,
        find: impl FnOnce(
            &mut LazyComposeEditor,
            &RichComposeDocument,
            f32,
            f32,
            ComposeEditorStyle,
            &str,
        ) -> Option<ComposeSelection>,
    ) -> bool {
        let ui = host.ui();
        let width = ui.get_editor_width().max(1.0);
        let scale_factor = host.window().scale_factor();
        let style = compose_editor_style(&ui);
        let preedit_text = ui.get_editor_preedit_text();
        let next = {
            let document = self.document.borrow();
            find(
                &mut self.editor.borrow_mut(),
                &document,
                width,
                scale_factor,
                style,
                preedit_text.as_str(),
            )
        };
        let Some(next) = next else {
            return false;
        };
        let mut document = self.document.borrow_mut();
        let selection = document.update_selection(next.start, next.end);
        let view = host.view();
        apply_rich_compose_state(&view.ui, &document, selection);
        apply_compose_editor_surface(&view, &document, selection, &mut self.editor.borrow_mut());
        true
    }

    fn pointer(&self, x: f32, y: f32, kind: &str, shift: bool) {
        if kind == "down" {
            self.editor_pressed();
        }
        let Some(host) = self.host() else {
            return;
        };
        self.select_from_surface(&host, |editor, document, width, scale, style, preedit| {
            editor.handle_pointer(
                document,
                document.selection(),
                width,
                scale,
                style,
                preedit,
                x,
                y,
                kind,
                shift,
            )
        });
    }

    fn navigate(&self, key: &str, control: bool, shift: bool, alt: bool, meta: bool) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        self.select_from_surface(&host, |editor, document, width, scale, style, preedit| {
            editor.handle_navigation(
                document,
                document.selection(),
                width,
                scale,
                style,
                preedit,
                key,
                control,
                shift,
                alt,
                meta,
            )
        })
    }

    fn layout_changed(&self) {
        let Some(host) = self.host() else {
            return;
        };
        let view = host.view();
        if !view.ui.get_open() || view.suspended {
            return;
        }
        let Ok(document) = self.document.try_borrow() else {
            return;
        };
        let Ok(mut editor) = self.editor.try_borrow_mut() else {
            return;
        };
        apply_compose_editor_surface(&view, &document, document.selection(), &mut editor);
        drop((document, editor));
        self.render_quote(&host);
    }

    fn pick_attachments(self: &Rc<Self>) {
        let Some(composers) = self.composers() else {
            return;
        };
        let Some(app) = composers.app.upgrade() else {
            return;
        };
        if self.picking.replace(true) {
            return;
        }
        let dialog = documents::DocumentDialog::titled(translated(
            &app,
            &UiMessage::plain("Attach files to this email"),
        ));
        let request = composers
            .documents
            .import_many(MAX_COMPOSE_ATTACHMENT_FILES, dialog);
        let weak = Rc::downgrade(self);
        let picked_in = self.generation.get();
        documents::run_request(&composers.runtime, request, move |imported| {
            let Some(session) = weak.upgrade() else {
                return;
            };
            session.picking.set(false);
            // The message was sent, discarded or replaced while the picker
            // was open; dropping the documents releases their copies.
            if session.generation.get() != picked_in {
                return;
            }
            let Some(host) = session.host() else {
                return;
            };
            let ui = host.ui();
            let documents = match imported {
                Ok(documents) => documents,
                Err(error) => {
                    tracing::warn!(%error, "compose attachment picker failed");
                    ui.set_notice(
                        UiMessage::detail("Could not add attachments: {}", error),
                        true,
                    );
                    return;
                }
            };
            if documents.is_empty() {
                return;
            }
            let mut files = session.files.borrow_mut();
            let mut total_size = files
                .iter()
                .map(|file| file.size)
                .sum::<u64>()
                .saturating_add(image_bytes(&session.document.borrow()));
            let mut added = 0usize;
            let mut skipped = 0usize;
            for document in documents {
                let path = std::fs::canonicalize(document.path())
                    .unwrap_or_else(|_| document.path().to_path_buf());
                if files.iter().any(|file| file.path == path) {
                    skipped += 1;
                    continue;
                }
                let Ok(metadata) = std::fs::metadata(&path) else {
                    skipped += 1;
                    continue;
                };
                if !metadata.is_file()
                    || total_size.saturating_add(metadata.len()) > MAX_COMPOSE_ATTACHMENT_BYTES
                {
                    skipped += 1;
                    continue;
                }
                let Some(filename) = document.name() else {
                    skipped += 1;
                    continue;
                };
                total_size += metadata.len();
                files.push(ComposeFile {
                    path,
                    filename,
                    size: metadata.len(),
                    _import: Some(Rc::new(document)),
                });
                added += 1;
            }
            apply_compose_files(&ui, &files);
            if skipped == 0 {
                ui.set_notice(
                    if added == 1 {
                        UiMessage::plain("Added 1 attachment.")
                    } else {
                        UiMessage::detail("Added {} attachments.", added)
                    },
                    false,
                );
            } else {
                ui.set_notice(
                    UiMessage::arguments(
                        "Attachments added: {}. Skipped: {} (duplicate, unreadable, or over the 25 MB total limit).",
                        added,
                        skipped,
                    ),
                    added == 0,
                );
            }
        });
    }

    /// Opens a picker for images to put in the text at the caret.
    fn pick_images(self: &Rc<Self>) {
        let Some(composers) = self.composers() else {
            return;
        };
        let Some(app) = composers.app.upgrade() else {
            return;
        };
        if self.picking.replace(true) {
            return;
        }
        let mut dialog = documents::DocumentDialog::titled(translated(
            &app,
            &UiMessage::plain("Insert images in this email"),
        ));
        dialog.filter = Some((
            translated(&app, &UiMessage::plain("Images")).into(),
            &["png", "jpg", "jpeg", "gif", "webp"],
        ));
        let request = composers
            .documents
            .import_many(MAX_COMPOSE_ATTACHMENT_FILES, dialog);
        let weak = Rc::downgrade(self);
        let picked_in = self.generation.get();
        documents::run_request(&composers.runtime, request, move |imported| {
            let Some(session) = weak.upgrade() else {
                return;
            };
            session.picking.set(false);
            if session.generation.get() != picked_in {
                return;
            }
            let Some(host) = session.host() else {
                return;
            };
            let ui = host.ui();
            let documents = match imported {
                Ok(documents) => documents,
                Err(error) => {
                    tracing::warn!(%error, "compose image picker failed");
                    ui.set_notice(UiMessage::detail("Could not add images: {}", error), true);
                    return;
                }
            };
            for document in documents {
                let name = document.name().unwrap_or_default();
                let image = std::fs::metadata(document.path())
                    .ok()
                    .filter(|metadata| metadata.len() <= MAX_COMPOSE_ATTACHMENT_BYTES)
                    .and_then(|_| std::fs::read(document.path()).ok())
                    .map(|bytes| crate::rich_compose::InlineImage::from_file_bytes(bytes, ""));
                match image {
                    Some(Some(image)) => {
                        if !session.insert_image(image) {
                            break;
                        }
                    }
                    Some(None) => ui.set_notice(
                        UiMessage::detail("{} isn't a PNG, JPEG, GIF or WebP image.", name),
                        true,
                    ),
                    None => {
                        ui.set_notice(
                            UiMessage::plain("Images and attachments can take up to 25 MB in all."),
                            true,
                        );
                        break;
                    }
                }
            }
        });
    }

    /// Puts an image in the text at the caret, unless it would take the message's images and
    /// attachments past what a message carries. Returns whether it went in.
    fn insert_image(&self, image: crate::rich_compose::InlineImage) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let fits = {
            let files = self
                .files
                .borrow()
                .iter()
                .map(|file| file.size)
                .sum::<u64>();
            files
                .saturating_add(image_bytes(&self.document.borrow()))
                .saturating_add(image.data.len() as u64)
                <= MAX_COMPOSE_ATTACHMENT_BYTES
        };
        if !fits {
            host.ui().set_notice(
                UiMessage::plain("Images and attachments can take up to 25 MB in all."),
                true,
            );
            return false;
        }
        self.edit(|document| document.insert_image(image));
        true
    }

    fn remove_attachment(&self, index: i32) {
        let Some(host) = self.host() else {
            return;
        };
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let mut files = self.files.borrow_mut();
        if index < files.len() {
            files.remove(index);
            let ui = host.ui();
            apply_compose_files(&ui, &files);
            ui.set_notice(UiMessage::plain("Attachment removed."), false);
        }
    }

    /// Attaches a forwarded or edited message's files once they are here, the
    /// way the attachment picker adds files, unless the message was sent,
    /// discarded or replaced meanwhile.
    fn attach_original_files(
        self: &Rc<Self>,
        core: &CoreMailSource,
        attachments: Vec<AttachmentMeta>,
    ) {
        if attachments.is_empty() {
            return;
        }
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        host.ui()
            .set_notice(UiMessage::plain("Attaching the original's files…"), false);
        let core = core.clone();
        let request: documents::DocumentFuture<FetchedFiles> = Box::pin(async move {
            let mut fetched = Vec::with_capacity(attachments.len());
            for attachment in attachments {
                let name = attachment
                    .filename
                    .clone()
                    .unwrap_or_else(|| "attachment".into());
                fetched.push(match core.attachment_path(attachment.id).await {
                    Ok(path) => {
                        let size = tokio::fs::metadata(&path)
                            .await
                            .map(|metadata| metadata.len())
                            .unwrap_or_default();
                        Ok((PathBuf::from(path), name, size))
                    }
                    Err(error) => Err(format!("{name}: {error}")),
                });
            }
            Ok(fetched)
        });
        let weak = Rc::downgrade(self);
        let started_in = self.generation.get();
        documents::run_request(&composers.runtime, request, move |fetched| {
            let Some(session) = weak.upgrade() else {
                return;
            };
            if session.generation.get() != started_in {
                return;
            }
            let Some(host) = session.host() else {
                return;
            };
            let mut files = session.files.borrow_mut();
            let mut total_size = files
                .iter()
                .map(|file| file.size)
                .sum::<u64>()
                .saturating_add(image_bytes(&session.document.borrow()));
            let mut failure = None;
            let mut too_large = false;
            for file in fetched.unwrap_or_else(|error| vec![Err(error)]) {
                match file {
                    Ok((path, filename, size)) => {
                        if total_size.saturating_add(size) > MAX_COMPOSE_ATTACHMENT_BYTES {
                            too_large = true;
                            continue;
                        }
                        total_size += size;
                        files.push(ComposeFile {
                            path,
                            filename,
                            size,
                            _import: None,
                        });
                    }
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            }
            let ui = host.ui();
            apply_compose_files(&ui, &files);
            let notice = match failure {
                Some(error) => {
                    UiMessage::detail("Could not attach the original's files: {}", error)
                }
                None if too_large => UiMessage::plain(
                    "Some of the original's files are over the 25 MB limit for attachments.",
                ),
                None => UiMessage::EMPTY,
            };
            let error = notice != UiMessage::EMPTY;
            ui.set_notice(notice, error);
        });
    }

    fn search_contacts(&self, recipients: &str, account_id: i32) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        let ui = host.ui();
        let query = compose_recipient_query(recipients);
        let core = composers.core();
        let (false, Some(core)) = (query.is_empty(), core) else {
            self.contacts.borrow_mut().clear();
            apply_compose_contacts(&ui, &[]);
            return;
        };
        let account_id = (account_id > 0).then_some(i64::from(account_id));
        match composers
            .runtime
            .block_on(core.list_contacts(query, account_id, 6))
        {
            Ok(mut contacts) => {
                let existing = recipients.to_lowercase();
                contacts.retain(|contact| !existing.contains(&contact.email.to_lowercase()));
                *self.contacts.borrow_mut() = contacts;
                apply_compose_contacts(&ui, &self.contacts.borrow());
            }
            Err(error) => {
                self.contacts.borrow_mut().clear();
                apply_compose_contacts(&ui, &[]);
                ui.set_notice(
                    UiMessage::detail("Could not load contacts: {}", error),
                    true,
                );
            }
        }
    }

    fn select_contact(&self, index: i32, current_recipients: SharedString) -> SharedString {
        let Some(host) = self.host() else {
            return current_recipients;
        };
        let Ok(index) = usize::try_from(index) else {
            return current_recipients;
        };
        let Some(contact) = self.contacts.borrow().get(index).cloned() else {
            return current_recipients;
        };
        let recipients = complete_compose_recipient(current_recipients.as_str(), &contact);
        self.contacts.borrow_mut().clear();
        apply_compose_contacts(&host.ui(), &[]);
        recipients.into()
    }

    #[allow(clippy::too_many_arguments)]
    fn save(
        self: &Rc<Self>,
        account_id: i32,
        to: &str,
        cc: &str,
        bcc: &str,
        subject: &str,
        body: &str,
        send: bool,
    ) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        let Some(app) = composers.app.upgrade() else {
            return;
        };
        let ui = host.ui();
        let preferences = host.preferences();
        if self.send_action.get().is_some() || ui.get_sending() {
            return;
        }
        if send && !preferences.get_composer_preferences_ready() {
            return;
        }
        if account_id <= 0 {
            ui.set_notice(
                UiMessage::plain("Choose the account this message should be sent from."),
                true,
            );
            return;
        }
        let Some(core) = composers.core() else {
            let message = UiMessage::plain("Connect an account before saving or sending mail.");
            ui.set_notice(message.clone(), true);
            app.set_sync_status(message);
            return;
        };
        let attachments = self
            .files
            .borrow()
            .iter()
            .map(|file| DraftAttachmentIn {
                file_path: file.path.to_string_lossy().into_owned(),
                filename: file.filename.clone(),
                mime_type: None,
            })
            .collect::<Vec<_>>();
        let (body_html, described) = {
            let mut document = self.document.borrow_mut();
            if document.text() != body {
                document.synchronize(body, body.len() as i32, body.len() as i32);
            }
            // Blank lines at the end give way to the one above the quote, in
            // both parts, so a reopened draft finds where the quote starts.
            let html = if self.quote.borrow().is_some() {
                document.body_html_above_quote()
            } else {
                document.body_html()
            };
            // Mail read as plain text names each image in its place. A draft keeps the
            // placeholders, which the HTML's images reopen into.
            (html, send.then(|| document.text_with_image_descriptions()))
        };
        let body = described.as_deref().unwrap_or(body);
        // The original goes below what the user wrote, in both parts.
        let (body, body_html) = match self.quote.borrow().as_ref() {
            Some(quote) => {
                let (text, html) = quote.below(body, body_html);
                (text, Some(html))
            }
            None => (body.to_owned(), body_html),
        };
        // Recipients reading plain text see quote blocks as quotes; a draft
        // keeps them as the editor writes them, to reopen as blocks.
        let body = if send {
            crate::rich_compose::with_plain_text_quotes(&body)
        } else {
            body
        };
        let body = body.as_str();
        let intent = self.intent.borrow().clone();
        let sender_email = preferences.get_composer_sender_email();
        if sender_email.is_empty() {
            ui.set_notice(
                UiMessage::plain("Choose a verified sender identity for this message."),
                true,
            );
            return;
        }
        let message = ComposeMessage {
            draft_id: intent.draft_id,
            account_id: i64::from(account_id),
            sender_email: Some(sender_email.as_str()),
            to,
            cc,
            bcc,
            subject,
            body,
            body_html: body_html.as_deref(),
            attachments: &attachments,
            mode: intent.mode.as_str(),
            in_reply_to_message_id: intent.in_reply_to_message_id,
            in_reply_to_header: intent.in_reply_to_header.as_deref(),
        };
        if send {
            let queued = composers
                .runtime
                .block_on(core.send_new_message(message))
                .and_then(|queued| {
                    let held = crate::undo::holds(&queued);
                    // Without an undo delay the message leaves now, and the
                    // composer waits for it below.
                    if !held {
                        composers
                            .runtime
                            .block_on(core.send_now(queued.action_id))?;
                    }
                    Ok((queued, held))
                });
            match queued {
                // With one, the composer closes at once, and a notice in the
                // main window can take the message back until it leaves.
                Ok((queued, true)) => {
                    self.dispose();
                    crate::undo::sending(&app, queued);
                    mailto_links::open_pending_soon(&app);
                }
                Ok((queued, false)) => {
                    // Keep the exact saved draft attached to this composer so
                    // a failed submission can be edited and retried in place.
                    self.intent.borrow_mut().draft_id = Some(queued.draft_id);
                    self.send_action.set(Some(queued.action_id));
                    ui.set_sending(true);
                    ui.set_notice(UiMessage::plain("Sending message…"), false);
                    app.set_sync_status(UiMessage::plain("Sending message…"));

                    let monitor = core.clone();
                    let updates = composers.send_updates.clone();
                    let action_id = queued.action_id;
                    composers.runtime.spawn(async move {
                        let result = monitor.wait_for_send(action_id).await;
                        let _ = updates.send(ComposeSendUpdate { action_id, result }).await;
                    });
                }
                Err(error) => {
                    let message = UiMessage::detail("Compose failed: {}", error);
                    ui.set_notice(message.clone(), true);
                    app.set_sync_status(message);
                }
            }
            return;
        }

        match composers.runtime.block_on(core.save_new_draft(message)) {
            Ok(draft_id) => {
                self.dispose();
                app.set_sync_status(UiMessage::detail("Draft saved locally (#{}).", draft_id));
                let _ = refresh_from_source(&app, &composers.state, &composers.runtime, true, &[]);
                mailto_links::open_pending_soon(&app);
            }
            Err(error) => {
                let message = UiMessage::detail("Compose failed: {}", error);
                ui.set_notice(message.clone(), true);
                app.set_sync_status(message);
            }
        }
    }

    fn refresh_templates(&self) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        let ui = host.ui();
        let Some(core) = composers.core() else {
            ui.set_notice(
                UiMessage::plain("Connect an account before managing email templates."),
                true,
            );
            return;
        };
        match composers.runtime.block_on(core.list_email_templates()) {
            Ok(templates) => {
                *composers.templates.borrow_mut() = templates;
                composers.apply_templates();
                ui.set_notice(UiMessage::EMPTY, false);
            }
            Err(error) => {
                ui.set_notice(
                    UiMessage::detail("Could not load email templates: {}", error),
                    true,
                );
            }
        }
    }

    fn save_template(
        &self,
        id: i32,
        name: &str,
        shortcut: &str,
        subject: &str,
        body: &str,
    ) -> bool {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return false;
        };
        let ui = host.ui();
        let name = name.trim();
        let subject = subject.trim();
        if name.is_empty() {
            ui.set_notice(UiMessage::plain("Enter a template name."), true);
            return false;
        }
        if subject.is_empty() && body.trim().is_empty() {
            ui.set_notice(
                UiMessage::plain("Add a subject or message to the template."),
                true,
            );
            return false;
        }
        let Some(core) = composers.core() else {
            ui.set_notice(
                UiMessage::plain("Connect an account before managing email templates."),
                true,
            );
            return false;
        };
        let shortcut = shortcut.trim().trim_start_matches('/').trim();
        let result = composers.runtime.block_on(core.save_email_template(
            (id >= 0).then_some(i64::from(id)),
            name.to_owned(),
            (!shortcut.is_empty()).then(|| shortcut.to_owned()),
            (!subject.is_empty()).then(|| subject.to_owned()),
            body.to_owned(),
        ));
        match result {
            Ok(saved) => {
                {
                    let mut templates = composers.templates.borrow_mut();
                    if let Some(existing) = templates.iter_mut().find(|item| item.id == saved.id) {
                        *existing = saved;
                    } else {
                        templates.push(saved);
                    }
                }
                composers.sort_templates();
                composers.apply_templates();
                ui.set_notice(UiMessage::plain("Email template saved."), false);
                true
            }
            Err(error) => {
                ui.set_notice(
                    UiMessage::detail("Could not save email template: {}", error),
                    true,
                );
                false
            }
        }
    }

    fn delete_template(&self, id: i32) -> bool {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return false;
        };
        let ui = host.ui();
        let Some(core) = composers.core() else {
            ui.set_notice(
                UiMessage::plain("Connect an account before managing email templates."),
                true,
            );
            return false;
        };
        match composers
            .runtime
            .block_on(core.delete_email_template(i64::from(id)))
        {
            Ok(()) => {
                composers
                    .templates
                    .borrow_mut()
                    .retain(|template| template.id != i64::from(id));
                composers.apply_templates();
                ui.set_notice(UiMessage::plain("Email template deleted."), false);
                true
            }
            Err(error) => {
                ui.set_notice(
                    UiMessage::detail("Could not delete email template: {}", error),
                    true,
                );
                false
            }
        }
    }

    fn use_template(&self, id: i32) -> bool {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return false;
        };
        let ui = host.ui();
        let Some(template) = composers
            .templates
            .borrow()
            .iter()
            .find(|template| template.id == i64::from(id))
            .cloned()
        else {
            ui.set_notice(
                UiMessage::plain("Email template is no longer available."),
                true,
            );
            return false;
        };

        if ui.get_subject().trim().is_empty()
            && let Some(subject) = template.subject.as_deref()
            && !subject.trim().is_empty()
        {
            ui.set_subject(subject.into());
        }
        self.edit(|document| document.insert_text(&template.body_text));

        let usage = composers.core().map(|core| {
            composers
                .runtime
                .block_on(core.use_email_template(template.id))
        });
        match usage {
            Some(Ok(())) => {
                if let Some(used) = composers
                    .templates
                    .borrow_mut()
                    .iter_mut()
                    .find(|item| item.id == template.id)
                {
                    used.usage_count = used.usage_count.saturating_add(1);
                }
                composers.sort_templates();
                composers.apply_templates();
                ui.set_notice(UiMessage::plain("Email template inserted."), false);
            }
            Some(Err(error)) => ui.set_notice(
                UiMessage::detail(
                    "Email template inserted, but its usage could not be saved: {}",
                    error,
                ),
                true,
            ),
            None => ui.set_notice(UiMessage::plain("Email template inserted."), false),
        }
        true
    }

    fn reset_preferences(&self) {
        self.preferences_generation
            .set(self.preferences_generation.get().wrapping_add(1));
        *self.signature.borrow_mut() = None;
        self.signatures.borrow_mut().clear();
        let Some(host) = self.host() else {
            return;
        };
        let preferences = host.preferences();
        preferences.set_composer_signature_names(ModelRc::default());
        preferences.set_composer_signature_index(0);
        preferences.set_composer_sender_identities(ModelRc::default());
        preferences.set_composer_sender_names(ModelRc::default());
        preferences.set_composer_sender_index(0);
        preferences.set_composer_sender_email("".into());
        preferences.set_composer_security("".into());
        preferences.set_composer_preferences_ready(false);
    }

    /// The message is sent from `account_id` now. The next new message
    /// starts from it too, as the last account mail was written from.
    fn account_chosen(self: &Rc<Self>, account_id: i32) {
        if let Some(app) = self
            .composers()
            .and_then(|composers| composers.app.upgrade())
        {
            account_controller::remember_compose_account(&app, account_id);
        }
        self.load_preferences(account_id);
    }

    /// Load the sending account's identities and signatures, then choose
    /// the sender and insert the default signature.
    fn load_preferences(self: &Rc<Self>, account_id: i32) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        let preferences = host.preferences();
        preferences.set_composer_preferences_ready(false);
        preferences.set_composer_signature_names(ModelRc::default());
        preferences.set_composer_sender_identities(ModelRc::default());
        preferences.set_composer_sender_names(ModelRc::default());
        preferences.set_composer_security("".into());
        let Some(core) = composers
            .state
            .borrow()
            .core
            .as_ref()
            .map(CoreMailSource::account_preferences_core)
        else {
            return;
        };
        self.preferences_generation
            .set(self.preferences_generation.get().wrapping_add(1));
        let expected = self.preferences_generation.get();
        let task = composers.runtime.spawn(async move {
            account_mail_preferences::cached_preferences(&core, i64::from(account_id)).await
        });
        let abort = task.abort_handle();
        let weak = Rc::downgrade(self);
        if slint::spawn_local(async move {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let Some(session) = weak.upgrade() else {
                return;
            };
            if session.preferences_generation.get() != expected {
                return;
            }
            session.apply_preferences(account_id, result);
        })
        .is_err()
        {
            abort.abort();
        }
    }

    fn apply_preferences(
        &self,
        account_id: i32,
        result: Result<(Settings, Vec<AccountConfig>, Vec<SenderIdentity>), String>,
    ) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        if ui.get_account_id() != account_id {
            return;
        }
        let preferences = host.preferences();
        let (settings, configs, identities) = match result {
            Ok(loaded) => loaded,
            Err(error) => {
                ui.set_notice(
                    UiMessage::detail("Could not load account preferences: {}", error),
                    true,
                );
                return;
            }
        };
        let Some(config) = configs
            .iter()
            .find(|config| config.id == i64::from(account_id))
        else {
            return;
        };
        let requested = preferences.get_composer_sender_email().to_string();
        let identities = account_mail_preferences::selectable_identities(identities, &requested);
        let sender_choices = identities
            .iter()
            .map(|identity| SenderIdentityChoice {
                email: identity.email.clone().into(),
                label: account_mail_preferences::identity_label(identity).into(),
            })
            .collect::<Vec<_>>();
        preferences.set_composer_sender_names(ModelRc::new(VecModel::from(
            sender_choices
                .iter()
                .map(|identity| identity.label.clone())
                .collect::<Vec<_>>(),
        )));
        preferences.set_composer_sender_identities(ModelRc::new(VecModel::from(sender_choices)));
        let sender_index = account_mail_preferences::sender_index(
            &identities,
            &requested,
            config.settings.default_sender_email.as_deref(),
        );
        preferences.set_composer_preferences_ready(true);
        preferences.invoke_composer_sender(i32::try_from(sender_index).unwrap_or(0));
        let signatures = account_mail_preferences::choices(&settings, i64::from(account_id));
        let mut names = vec![SharedString::from("No signature")];
        names.extend(signatures.iter().map(|s| SharedString::from(&s.name)));
        preferences.set_composer_signature_names(ModelRc::new(VecModel::from(names)));
        *self.signatures.borrow_mut() = signatures.clone();
        preferences.set_composer_security(
            account_mail_preferences::security_summary(&config.settings.security).into(),
        );
        // A draft, and a message edited as new, have whatever signature they
        // had.
        if matches!(ui.get_mode().as_str(), "draft" | "edit_as_new") {
            *self.signature.borrow_mut() = None;
            preferences.set_composer_signature_index(0);
            return;
        }
        // A cleared composer starts a fresh insertion lifecycle.
        if ui.get_body().is_empty() {
            *self.signature.borrow_mut() = None;
        }
        let defaults = settings
            .signature_defaults
            .get(&account_id.to_string())
            .cloned()
            .unwrap_or_default();
        let id = if ui.get_mode() == "new" {
            defaults.new_id
        } else {
            defaults.reply_id
        };
        let index = signatures
            .iter()
            .position(|s| Some(&s.id) == id.as_ref())
            .map_or(0, |i| i as i32 + 1);
        preferences.invoke_composer_signature(index);
    }

    fn choose_sender(&self, index: i32) {
        let Some(host) = self.host() else {
            return;
        };
        let preferences = host.preferences();
        let Some(identity) = usize::try_from(index)
            .ok()
            .and_then(|index| preferences.get_composer_sender_identities().row_data(index))
        else {
            return;
        };
        preferences.set_composer_sender_index(index);
        preferences.set_composer_sender_email(identity.email);
    }

    /// Only replace the exact unedited block this composer inserted. User
    /// edits are never discarded when switching identity or signature.
    fn choose_signature(&self, index: i32) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        let preferences = host.preferences();
        let current = self.document.borrow().text().to_owned();
        let old = self.signature.borrow().clone();
        let body = match account_mail_preferences::remove_managed_signature(
            &current,
            old.as_deref(),
        ) {
            Ok(body) => body,
            Err(()) if index == 0 => {
                *self.signature.borrow_mut() = None;
                preferences.set_composer_signature_index(0);
                return;
            }
            Err(()) => {
                ui.set_notice(
                    UiMessage::plain(
                        "Your edited signature was preserved. Choose No signature to keep it as message text before inserting another.",
                    ),
                    true,
                );
                return;
            }
        };
        let signature = usize::try_from(index - 1)
            .ok()
            .and_then(|i| self.signatures.borrow().get(i).cloned());
        let text = signature
            .as_ref()
            .map(flectar_mail_core::signatures::plain_text)
            .unwrap_or_default();
        let new_body = flectar_mail_core::signatures::insert(&body, &text);
        *self.signature.borrow_mut() = if text.is_empty() {
            None
        } else {
            Some(format!("\n\n-- \n{}", text.trim()))
        };
        self.edit(|document| document.synchronize(&new_body, 0, 0));
        preferences.set_composer_signature_index(index);
    }
}

type FetchedFiles = Vec<Result<(PathBuf, String, u64), String>>;

/// The size of the images in a message's text, which count with its attachments.
fn image_bytes(document: &RichComposeDocument) -> u64 {
    document.images().map(|image| image.data.len() as u64).sum()
}

#[cfg(all(test, desktop))]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, WindowAdapter, WindowEvent};

    /// A native window for every component, as the desktop backend gives.
    struct Windows;
    impl Platform for Windows {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
        }
    }

    struct Setup {
        app: AppWindow,
        composers: Rc<Composers>,
        _runtime: tokio::runtime::Runtime,
        _dir: tempfile::TempDir,
    }

    fn setup() -> Setup {
        slint::platform::set_platform(Box::new(Windows)).unwrap();
        let app = AppWindow::new().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (favicons, _) = bounded_ui_channel();
        let (avatars, _) = bounded_ui_channel();
        let mut state = InboxState::empty(
            None,
            UiSender::new(favicons, UiWake::new(app.as_weak(), |_| {})),
            None,
            UiSender::new(avatars, UiWake::new(app.as_weak(), |_| {})),
            false,
            false,
            WarmStartCacheWriter::spawn(runtime.handle(), dir.path().join("warm.json")),
        );
        state.connected_accounts = vec![Account {
            id: 1,
            email: "me@example.test".into(),
            display_name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: flectar_mail_core::models::AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            sync_state: "idle".into(),
            sync_error: None,
            can_create_top_level_mailbox: true,
            server_keeps_mail_days: None,
        }];
        let state = Rc::new(RefCell::new(state));
        app.set_compose_account_id(1);
        let composers = install(
            &app,
            &state,
            runtime.handle(),
            documents::default_provider(),
            true,
        );

        Setup {
            app,
            composers,
            _runtime: runtime,
            _dir: dir,
        }
    }

    #[test]
    fn a_message_pops_out_into_its_own_window_and_frees_the_main_one() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        let main = app.global::<ComposerUi>();

        // A new message opens in the main window, which offers to pop it out.
        app.invoke_open_compose("first@example.com".into());
        assert!(main.get_open());
        assert!(main.get_can_pop_out());
        assert_eq!(open_windows(), 0);
        main.set_subject("Plans".into());
        app.global::<AccountMailPreferences>()
            .set_composer_sender_email("me@example.test".into());
        let first = composers.in_app().unwrap();

        main.invoke_pop_out();
        assert_eq!(open_windows(), 1);
        assert!(!main.get_open(), "the main window's composer is free again");
        assert_eq!(main.get_to(), "");
        let popped = first.host().unwrap();
        assert!(matches!(popped, HostRef::Window(_)));
        assert!(popped.ui().get_open());
        assert!(!popped.ui().get_can_pop_out());
        assert_eq!(popped.ui().get_to(), "first@example.com");
        assert_eq!(popped.ui().get_subject(), "Plans");
        assert_eq!(popped.ui().get_account_id(), 1);
        assert_eq!(
            popped.preferences().get_composer_sender_email(),
            "me@example.test",
            "the chosen sender moves with the message"
        );

        // What the main window's composer left behind no longer reaches it.
        main.invoke_close();
        assert_eq!(open_windows(), 1);
        assert!(popped.ui().get_open());

        // Another message opens in the main window and pops out beside it.
        app.invoke_open_compose("".into());
        assert!(main.get_open());
        main.invoke_pop_out();
        assert_eq!(open_windows(), 2);
        let sessions = composers.sessions.borrow().clone();
        assert_eq!(sessions.len(), 2);
        let second = sessions[1].host().unwrap();
        assert!(second.ui().get_open());
        assert_eq!(second.ui().get_to(), "");

        // Each window lists the accounts the main window lists.
        app.set_connected_accounts(ModelRc::new(VecModel::from(vec![
            AccountRow {
                id: 1,
                ..Default::default()
            },
            AccountRow {
                id: 2,
                ..Default::default()
            },
        ])));
        accounts_changed(&app);
        assert_eq!(second.ui().get_accounts().row_count(), 2);

        // The account a message is written from is where the next new one
        // starts, wherever the message is.
        popped.preferences().invoke_composer_account_changed(2);
        assert_eq!(app.get_compose_account_id(), 2);

        // A message whose account is removed moves to the account new mail
        // starts from.
        second.ui().set_account_id(2);
        app.set_compose_account_id(1);
        app.set_connected_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
            id: 1,
            ..Default::default()
        }])));
        accounts_changed(&app);
        assert_eq!(second.ui().get_account_id(), 1);
        assert_eq!(second.ui().get_accounts().row_count(), 1);
        assert_eq!(popped.ui().get_account_id(), 1);

        // The close button keeps a message with content behind the discard
        // confirmation.
        popped.window().dispatch_event(WindowEvent::CloseRequested);
        slint::platform::update_timers_and_animations();
        assert_eq!(open_windows(), 2);
        assert!(popped.ui().get_open());

        // An empty message closes, and its window goes on the next turn.
        second.window().dispatch_event(WindowEvent::CloseRequested);
        assert_eq!(open_windows(), 1);
        slint::platform::update_timers_and_animations();
        assert_eq!(composers.sessions.borrow().len(), 1);
        assert!(Rc::ptr_eq(&composers.sessions.borrow()[0], &first));
    }

    #[test]
    fn a_message_whose_window_never_opens_comes_back_to_the_main_one() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        let main = app.global::<ComposerUi>();
        app.invoke_open_compose("first@example.com".into());
        main.set_subject("Plans".into());
        let session = composers.in_app().unwrap();
        main.invoke_pop_out();
        assert_eq!(open_windows(), 1);
        assert!(!main.get_open());

        // Winit never created the window.
        session.window_never_opened();
        assert_eq!(open_windows(), 0);
        assert!(session.is_in_app());
        assert!(main.get_open());
        assert_eq!(main.get_to(), "first@example.com");
        assert_eq!(main.get_subject(), "Plans");
        assert_eq!(
            main.get_message().key,
            "Could not open the message in its own window."
        );
        assert!(main.get_notice_is_error());

        // The main window's composer works for it again.
        main.invoke_pop_out();
        assert_eq!(open_windows(), 1);
        assert_eq!(session.host().unwrap().ui().get_subject(), "Plans");
    }

    #[test]
    fn a_reply_shows_its_original_read_only_until_edited_or_removed() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        app.invoke_open_compose("".into());
        let session = composers.in_app().unwrap();
        let host = session.host().unwrap();
        let ui = host.ui();
        ui.set_editor_width(600.0);
        ui.set_editor_viewport_height(400.0);
        let original = || {
            ComposeQuote::reply(
                "Maya wrote:",
                "First line",
                Some("<p>First <b>line</b></p><p>Second</p>"),
            )
        };

        session.set_quote(&ui, Some(original()));
        assert_eq!(ui.get_quote(), "reply");
        assert!(
            !ui.get_quote_expanded(),
            "a reply's original starts collapsed"
        );
        session.layout_changed();
        assert_eq!(ui.get_quote_tiles().row_count(), 0);

        ui.set_quote_expanded(true);
        session.layout_changed();
        assert!(ui.get_quote_tiles().row_count() > 0);
        assert!(ui.get_quote_height() > 0.0);
        ui.set_quote_expanded(false);
        session.layout_changed();
        assert_eq!(ui.get_quote_tiles().row_count(), 0);
        assert!(session.quote_renderer.borrow().is_none());

        // Editing moves the original into the editor as a quote block.
        ui.set_quote_expanded(true);
        session.edit_quote();
        assert_eq!(ui.get_quote(), "");
        let edited = || {
            session
                .document
                .borrow()
                .text()
                .ends_with("Maya wrote:\n│ First line")
        };
        assert!(edited());

        // Undoing that puts the drawn original back as it was, and redoing
        // moves it into the text again.
        session.history("undo");
        assert!(!edited());
        assert_eq!(ui.get_quote(), "reply");
        assert!(ui.get_quote_expanded());
        assert!(ui.get_quote_tiles().row_count() > 0);
        session.history("redo");
        assert!(edited());
        assert_eq!(ui.get_quote(), "");

        // A removed original stays out of the message until put back.
        session.set_quote(&ui, Some(original()));
        session.remove_quote();
        assert_eq!(ui.get_quote(), "");
        assert!(session.quote.borrow().is_none());
        assert_eq!(ui.get_quote_left_out(), "reply");
        session.restore_quote();
        assert_eq!(ui.get_quote(), "reply");
        assert_eq!(ui.get_quote_left_out(), "");
        assert_eq!(session.quote.borrow().as_ref(), Some(&original()));
    }

    #[test]
    fn an_original_the_renderer_cannot_lay_out_shows_as_text() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        app.invoke_open_compose("".into());
        let session = composers.in_app().unwrap();
        let host = session.host().unwrap();
        let ui = host.ui();
        ui.set_editor_width(600.0);
        ui.set_editor_viewport_height(400.0);
        // An image at a relative address, which the renderer cannot resolve.
        let quote =
            ComposeQuote::reply("Maya wrote:", "Hello", Some("<p>Hello</p><img src=\"x\">"));
        session.set_quote(&ui, Some(quote));
        ui.set_quote_expanded(true);
        session.layout_changed();
        assert!(ui.get_quote_tiles().row_count() > 0);
        assert!(!ui.get_notice_is_error());
        assert!(!session.quote_unavailable.get());
    }

    #[test]
    fn an_original_wider_than_the_composer_scales_down_whole() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        app.invoke_open_compose("".into());
        let session = composers.in_app().unwrap();
        let host = session.host().unwrap();
        let ui = host.ui();
        ui.set_editor_width(300.0);
        ui.set_editor_viewport_height(400.0);
        // Wider than auto-fit zooms out to, at its least, for this width.
        let wide = "<table style=\"width:900px\"><tr><td>Wide</td></tr></table>";
        let forward = ComposeQuote::forward(&["Forwarded".to_owned()], "Wide", Some(wide));
        session.set_quote(&ui, Some(forward));
        session.layout_changed();
        let tiles = ui.get_quote_tiles();
        assert!(tiles.row_count() > 0);
        for tile in tiles.iter() {
            let size = tile.image.size();
            let shown = tile.height / 300.0;
            let drawn = size.height as f32 / size.width as f32;
            assert!((shown - drawn).abs() < 0.02, "shown {shown}, drawn {drawn}");
        }
    }
}
