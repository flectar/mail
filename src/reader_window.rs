//! Conversations in windows of their own on the desktop (#254). A
//! double-click, Shift+Enter or "Open in new window" opens a conversation of
//! the list in a window that shows what the main window's reader shows: the
//! conversation's messages, its files, cards and bars, and the same actions.
//! Each window has its own renderer and reader globals (`ReaderRef::Window`),
//! so it stays on its conversation while the main window shows others.
//!
//! Opening a conversation that has a window brings that window forward, as
//! Evolution and Mailspring do. A window closes once its conversation is
//! archived, deleted or moved from it, as Apple Mail's and KMail's do, and
//! once the conversation is gone from the mail. Replies start in windows of
//! their own.

use super::*;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use std::sync::atomic::{AtomicBool, Ordering};

/// A conversation's window.
struct WindowReader {
    id: u64,
    window: ReaderWindow,
    /// The list row it was opened from, which names its thread or message.
    row: RefCell<MailMessage>,
    /// The mailbox the list showed the conversation in.
    scope: Scope,
    /// The conversation's messages, once loaded, and the one shown.
    messages: RefCell<Vec<MailMessage>>,
    selected: Cell<usize>,
    renderer: Rc<RefCell<GpuEmailRenderer>>,
    thread_rows: Rc<VecModel<ThreadMessageRow>>,
    /// The user loaded remote images in this conversation here.
    remote_images: Cell<bool>,
    /// Counts loads, so that only the latest one shows.
    loads: Cell<u64>,
    /// The message whose body a load is reading, so asking again waits.
    loading: Cell<Option<i32>>,
    /// A repaint for decoded images and fonts is on its way.
    resources_pending: Cell<bool>,
    /// The wait before the conversation counts as read.
    read_wait: Timer,
}

impl WindowReader {
    fn reader(&self, app: &AppWindow) -> ReaderRef {
        ReaderRef::Window {
            window: self.window.clone_strong(),
            app: app.clone_strong(),
            id: self.id,
        }
    }

    /// The message the window shows: one of its conversation's, or its row
    /// while the conversation loads.
    fn shown(&self) -> MailMessage {
        self.messages
            .borrow()
            .get(self.selected.get())
            .cloned()
            .unwrap_or_else(|| self.row.borrow().clone())
    }

    /// Whether `row` is the conversation this window shows.
    fn shows(&self, row: &MailMessage) -> bool {
        let own = self.row.borrow();
        (own.thread_id, own.message_id) == (row.thread_id, row.message_id)
    }
}

struct Windows {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
    documents: Arc<dyn documents::DocumentProvider>,
    opener: ExternalUrlOpener,
    next_id: Cell<u64>,
    open: RefCell<Vec<Rc<WindowReader>>>,
}

thread_local! {
    static WINDOWS: RefCell<Option<Rc<Windows>>> = const { RefCell::new(None) };
}

fn registry() -> Option<Rc<Windows>> {
    WINDOWS.with_borrow(Clone::clone)
}

fn find(id: u64) -> Option<Rc<WindowReader>> {
    registry()?
        .open
        .borrow()
        .iter()
        .find(|reader| reader.id == id)
        .cloned()
}

fn all() -> Vec<Rc<WindowReader>> {
    registry()
        .map(|windows| windows.open.borrow().clone())
        .unwrap_or_default()
}

/// Let the main window open conversations in windows of their own.
pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    documents: Arc<dyn documents::DocumentProvider>,
    opener: ExternalUrlOpener,
) {
    WINDOWS.set(Some(Rc::new(Windows {
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
        documents,
        opener,
        next_id: Cell::new(0),
        open: RefCell::default(),
    })));
    let ui = app.global::<ReaderWindows>();
    ui.set_available(true);
    ui.on_open(open);
}

/// How many conversation windows are open.
pub(super) fn open_windows() -> usize {
    registry().map_or(0, |windows| windows.open.borrow().len())
}

// ---- What the reader host asks of a window ---------------------------------

pub(crate) fn host(id: u64) -> Option<ReaderHost> {
    let windows = registry()?;
    let reader = find(id)?;
    Some(ReaderHost::Window {
        window: reader.window.as_weak(),
        app: windows.app.clone(),
        id,
    })
}

