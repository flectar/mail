//! Messages being written. Each composer is a session that owns its
//! document, attachments and send state. A message opens inside the main
//! window; on the desktop it can move into a window of its own, leaving the
//! main window free to read mail or start another message.

use super::*;
use crate::rich_compose::{EditMark, EditState};
use flectar_mail_core::models::{
    Address, AttachmentMeta, AutomaticCopies, Importance, SaveDraftArgs, SenderIdentity, Settings,
    Signature,
};
use slint::SharedString;
#[cfg(desktop)]
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use std::rc::Weak;

mod forward;
mod link_block;
mod links;
mod message_spelling;
mod photos;
mod placeholders;
mod plain_text;
pub(crate) use message_spelling::spelling_changed;
use message_spelling::{MessageSpelling, apply_spelling_state};
use photos::MessagePhotos;
use plain_text::PlainEdit;

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
    /// Where shrunk copies of attached photos are kept while their messages are open.
    temp_dir: PathBuf,
    templates: RefCell<Vec<Snippet>>,
    send_updates: UiSender<ComposeSendUpdate>,
    sessions: RefCell<Vec<Rc<ComposeSession>>>,
    /// Wakes the composers when a quoted message's image finished loading.
    quote_wake: UiWake,
    quote_poll_pending: Cell<bool>,
    /// Files dropped on the main window with no message open there, for the
    /// new one they start. They arrive one at a time.
    #[cfg(desktop)]
    dropped: RefCell<Vec<PathBuf>>,
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
    /// Downloads of a forward's files still on their way, and uploads of files sent by link,
    /// which sending waits for.
    attaching: Cell<u32>,
    /// Files the message sends by link, and the timer that shows how far along their uploads
    /// are.
    links: RefCell<Vec<links::LinkedFile>>,
    next_link: Cell<u64>,
    link_timer: Timer,
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
    /// Files dropped on the message, which go in together once the drop ends.
    #[cfg(desktop)]
    dropped: RefCell<Vec<PathBuf>>,
    /// The signature block this composer inserted, while the user has not
    /// edited it, and the signatures of its sending account.
    signature: RefCell<Option<String>>,
    signatures: RefCell<Vec<Signature>>,
    /// The signature this composer writes, in place of a message.
    signature_target: RefCell<Option<SignatureTarget>>,
    /// Bumped for each sending-account change; older results are dropped.
    preferences_generation: Cell<u64>,
    /// The copies mail from each of the sending account's identities starts
    /// with, by the identity's address in lowercase (#252).
    copies: RefCell<HashMap<String, AutomaticCopies>>,
    /// The copies this composer added to the message, which choosing another
    /// sender takes away.
    added_copies: RefCell<automatic_copies::Added>,
    /// A sender has been chosen for the message since it opened.
    sender_chosen: Cell<bool>,
    spelling: MessageSpelling,
    photos: MessagePhotos,
    /// The message goes out as plain text only.
    plain_text: Cell<bool>,
    /// The edit that took the message's formatting out, while undo can take it back.
    plain_edit: RefCell<Option<PlainEdit>>,
    /// An email template went into the message, whose placeholders sending checks for.
    templated: Cell<bool>,
    /// The placeholders the message was last sent with, which sending warned about.
    placeholder_warning: RefCell<Vec<String>>,
    /// The user said to send without a subject, or without the file the
    /// message mentions (#236), while sending goes on.
    subject_confirmed: Cell<bool>,
    attachment_confirmed: Cell<bool>,
    /// The subject a reply or forward started with, which mentions files of
    /// the original rather than the user's.
    opened_subject: RefCell<String>,
    /// Until the sending account's preference chooses how the message starts: whether it
    /// answers or forwards a plain-text message.
    format_pending: Cell<Option<bool>>,
    /// Closed, and waiting for the event loop to let its window go.
    disposed: Cell<bool>,
    /// When the message goes out, once Schedule send picked a time; taken
    /// by the save that schedules it.
    send_at: Cell<Option<i64>>,
    /// When the message reminds the user unless someone replies.
    remind_at: Cell<Option<i64>>,
    /// The picker of the message's window of its own, which asks there.
    #[cfg(desktop)]
    window_picker: RefCell<Option<when_picker::PickerState>>,
    /// The draft an AI agent asked to send, which this composer shows the
    /// user to check (#237).
    #[cfg(desktop)]
    reviewing: Cell<Option<i64>>,
}

/// What a composer asks the picker: when its message goes out, or when it
/// reminds the user unless someone replies.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TimeFor {
    Send,
    Remind,
}

/// A composer's question to the picker, answered in the window it asked in.
pub(super) struct Asking {
    session: SessionRef,
    what: TimeFor,
}

impl Asking {
    pub(super) fn reminds(&self) -> bool {
        self.what == TimeFor::Remind
    }
}