pub(crate) fn with_messages<R>(id: u64, f: impl FnOnce(&mut Vec<MailMessage>) -> R) -> Option<R> {
    let reader = find(id)?;
    let mut messages = reader.messages.borrow_mut();
    Some(f(&mut messages))
}

pub(crate) fn kept_message(id: u64, message_id: i32) -> Option<MailMessage> {
    find(id)?
        .messages
        .borrow()
        .iter()
        .find(|message| message.id == message_id)
        .cloned()
}

pub(crate) fn row(id: u64) -> Option<MailMessage> {
    Some(find(id)?.row.borrow().clone())
}

pub(crate) fn shown_message(id: u64) -> Option<MailMessage> {
    Some(find(id)?.shown())
}

/// The message of the conversation the window shows, once the
/// conversation is here.
pub(crate) fn shown_conversation_message(id: u64) -> Option<i64> {
    let reader = find(id)?;
    let messages = reader.messages.borrow();
    messages
        .get(reader.selected.get())
        .map(|message| i64::from(message.id))
}

pub(crate) fn scope(id: u64) -> Option<Scope> {
    Some(find(id)?.scope)
}

/// Show the window's message again, as after it was translated.
pub(crate) fn show_again(id: u64) {
    let (Some(windows), Some(reader)) = (registry(), find(id)) else {
        return;
    };
    reader.renderer.borrow_mut().loaded_key = None;
    render(&windows, &reader);
}

pub(crate) fn for_each(f: &impl Fn(&ReaderRef)) {
    let Some(app) = registry().and_then(|windows| windows.app.upgrade()) else {
        return;
    };
    for reader in all() {
        f(&reader.reader(&app));
    }
}

pub(crate) fn each_kept_message(f: &mut impl FnMut(&mut MailMessage)) {
    for reader in all() {
        reader.messages.borrow_mut().iter_mut().for_each(&mut *f);
    }
}

/// The conversation of window `id` left where it was: it was archived,
/// deleted, moved or snoozed from the window.
pub(crate) fn conversation_left(id: u64) {
    close(id);
}

// ---- Opening and closing ----------------------------------------------------

/// Open the list's conversation with row id `row_id` in a window of its own,
/// or bring forward the window that shows it.
fn open(row_id: i32) {
    let Some(windows) = registry() else {
        return;
    };
    let Some(app) = windows.app.upgrade() else {
        return;
    };
    let (row, scope) = {
        let state = windows.state.borrow();
        let Some(row) = state.messages.iter().find(|row| row.id == row_id).cloned() else {
            return;
        };
        (row, state.scope)
    };
    if MailTarget::of(&row).is_none() {
        return;
    }
    let open = windows
        .open
        .borrow()
        .iter()
        .find(|reader| reader.shows(&row))
        .cloned();
    if let Some(open) = open {
        bring_forward(&open.window);
        return;
    }
    let window = match crate::window_chrome::secondary_window(ReaderWindow::new) {
        Ok(window) => window,
        Err(error) => {
            app.set_render_status(UiMessage::detail(
                "Could not open the conversation's window: {}",
                error,
            ));
            return;
        }
    };
    let id = windows.next_id.get() + 1;
    windows.next_id.set(id);
    copy_appearance(&app, &window);
    let renderer = new_renderer(&windows, &window, id);
    let reader = Rc::new(WindowReader {
        id,
        window,
        row: RefCell::new(row),
        scope,
        messages: RefCell::default(),
        selected: Cell::new(0),
        renderer,
        thread_rows: Rc::new(VecModel::default()),
        remote_images: Cell::new(false),
        loads: Cell::new(0),
        loading: Cell::new(None),
        resources_pending: Cell::new(false),
        read_wait: Timer::default(),
    });
    windows.open.borrow_mut().push(Rc::clone(&reader));
    connect(&windows, &app, &reader);
    // The row shows while the conversation loads, as in the main window.
    render(&windows, &reader);
    crate::window_places::restore(crate::window_places::Kind::Reader, &reader.window);
    if let Err(error) = reader.window.show() {
        close(id);
        app.set_render_status(UiMessage::detail(
            "Could not open the conversation's window: {}",
            error,
        ));
        return;
    }
    crate::window_places::shown(crate::window_places::Kind::Reader, &reader.window);
    load(&windows, &reader, None);
    mark_read(&windows, &reader);
}

/// The reader's own renderer, which paints on the CPU like the composer's
/// quotes: the window shares no GPU surface with the main window.
fn new_renderer(
    windows: &Windows,
    window: &ReaderWindow,
    id: u64,
) -> Rc<RefCell<GpuEmailRenderer>> {
    let mut renderer = GpuEmailRenderer::default();
    renderer.set_appearance(crate::message_appearance::EmailAppearance::of(
        &window.global::<MessageColors>(),
    ));
    // Decoded images and fonts arrive on worker threads: one repaint
    // follows a burst of them, as in the main window.
    let posting = Arc::new(AtomicBool::new(false));
    renderer.set_resource_notifier(Arc::new(move || {
        if posting.swap(true, Ordering::AcqRel) {
            return;
        }
        let delivered = Arc::clone(&posting);
        let posted = slint::invoke_from_event_loop(move || {
            delivered.store(false, Ordering::Release);
            if let Some(reader) = find(id)
                && !reader.resources_pending.replace(true)
            {
                Timer::single_shot(Duration::from_millis(50), move || resources_ready(id));
            }
        });
        if posted.is_err() {
            posting.store(false, Ordering::Release);
        }
    }));
    let remote = windows.state.borrow().remote_images_enabled;
    if let Err(error) = renderer.configure_resources(windows.runtime.clone(), remote) {
        tracing::warn!(%error, "message images unavailable in a conversation window");
    }
    Rc::new(RefCell::new(renderer))
}

/// Images or fonts of the window's message were decoded.
fn resources_ready(id: u64) {
    let (Some(windows), Some(reader)) = (registry(), find(id)) else {
        return;
    };
    reader.resources_pending.set(false);
    let Some(app) = windows.app.upgrade() else {
        return;
    };
    if !reader.renderer.borrow_mut().poll_resources() {
        return;
    }
    let host = reader.reader(&app);
    if !reader.window.get_render_suspended() {
        let (width, height) = email_viewport_size(&host);
        let frame = reader.renderer.borrow_mut().render_cpu_if_needed(
            width,
            height,
            host.window().scale_factor(),
        );
        match frame {
            Ok(Some(frame)) => apply_cpu_frame(&host, frame),
            Ok(None) => {}
            Err(error) => {
                host.email().set_notice(error.into());
                host.ui().set_text_mode(true);
            }
        }
    }
    sync_reader_metadata(&host, &reader.renderer);
    host.window().request_redraw();
}

/// Show `window` to the user again: restore it and give it focus.
fn bring_forward(window: &ReaderWindow) {
    let native = window.window();
    if native.is_minimized() {
        native.set_minimized(false);
    }
    let _ = window.show();
    let _ = native.with_winit_window(|window| window.focus_window());
}

/// Close window `id` and let its conversation go.
fn close(id: u64) {
    let Some(windows) = registry() else {
        return;
    };
    let reader = {
        let mut open = windows.open.borrow_mut();
        let Some(index) = open.iter().position(|reader| reader.id == id) else {
            return;
        };
        open.remove(index)
    };
    let key = ReaderKey::Window(id);
    reader.read_wait.stop();
    let _ = reader.window.hide();
    crate::invitation::forget(key);
    crate::read_receipts::forget(key);
    crate::translation::forget(key);
    crate::ai_features::forget(key);
    crate::unsubscribe::forget(key);
    crate::message_source::forget(key);
    // The window goes once the event that closed it is handled, as the
    // callback that asked may still be running.
    Timer::single_shot(Duration::ZERO, move || {
        reader.renderer.borrow_mut().clear();
        drop(reader);
        Timer::single_shot(
            crate::window_controller::TRIM_DELAY,
            crate::memory::release_free_heap_when_idle,
        );
    });
    // A main window closed meanwhile may now end the app.
    crate::window_controller::window_closed();
}

// ---- What the window shows ---------------------------------------------------