/// The picker's answer to `asking`: a time, or "Don't remind me" as `None`.
pub(super) fn answered(asking: Asking, at: Option<chrono::DateTime<chrono::Local>>) {
    if let Some(session) = asking.session.upgrade() {
        session.time_picked(asking.what, at);
    }
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
    temp_dir: PathBuf,
    pop_out: bool,
) -> Rc<Composers> {
    let (send_tx, send_rx) = bounded_ui_channel::<ComposeSendUpdate>();
    let composers = Rc::new(Composers {
        pop_out,
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
        documents,
        temp_dir,
        templates: RefCell::default(),
        send_updates: UiSender::new(
            send_tx,
            UiWake::new(app.as_weak(), |app| app.invoke_drain_compose_send_updates()),
        ),
        sessions: RefCell::default(),
        quote_wake: UiWake::new(app.as_weak(), |app| app.invoke_refresh_composer_quotes()),
        quote_poll_pending: Cell::new(false),
        #[cfg(desktop)]
        dropped: RefCell::default(),
    });
    COMPOSERS.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&composers)));
    #[cfg(desktop)]
    crate::agents::on_send_review(|draft_id, agent| {
        let posted = slint::invoke_from_event_loop(move || {
            let opened = with_composers(|composers| composers.review_send(draft_id, &agent));
            if opened != Some(true) {
                crate::agents::review_ended(draft_id);
            }
        });
        if posted.is_err() {
            crate::agents::review_ended(draft_id);
        }
    });

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
            if action == "forward_attachment" {
                composers.forward_shown_as_attachment(&app);
            } else {
                composers.open_from_message(&app, action.as_str());
            }
        }
    });
    // The AI summary's reply, or a suggested one, goes into a reply to the
    // shown message, for the user to edit and send or not (#241).
    let weak = Rc::downgrade(&composers);
    app.global::<AiSummary>().on_reply_with(move |text| {
        if let Some(composers) = weak.upgrade()
            && let Some(app) = composers.app.upgrade()
            && let Some(session) = composers.open_from_message(&app, "reply")
        {
            session.edit(|document| {
                document.update_selection(0, 0);
                document.insert_text(&format!("{}\n", text.trim()))
            });
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
/// Opens a signature, or a new one when `signature` is `None`, in a composer to write.
pub(super) fn edit_signature(app: &AppWindow, account_id: i64, signature: Option<Signature>) {
    with_composers(|composers| composers.edit_signature(app, account_id, signature));
}

/// Files dragged over the main window, for its open message to show where
/// they'll go, or the window when they'd start a new one.
#[cfg(desktop)]
pub(super) fn files_hovered(hovering: bool) {
    with_composers(|composers| match composers.in_app() {
        Some(session) => session.files_hovered(hovering),
        None => composers.new_message_hint(hovering),
    });
}

/// A file dropped on the main window goes into its open message, or starts a
/// new one.
#[cfg(desktop)]
pub(super) fn file_dropped(path: PathBuf) {
    with_composers(|composers| match composers.in_app() {
        Some(session) => session.file_dropped(path),
        None => composers.dropped_for_new_message(path),
    });
}

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

/// Composers list the accounts the main window lists that send mail. A
/// message whose account was removed, or stopped sending, moves to the one
/// new mail starts from.
pub(super) fn accounts_changed(app: &AppWindow) {
    with_composers(|composers| {
        let sessions = composers.sessions.borrow().clone();
        for session in sessions {
            let Some(host) = session.host() else {
                continue;
            };
            let ui = host.ui();
            let accounts = account_controller::sending_accounts(app);
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
            // A signature being written isn't a message, and has no draft.
            if session.disposed.get()
                || session.send_action.get().is_some()
                || session.signature_target.borrow().is_some()
            {
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

/// Where the message's own text is in `text`: above the signature, under its
/// `-- ` line, which stays as it is when the AI server improves the message.
fn own_text(text: &str) -> std::ops::Range<usize> {
    let end = text
        .rfind("\n-- \n")
        .filter(|at| !text[..*at].trim().is_empty())
        .unwrap_or(text.len());
    0..end
}

/// Translation's or the AI server's settings changed: composers offer to
/// translate the message once a service is set up (#240), and to improve it,
/// with the user's prompts, once an AI server is (#241).
pub(super) fn services_changed() {
    with_composers(|composers| {
        let sessions = composers.sessions.borrow().clone();
        for session in sessions {
            if let Some(host) = session.host() {
                let ui = host.ui();
                ui.set_improve_shown(crate::ai_features::ready());
                ui.set_ai_prompts(crate::ai_features::prompt_names());
                ui.set_translate_shown(crate::translation::ready());
            }
        }
    });
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
    to.set_attaching_files(from.get_attaching_files());
    to.set_links(from.get_links());
    to.set_link_sharing(from.get_link_sharing());
    to.set_plain_text(from.get_plain_text());
    to.set_translate_shown(from.get_translate_shown());
    to.set_translate_choices(from.get_translate_choices());
    to.set_translate_index(from.get_translate_index());
    to.set_improve_shown(from.get_improve_shown());
    to.set_ai_prompts(from.get_ai_prompts());
    to.set_improving(from.get_improving());
    to.set_request_receipt(from.get_request_receipt());
    to.set_importance(from.get_importance());
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
    to.set_remind_time(from.get_remind_time());
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
        self.start_session(app, Host::Main(app.as_weak()))
    }

    /// A new, empty composer in `host`.
    fn start_session(self: &Rc<Self>, app: &AppWindow, host: Host) -> Option<Rc<ComposeSession>> {
        let session = Rc::new_cyclic(|this| ComposeSession {
            this: this.clone(),
            composers: Rc::downgrade(self),
            host: RefCell::new(host),
            host_id: Cell::new(0),
            files: RefCell::default(),
            attaching: Cell::new(0),
            links: RefCell::default(),
            next_link: Cell::new(0),
            link_timer: Timer::default(),
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
            #[cfg(desktop)]
            dropped: RefCell::default(),
            signature: RefCell::default(),
            signatures: RefCell::default(),
            signature_target: RefCell::default(),
            preferences_generation: Cell::new(0),
            copies: RefCell::default(),
            added_copies: RefCell::default(),
            sender_chosen: Cell::new(false),
            spelling: MessageSpelling::default(),
            photos: MessagePhotos::default(),
            plain_text: Cell::new(false),
            plain_edit: RefCell::default(),
            templated: Cell::new(false),
            placeholder_warning: RefCell::default(),
            subject_confirmed: Cell::new(false),
            attachment_confirmed: Cell::new(false),
            opened_subject: RefCell::default(),
            format_pending: Cell::new(Some(false)),
            disposed: Cell::new(false),
            send_at: Cell::new(None),
            remind_at: Cell::new(None),
            #[cfg(desktop)]
            window_picker: RefCell::default(),
            #[cfg(desktop)]
            reviewing: Cell::new(None),
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
        ui.set_can_pop_out(self.pop_out && matches!(host, HostRef::Main(_)));
        ui.set_accounts(account_controller::sending_accounts(app));
        ui.set_account_id(app.get_compose_account_id());
        ui.set_from_label(app.get_compose_from_label());
        apply_email_templates(&ui, &self.templates.borrow());
        session.clear_fields(&host);
        self.sessions.borrow_mut().push(Rc::clone(&session));
        Some(session)
    }

    /// Opens a signature to write in a composer of its own: on the desktop in a window beside
    /// Settings and any message being written, and on phones in place of Settings.
    fn edit_signature(
        self: &Rc<Self>,
        app: &AppWindow,
        account_id: i64,
        signature: Option<Signature>,
    ) {
        let session = if self.pop_out {
            let Some(host) = new_window(app) else {
                return;
            };
            self.start_session(app, host)
        } else {
            // Phones show one composer, which a message being written keeps.
            if self.in_app().is_some() {
                app.global::<AccountMailPreferences>()
                    .set_status("Close the message you're writing to edit a signature.".into());
                return;
            }
            app.invoke_close_settings();
            self.open(app)
        };
        if let Some(session) = session {
            session.start_signature(account_id, signature, !self.pop_out);
        }
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

    /// Shows over the main window that files dropped there start a new
    /// message, while one could.
    #[cfg(desktop)]
    fn new_message_hint(&self, hovering: bool) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let can_start = !self.state.borrow().connected_accounts.is_empty();
        app.set_drop_starts_message(hovering && can_start);
    }

    /// A file dropped on the main window with no message open there. The
    /// others of the same drop follow at once, and they start one new
    /// message together.
    #[cfg(desktop)]
    fn dropped_for_new_message(self: &Rc<Self>, path: PathBuf) {
        let mut dropped = self.dropped.borrow_mut();
        dropped.push(path);
        if dropped.len() > 1 {
            return;
        }
        let composers = Rc::downgrade(self);
        Timer::single_shot(Duration::from_millis(30), move || {
            if let Some(composers) = composers.upgrade() {
                composers.start_with_dropped();
            }
        });
    }

    #[cfg(desktop)]
    fn start_with_dropped(self: &Rc<Self>) {
        let paths = std::mem::take(&mut *self.dropped.borrow_mut());
        let Some(app) = self.app.upgrade() else {
            return;
        };
        // No event ends the hover once files are dropped.
        app.set_drop_starts_message(false);
        if paths.is_empty() {
            return;
        }
        if let Some(session) = self.open_new(&app, "") {
            session.place_files(paths);
        }
    }

    /// Reply, reply all, forward, edit as new, or edit the selected draft.
    /// Opens the composer to answer, forward or edit the shown message, and
    /// returns its session.
    fn open_from_message(
        self: &Rc<Self>,
        app: &AppWindow,
        action: &str,
    ) -> Option<Rc<ComposeSession>> {
        let (row, conversation_message_id) = self.shown_message();
        let core = self.core();
        let Some(core) = core else {
            app.set_render_status(UiMessage::plain(
                "Connect an account before replying or forwarding.",
            ));
            return None;
        };
        let Some(row) = row else {
            app.set_render_status(UiMessage::plain("Choose a message first."));
            return None;
        };

        if action == "edit_draft" {
            self.open_draft(app, &core, &row);
            return None;
        }

        // Local mail is answered from an account.
        if self.state.borrow().connected_accounts.is_empty() {
            app.set_render_status(UiMessage::plain("Add an account to send mail."));
            return None;
        }
        let source = match self
            .runtime
            .block_on(core.load_compose_source(&row, conversation_message_id))
        {
            Ok(source) => source,
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not open composer: {}", error));
                return None;
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
                return None;
            }
        };
        let Ok(account_id) = i32::try_from(prepared.account_id) else {
            app.set_render_status(UiMessage::plain(
                "Could not open composer: invalid account identifier",
            ));
            return None;
        };

        let session = self.open(app)?;
        let host = session.host()?;
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
        *session.opened_subject.borrow_mut() = prepared.subject.clone();
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
            // HTML another app wrote opens with the formatting and images pasting keeps.
            if split.is_none()
                && saved
                    .and_then(|saved| {
                        document
                            .load_html(&saved.html, &saved.text)
                            .or_else(|| document.load_foreign_html(&saved.html))
                    })
                    .is_none()
            {
                document.load(&prepared.body, 0, 0);
            }
            split
        };
        session.set_quote(&ui, prepared.quote.or(quote));
        // A message edited as new keeps the original's format; replies and forwards start as
        // the account's preference says once it's loaded, which may follow the original's.
        if action == "edit_as_new" {
            session.start_format(prepared.plain_original);
        } else {
            session.format_pending.set(Some(prepared.plain_original));
        }
        *session.intent.borrow_mut() = prepared.intent;
        session.show(action, Some(prepared.sender_email));
        // A forward, and a message edited as new, carry the original's files.
        if matches!(action, "forward" | "edit_as_new") {
            session.attach_original_files(&core, source.attachments);
        }
        Some(session)
    }

    fn open_draft(self: &Rc<Self>, app: &AppWindow, core: &CoreMailSource, row: &MailMessage) {
        if row.folder != "Drafts" && row.send_at.is_none() && row.label != "DRAFT" {
            app.set_render_status(UiMessage::plain("Only drafts can be reopened for editing."));
            return;
        }
        if row.send_at.is_none() {
            self.load_draft(app, core, row);
            return;
        }
        // Editing a scheduled draft cancels its send first, as in Gmail. The
        // mail server may be holding it, so the window keeps going while the
        // server lets go, and the draft opens after.
        if !self.state.borrow_mut().start_send_change() {
            return;
        }
        let cancel: documents::DocumentFuture<bool> = {
            let (core, row) = (core.clone(), row.clone());
            Box::pin(async move { core.cancel_scheduled_row(&row).await })
        };
        let composers = Rc::downgrade(self);
        let state = Rc::clone(&self.state);
        let (core, row) = (core.clone(), row.clone());
        documents::run_request(&self.runtime, cancel, move |cancelled| {
            state.borrow_mut().changing_send = false;
            let Some(composers) = composers.upgrade() else {
                return;
            };
            let Some(app) = composers.app.upgrade() else {
                return;
            };
            match cancelled {
                Ok(true) => {
                    app.set_render_status(UiMessage::plain(
                        "Send cancelled. The message is a draft again.",
                    ));
                    // A message the user started meanwhile stays in the
                    // composer.
                    if composers.in_app().is_none_or(|open| open.is_blank()) {
                        composers.load_draft(&app, &core, &row);
                    }
                }
                // It went out, or is going out, meanwhile.
                Ok(false) => {
                    app.set_render_status(UiMessage::plain("This message is no longer scheduled."))
                }
                Err(error) => {
                    app.set_render_status(UiMessage::detail("Could not cancel the send: {}", error))
                }
            }
        });
    }

    /// Open the draft in the main window's composer.
    fn load_draft(self: &Rc<Self>, app: &AppWindow, core: &CoreMailSource, row: &MailMessage) {
        match self.runtime.block_on(core.load_draft(row)) {
            Ok(draft) => self.show_draft(app, draft),
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not open draft: {}", error));
            }
        }
    }

    /// Opens draft `draft_id`, which the AI agent `agent` asked to send, for
    /// the user to check and send themselves (#237). False when it couldn't
    /// open.
    #[cfg(desktop)]
    fn review_send(self: &Rc<Self>, draft_id: i64, agent: &str) -> bool {
        if let Some(open) = self.holding_draft(draft_id) {
            open.bring_forward();
            return self.mark_review(&open, draft_id, agent);
        }
        let Some(core) = self.core() else {
            return false;
        };
        let Ok(draft) = self.runtime.block_on(core.file_core().get_draft(draft_id)) else {
            return false;
        };
        self.show_review(draft, agent)
    }

    /// Opens `draft`, which an AI agent asked to send, in a window of its
    /// own, so a message the main window holds stays as it is.
    #[cfg(desktop)]
    fn show_review(self: &Rc<Self>, draft: SaveDraftArgs, agent: &str) -> bool {
        let (Some(app), Some(draft_id)) = (self.app.upgrade(), draft.draft_id) else {
            return false;
        };
        if let Some(open) = self.holding_draft(draft_id) {
            open.bring_forward();
            return self.mark_review(&open, draft_id, agent);
        }
        let Some(window) = new_window(&app) else {
            return false;
        };
        match self.show_written_in(&app, draft, "draft", Some(window)) {
            Some(session) => self.mark_review(&session, draft_id, agent),
            None => false,
        }
    }

    /// Notes over `session` that `agent` asked to send it.
    #[cfg(desktop)]
    fn mark_review(&self, session: &ComposeSession, draft_id: i64, agent: &str) -> bool {
        let (Some(app), Some(host)) = (self.app.upgrade(), session.host()) else {
            return false;
        };
        session.reviewing.set(Some(draft_id));
        host.ui().set_review_note(translated(
            &app,
            &UiMessage::detail(
                "{} asked to send this message. Check it, and send it only if it's right.",
                agent,
            ),
        ));
        true
    }

    /// Open a saved draft in a composer.
    fn show_draft(self: &Rc<Self>, app: &AppWindow, draft: SaveDraftArgs) {
        self.show_written(app, draft, "draft");
    }

    /// Open a written message in a composer: a saved draft (`mode` "draft"),
    /// or a new message the user has yet to look over ("new").
    fn show_written(self: &Rc<Self>, app: &AppWindow, draft: SaveDraftArgs, mode: &str) {
        self.show_written_in(app, draft, mode, None);
    }

    /// Open a written message in a composer in `window`, or in the main
    /// window's, and give the composer that shows it.
    fn show_written_in(
        self: &Rc<Self>,
        app: &AppWindow,
        draft: SaveDraftArgs,
        mode: &str,
        window: Option<Host>,
    ) -> Option<Rc<ComposeSession>> {
        // A draft already open in a composer comes forward instead of opening
        // twice, where each copy's saves would overwrite the other's.
        if let Some(open) = draft.draft_id.and_then(|id| self.holding_draft(id)) {
            open.bring_forward();
            return Some(open);
        }
        let Ok(account_id) = i32::try_from(draft.account_id) else {
            app.set_render_status(UiMessage::plain(
                "Could not open draft: invalid account identifier",
            ));
            return None;
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

        let session = match window {
            Some(window) => self.start_session(app, window),
            None => self.open(app),
        }?;
        let host = session.host()?;
        let ui = host.ui();
        ui.set_account_id(account_id);
        ui.set_from_label(account_label.into());
        ui.set_to(compose_addresses(&draft.to).into());
        ui.set_cc(compose_addresses(&draft.cc).into());
        ui.set_bcc(compose_addresses(&draft.bcc).into());
        ui.set_subject(draft.subject.clone().into());
        ui.set_request_receipt(draft.request_read_receipt);
        ui.set_importance(i32::try_from(draft.importance.as_i64()).unwrap_or_default());
        session.opened_subject.borrow_mut().clear();

        *session.files.borrow_mut() = draft
            .attachments
            .iter()
            .map(|attachment| {
                let path = PathBuf::from(&attachment.file_path);
                let size = std::fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or_default();
                ComposeFile {
                    shrinkable: crate::photo_shrink::can_shrink_file(&path),
                    path,
                    filename: attachment.filename.clone(),
                    size,
                    _import: None,
                    original: None,
                }
            })
            .collect();
        apply_compose_files(&ui, &session.files.borrow());
        // A draft saved as plain text has text and no HTML.
        let plain = draft
            .body_html
            .as_deref()
            .is_none_or(|html| html.trim().is_empty())
            && !draft.body_text.trim().is_empty();
        let quote = {
            let mut document = session.document.borrow_mut();
            // Files the draft sends by link come back as such.
            let html = session.restore_links(draft.body_html.as_deref().unwrap_or_default());
            let html = html.as_str();
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
        session.start_format(plain);
        *session.intent.borrow_mut() = ComposeIntent {
            mode: draft.mode,
            in_reply_to_message_id: draft.in_reply_to_message_id,
            in_reply_to_header: draft.in_reply_to_header,
            draft_id: draft.draft_id,
        };
        session.show(mode, Some(draft_sender_email));
        Some(session)
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
        self.refresh_photos();
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
            // the reader's 16px default. A plain-text message shows the
            // original as the text that goes out.
            let html = if self.plain_text.get() {
                plain_text::plain_quote_html(&quote.plain)
            } else {
                format!(
                    "<body style=\"margin:8px 0;font-size:{}px\">{}</body>",
                    crate::compose_editor::FONT_SIZE_LOGICAL,
                    quote.html
                )
            };
            // An original the renderer can't lay out shows as its text, as
            // the reader shows such a message; what is sent stays the same.
            let prepared = renderer.prepare_email_html(&html, false).or_else(|error| {
                tracing::warn!(%error, "quoted message shown as text");
                renderer.prepare_email_html(&plain_text::plain_quote_html(&quote.plain), false)
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
        self.reset_send_checks();
        self.opened_subject.borrow_mut().clear();
        ui.set_send_check_open(false);
        ui.set_editor_tiles(ModelRc::default());
        apply_compose_files(&ui, &[]);
        ui.set_links(ModelRc::default());
        ui.set_attaching_files(false);
        photos::apply_photos(&ui, &photos::PhotoSummary::default());
        ui.set_plain_text(false);
        ui.set_translate_shown(crate::translation::ready());
        ui.set_translate_open(false);
        ui.set_translating(false);
        ui.set_improve_shown(crate::ai_features::ready());
        ui.set_ai_prompts(crate::ai_features::prompt_names());
        ui.set_improving(false);
        apply_compose_contacts(&ui, &[]);
        ui.set_sending(false);
        ui.set_mode("new".into());
        ui.set_importance(0);
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
        ui.set_remind_time("".into());
        ui.set_notice(UiMessage::EMPTY, false);
        ui.set_review_note("".into());
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
        ui.set_link_sharing(self.can_share_by_link());
        // A signature has no sender, and no signature of its own to insert.
        if mode != "signature" {
            let preferences = host.preferences();
            preferences.invoke_composer_reset();
            if let Some(sender_email) = sender_email {
                preferences.set_composer_sender_email(sender_email.into());
            }
            preferences.invoke_composer_account_changed(ui.get_account_id());
        }
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
    /// minimized, hands its help to the keyboard on a key press as the main
    /// window does, and quits the app for Control+Q as the main window does.
    #[cfg(desktop)]
    fn watch_window(self: &Rc<Self>, window: &ComposerWindow) {
        let session = self.reference();
        let help = window.as_weak();
        #[cfg(linux_desktop)]
        let modifiers = std::cell::Cell::new(winit::keyboard::ModifiersState::empty());
        window.window().on_winit_window_event(move |native, event| {
            use winit::event::WindowEvent;
            if !matches!(event, WindowEvent::RedrawRequested) {
                crate::memory::release_free_heap_when_idle();
            }
            if let WindowEvent::KeyboardInput { event, .. } = event
                && event.state.is_pressed()
                && let Some(window) = help.upgrade()
            {
                window.global::<crate::ActionTooltip>().set_keyboard(true);
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
            match event {
                WindowEvent::HoveredFile(_) | WindowEvent::HoveredFileCancelled => {
                    if let Some(session) = session.upgrade() {
                        session.files_hovered(matches!(event, WindowEvent::HoveredFile(_)));
                    }
                }
                WindowEvent::DroppedFile(path) => {
                    if let Some(session) = session.upgrade() {
                        session.file_dropped(path.clone());
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
        #[cfg(desktop)]
        if let Some(draft_id) = self.reviewing.take() {
            crate::agents::review_ended(draft_id);
        }
        self.files.borrow_mut().clear();
        self.attaching.set(0);
        // Files already shared stay so: a sent message links to them.
        self.links.borrow_mut().clear();
        self.link_timer.stop();
        self.templated.set(false);
        self.placeholder_warning.borrow_mut().clear();
        self.photos.clear();
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
        *self.signature_target.borrow_mut() = None;
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

    /// Ask when the message goes out, or when it reminds the user, in the
    /// window that shows it.
    fn ask_time(self: &Rc<Self>, what: TimeFor) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        let Some(app) = composers.app.upgrade() else {
            return;
        };
        if self.send_action.get().is_some() {
            return;
        }
        let asking = Asking {
            session: self.reference(),
            what,
        };
        // "Don't remind me" ends a reminder already set.
        let droppable = what == TimeFor::Remind && self.remind_at.get().is_some();
        let placeholder = app.get_time_format_sample();
        match &host {
            HostRef::Main(main) => {
                if let Some(picker) = when_picker::main_picker() {
                    when_picker::ask(
                        &main.global::<WhenUi>(),
                        placeholder,
                        &picker,
                        asking,
                        droppable,
                    );
                }
            }
            #[cfg(desktop)]
            HostRef::Window(window) => {
                if let Some(picker) = self.window_picker.borrow().as_ref() {
                    when_picker::ask(
                        &window.global::<WhenUi>(),
                        placeholder,
                        picker,
                        asking,
                        droppable,
                    );
                }
            }
        }
    }

    /// The picker's answer for this message.
    fn time_picked(self: &Rc<Self>, what: TimeFor, at: Option<chrono::DateTime<chrono::Local>>) {
        if self.disposed.get() {
            return;
        }
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        match (what, at) {
            // The message reminds the user once it went out.
            (TimeFor::Remind, Some(at)) => {
                self.remind_at.set(Some(at.timestamp_millis()));
                ui.set_remind_time(when_picker::when_label(at, chrono::Local::now()).into());
            }
            (TimeFor::Remind, None) => {
                self.remind_at.set(None);
                ui.set_remind_time("".into());
            }
            // The message goes out at that time.
            (TimeFor::Send, Some(at)) => {
                // A reminder at or before then would come as it goes out.
                if self
                    .remind_at
                    .get()
                    .is_some_and(|remind_at| remind_at <= at.timestamp_millis())
                {
                    ui.set_notice(
                        UiMessage::plain("Choose a reminder after the time the message goes out."),
                        true,
                    );
                    return;
                }
                self.send_at.set(Some(at.timestamp_millis()));
                self.save(
                    ui.get_account_id(),
                    &ui.get_to(),
                    &ui.get_cc(),
                    &ui.get_bcc(),
                    &ui.get_subject(),
                    &ui.get_body(),
                    true,
                );
                self.send_at.set(None);
            }
            (TimeFor::Send, None) => {}
        }
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
        ui.on_save_signature(move || {
            if let Some(session) = weak.upgrade() {
                session.save_signature();
            }
        });
        let weak = self.reference();
        ui.on_save(move |account_id, to, cc, bcc, subject, body, send| {
            if let Some(session) = weak.upgrade() {
                session.save(account_id, &to, &cc, &bcc, &subject, &body, send);
            }
        });
        let weak = self.reference();
        ui.on_schedule_send(move || {
            if let Some(session) = weak.upgrade() {
                session.ask_time(TimeFor::Send);
            }
        });
        let weak = self.reference();
        ui.on_remind(move || {
            if let Some(session) = weak.upgrade() {
                session.ask_time(TimeFor::Remind);
            }
        });
        // A message in a window of its own asks there.
        #[cfg(desktop)]
        if let HostRef::Window(window) = &host {
            *self.window_picker.borrow_mut() = Some(when_picker::attach(window));
        }
        let weak = self.reference();
        ui.on_answer_send_check(move |answer| {
            if let Some(session) = weak.upgrade() {
                session.answer_send_check(&answer);
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
        ui.on_translate_opened(move || {
            if let Some(session) = weak.upgrade() {
                session.translate_opened();
            }
        });
        let weak = self.reference();
        ui.on_translate(move |index, body, anchor, cursor| {
            if let Some(session) = weak.upgrade() {
                session.translate(index, &body, anchor, cursor);
            }
        });
        let weak = self.reference();
        ui.on_improve(move |index, body, anchor, cursor| {
            if let Some(session) = weak.upgrade() {
                session.improve(index, &body, anchor, cursor);
            }
        });
        let weak = self.reference();
        ui.on_edit_prompts(move || {
            if let Some(app) = weak
                .upgrade()
                .and_then(|session| session.composers())
                .and_then(|composers| composers.app.upgrade())
            {
                crate::ai_features::open_settings(&app);
            }
        });
        let weak = self.reference();
        ui.on_remove_attachment(move |index| {
            if let Some(session) = weak.upgrade() {
                session.remove_attachment(index);
            }
        });
        let weak = self.reference();
        ui.on_shrink_photos_toggled(move |on| {
            if let Some(session) = weak.upgrade() {
                session.set_shrink_photos(on);
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
            // Plain text has no formatting to apply, also from the keyboard.
            if let Some(session) = weak.upgrade()
                && !session.plain_text.get()
            {
                session.edit(|document| document.format(&kind, &body, anchor, cursor));
            }
        });
        let weak = self.reference();
        ui.on_plain_text_chosen(move |plain| {
            if let Some(session) = weak.upgrade() {
                session.set_plain_text(plain);
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
        ui.on_splice_text(|text, start, end, insert| {
            placeholders::splice(&text, start, end, &insert).into()
        });
        let weak = self.reference();
        ui.on_remove_link(move |index| {
            if let Some(session) = weak.upgrade() {
                session.remove_link(index);
            }
        });
        let weak = self.reference();
        ui.on_share_by_link(move || {
            if let Some(session) = weak.upgrade() {
                session.pick_link_files();
            }
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
        preferences.on_composer_sender_search(move |query| {
            if let Some(session) = weak.upgrade() {
                session.search_senders(&query);
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
        let back_to_settings = self
            .signature_target
            .borrow()
            .as_ref()
            .is_some_and(|target| target.return_to_settings);
        self.dispose();
        if back_to_settings
            && let Some(app) = self
                .composers()
                .and_then(|composers| composers.app.upgrade())
        {
            app.invoke_open_settings();
        }
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
        self.refresh_photos();
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
        // Plain text takes what's pasted as text.
        if self.plain_text.get() {
            return false;
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
                files.saturating_add(self.photos.image_bytes(document))
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
        self.refresh_photos();
        true
    }

    /// Fills the Translate dialog (#240) with the service's languages, the one the last message
    /// was translated into chosen, else the language of the message a reply answers.
    fn translate_opened(&self) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        let ui = host.ui();
        ui.set_translate_failure("".into());
        ui.set_translate_detail("".into());
        let Some(core) = composers.core().map(|core| core.file_core()) else {
            return;
        };
        let answered = self.intent.borrow().in_reply_to_message_id;
        let task = composers.runtime.spawn(async move {
            let settings = core.translation_settings().await?;
            let targets = core.translation_targets().await?;
            let language = match answered {
                Some(id) => core
                    .translation_offer(id, Default::default())
                    .await
                    .ok()
                    .and_then(|offer| offer.language),
                None => None,
            };
            Ok::<_, flectar_mail_core::error::CoreError>((
                settings.compose_target,
                targets
                    .into_iter()
                    .map(|target| (target.code, target.name))
                    .collect::<Vec<_>>(),
                language,
            ))
        });
        let weak = self.this.clone();
        let abort = task.abort_handle();
        if slint::spawn_local(async move {
            let result = task.await;
            let Some(session) = weak.upgrade() else {
                return;
            };
            let Some(host) = session.host() else {
                return;
            };
            let ui = host.ui();
            match result {
                Ok(Ok((last, targets, language))) => {
                    let (names, index) =
                        crate::translation::draft_choices(targets, &last, language.as_deref());
                    ui.set_translate_choices(ModelRc::new(VecModel::from(names)));
                    ui.set_translate_index(index);
                }
                Ok(Err(error)) => {
                    let (kind, detail) = crate::translation::failure(&error);
                    ui.set_translate_failure(kind.into());
                    ui.set_translate_detail(detail.into());
                }
                Err(_) => {}
            }
        })
        .is_err()
        {
            abort.abort();
        }
    }

    /// Translates the selection of `body`, or the whole message when nothing is selected, into
    /// the dialog's choice `index` (#240). The translation takes the place of what was
    /// translated, as one step undo takes back. Text edited while the service works stays as
    /// it is.
    fn translate(&self, index: i32, body: &str, anchor: i32, cursor: i32) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        let Some(target) = crate::translation::draft_target(index) else {
            return;
        };
        let Some(core) = composers.core().map(|core| core.file_core()) else {
            return;
        };
        let plain = self.plain_text.get();
        let (revision, range, html, text) = {
            let mut document = self.document.borrow_mut();
            if document.text() != body {
                document.synchronize(body, anchor, cursor);
            } else {
                document.update_selection(anchor, cursor);
            }
            let selection = document.selection();
            let (mut start, mut end) = (
                selection.start.min(selection.end),
                selection.start.max(selection.end),
            );
            if start == end {
                start = 0;
                end = i32::try_from(document.text().len()).unwrap_or(i32::MAX);
                document.update_selection(start, end);
            }
            let Some((html, text)) = document.selection_html() else {
                return;
            };
            let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
                return;
            };
            (document.revision(), start..end, html, text)
        };
        let ui = host.ui();
        ui.set_translating(true);
        ui.set_translate_failure("".into());
        ui.set_translate_detail("".into());
        crate::translation::remember_draft_target(&target);
        let html = crate::rich_compose::without_copied_mark(&html).to_owned();
        let task = composers.runtime.spawn(async move {
            if plain {
                core.translate_draft_text(text, target).await
            } else {
                core.translate_draft_html(html, target).await
            }
        });
        let weak = self.this.clone();
        let abort = task.abort_handle();
        if slint::spawn_local(async move {
            let result = task.await;
            if let Some(session) = weak.upgrade() {
                session.translated(revision, range, plain, result);
            }
        })
        .is_err()
        {
            abort.abort();
            ui.set_translating(false);
        }
    }

    fn translated(
        &self,
        revision: u64,
        range: std::ops::Range<usize>,
        plain: bool,
        result: Result<Result<String, flectar_mail_core::error::CoreError>, tokio::task::JoinError>,
    ) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        ui.set_translating(false);
        let translation = match result {
            Ok(Ok(translation)) => translation,
            Ok(Err(error)) => {
                let (kind, detail) = crate::translation::failure(&error);
                ui.set_translate_failure(kind.into());
                ui.set_translate_detail(detail.into());
                return;
            }
            Err(error) => {
                ui.set_translate_failure("service".into());
                ui.set_translate_detail(error.to_string().into());
                return;
            }
        };
        let followed = self.document.borrow().follow(revision, range);
        let range = match followed {
            crate::rich_compose::Followed::Kept(range)
            | crate::rich_compose::Followed::Bordered(range) => range,
            crate::rich_compose::Followed::Lost => {
                ui.set_translate_failure("changed".into());
                return;
            }
        };
        let (Ok(start), Ok(end)) = (i32::try_from(range.start), i32::try_from(range.end)) else {
            return;
        };
        self.document.borrow_mut().update_selection(start, end);
        if plain {
            self.edit(|document| document.insert_text(&translation));
        } else {
            self.paste_html(&host, &crate::rich_compose::marked_as_copied(&translation));
        }
        ui.set_translate_open(false);
    }

    /// Runs Proofread, -1, or the user's prompt `index` on the selection of `body`, or on the
    /// message above its signature when nothing is selected (#241). What the AI server writes
    /// takes the place of the words it changes, as one step undo takes back, and words it keeps
    /// keep their formatting. Text edited while the server works stays as it is.
    fn improve(&self, index: i32, body: &str, anchor: i32, cursor: i32) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        let Some(instruction) = crate::ai_features::instruction(index) else {
            return;
        };
        let Some(core) = composers.core().map(|core| core.file_core()) else {
            return;
        };
        let ui = host.ui();
        let (revision, range, text, selected) = {
            let mut document = self.document.borrow_mut();
            if document.text() != body {
                document.synchronize(body, anchor, cursor);
            } else {
                document.update_selection(anchor, cursor);
            }
            let selection = document.selection();
            let (Ok(start), Ok(end)) = (
                usize::try_from(selection.start.min(selection.end)),
                usize::try_from(selection.start.max(selection.end)),
            ) else {
                return;
            };
            let selected = start != end;
            let range = if selected {
                start..end
            } else {
                own_text(document.text())
            };
            // White space around the text stays as it is.
            let text = &document.text()[range.clone()];
            let lead = text.len() - text.trim_start().len();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                ui.set_notice(UiMessage::plain("Write something to improve first."), false);
                return;
            }
            let range = range.start + lead..range.start + lead + trimmed.len();
            (document.revision(), range, trimmed.to_owned(), selected)
        };
        ui.set_improving(true);
        ui.set_notice(UiMessage::plain("Asking the AI server…"), false);
        let task = composers
            .runtime
            .spawn(async move { core.ai_rewrite(text, instruction).await });
        let weak = self.this.clone();
        let abort = task.abort_handle();
        if slint::spawn_local(async move {
            let result = task.await;
            if let Some(session) = weak.upgrade() {
                session.improved(revision, range, selected, result);
            }
        })
        .is_err()
        {
            abort.abort();
            ui.set_improving(false);
            ui.set_notice(UiMessage::EMPTY, false);
        }
    }

    fn improved(
        &self,
        revision: u64,
        range: std::ops::Range<usize>,
        selected: bool,
        result: Result<Result<String, flectar_mail_core::error::CoreError>, tokio::task::JoinError>,
    ) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        ui.set_improving(false);
        let revised = match result {
            Ok(Ok(revised)) if !revised.trim().is_empty() => revised,
            Ok(Ok(_)) => {
                ui.set_notice(
                    UiMessage::plain(
                        "The AI server sent back no text, so the message stays as it is.",
                    ),
                    true,
                );
                return;
            }
            Ok(Err(error)) => {
                ui.set_notice(crate::ai_features::failure_notice(&error), true);
                return;
            }
            Err(error) => {
                ui.set_notice(
                    UiMessage::detail(
                        "The AI server didn't answer as expected. It said: {}",
                        error,
                    ),
                    true,
                );
                return;
            }
        };
        let followed = self.document.borrow().follow(revision, range);
        let range = match followed {
            crate::rich_compose::Followed::Kept(range)
            | crate::rich_compose::Followed::Bordered(range) => range,
            crate::rich_compose::Followed::Lost => {
                ui.set_notice(
                    UiMessage::plain(
                        "The text changed while the AI server worked, so it stays as it is.",
                    ),
                    true,
                );
                return;
            }
        };
        let mut changed = false;
        self.edit(
            |document| match document.revise(range, &revised, selected) {
                Some(selection) => {
                    changed = true;
                    selection
                }
                None => document.selection(),
            },
        );
        ui.set_notice(
            if changed {
                UiMessage::plain(
                    "Changed by AI. Check it before you send it. Undo restores your text.",
                )
            } else {
                UiMessage::plain("The AI server found nothing to change.")
            },
            false,
        );
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
            // Plain text copies as text.
            if self.plain_text.get() {
                return false;
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
        self.refresh_photos();
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
        self.follow_plain_edit(&self.document.borrow());
        self.check_spelling_soon();
        self.refresh_photos();
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

    /// What sending asks first, if anything: whether to send without a
    /// subject, then without the file the message mentions, as the first
    /// keyword it mentions. Only what the user wrote counts.
    fn send_check(
        &self,
        app: &AppWindow,
        subject: &str,
        body: &str,
    ) -> Option<(&'static str, String)> {
        if !self.subject_confirmed.get() && subject.trim().is_empty() {
            return Some(("subject", String::new()));
        }
        let carries_files = !self.files.borrow().is_empty()
            || !self.links.borrow().is_empty()
            || self.document.borrow().images().next().is_some();
        if self.attachment_confirmed.get() || carries_files {
            return None;
        }
        let own_subject =
            (subject.trim() != self.opened_subject.borrow().trim()).then_some(subject);
        let text =
            attachment_reminder::own_text(own_subject, body, self.signature.borrow().as_deref());
        let keywords = attachment_reminder::keywords();
        let local = translated(app, &keywords);
        attachment_reminder::mentioned(&text, &[keywords.key(), local.as_str()])
            .map(|keyword| ("attachment", keyword))
    }

    /// Sends on, attaches files, or goes back to the message, as the user
    /// answered what sending asked.
    fn answer_send_check(self: &Rc<Self>, answer: &str) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        match answer {
            "send" => {
                if ui.get_send_check() == "subject" {
                    self.subject_confirmed.set(true);
                } else {
                    self.attachment_confirmed.set(true);
                }
                self.save(
                    ui.get_account_id(),
                    &ui.get_to(),
                    &ui.get_cc(),
                    &ui.get_bcc(),
                    &ui.get_subject(),
                    &ui.get_body(),
                    true,
                );
            }
            "attach" => {
                self.reset_send_checks();
                self.pick_attachments();
            }
            _ => self.reset_send_checks(),
        }
    }

    fn reset_send_checks(&self) {
        self.subject_confirmed.set(false);
        self.attachment_confirmed.set(false);
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
            session.attach(
                documents
                    .into_iter()
                    .map(|document| NewAttachment {
                        path: document.path().to_path_buf(),
                        filename: document.name(),
                        import: Some(document),
                    })
                    .collect(),
            );
        });
    }

    /// Attaches `new`, leaving out files already attached, ones that can't be read, and any
    /// that would take the message's images and attachments past what it carries, which go
    /// by link instead when a storage service is connected, and says what went in.
    fn attach(self: &Rc<Self>, new: Vec<NewAttachment>) {
        if new.is_empty() {
            return;
        }
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        if self.signature_target.borrow().is_some() {
            ui.set_notice(
                UiMessage::plain("A signature can hold pictures, but not attachments."),
                true,
            );
            return;
        }
        let mut files = self.files.borrow_mut();
        let mut total_size = files
            .iter()
            .map(|file| file.size)
            .sum::<u64>()
            .saturating_add(self.photos.image_bytes(&self.document.borrow()));
        let mut added = 0usize;
        let mut skipped = 0usize;
        let mut shrinking = Vec::new();
        let mut by_link = Vec::new();
        let mut linking = None;
        for document in new {
            let path = std::fs::canonicalize(&document.path).unwrap_or(document.path);
            if files.iter().any(|file| file.holds(&path)) || self.photos.waiting_for(&path) {
                skipped += 1;
                continue;
            }
            let Ok(metadata) = std::fs::metadata(&path) else {
                skipped += 1;
                continue;
            };
            let Some(filename) = document.filename.filter(|_| metadata.is_file()) else {
                skipped += 1;
                continue;
            };
            let shrinkable = crate::photo_shrink::can_shrink_file(&path);
            // A file the message can't carry goes by link, where a storage service is
            // connected.
            if !(shrinkable && self.photos.on())
                && total_size.saturating_add(metadata.len()) > MAX_COMPOSE_ATTACHMENT_BYTES
            {
                if *linking.get_or_insert_with(|| self.can_share_by_link()) {
                    by_link.push(NewAttachment {
                        path,
                        filename: Some(filename),
                        import: document.import,
                    });
                } else {
                    skipped += 1;
                }
                continue;
            }
            let file = ComposeFile {
                shrinkable,
                path,
                filename,
                size: metadata.len(),
                _import: document.import.map(Rc::new),
                original: None,
            };
            // A photo going out shrunk goes in at the size it'll have.
            if file.shrinkable && self.photos.on() {
                shrinking.push(file);
                continue;
            }
            total_size += file.size;
            files.push(file);
            added += 1;
        }
        apply_compose_files(&ui, &files);
        drop(files);
        if !by_link.is_empty() {
            ui.set_notice(
                UiMessage::plain("Files over the 25 MB limit go as links."),
                false,
            );
            self.share_by_link(by_link);
        } else if shrinking.is_empty() || added + skipped > 0 {
            let (notice, error) = attachments_notice(added, skipped);
            ui.set_notice(notice, error);
        }
        self.photos.attach_when_shrunk(shrinking);
        self.refresh_photos();
    }

    /// The storage service the message's files go by link from, as its account says: the
    /// account's own Google Drive or OneDrive, else the Nextcloud or ownCloud server.
    pub(super) fn link_service(&self) -> Option<flectar_mail_core::links::LinkService> {
        let (composers, host) = (self.composers()?, self.host()?);
        let core = composers.core()?;
        let account_id = i64::from(host.ui().get_account_id());
        composers
            .runtime
            .block_on(core.link_service_for_account(account_id))
            .ok()
            .flatten()
    }

    /// Whether the message's files can go by link.
    fn can_share_by_link(&self) -> bool {
        self.link_service().is_some()
    }

    /// Files dragged over the message's window show where they'll go, until they're dropped
    /// or taken away.
    #[cfg(desktop)]
    fn files_hovered(&self, hovering: bool) {
        if let Some(host) = self.host() {
            let ui = host.ui();
            ui.set_drop_hint(hovering && !ui.get_sending());
        }
    }

    /// A file dropped on the message. Several come one after another, and go in together.
    #[cfg(desktop)]
    fn file_dropped(self: &Rc<Self>, path: PathBuf) {
        let mut dropped = self.dropped.borrow_mut();
        dropped.push(path);
        if dropped.len() > 1 {
            return;
        }
        let weak = Rc::downgrade(self);
        Timer::single_shot(Duration::from_millis(30), move || {
            if let Some(session) = weak.upgrade() {
                session.place_dropped_files();
            }
        });
    }

    #[cfg(desktop)]
    fn place_dropped_files(self: &Rc<Self>) {
        let paths = std::mem::take(&mut *self.dropped.borrow_mut());
        self.place_files(paths);
    }

    /// Puts dropped pictures in the text at the caret, as pasting does, and attaches the
    /// other files.
    #[cfg(desktop)]
    fn place_files(self: &Rc<Self>, paths: Vec<PathBuf>) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        ui.set_drop_hint(false);
        if ui.get_sending() {
            return;
        }
        let mut attachments = Vec::new();
        for path in paths {
            // Plain text holds no pictures, so they're attached.
            let image = (is_image_name(&path) && !self.plain_text.get())
                .then(|| std::fs::metadata(&path).ok())
                .flatten()
                .filter(|metadata| {
                    metadata.is_file() && metadata.len() <= MAX_COMPOSE_ATTACHMENT_BYTES
                })
                .and_then(|_| std::fs::read(&path).ok())
                .and_then(|bytes| crate::rich_compose::InlineImage::from_file_bytes(bytes, ""));
            match image {
                Some(image) => {
                    if !self.insert_image(image) {
                        break;
                    }
                }
                None => attachments.push(NewAttachment {
                    filename: path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned()),
                    path,
                    import: None,
                }),
            }
        }
        self.attach(attachments);
    }

    /// Opens a picker for images to put in the text at the caret.
    fn pick_images(self: &Rc<Self>) {
        if self.plain_text.get() {
            return;
        }
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
                .saturating_add(self.photos.image_bytes(&self.document.borrow()))
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
            drop(files);
            self.refresh_photos();
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
        self.count_attaching(true);
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
            session.count_attaching(false);
            let Some(host) = session.host() else {
                return;
            };
            let mut files = session.files.borrow_mut();
            let mut total_size = files
                .iter()
                .map(|file| file.size)
                .sum::<u64>()
                .saturating_add(session.photos.image_bytes(&session.document.borrow()));
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
                            shrinkable: crate::photo_shrink::can_shrink_file(&path),
                            path,
                            filename,
                            size,
                            _import: None,
                            original: None,
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
            drop(files);
            session.refresh_photos();
        });
    }

    /// Count a download of a forward's files starting or finishing: sending waits while any
    /// is on its way.
    fn count_attaching(&self, started: bool) {
        let count = if started {
            self.attaching.get().saturating_add(1)
        } else {
            self.attaching.get().saturating_sub(1)
        };
        self.attaching.set(count);
        if let Some(host) = self.host() {
            host.ui().set_attaching_files(count > 0);
        }
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
        // Set by Schedule send just before, for this save alone.
        let send_at = self.send_at.take().filter(|_| send);
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
        // Photos going out shrunk are sent once they are, and a forward once its files are here.
        if send && (self.photos.on() && self.photos.busy() || self.attaching.get() > 0) {
            return;
        }
        // A file that couldn't go up by link holds the message back until it's removed.
        if send && let Some(name) = self.failed_link() {
            ui.set_notice(
                UiMessage::detail("Remove {} to send the message.", name),
                true,
            );
            return;
        }
        // A message from a template says which placeholders it still has, and goes as it is
        // when sent again.
        if send && self.templated.get() {
            let left = placeholders::placeholders_in(&format!("{subject}\n{body}"));
            if !left.is_empty() && *self.placeholder_warning.borrow() != left {
                ui.set_notice(
                    UiMessage::detail("Fill in {} or send again.", left.join(", ")),
                    true,
                );
                *self.placeholder_warning.borrow_mut() = left;
                return;
            }
        }
        if account_id <= 0 {
            ui.set_notice(
                UiMessage::plain("Choose the account this message should be sent from."),
                true,
            );
            return;
        }
        // A message without a subject, or that mentions a file it doesn't
        // carry, goes once the user says so (#236).
        if send && let Some((check, keyword)) = self.send_check(&app, subject, body) {
            ui.set_send_check(check.into());
            ui.set_send_check_keyword(keyword.into());
            ui.set_send_check_open(true);
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
            // Photos go out shrunk when the message's photos do.
            let html = document.body_html_with(self.quote.borrow().is_some(), |image| {
                self.photos.shrunk(image)
            });
            // Mail read as plain text has each link's address and each image's name in
            // its place. A draft keeps the placeholders, which the HTML's images reopen
            // into.
            (html, send.then(|| document.plain_text()))
        };
        let body = described.as_deref().unwrap_or(body);
        // Files sent by link go in a block after what's written. A draft keeps the block in
        // its HTML only, with each link's details, to reopen as the files.
        let (body, body_html) = match self.link_blocks(!send) {
            Some((html_block, text_block)) => {
                let rich = !self.plain_text.get();
                let text = if send || !rich {
                    match body.trim_end() {
                        "" => text_block,
                        written => format!("{written}\n\n{text_block}"),
                    }
                } else {
                    body.to_owned()
                };
                let html = if rich {
                    Some(format!("{}{html_block}", body_html.unwrap_or_default()))
                } else {
                    body_html
                };
                (text, html)
            }
            None => (body.to_owned(), body_html),
        };
        let body = body.as_str();
        // The original goes below what the user wrote, in both parts.
        let (body, body_html) = match self.quote.borrow().as_ref() {
            Some(quote) => {
                let (text, html) = quote.below(body, body_html);
                (text, Some(html))
            }
            None => (body.to_owned(), body_html),
        };
        // A plain-text message goes out, and saves, without HTML.
        let body_html = body_html.filter(|_| !self.plain_text.get());
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
            request_read_receipt: ui.get_request_receipt(),
            importance: Importance::from_i64(i64::from(ui.get_importance())),
        };
        let remind_at = self.remind_at.get();
        if let Some(send_at) = send_at {
            match composers
                .runtime
                .block_on(core.schedule_new_message(message, send_at, remind_at))
            {
                Ok(_) => {
                    self.dispose();
                    app.set_sync_status(UiMessage::detail(
                        "Scheduled to send {}",
                        mail_view_model::upcoming_time(send_at, &chrono::Local::now()),
                    ));
                    let _ =
                        refresh_from_source(&app, &composers.state, &composers.runtime, true, &[]);
                    mailto_links::open_pending_soon(&app);
                }
                Err(error) => {
                    let message = UiMessage::detail("Could not schedule the message: {}", error);
                    ui.set_notice(message.clone(), true);
                    app.set_sync_status(message);
                }
            }
            return;
        }
        if send {
            let queued = composers
                .runtime
                .block_on(core.send_new_message(message, remind_at))
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

        // Its placeholders take the message's first recipient, its sender and today's date,
        // and those it can't fill stay for the writer.
        let values = self.placeholder_values(&composers, &host);
        let mut left = Vec::new();
        if ui.get_subject().trim().is_empty()
            && let Some(subject) = template.subject.as_deref()
            && !subject.trim().is_empty()
        {
            let subject = placeholders::fill(subject, &values);
            left = placeholders::placeholders_in(&subject);
            ui.set_subject(subject.into());
        }
        let body = placeholders::fill(&template.body_text, &values);
        for placeholder in placeholders::placeholders_in(&body) {
            if !left.contains(&placeholder) {
                left.push(placeholder);
            }
        }
        self.edit(|document| document.insert_text(&body));
        self.templated.set(true);
        let inserted = if left.is_empty() {
            UiMessage::plain("Email template inserted.")
        } else {
            UiMessage::detail("Fill in {} before sending.", left.join(", "))
        };

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
                ui.set_notice(inserted, false);
            }
            Some(Err(error)) => ui.set_notice(
                UiMessage::detail(
                    "Email template inserted, but its usage could not be saved: {}",
                    error,
                ),
                true,
            ),
            None => ui.set_notice(inserted, false),
        }
        true
    }

    /// What a template's placeholders take from this message: its first recipient, by the
    /// name the message shows or else its contact's, its sender, and today's date.
    fn placeholder_values(
        &self,
        composers: &Composers,
        host: &HostRef,
    ) -> placeholders::PlaceholderValues {
        let ui = host.ui();
        let core = composers.core();
        let account_id = i64::from(ui.get_account_id());
        let to = ui.get_to();
        let recipient = crate::mail::recipient_entries(&to)
            .find_map(|item| crate::mail::parse_address(item).ok())
            .map_or_else(placeholders::Person::default, |address| {
                let name = address.name.clone().or_else(|| {
                    let contacts = composers
                        .runtime
                        .block_on(core.as_ref()?.list_contacts(
                            address.email.clone(),
                            (account_id > 0).then_some(account_id),
                            6,
                        ))
                        .ok()?;
                    contacts
                        .into_iter()
                        .find(|contact| contact.email.eq_ignore_ascii_case(&address.email))
                        .and_then(|contact| contact.name)
                });
                placeholders::Person::new(name.as_deref(), Some(&address.email))
            });
        let sender_email = host.preferences().get_composer_sender_email().to_string();
        let sender_name = core
            .as_ref()
            .and_then(|core| {
                composers
                    .runtime
                    .block_on(
                        core.account_preferences_core()
                            .list_sender_identities(account_id),
                    )
                    .ok()
            })
            .and_then(|identities| {
                identities
                    .into_iter()
                    .find(|identity| identity.email.eq_ignore_ascii_case(&sender_email))
                    .and_then(|identity| identity.display_name)
            })
            .or_else(|| {
                composers
                    .state
                    .borrow()
                    .connected_accounts
                    .iter()
                    .find(|account| account.email.eq_ignore_ascii_case(&sender_email))
                    .and_then(|account| account.display_name.clone())
            });
        placeholders::PlaceholderValues {
            recipient,
            sender: placeholders::Person::new(sender_name.as_deref(), Some(&sender_email)),
            date: crate::date_format::current().date(
                crate::date_format::DateStyle::Medium,
                chrono::Local::now().date_naive(),
            ),
        }
    }

    fn reset_preferences(&self) {
        self.preferences_generation
            .set(self.preferences_generation.get().wrapping_add(1));
        *self.signature.borrow_mut() = None;
        self.signatures.borrow_mut().clear();
        self.copies.borrow_mut().clear();
        *self.added_copies.borrow_mut() = automatic_copies::Added::default();
        self.sender_chosen.set(false);
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
        preferences.set_composer_sender_picker_open(false);
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
        // Files go by link from the account's own storage, where it has one on.
        if let Some(host) = self.host() {
            host.ui().set_link_sharing(self.can_share_by_link());
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
        // How the message starts, before its signature goes in to match.
        self.choose_starting_format(&config.settings);
        // A draft, and a message edited as new, keep their own request.
        if !matches!(ui.get_mode().as_str(), "draft" | "edit_as_new") {
            ui.set_request_receipt(config.settings.request_read_receipts);
        }
        let requested = preferences.get_composer_sender_email().to_string();
        let identities = account_mail_preferences::selectable_identities(identities, &requested);
        *self.copies.borrow_mut() = identities
            .iter()
            .map(|identity| {
                (
                    identity.email.trim().to_lowercase(),
                    config.settings.copies_for(identity),
                )
            })
            .collect();
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
        preferences.set_composer_sender_email(identity.email.clone());
        self.apply_copies(&identity.email);
    }

    /// List the sender addresses matching `query` in the From picker, for an
    /// account with more than a list shows (#252).
    fn search_senders(&self, query: &str) {
        let Some(host) = self.host() else {
            return;
        };
        let preferences = host.preferences();
        let choices = preferences
            .get_composer_sender_identities()
            .iter()
            .map(|choice| (choice.label.to_string(), choice.email.to_string()))
            .collect::<Vec<_>>();
        let (rows, total) = sender_picker::rows(&choices, query);
        preferences.set_composer_sender_matches(ModelRc::new(VecModel::from(rows)));
        preferences.set_composer_sender_match_count(i32::try_from(total).unwrap_or(i32::MAX));
    }

    /// Give the message the copies mail from `sender` starts with, in place
    /// of the ones this composer added for the sender before (#252). A draft
    /// keeps the copies it was saved with, and a signature has none.
    fn apply_copies(&self, sender: &str) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        let mode = ui.get_mode();
        if mode == "signature" {
            return;
        }
        let copies = self
            .copies
            .borrow()
            .get(&sender.trim().to_lowercase())
            .cloned()
            .unwrap_or_default();
        let (to, cc, bcc) = (ui.get_to(), ui.get_cc(), ui.get_bcc());
        let first = !self.sender_chosen.replace(true);
        if first && mode == "draft" {
            *self.added_copies.borrow_mut() = automatic_copies::adopt(&cc, &bcc, &copies);
            return;
        }
        let (new_cc, new_bcc, added) =
            automatic_copies::apply(&to, &cc, &bcc, &self.added_copies.borrow(), &copies);
        if new_cc != cc.as_str() {
            ui.set_cc(new_cc.into());
        }
        if new_bcc != bcc.as_str() {
            ui.set_bcc(new_bcc.into());
        }
        *self.added_copies.borrow_mut() = added;
    }

    /// Starts writing `signature`, or a new one, for `account_id`: its name where a message
    /// has its subject, and its text in the editor in the formatting it was saved with.
    fn start_signature(
        self: &Rc<Self>,
        account_id: i64,
        signature: Option<Signature>,
        return_to_settings: bool,
    ) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        let name = signature
            .as_ref()
            .map(|signature| signature.name.clone())
            .unwrap_or_default();
        *self.signature_target.borrow_mut() = Some(SignatureTarget {
            account_id,
            id: signature
                .as_ref()
                .map(|signature| signature.id.clone())
                .unwrap_or_default(),
            return_to_settings,
        });
        ui.set_account_id(i32::try_from(account_id).unwrap_or_default());
        ui.set_subject(name.as_str().into());
        ui.set_signature_saved_name(name.into());
        {
            let mut document = self.document.borrow_mut();
            // Plain text names no font, so a signature takes each message's own.
            document.set_base_style(
                crate::rich_compose::FontFamily::Default,
                crate::rich_compose::TextSize::Normal,
            );
            match &signature {
                Some(signature) => {
                    document.load_signature_html(&signature.html);
                }
                None => {
                    document.load("", 0, 0);
                    document.mark_saved();
                }
            }
        }
        // A signature keeps its formatting, for formatted messages to show.
        self.start_format(false);
        self.show("signature", None);
        #[cfg(desktop)]
        if let HostRef::Window(handle) = &host {
            self.watch_window(handle);
            if let Err(error) = handle.show() {
                ui.set_notice(
                    UiMessage::detail("Could not open composer: {}", error),
                    true,
                );
            }
        }
    }

    /// Saves the signature being written to Settings, and closes it once saved.
    fn save_signature(self: &Rc<Self>) {
        let Some(target) = self.signature_target.borrow().clone() else {
            return;
        };
        let (Some(host), Some(composers)) = (self.host(), self.composers()) else {
            return;
        };
        let ui = host.ui();
        if ui.get_sending() {
            return;
        }
        let name = ui.get_subject().trim().to_owned();
        if name.is_empty() {
            ui.set_notice(UiMessage::plain("Give the signature a name."), true);
            return;
        }
        let html = {
            let body = ui.get_body();
            let mut document = self.document.borrow_mut();
            if document.text() != body.as_str() {
                let end = i32::try_from(body.len()).unwrap_or(i32::MAX);
                document.synchronize(&body, end, end);
            }
            document.body_html().unwrap_or_default()
        };
        let Some(core) = composers
            .state
            .borrow()
            .core
            .as_ref()
            .map(CoreMailSource::account_preferences_core)
        else {
            return;
        };
        let signature = Signature {
            id: target.id,
            account_id: target.account_id,
            name,
            html,
        };
        // Saving holds the composer, as sending does, so it saves once.
        ui.set_sending(true);
        let task = composers.runtime.spawn(async move {
            let saved = core
                .save_signature(signature)
                .await
                .map_err(|error| error.to_string())?;
            let settings = core
                .get_settings()
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((saved, settings))
        });
        let session = self.reference();
        let spawned = slint::spawn_local(async move {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let Some(session) = session.upgrade() else {
                return;
            };
            match result {
                Ok((saved, settings)) => session.signature_saved(&saved, &settings),
                Err(error) => {
                    if let Some(host) = session.host() {
                        let ui = host.ui();
                        ui.set_sending(false);
                        ui.set_notice(
                            UiMessage::detail("Could not save the signature: {}", error),
                            true,
                        );
                    }
                }
            }
        });
        if let Err(error) = spawned {
            ui.set_sending(false);
            ui.set_notice(
                UiMessage::detail("Could not save the signature: {}", error),
                true,
            );
        }
    }

    /// The signature was saved: Settings shows it, and its composer closes.
    fn signature_saved(self: &Rc<Self>, saved: &Signature, settings: &Settings) {
        if let Some(host) = self.host() {
            host.ui().set_sending(false);
        }
        if let Some(app) = self
            .composers()
            .and_then(|composers| composers.app.upgrade())
        {
            account_mail_preferences::signature_saved(&app, saved, settings);
        }
        self.close();
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
        // The new signature takes the old one's place, in its formatting and with its
        // pictures, or goes above the quotation.
        let at = flectar_mail_core::signatures::insertion_point(&body);
        let mut inserted = None;
        self.edit(|document| {
            let (selection, block) = document.replace_signature(
                old.as_deref(),
                at,
                signature.as_ref().map(|signature| signature.html.as_str()),
                self.plain_text.get(),
            );
            inserted = block;
            selection
        });
        *self.signature.borrow_mut() = inserted;
        preferences.set_composer_signature_index(index);
    }
}

type FetchedFiles = Vec<Result<(PathBuf, String, u64), String>>;

/// The signature a composer writes: its account, its id, empty for a new one, and whether
/// Settings comes back when it closes, as on phones, where the composer took its place.
#[derive(Clone, Debug)]
struct SignatureTarget {
    account_id: i64,
    id: String,
    return_to_settings: bool,
}

/// A file to attach: where it is, the name it goes out with, and the copy that keeps it
/// readable, when the host made one.
struct NewAttachment {
    path: PathBuf,
    filename: Option<String>,
    import: Option<documents::ImportedDocument>,
}

/// What adding files did, and whether that's an error: none of them went in.
fn attachments_notice(added: usize, skipped: usize) -> (UiMessage, bool) {
    if skipped == 0 {
        let notice = if added == 1 {
            UiMessage::plain("Added 1 attachment.")
        } else {
            UiMessage::detail("Added {} attachments.", added)
        };
        return (notice, false);
    }
    (
        UiMessage::arguments(
            "Attachments added: {}. Skipped: {} (duplicate, unreadable, or over the 25 MB total limit).",
            added,
            skipped,
        ),
        added == 0,
    )
}

/// Whether a file's name says it's a picture the text can show.
#[cfg(desktop)]
fn is_image_name(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "gif", "webp"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
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

    pub(super) struct Setup {
        pub(super) app: AppWindow,
        pub(super) composers: Rc<Composers>,
        pub(super) _runtime: tokio::runtime::Runtime,
        pub(super) _dir: tempfile::TempDir,
    }

    pub(super) fn setup() -> Setup {
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
            name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: flectar_mail_core::models::AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            sync_state: "idle".into(),
            sync_error: None,
            sync_retry_at: None,
            can_create_top_level_mailbox: true,
            server_keeps_mail_days: None,
            caught_up_at: None,
        }];
        let state = Rc::new(RefCell::new(state));
        app.set_compose_account_id(1);
        let composers = install(
            &app,
            &state,
            runtime.handle(),
            documents::default_provider(),
            std::env::temp_dir(),
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
    fn a_draft_an_agent_asks_to_send_opens_in_a_window_of_its_own() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        let main = app.global::<ComposerUi>();
        // The user is writing a message of their own.
        app.invoke_open_compose("friend@example.com".into());
        main.set_subject("Mine".into());

        let draft = SaveDraftArgs {
            draft_id: Some(42),
            account_id: 1,
            from: None,
            to: vec![Address {
                name: None,
                email: "priya@example.com".into(),
            }],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Plans".into(),
            body_text: "Thursday works.".into(),
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            in_reply_to_header: None,
            attachments: Vec::new(),
            request_read_receipt: false,
            importance: Default::default(),
        };
        assert!(composers.show_review(draft.clone(), "Claude"));
        assert_eq!(open_windows(), 1);
        let review = composers.holding_draft(42).unwrap();
        let host = review.host().unwrap();
        let ui = host.ui();
        assert_eq!(ui.get_subject(), "Plans");
        assert_eq!(ui.get_to(), "priya@example.com");
        assert!(ui.get_review_note().contains("Claude"));
        // Asked again, the same window comes forward.
        assert!(composers.show_review(draft, "Claude"));
        assert_eq!(open_windows(), 1);
        // The user's own message stays as it was, without the note.
        assert!(main.get_open());
        assert_eq!(main.get_subject(), "Mine");
        assert_eq!(main.get_review_note(), "");

        // Closing it ends the review.
        review.dispose();
        assert_eq!(review.reviewing.get(), None);
        assert_eq!(open_windows(), 0);
        assert_eq!(main.get_subject(), "Mine");
    }

    #[cfg(desktop)]
    #[test]
    fn files_dropped_with_no_message_open_start_one_with_all_of_them() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        let files = tempfile::tempdir().unwrap();
        let notes = files.path().join("notes.txt");
        let plan = files.path().join("plan.pdf");
        std::fs::write(&notes, "notes").unwrap();
        std::fs::write(&plan, "%PDF-1.4").unwrap();
        let wait_for_the_rest = || {
            std::thread::sleep(Duration::from_millis(40));
            slint::platform::update_timers_and_animations();
        };

        // With no message open, the window says the files would start one.
        files_hovered(true);
        assert!(app.get_drop_starts_message());
        files_hovered(false);
        assert!(!app.get_drop_starts_message());

        // A drop's files arrive one at a time and start one message.
        files_hovered(true);
        file_dropped(notes);
        file_dropped(plan.clone());
        assert!(composers.in_app().is_none());
        wait_for_the_rest();
        assert!(
            !app.get_drop_starts_message(),
            "the hint goes with the drop"
        );
        let session = composers.in_app().expect("a new message opened");
        let names = || {
            session
                .files
                .borrow()
                .iter()
                .map(|file| file.filename.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(), ["notes.txt", "plan.pdf"]);

        // Files dropped while it's open go into it, not into another one.
        files_hovered(true);
        assert!(!app.get_drop_starts_message());
        let agenda = files.path().join("agenda.txt");
        std::fs::write(&agenda, "agenda").unwrap();
        file_dropped(agenda);
        wait_for_the_rest();
        assert!(Rc::ptr_eq(&composers.in_app().unwrap(), &session));
        assert_eq!(names(), ["notes.txt", "plan.pdf", "agenda.txt"]);
    }

    #[test]
    fn a_forward_goes_once_its_files_are_in() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        app.invoke_open_compose("ana@example.test".into());
        let session = composers.in_app().unwrap();
        session.count_attaching(true);
        assert!(app.global::<ComposerUi>().get_attaching_files());

        // The wait moves with the message to a window of its own.
        app.global::<ComposerUi>().invoke_pop_out();
        let host = session.host().unwrap();
        let ui = host.ui();
        assert!(ui.get_attaching_files());
        host.preferences().set_composer_preferences_ready(true);
        let send = || session.save(1, "ana@example.test", "", "", "Fwd: Plans", "Here", true);
        send();
        assert!(!ui.get_notice_is_error(), "nothing was sent");

        // Once they're in, it goes, which with no account connected it can't.
        session.count_attaching(false);
        assert!(!ui.get_attaching_files());
        send();
        assert!(ui.get_notice_is_error());
    }

    #[test]
    fn a_template_fills_its_placeholders_and_sending_asks_about_the_rest() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        app.invoke_open_compose("Ana María Ruiz <ana@example.test>".into());
        let session = composers.in_app().unwrap();
        let host = session.host().unwrap();
        let ui = host.ui();
        host.preferences().set_composer_preferences_ready(true);
        host.preferences()
            .set_composer_sender_email("me@example.test".into());
        composers.templates.borrow_mut().push(Snippet {
            id: 7,
            name: "Thanks".into(),
            shortcut: None,
            subject: Some("Thanks, {{recipient.first_name}}".into()),
            body_text: "Hi {{recipient.first_name}}, about {{company}}. {{sender.email}}".into(),
            usage_count: 0,
        });

        // What the message knows goes in, and what it doesn't stays for the writer.
        assert!(session.use_template(7));
        assert_eq!(ui.get_subject(), "Thanks, Ana");
        assert_eq!(
            session.document.borrow().text(),
            "Hi Ana, about {{company}}. me@example.test"
        );
        let notice = ui.get_message();
        assert_eq!(notice.key, "Fill in {} before sending.");
        assert_eq!(notice.argument, "{{company}}");

        // Sending asks once about it, then sends it as it is, which with no account
        // connected it can't.
        let send = || {
            let body = session.document.borrow().text().to_owned();
            session.save(1, &ui.get_to(), "", "", &ui.get_subject(), &body, true);
        };
        send();
        let notice = ui.get_message();
        assert_eq!(notice.key, "Fill in {} or send again.");
        assert_eq!(notice.argument, "{{company}}");
        send();
        assert_eq!(
            ui.get_message().key,
            "Connect an account before saving or sending mail."
        );
    }

    #[test]
    fn sending_asks_about_a_missing_subject_then_a_mentioned_file() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        app.invoke_open_compose("ana@example.test".into());
        let session = composers.in_app().unwrap();
        let host = session.host().unwrap();
        let ui = host.ui();
        host.preferences().set_composer_preferences_ready(true);
        ui.set_subject(" ".into());
        ui.set_body("I've attached the plan.\n> The report is attached".into());
        let send = || {
            session.save(
                1,
                &ui.get_to(),
                "",
                "",
                &ui.get_subject(),
                &ui.get_body(),
                true,
            );
        };

        send();
        assert!(ui.get_send_check_open());
        assert_eq!(ui.get_send_check(), "subject");
        // Sent without a subject, it asks about the file the message mentions.
        session.answer_send_check("send");
        assert_eq!(ui.get_send_check(), "attachment");
        assert_eq!(ui.get_send_check_keyword(), "attached");
        // Going back asks both again next time.
        session.answer_send_check("cancel");
        send();
        assert_eq!(ui.get_send_check(), "subject");

        // With a subject, and sent anyway, the message goes, which with no
        // account connected it can't.
        ui.set_subject("Plan".into());
        send();
        assert_eq!(ui.get_send_check(), "attachment");
        session.answer_send_check("send");
        assert_eq!(
            ui.get_message().key,
            "Connect an account before saving or sending mail."
        );

        // A quoted mention isn't the user's, so it asks nothing.
        session.reset_send_checks();
        ui.set_send_check_open(false);
        ui.set_notice(UiMessage::EMPTY, false);
        ui.set_body("Thanks!\n> The report is attached".into());
        send();
        assert!(!ui.get_send_check_open());
        assert_eq!(
            ui.get_message().key,
            "Connect an account before saving or sending mail."
        );
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
    fn a_message_asks_for_its_times_in_the_window_that_shows_it() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        when_picker::register(&app, &composers.state, &composers.runtime);
        let main = app.global::<ComposerUi>();
        let main_picker = app.global::<WhenUi>();
        // The dialog shuts itself before it answers, as a click does.
        let choose = |picker: &WhenUi<'_>, index: usize| {
            picker.set_open(false);
            picker.invoke_choose(i32::try_from(index).unwrap());
        };

        // A message in the main window asks there.
        app.invoke_open_compose("first@example.com".into());
        let first = composers.in_app().unwrap();
        main.invoke_remind();
        assert!(main_picker.get_open());
        assert_eq!(main_picker.get_purpose(), "remind");
        choose(&main_picker, 0);
        let reminds = main.get_remind_time();
        assert_ne!(reminds, "");
        assert!(first.remind_at.get().is_some());

        // In a window of its own, it keeps its reminder and asks there; the
        // main window's picker stays shut.
        main.invoke_pop_out();
        assert_eq!(
            main.get_remind_time(),
            "",
            "the main window's composer is free"
        );
        let popped = first.host().unwrap();
        let HostRef::Window(window) = &popped else {
            panic!("the message has a window of its own");
        };
        let picker = window.global::<WhenUi>();
        assert_eq!(popped.ui().get_remind_time(), reminds);
        popped.ui().invoke_remind();
        assert!(picker.get_open());
        assert!(!main_picker.get_open());

        // "Don't remind me" comes last, and ends the reminder.
        let choices = picker.get_choices();
        let last = choices.row_count() - 1;
        assert_eq!(choices.row_data(last).unwrap().kind, "none");
        choose(&picker, last);
        assert_eq!(popped.ui().get_remind_time(), "");
        assert!(first.remind_at.get().is_none());

        // Schedule send asks there too, and refuses a time that a reminder
        // would come before: tomorrow at eight both times.
        popped.ui().invoke_remind();
        choose(&picker, 0);
        popped.ui().invoke_schedule_send();
        assert!(picker.get_open());
        assert_eq!(picker.get_purpose(), "send");
        choose(&picker, 0);
        assert_eq!(
            popped.ui().get_message().key,
            "Choose a reminder after the time the message goes out."
        );
        assert!(popped.ui().get_notice_is_error());
        assert!(popped.ui().get_open(), "the message stays to choose again");
        assert!(first.send_at.get().is_none());
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