/// Read the conversation again, and show the message `selected` of it, else
/// the one shown, else its last.
fn load(windows: &Rc<Windows>, reader: &Rc<WindowReader>, selected: Option<i64>) {
    let Some(core) = windows.state.borrow().core.clone() else {
        return;
    };
    let row = reader.row.borrow().clone();
    let shown = selected.or_else(|| shown_conversation_message(reader.id));
    let load = reader.loads.get() + 1;
    reader.loads.set(load);
    reader
        .loading
        .set(shown.and_then(|id| i32::try_from(id).ok()));
    let id = reader.id;
    let task = windows
        .runtime
        .spawn(async move { core.load_conversation(&row, shown).await });
    let spawned = slint::spawn_local(async move {
        let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
        let (Some(windows), Some(reader)) = (registry(), find(id)) else {
            return;
        };
        if reader.loads.get() != load {
            return;
        }
        reader.loading.set(None);
        match result {
            Ok(conversation) if !conversation.messages.is_empty() => {
                let index = shown
                    .and_then(|shown| {
                        conversation
                            .messages
                            .iter()
                            .position(|message| i64::from(message.id) == shown)
                    })
                    .unwrap_or(conversation.messages.len() - 1);
                *reader.messages.borrow_mut() = conversation.messages;
                reader.selected.set(index);
                render(&windows, &reader);
            }
            // The conversation is gone from the mail.
            Ok(_) => close(id),
            Err(error) if error.starts_with("not found") => close(id),
            Err(error) => {
                if let Some(app) = windows.app.upgrade() {
                    let host = reader.reader(&app);
                    host.email().set_notice(translated(
                        &app,
                        &UiMessage::detail("Could not load the message: {}", error),
                    ));
                }
            }
        }
    });
    if spawned.is_err() {
        reader.loading.set(None);
    }
}

/// Show what the window's reader shows, as the main window's does for its
/// selection.
fn render(windows: &Windows, reader: &WindowReader) {
    let Some(app) = windows.app.upgrade() else {
        return;
    };
    let host = reader.reader(&app);
    refresh_row(windows, reader);
    let row = reader.row.borrow().clone();
    let (shown, sent_here, allow_remote) = {
        let state = windows.state.borrow();
        let messages = reader.messages.borrow();
        let loaded = !messages.is_empty();
        let index = reader.selected.get().min(messages.len().saturating_sub(1));
        reader.selected.set(index);
        let mut shown = messages.get(index).cloned().unwrap_or_else(|| row.clone());
        if loaded {
            // A conversation goes by its own subject, as the list shows it.
            shown.subject = row.subject.clone();
            reconcile_model_rows_by(
                &reader.thread_rows,
                crate::mail_view_model::make_thread_rows(&messages, index, &state.favicon_icons),
                |row| row.index,
                PartialEq::eq,
            );
        } else {
            reader.thread_rows.set_vec(Vec::new());
        }
        // A reply reminder waits on mail the user sent in the conversation.
        let sent_here = if loaded {
            messages.iter().any(|message| message.is_outgoing)
        } else {
            row.is_outgoing || row.has_replied
        };
        let ui = host.ui();
        ui.set_selected_thread_index(i32::try_from(index).unwrap_or(i32::MAX));
        ui.set_selected_account_uses_labels(state.connected_accounts.iter().any(|account| {
            account.id == row.account_id && account.mail_protocol == MailProtocol::Gmail
        }));
        copy_local_mail(&app, &host);
        host.global::<LocalMailUi>()
            .set_selected(state.local_mail.map(|local| local.account_id) == Some(row.account_id));
        crate::mail_view_model::apply_label_rows(&host, &state.labels, Some(&row));
        crate::mail_view_model::apply_selected_favicon(
            &host,
            state.favicon_icons.get(&shown.domain),
        );
        let allow_remote = state.remote_images_enabled || reader.remote_images.get();
        (shown, sent_here, allow_remote)
    };
    reader
        .window
        .set_trash_view(crate::mail_view_model::is_trash_scope(reader.scope));
    let reminder = host.reminder();
    reminder.set_open_remind_time(
        shown
            .reply_reminder_at
            .map(|at| crate::mail_view_model::upcoming_time(at, &Local::now()))
            .unwrap_or_default()
            .into(),
    );
    reminder.set_open_due(shown.reply_reminder_due);
    reminder.set_can_remind(sent_here);
    if let Err(error) = apply_email(&host, shown, &reader.renderer, false, allow_remote) {
        app.set_render_status(UiMessage::detail("Could not show the message: {}", error));
    }
}

/// The window's row as the list has it now: labels and flags change there.
fn refresh_row(windows: &Windows, reader: &WindowReader) {
    let state = windows.state.borrow();
    let current = state
        .messages
        .iter()
        .find(|row| row.id == reader.row.borrow().id && reader.shows(row));
    if let Some(current) = current {
        *reader.row.borrow_mut() = current.clone();
    }
}

/// The list changed: a window whose row's labels changed there shows them.
pub(super) fn list_changed() {
    let Some(windows) = registry() else {
        return;
    };
    let Some(app) = windows.app.upgrade() else {
        return;
    };
    for reader in all() {
        let labels = reader.row.borrow().labels.clone();
        refresh_row(&windows, &reader);
        let row = reader.row.borrow().clone();
        if row.labels != labels {
            let state = windows.state.borrow();
            crate::mail_view_model::apply_label_rows(
                &reader.reader(&app),
                &state.labels,
                Some(&row),
            );
        }
    }
}

/// Mail changed: windows whose conversation did read it again, and close
/// when it is gone.
pub(super) fn mail_changed(changed_threads: &HashSet<i64>, all_mail: bool) {
    let Some(windows) = registry() else {
        return;
    };
    for reader in all() {
        let thread = reader.row.borrow().thread_id;
        if all_mail || thread.is_some_and(|thread| changed_threads.contains(&thread)) {
            load(&windows, &reader, None);
        }
    }
}

/// Mark the window's conversation read as Settings says: as it opens, or
/// once it has stayed open a while.
fn mark_read(windows: &Windows, reader: &WindowReader) {
    let (delay, core) = {
        let state = windows.state.borrow();
        if !state.using_core {
            return;
        }
        (state.mark_read.delay(), state.core.clone())
    };
    let row = reader.row.borrow().clone();
    let (Some(delay), Some(core), Some(target)) = (delay, core, MailTarget::of(&row)) else {
        return;
    };
    if !row.unread {
        return;
    }
    let runtime = windows.runtime.clone();
    let app = windows.app.clone();
    let mark = move || {
        runtime.spawn(async move {
            if let Err(error) = core.perform_message_action(target, "mark_read").await {
                let _ = app.upgrade_in_event_loop(move |app| {
                    app.set_render_status(UiMessage::detail(
                        "Could not mark message as read: {}",
                        error,
                    ));
                });
            }
        });
    };
    if delay.is_zero() {
        mark();
    } else {
        // The window owns the wait: closing it first leaves the mail unread.
        let mut mark = Some(mark);
        reader
            .read_wait
            .start(slint::TimerMode::SingleShot, delay, move || {
                if let Some(mark) = mark.take() {
                    mark();
                }
            });
    }
}

// ---- The window's look and settings ------------------------------------------

/// The main window's look: its theme, colors, icon and text size, how much
/// it moves, and how messages are colored.
fn copy_appearance(app: &AppWindow, window: &ReaderWindow) {
    crate::theme::copy_look(app, window);
    window.set_theme(app.get_theme_mode());
    window.set_interface_text_size(app.get_interface_text_size());
    window.set_print_supported(app.get_print_supported());
    let preference = app.global::<MessageColors>().get_preference();
    let colors = window.global::<MessageColors>();
    if colors.get_preference() != preference {
        colors.set_preference(preference);
    }
}

/// The main window's look changed: the windows follow it.
pub(super) fn appearance_changed(app: &AppWindow) {
    for reader in all() {
        copy_appearance(app, &reader.window);
    }
}

/// What the reader's menus offer for local mail.
fn copy_local_mail(app: &AppWindow, host: &ReaderRef) {
    let (from, to) = (app.global::<LocalMailUi>(), host.global::<LocalMailUi>());
    to.set_account_id(from.get_account_id());
    to.set_can_send(from.get_can_send());
    to.set_messages(from.get_messages());
    to.set_size(from.get_size());
}

/// Remote images were allowed or blocked in Settings.
pub(super) fn remote_images_changed(enabled: bool) {
    let Some(windows) = registry() else {
        return;
    };
    for reader in all() {
        let configured = reader
            .renderer
            .borrow_mut()
            .configure_resources(windows.runtime.clone(), enabled);
        if let Err(error) = configured {
            tracing::warn!(%error, "message images unavailable in a conversation window");
        }
        reader.renderer.borrow_mut().loaded_key = None;
        render(&windows, &reader);
    }
}

// ---- What the window does ---------------------------------------------------

/// Connect the window to the app: its reader's input and features, what is
/// done to its conversation, and the window's own life.
fn connect(windows: &Rc<Windows>, app: &AppWindow, reader: &Rc<WindowReader>) {
    let host = reader.reader(app);
    let id = reader.id;
    let window = &reader.window;
    host.ui()
        .set_thread_messages(ModelRc::from(reader.thread_rows.clone()));
    host.global::<KeyboardKeys>()
        .on_chord(crate::keyboard_keys::chord);
    crate::theme::register_theme_utilities(&host.global::<ThemeUtilities>());
    host.global::<ZoomApi>()
        .on_resolve(|action, current, minimum, maximum| {
            crate::preview_controls::zoom(&action, current, minimum, maximum)
        });
    let email = host.email();
    email.on_mailbox_text(|name, address| mailbox_text(&name, &address).into());
    email.on_ensure_body(move |message_id| ensure_body(id, message_id));
    email.on_retry_images(move || show_again(id));

    register_renderer_input_callbacks(&host, &reader.renderer, false, windows.opener.clone());
    crate::invitation::attach(&host);
    crate::read_receipts::attach(&host);
    crate::translation::attach(&host);
    crate::ai_features::attach(&host);
    crate::unsubscribe::attach(&host);
    crate::block_sender::attach(&host);
    crate::message_source::attach(&host);
    crate::mail_rules::attach(&host);
    crate::attachment_controller::register(
        &host,
        &windows.state,
        &windows.runtime,
        windows.documents.clone(),
    );
    crate::move_picker::attach(&host, &windows.state, &windows.runtime);
    crate::when_picker::attach_reader(&host, &windows.state, &windows.runtime);
    crate::account_mail_preferences::attach_reader(&host, &windows.state, &windows.runtime);
    host.global::<AiSummary>().on_reply_with(move |text| {
        compose(id, "reply", Some(text.to_string()));
    });

    window.on_message_action(move |action| act(id, &action));
    window.on_compose_message_action(move |action| compose(id, &action, None));
    window.on_toggle_mail_label(move |label_id, applied| toggle_label(id, label_id, applied));
    window.on_save_mail_label(move |label_id, name, color, global| {
        save_label(id, label_id, name.to_string(), color, global);
    });
    window.on_search_mail_labels(move |query| search_labels(id, &query));
    window.on_select_thread_message(move |index| select_thread_message(id, index));
    window.on_print_message(move || print(id));
    window.on_open_message_in_browser(move || open_in_browser(id));
    window.on_report_rendering_issue(move || {
        if let Some(windows) = registry()
            && let Some(app) = windows.app.upgrade()
            && let Some(reader) = find(id)
        {
            crate::rendering_report::report(&reader.reader(&app));
        }
    });
    window.on_load_remote_images_for_message(move || load_remote_images(id));
    window.on_request_close(move || close(id));
    window.window().on_close_requested(move || {
        close(id);
        slint::CloseRequestResponse::HideWindow
    });
    watch(window, id);
}

/// The window's own events: it returns the memory input leaves behind
/// once it goes still, keeps no pixels while minimized, hands its help to
/// the keyboard on a key press, and quits the app for Control+Q, as the
/// main window does.
fn watch(window: &ReaderWindow, id: u64) {
    let help = window.as_weak();
    #[cfg(linux_desktop)]
    let modifiers = Cell::new(winit::keyboard::ModifiersState::empty());
    window.window().on_winit_window_event(move |native, event| {
        use winit::event::WindowEvent;
        if !matches!(event, WindowEvent::RedrawRequested) {
            crate::memory::release_free_heap_when_idle();
        }
        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state.is_pressed()
            && let Some(window) = help.upgrade()
        {
            window.global::<ActionTooltip>().set_keyboard(true);
        }
        #[cfg(linux_desktop)]
        match event {
            WindowEvent::ModifiersChanged(state) => modifiers.set(state.state()),
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(app) = registry().and_then(|windows| windows.app.upgrade())
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
        ) {
            let minimized = native
                .with_winit_window(|window| window.is_minimized())
                .flatten()
                .unwrap_or(false);
            minimized_changed(id, minimized);
        }
        if matches!(event, WindowEvent::Resized(_))
            && let Some(window) = help.upgrade()
        {
            crate::window_places::changed(crate::window_places::Kind::Reader, &window);
        }
        EventResult::Propagate
    });
}

/// The window was minimized, or shown again: it keeps no pixels meanwhile.
fn minimized_changed(id: u64, minimized: bool) {
    let (Some(windows), Some(reader)) = (registry(), find(id)) else {
        return;
    };
    if reader.window.get_render_suspended() == minimized {
        return;
    }
    reader.window.set_render_suspended(minimized);
    reader.renderer.borrow_mut().set_suspended(minimized);
    let Some(app) = windows.app.upgrade() else {
        return;
    };
    let host = reader.reader(&app);
    if minimized {
        host.ui().set_email_tiles(ModelRc::default());
    } else {
        let (width, height) = email_viewport_size(&host);
        let frame = reader.renderer.borrow_mut().render_cpu_if_needed(
            width,
            height,
            host.window().scale_factor(),
        );
        if let Ok(Some(frame)) = frame {
            apply_cpu_frame(&host, frame);
        }
    }
}

/// The reader asks for a message of the conversation again, as "Reload
/// message" does.
fn ensure_body(id: u64, message_id: i32) {
    let (Some(windows), Some(reader)) = (registry(), find(id)) else {
        return;
    };
    if message_id < 0 || reader.loading.get() == Some(message_id) {
        return;
    }
    let known = reader
        .messages
        .borrow()
        .iter()
        .any(|message| message.id == message_id);
    load(&windows, &reader, known.then_some(i64::from(message_id)));
}

/// Another message of the conversation opens in the window.
fn select_thread_message(id: u64, index: i32) {
    let (Some(windows), Some(reader)) = (registry(), find(id)) else {
        return;
    };
    let Ok(index) = usize::try_from(index) else {
        return;
    };
    let message = reader.messages.borrow().get(index).cloned();
    let Some(message) = message else {
        return;
    };
    if index == reader.selected.get() {
        return;
    }
    reader.selected.set(index);
    if let Some(app) = windows.app.upgrade() {
        let host = reader.reader(&app);
        let ui = host.ui();
        ui.set_text_mode(false);
        ui.set_source_mode(false);
        crate::message_source::hide(&host);
        ui.set_rendering_info_open(false);
    }
    render(&windows, &reader);
    if message.body_pending || message.html.is_none() {
        load(&windows, &reader, Some(i64::from(message.id)));
    }
}

/// Archive, delete, star, mark or report the window's conversation. What
/// takes it from where it was closes the window.
fn act(id: u64, action: &str) {
    let (Some(windows), Some(reader)) = (registry(), find(id)) else {
        return;
    };
    let Some(app) = windows.app.upgrade() else {
        return;
    };
    let row = reader.row.borrow().clone();
    let result = (|| {
        let core = windows
            .state
            .borrow()
            .core
            .clone()
            .filter(|_| windows.state.borrow().using_core)
            .ok_or_else(|| "mail account is not ready".to_owned())?;
        let target = MailTarget::of(&row).ok_or_else(|| "no message is selected".to_owned())?;
        mail_work::enqueue(
            &windows.state,
            core,
            vec![(
                row.id,
                target,
                mail_work::Operation::Action(action.to_owned()),
            )],
        )
    })();
    match result {
        Ok(()) if matches!(action, "archive" | "trash" | "delete_permanently" | "spam") => {
            close(id);
        }
        Ok(()) => {}
        Err(error) => {
            app.set_render_status(UiMessage::detail("Message action failed: {}", error));
        }
    }
}

/// Reply, forward or edit from the window: the message opens in a composer
/// window of its own, with `reply` written in it when there is one.
fn compose(id: u64, action: &str, reply: Option<String>) {
    let Some(windows) = registry() else {
        return;
    };
    let (Some(app), Some(reader)) = (windows.app.upgrade(), find(id)) else {
        return;
    };
    crate::composer::compose_from_reader(&app, &reader.reader(&app), action, reply);
}

fn toggle_label(id: u64, label_id: i32, applied: bool) {
    let (Some(windows), Some(reader)) = (registry(), find(id)) else {
        return;
    };
    let row = reader.row.borrow().clone();
    match crate::toggle_mail_label(&windows.state, Some(&row), label_id, applied) {
        Ok(()) => {
            // The chips change at once; the list's row follows later.
            let mut row = reader.row.borrow_mut();
            let label = i64::from(label_id);
            row.labels.retain(|applied| *applied != label);
            if applied {
                row.labels.push(label);
            }
            drop(row);
            if let Some(app) = windows.app.upgrade() {
                let state = windows.state.borrow();
                crate::mail_view_model::apply_label_rows(
                    &reader.reader(&app),
                    &state.labels,
                    Some(&reader.row.borrow()),
                );
            }
        }
        Err(error) => {
            if let Some(app) = windows.app.upgrade() {
                app.set_render_status(UiMessage::detail("Could not update label: {}", error));
            }
        }
    }
}

fn save_label(id: u64, label_id: i32, name: String, color: slint::Color, global: bool) {
    let (Some(windows), Some(reader)) = (registry(), find(id)) else {
        return;
    };
    let Some(app) = windows.app.upgrade() else {
        return;
    };
    let row = reader.row.borrow().clone();
    crate::save_mail_label(
        &app,
        &windows.state,
        &windows.runtime,
        Some(&row),
        crate::LabelEdit {
            label_id,
            owner_account_id: -1,
            name,
            color,
            global,
            parent_label_id: -1,
        },
    );
    load(&windows, &reader, None);
}

fn search_labels(id: u64, query: &str) {
    let (Some(windows), Some(reader)) = (registry(), find(id)) else {
        return;
    };
    let Some(app) = windows.app.upgrade() else {
        return;
    };
    let state = windows.state.borrow();
    let rows =
        crate::mail_view_model::make_label_rows(&state.labels, Some(&reader.row.borrow()), query);
    reader
        .reader(&app)
        .ui()
        .set_mail_label_results(ModelRc::new(VecModel::from(rows)));
}

fn print(id: u64) {
    let Some(windows) = registry() else {
        return;
    };
    let Some(app) = windows.app.upgrade() else {
        return;
    };
    match crate::account_controller::print_shown_message(&windows.state, ReaderKey::Window(id)) {
        Ok(()) => app.set_render_status(UiMessage::plain("Opened the system print view.")),
        Err(error) => {
            app.set_render_status(UiMessage::detail("Could not print message: {}", error))
        }
    }
}

fn open_in_browser(id: u64) {
    let Some(windows) = registry() else {
        return;
    };
    let Some(app) = windows.app.upgrade() else {
        return;
    };
    match crate::account_controller::open_shown_message_in_browser(
        &windows.state,
        ReaderKey::Window(id),
    ) {
        Ok(()) => app.set_render_status(UiMessage::plain("Opened the message in your browser.")),
        Err(error) => app.set_render_status(UiMessage::detail(
            "Could not open message in browser: {}",
            error,
        )),
    }
}

/// "Load images" on the window's message: remote images show in this
/// conversation here, as they do in the main window for its selection.
fn load_remote_images(id: u64) {
    let (Some(windows), Some(reader)) = (registry(), find(id)) else {
        return;
    };
    if !cfg!(feature = "remote-content") {
        if let Some(app) = windows.app.upgrade() {
            app.set_render_status(UiMessage::plain("Remote-image loading is disabled."));
        }
        return;
    }
    reader.remote_images.set(true);
    reader.renderer.borrow_mut().loaded_key = None;
    render(&windows, &reader);
}
