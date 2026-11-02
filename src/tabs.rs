//! Browser-style tabs.
//!
//! A tab is a saved place: a workspace plus, for mail, the mailbox, search,
//! filter and open conversation; the Settings page when Settings is open over
//! that place; and how far the mail list and reader were scrolled. Only the
//! active tab is live. An inactive mail tab keeps a copy of its list rows
//! without message bodies, so switching back shows its list and conversation
//! at once, and the refresh that follows re-reads every row so none of them
//! stays stale. Rendered email documents are never kept per tab.

use super::*;
use chrono::NaiveDate;
use std::path::Path;
use std::rc::Weak;

/// Tabs beyond this many are refused; each inactive mail tab keeps its rows.
const MAX_TABS: usize = 32;
/// Closed tabs that can be reopened, the newest last.
const MAX_CLOSED: usize = 16;
const SAVE_DELAY: Duration = Duration::from_millis(500);
/// How often, and for how long, a shown tab's reader looks for its laid-out
/// message to scroll back to.
const READER_POLL: Duration = Duration::from_millis(50);
const READER_DEADLINE: Duration = Duration::from_secs(10);
/// Settings pages a tab can reopen: the `settings_tab` values of the UI.
const SETTINGS_PAGES: [&str; 8] = [
    "General",
    "Accounts",
    "Contacts",
    "Notifications",
    "Shortcuts",
    "Data",
    "Services",
    "About",
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "workspace", rename_all = "lowercase")]
pub(crate) enum Location {
    Mail {
        #[serde(deserialize_with = "saved_scope")]
        scope: Scope,
        #[serde(default)]
        query: String,
        #[serde(default = "default_filter")]
        filter: String,
        /// Open conversation (thread row id); `None` shows an empty reader.
        #[serde(default)]
        conversation: Option<i32>,
    },
    Calendar {
        date: NaiveDate,
        mode: String,
    },
    Contacts,
    Files,
}

fn default_filter() -> String {
    "All mail".to_owned()
}

/// Tabs saved before account mailboxes were keyed by id named them by display
/// name. Reopen such a tab at the inbox instead of discarding every tab.
fn saved_scope<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Scope, D::Error> {
    let key = <String as serde::Deserialize>::deserialize(deserializer)?;
    Ok(key.parse().unwrap_or_default())
}

impl Location {
    fn inbox() -> Self {
        Self::Mail {
            scope: Scope::UNIFIED_INBOX,
            query: String::new(),
            filter: default_filter(),
            conversation: None,
        }
    }

    fn workspace(&self) -> &'static str {
        match self {
            Self::Mail { .. } => "mail",
            Self::Calendar { .. } => "calendar",
            Self::Contacts => "contacts",
            Self::Files => "files",
        }
    }
}

impl Default for Location {
    fn default() -> Self {
        Self::inbox()
    }
}

/// How far a tab's views were scrolled, in logical pixels from the top: the
/// mail list, and the body of the message open in the reader.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Scroll {
    #[serde(default)]
    list: f32,
    #[serde(default)]
    reader: f32,
}

impl Scroll {
    /// Saved offsets come from a file; keep them finite and on the page.
    fn sanitized(self) -> Self {
        let offset = |value: f32| {
            if value.is_finite() {
                value.clamp(0.0, 10_000_000.0)
            } else {
                0.0
            }
        };
        Self {
            list: offset(self.list),
            reader: offset(self.reader),
        }
    }
}

/// Where a tab is: its location, the Settings page open over it, the
/// message open in its conversation, and how far its views were scrolled.
#[derive(Clone, Debug, Default, PartialEq)]
struct Place {
    location: Location,
    settings: Option<String>,
    /// Message open in a multi-message conversation; `None` opens the newest.
    thread_message: Option<i32>,
    /// Account of the open conversation, so the tab forgets it with the
    /// account.
    account: Option<i64>,
    scroll: Scroll,
    /// A conversation opened in the background and not shown yet. Showing it
    /// opens it, so it is marked read like any other opened conversation.
    unseen: bool,
}

impl Place {
    fn conversation(&self) -> Option<i32> {
        match self.location {
            Location::Mail { conversation, .. } => conversation,
            _ => None,
        }
    }
}

/// List rows an inactive mail tab returns to, without message bodies, and
/// the date groups it had collapsed.
struct CachedRows {
    messages: Vec<MailMessage>,
    next_cursor: Option<MailCursor>,
    collapsed: HashSet<String>,
}

struct Tab {
    id: i32,
    place: Place,
    /// Title shown while the tab is inactive; the active tab shows live state.
    title: String,
    rows: Option<CachedRows>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct SavedTab {
    location: Location,
    #[serde(default)]
    title: String,
    /// Settings page open over the location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settings: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thread_message: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<i64>,
    #[serde(default)]
    scroll: Scroll,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    unseen: bool,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct SavedTabs {
    tabs: Vec<SavedTab>,
    #[serde(default)]
    active: usize,
}

impl SavedTab {
    fn into_place(self) -> (Place, String) {
        let settings = self
            .settings
            .filter(|page| SETTINGS_PAGES.contains(&page.as_str()));
        (
            Place {
                location: self.location,
                settings,
                thread_message: self.thread_message,
                account: self.account,
                scroll: self.scroll.sanitized(),
                unseen: self.unseen,
            },
            self.title,
        )
    }
}

/// A closed tab that can be reopened where it was.
struct ClosedTab {
    index: usize,
    place: Place,
    title: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Closed {
    /// Nothing was closed.
    Missing,
    /// The active tab stayed open.
    Inactive,
    /// The active tab changed; the tab at this index is now active.
    Active(usize),
}

struct TabStrip {
    tabs: Vec<Tab>,
    active: usize,
    next_id: i32,
    closed: Vec<ClosedTab>,
}

impl TabStrip {
    fn new(place: Place) -> Self {
        let mut strip = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            closed: Vec::new(),
        };
        strip.insert(0, place, String::new(), None);
        strip
    }

    fn restore(saved: SavedTabs) -> Self {
        let mut tabs = saved
            .tabs
            .into_iter()
            .take(MAX_TABS)
            .map(SavedTab::into_place);
        let Some((place, title)) = tabs.next() else {
            return Self::new(Place::default());
        };
        let mut strip = Self::new(place);
        strip.tabs[0].title = title;
        for (place, title) in tabs {
            let index = strip.tabs.len();
            strip.insert(index, place, title, None);
        }
        strip.active = saved.active.min(strip.tabs.len() - 1);
        strip
    }

    fn saved(&self) -> SavedTabs {
        SavedTabs {
            tabs: self
                .tabs
                .iter()
                .map(|tab| SavedTab {
                    location: tab.place.location.clone(),
                    title: tab.title.clone(),
                    settings: tab.place.settings.clone(),
                    thread_message: tab.place.thread_message,
                    account: tab.place.account,
                    scroll: tab.place.scroll,
                    unseen: tab.place.unseen,
                })
                .collect(),
            active: self.active,
        }
    }

    fn insert(
        &mut self,
        index: usize,
        place: Place,
        title: String,
        rows: Option<CachedRows>,
    ) -> usize {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.tabs.insert(
            index,
            Tab {
                id,
                place,
                title,
                rows,
            },
        );
        if index <= self.active && self.tabs.len() > 1 {
            self.active += 1;
        }
        index
    }

    /// Open a tab to the right of the active one, as browsers do.
    fn open(&mut self, place: Place, title: String, rows: Option<CachedRows>) -> Option<usize> {
        if self.tabs.len() >= MAX_TABS {
            return None;
        }
        Some(self.insert(self.active + 1, place, title, rows))
    }

    fn index_of(&self, id: i32) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Keep a closed tab so it can be reopened.
    fn remember(&mut self, index: usize, tab: &Tab) {
        if self.closed.len() == MAX_CLOSED {
            self.closed.remove(0);
        }
        self.closed.push(ClosedTab {
            index,
            place: tab.place.clone(),
            title: tab.title.clone(),
        });
    }

    /// Close a tab. The only tab stays and returns to `home`, the inbox.
    fn close(&mut self, id: i32, home: &Place) -> Closed {
        let Some(index) = self.index_of(id) else {
            return Closed::Missing;
        };
        if self.tabs.len() == 1 {
            if self.tabs[0].place == *home {
                return Closed::Missing;
            }
            let tab = std::mem::replace(
                &mut self.tabs[0],
                Tab {
                    id,
                    place: home.clone(),
                    title: String::new(),
                    rows: None,
                },
            );
            self.remember(0, &tab);
            return Closed::Active(0);
        }
        let tab = self.tabs.remove(index);
        self.remember(index, &tab);
        if index == self.active {
            // Browsers activate the tab that took the closed tab's place.
            self.active = index.min(self.tabs.len() - 1);
            Closed::Active(self.active)
        } else {
            if index < self.active {
                self.active -= 1;
            }
            Closed::Inactive
        }
    }

    /// Close every tab but `id`. Closed tabs are remembered right to left, so
    /// reopening them one by one puts each back where it was.
    fn close_others(&mut self, id: i32) -> Closed {
        let Some(keep) = self.index_of(id) else {
            return Closed::Missing;
        };
        if self.tabs.len() == 1 {
            return Closed::Missing;
        }
        let active_closed = keep != self.active;
        let mut tabs = std::mem::take(&mut self.tabs);
        let kept = tabs.remove(keep);
        for (index, tab) in tabs.iter().enumerate().rev() {
            let index = if index >= keep { index + 1 } else { index };
            self.remember(index, tab);
        }
        self.tabs.push(kept);
        self.active = 0;
        if active_closed {
            Closed::Active(0)
        } else {
            Closed::Inactive
        }
    }

    /// Close the tabs to the right of `id`.
    fn close_to_right(&mut self, id: i32) -> Closed {
        let Some(keep) = self.index_of(id) else {
            return Closed::Missing;
        };
        if keep + 1 == self.tabs.len() {
            return Closed::Missing;
        }
        let closed = self.tabs.split_off(keep + 1);
        for (offset, tab) in closed.iter().enumerate().rev() {
            self.remember(keep + 1 + offset, tab);
        }
        if self.active > keep {
            self.active = keep;
            Closed::Active(keep)
        } else {
            Closed::Inactive
        }
    }

    /// Reopen the most recently closed tab where it was.
    fn reopen(&mut self) -> Option<usize> {
        if self.tabs.len() >= MAX_TABS {
            return None;
        }
        let closed = self.closed.pop()?;
        let index = closed.index.min(self.tabs.len());
        Some(self.insert(index, closed.place, closed.title, None))
    }

    fn move_tab(&mut self, id: i32, to: usize) {
        let Some(from) = self.index_of(id) else {
            return;
        };
        let to = to.min(self.tabs.len() - 1);
        if from == to {
            return;
        }
        let active_id = self.tabs[self.active].id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self.index_of(active_id).unwrap_or(0);
    }

    fn cycled(&self, delta: isize) -> usize {
        let count = self.tabs.len() as isize;
        (self.active as isize + delta).rem_euclid(count) as usize
    }
}

/// Keyboard commands, matching browser conventions on each platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    New,
    Close,
    Reopen,
    Next,
    Previous,
    /// Zero-based tab index.
    Select(usize),
    Last,
}

impl Command {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "new" => Self::New,
            "close" => Self::Close,
            "reopen" => Self::Reopen,
            "next" => Self::Next,
            "previous" => Self::Previous,
            "last" => Self::Last,
            index => Self::Select(index.parse::<usize>().ok()?),
        })
    }

    #[cfg(any(test, desktop))]
    pub(crate) fn key(self) -> String {
        match self {
            Self::New => "new".into(),
            Self::Close => "close".into(),
            Self::Reopen => "reopen".into(),
            Self::Next => "next".into(),
            Self::Previous => "previous".into(),
            Self::Last => "last".into(),
            Self::Select(index) => index.to_string(),
        }
    }
}

/// Map a key press to a tab command. `primary` is Command on macOS and
/// Control elsewhere; Control+Tab cycles tabs on every platform.
#[cfg(desktop)]
pub(crate) fn shortcut(
    key: &slint::winit_030::winit::keyboard::Key,
    modifiers: slint::winit_030::winit::keyboard::ModifiersState,
) -> Option<Command> {
    use slint::winit_030::winit::keyboard::{Key, NamedKey};
    let primary = if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    };
    let shift = modifiers.shift_key();
    match key {
        Key::Named(NamedKey::Tab) if modifiers.control_key() && !modifiers.alt_key() => {
            Some(if shift {
                Command::Previous
            } else {
                Command::Next
            })
        }
        Key::Named(NamedKey::PageDown) if modifiers.control_key() && !shift => Some(Command::Next),
        Key::Named(NamedKey::PageUp) if modifiers.control_key() && !shift => {
            Some(Command::Previous)
        }
        Key::Character(text) if primary && !modifiers.alt_key() => {
            let text = text.to_lowercase();
            match (text.as_str(), shift) {
                ("t", false) => Some(Command::New),
                ("t", true) => Some(Command::Reopen),
                ("w", false) => Some(Command::Close),
                // Safari and Chrome on macOS also cycle with Command+Shift+[ ].
                ("[" | "{", true) if cfg!(target_os = "macos") => Some(Command::Previous),
                ("]" | "}", true) if cfg!(target_os = "macos") => Some(Command::Next),
                ("9", false) => Some(Command::Last),
                (digit, false) => digit
                    .parse::<usize>()
                    .ok()
                    .filter(|digit| (1..=8).contains(digit))
                    .map(|digit| Command::Select(digit - 1)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn read_file(path: &Path) -> Option<SavedTabs> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 256 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn write_file(path: &Path, tabs: &SavedTabs) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().ok_or("tab state has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, tabs)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

/// Everything a tab switch needs from the mail, calendar and window state.
pub(crate) struct TabContext {
    pub(crate) app: slint::Weak<AppWindow>,
    pub(crate) state: Rc<RefCell<InboxState>>,
    pub(crate) runtime: Rc<tokio::runtime::Runtime>,
    pub(crate) calendar: Rc<RefCell<LocalCalendarState>>,
    pub(crate) mail_pagination_generation: Rc<Cell<u64>>,
    pub(crate) mail_pagination_in_progress: Rc<Cell<bool>>,
    pub(crate) path: PathBuf,
}

/// The reader position a shown tab is scrolling back to.
#[derive(Clone, Copy, Debug)]
struct ReaderRestore {
    conversation: i32,
    /// The message of the conversation the offset belongs to, when known.
    message: Option<i32>,
    offset: f32,
    /// The message has been laid out for a poll, so the reader's own resets
    /// for a newly shown message have run.
    settled: bool,
    /// Where it was last scrolled to, which tells the user's own scrolling
    /// apart.
    applied: Option<f32>,
    started: Instant,
}

struct Tabs {
    context: TabContext,
    strip: RefCell<TabStrip>,
    /// The strip's rows. They are updated in place: a new model would
    /// recreate every tab element and drop a press or drag under way.
    rows: Rc<VecModel<TabRow>>,
    save_timer: slint::Timer,
    /// The tabs last written, so an unchanged strip is not written again.
    written: RefCell<Option<SavedTabs>>,
    /// Writes of the file, numbered in the order they were asked for.
    writes: Cell<u64>,
    /// The newest write done. Writes take turns, and one older than it skips.
    newest_write: Arc<Mutex<u64>>,
    reader_timer: slint::Timer,
    reader_restore: Cell<Option<ReaderRestore>>,
    /// Tabs respond, and navigation is saved, only once the saved active tab
    /// has been shown; until then the window shows the startup inbox, which
    /// must not overwrite a tab's place.
    restored: Cell<bool>,
    weak: Weak<Tabs>,
}

pub(crate) fn register(app: &AppWindow, context: TabContext) {
    let tabs = Tabs::new(app, context);
    let global = app.global::<TabsUi>();

    // The UI callbacks own the tab state: nothing else keeps it alive, and
    // `Tabs` refers back to the window only weakly, so there is no cycle.
    let owner = Rc::clone(&tabs);
    global.on_select(move |id| {
        // Release the strip borrow before activating, which mutates it.
        let index = owner.strip.borrow().index_of(id);
        if let Some(index) = index
            && owner.restored.get()
        {
            owner.activate(index);
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_close(move |id| {
        if owner.restored.get() {
            owner.close(id);
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_close_others(move |id| {
        if owner.restored.get() {
            owner.capture(false);
            let closed = owner.strip.borrow_mut().close_others(id);
            owner.closed(closed);
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_close_to_right(move |id| {
        if owner.restored.get() {
            owner.capture(false);
            let closed = owner.strip.borrow_mut().close_to_right(id);
            owner.closed(closed);
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_create(move || {
        if owner.restored.get() {
            owner.create();
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_reopen(move || {
        if owner.restored.get() {
            owner.reopen();
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_move(move |id, to| {
        if owner.restored.get() {
            owner
                .strip
                .borrow_mut()
                .move_tab(id, usize::try_from(to).unwrap_or(0));
            owner.changed();
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_open_conversation(move |id, activate| {
        if owner.restored.get() {
            owner.open_conversation(id, activate);
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_command(move |command| {
        if let Some(command) = Command::parse(&command)
            && owner.restored.get()
        {
            owner.command(command);
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_account_removed(move |account| {
        if let Ok(account) = account.parse() {
            owner.account_removed(account);
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_navigated(move || {
        if owner.restored.get() {
            owner.schedule_save();
        }
    });
    let owner = Rc::clone(&tabs);
    global.on_reset(move || owner.reset());
    let owner = Rc::clone(&tabs);
    global.on_restore_active(move || {
        owner.restored.set(true);
        let active = owner.strip.borrow().active;
        owner.restore(active);
    });
    global.on_flush(move || {
        tabs.save_timer.stop();
        tabs.save(true);
    });
}

impl Tabs {
    /// Load the saved tabs and show them in the strip.
    fn new(app: &AppWindow, context: TabContext) -> Rc<Self> {
        let strip = read_file(&context.path)
            .map_or_else(|| TabStrip::new(Place::default()), TabStrip::restore);
        let tabs = Rc::new_cyclic(|weak| Self {
            context,
            strip: RefCell::new(strip),
            rows: Rc::new(VecModel::default()),
            save_timer: slint::Timer::default(),
            written: RefCell::new(None),
            writes: Cell::new(0),
            newest_write: Arc::new(Mutex::new(0)),
            reader_timer: slint::Timer::default(),
            reader_restore: Cell::new(None),
            restored: Cell::new(false),
            weak: weak.clone(),
        });
        app.global::<TabsUi>()
            .set_rows(ModelRc::from(Rc::clone(&tabs.rows)));
        tabs.publish();
        tabs
    }

    fn app(&self) -> Option<AppWindow> {
        self.context.app.upgrade()
    }

    fn command(&self, command: Command) {
        let Some(app) = self.app() else {
            return;
        };
        // Tabs live in the desktop top bar; the composer and the calendar
        // editor keep their keys.
        if !app.global::<TabsUi>().get_enabled()
            || app.get_compose_open()
            || app.get_calendar_editor_open()
        {
            return;
        }
        let (count, active) = {
            let strip = self.strip.borrow();
            (strip.tabs.len(), strip.active)
        };
        match command {
            Command::New => self.create(),
            Command::Close => {
                let id = self.strip.borrow().tabs[active].id;
                self.close(id);
            }
            Command::Reopen => self.reopen(),
            Command::Next => {
                let index = self.strip.borrow().cycled(1);
                self.activate(index);
            }
            Command::Previous => {
                let index = self.strip.borrow().cycled(-1);
                self.activate(index);
            }
            Command::Last => self.activate(count - 1),
            Command::Select(index) if index < count => self.activate(index),
            Command::Select(_) => {}
        }
    }

    fn create(&self) {
        let opened = self
            .strip
            .borrow_mut()
            .open(Place::default(), String::new(), None);
        match opened {
            Some(index) => self.activate(index),
            None => self.refuse(),
        }
    }

    fn reopen(&self) {
        let reopened = self.strip.borrow_mut().reopen();
        match reopened {
            Some(index) => self.activate(index),
            None if !self.strip.borrow().closed.is_empty() => self.refuse(),
            None => {}
        }
    }

    fn refuse(&self) {
        if let Some(app) = self.app() {
            app.set_sync_status(UiMessage::detail(
                "At most {} tabs can be open. Close a tab to open another.",
                MAX_TABS,
            ));
        }
    }

    /// Open a conversation from the current mail list in a new tab.
    fn open_conversation(&self, id: i32, activate: bool) {
        let Some(app) = self.app() else {
            return;
        };
        let (place, title, rows) = {
            let state = self.context.state.borrow();
            let Some(row) = state.messages.iter().find(|row| row.id == id) else {
                return;
            };
            let place = Place {
                location: Location::Mail {
                    scope: state.scope,
                    query: state.query.clone(),
                    filter: state.search_filter.clone(),
                    conversation: Some(id),
                },
                account: Some(row.account_id),
                // The new tab's list starts where this one is.
                scroll: Scroll {
                    list: app.global::<TabsUi>().get_list_offset().max(0.0),
                    reader: 0.0,
                },
                unseen: true,
                ..Place::default()
            };
            let title = if row.subject.is_empty() {
                app.get_selected_scope_title().to_string()
            } else {
                row.subject.clone()
            };
            (place, title, cached_rows(&state))
        };
        let opened = self.strip.borrow_mut().open(place, title, Some(rows));
        match opened {
            Some(index) if activate => self.activate(index),
            Some(_) => self.changed(),
            None => self.refuse(),
        }
    }

    fn close(&self, id: i32) {
        // Remember where the active tab is now, so reopening it returns there.
        let active_id = {
            let strip = self.strip.borrow();
            strip.tabs[strip.active].id
        };
        if id == active_id {
            self.capture(false);
        }
        let home = self.home();
        let closed = self.strip.borrow_mut().close(id, &home);
        self.closed(closed);
    }

    /// The inbox as the window names it: by its account when it shows only
    /// one.
    fn home(&self) -> Place {
        let state = self.context.state.borrow();
        Place {
            location: Location::Mail {
                scope: Scope::UNIFIED_INBOX.visible_for(&state.mail_accounts()),
                query: String::new(),
                filter: default_filter(),
                conversation: None,
            },
            ..Place::default()
        }
    }

    fn closed(&self, closed: Closed) {
        match closed {
            Closed::Missing => return,
            Closed::Active(index) => self.restore(index),
            Closed::Inactive => {}
        }
        self.changed();
    }

    fn activate(&self, index: usize) {
        {
            let strip = self.strip.borrow();
            if index == strip.active || index >= strip.tabs.len() {
                return;
            }
        }
        self.capture(true);
        self.strip.borrow_mut().active = index;
        self.restore(index);
        self.changed();
    }

    /// Save where the active tab is now, and with `with_rows` the list rows
    /// it returns to.
    fn capture(&self, with_rows: bool) {
        let Some(app) = self.app() else {
            return;
        };
        let (mut place, title, rows) = self.live(&app, with_rows);
        let mut strip = self.strip.borrow_mut();
        let active = strip.active;
        let tab = &mut strip.tabs[active];
        // A conversation still loading has no row to name its account yet.
        if place.account.is_none() && place.conversation() == tab.place.conversation() {
            place.account = tab.place.account;
        }
        tab.place = place;
        if let Some(title) = title {
            tab.title = title;
        }
        if with_rows {
            tab.rows = rows;
        }
    }

    /// The active tab's live place, its title when known, and with
    /// `with_rows` a copy of its list rows.
    fn live(
        &self,
        app: &AppWindow,
        with_rows: bool,
    ) -> (Place, Option<String>, Option<CachedRows>) {
        let settings = app
            .get_settings_open()
            .then(|| app.get_settings_tab().to_string());
        let mut place = Place {
            settings,
            ..Place::default()
        };
        let (title, rows) = match app.get_active_view().as_str() {
            "calendar" => {
                let calendar = self.context.calendar.borrow();
                place.location = Location::Calendar {
                    date: calendar.selected_date,
                    mode: calendar.view_mode.clone(),
                };
                (Some(String::new()), None)
            }
            "contacts" => {
                place.location = Location::Contacts;
                (Some(String::new()), None)
            }
            "files" => {
                place.location = Location::Files;
                (Some(String::new()), None)
            }
            _ => {
                let ui = app.global::<TabsUi>();
                let state = self.context.state.borrow();
                let shown = state.selected_id.filter(|_| !state.preview_closed);
                // A shown tab may still be loading the conversation it reopens.
                let conversation = state.pending_selection.or(shown);
                let scope_title = || app.get_selected_scope_title().to_string();
                let row =
                    conversation.and_then(|id| state.messages.iter().find(|row| row.id == id));
                let title = match (conversation, row) {
                    (None, _) => Some(scope_title()),
                    (Some(_), Some(row)) if row.subject.is_empty() => Some(scope_title()),
                    (Some(_), Some(row)) => Some(row.subject.clone()),
                    // Unknown while loading; the tab keeps its saved title.
                    (Some(_), None) => None,
                };
                place.account = row.map(|row| row.account_id);
                // A background tab shown before its first page arrived has
                // still not been seen.
                place.unseen =
                    state.pending_selection_opens && state.pending_selection == conversation;
                place.thread_message = conversation.and_then(|id| {
                    // A conversation still loading may be another tab's.
                    if state.conversation_owner_id == Some(id) && state.rendered_id == Some(id) {
                        state
                            .conversation_messages
                            .get(state.conversation_selected_index)
                            .map(|message| message.id)
                    } else {
                        state
                            .restore_thread_message
                            .filter(|(owner, _)| *owner == id)
                            .map(|(_, message)| message)
                    }
                });
                let reader_restore = self
                    .reader_restore
                    .get()
                    .filter(|restore| Some(restore.conversation) == conversation);
                place.scroll = Scroll {
                    // Positions still being restored count as where the tab is.
                    list: if ui.get_list_restore() >= 0.0 {
                        ui.get_list_restore()
                    } else {
                        ui.get_list_offset().max(0.0)
                    },
                    reader: match (reader_restore, conversation) {
                        (Some(restore), _) => restore.offset,
                        (None, Some(_)) => (-app.get_email_scroll_y()).max(0.0),
                        (None, None) => 0.0,
                    },
                };
                place.location = Location::Mail {
                    scope: state.scope,
                    query: state.query.clone(),
                    filter: state.search_filter.clone(),
                    conversation,
                };
                (
                    title,
                    // A list still loading has no rows to return to.
                    (with_rows && state.using_core && !mail_work::replacing(&state))
                        .then(|| cached_rows(&state)),
                )
            }
        };
        (place, title, rows)
    }

    /// Show a tab's place through the app's normal navigation state.
    fn restore(&self, index: usize) {
        let Some(app) = self.app() else {
            return;
        };
        let (place, rows) = {
            let mut strip = self.strip.borrow_mut();
            let tab = &mut strip.tabs[index];
            let place = tab.place.clone();
            // Showing the tab is what it waited for; the live state owns its
            // rows while it is active.
            tab.place.unseen = false;
            (place, tab.rows.take())
        };
        self.reader_timer.stop();
        self.reader_restore.set(None);
        let ui = app.global::<TabsUi>();
        let Place {
            location,
            settings,
            thread_message,
            scroll,
            unseen,
            ..
        } = place;
        if !matches!(location, Location::Mail { .. }) {
            // A mail tab left before its page arrived has saved that its
            // conversation is unseen; the page must not open it now.
            self.context.state.borrow_mut().pending_selection_opens = false;
        }
        match location {
            Location::Mail {
                scope,
                query,
                filter,
                conversation,
            } => self.restore_mail(
                &app,
                MailPlace {
                    scope,
                    query,
                    filter,
                    conversation,
                    thread_message,
                    scroll,
                },
                unseen,
                rows,
            ),
            Location::Calendar { date, mode } => {
                ui.set_list_restore(-1.0);
                self.restore_calendar(&app, date, mode);
            }
            Location::Contacts | Location::Files => {
                ui.set_list_restore(-1.0);
                app.set_active_view(location.workspace().into());
            }
        }
        // Settings opens over the place it was opened from.
        if let Some(page) = &settings {
            app.set_settings_tab(page.as_str().into());
        }
        app.set_settings_open(settings.is_some());
    }

    fn restore_calendar(&self, app: &AppWindow, date: NaiveDate, mode: String) {
        {
            let mut calendar = self.context.calendar.borrow_mut();
            calendar.selected_date = date;
            calendar.visible_month = first_of_month(date);
            calendar.view_mode = mode;
        }
        if app.get_active_view() != "calendar" {
            // Entering the calendar loads the visible month.
            app.set_active_view("calendar".into());
            return;
        }
        let mut calendar = self.context.calendar.borrow_mut();
        let core = self.context.state.borrow().core.clone();
        if let Some(core) = core
            && let Err(error) = refresh_calendar_events(
                &core,
                &self.context.runtime,
                &mut calendar,
                &self.context.state.borrow().connected_accounts,
            )
        {
            app.set_sync_status(UiMessage::detail("Could not load calendar: {}", error));
        }
        apply_calendar(app, &calendar, Local::now().date_naive());
    }

    fn restore_mail(
        &self,
        app: &AppWindow,
        place: MailPlace,
        unseen: bool,
        rows: Option<CachedRows>,
    ) {
        let MailPlace {
            scope,
            query,
            filter,
            conversation,
            thread_message,
            scroll,
        } = place;
        let context = &self.context;
        context
            .mail_pagination_generation
            .set(context.mail_pagination_generation.get().wrapping_add(1));
        context.mail_pagination_in_progress.set(false);
        app.set_mail_loading_more(false);
        let (using_core, shown, view, conversation, thread_message, scroll) = {
            let mut state = context.state.borrow_mut();
            // A mailbox that is gone reopens at the top of the inbox, without
            // its rows or conversation.
            let (scope, rows, conversation, thread_message, scroll) =
                match available_scope(scope, &state) {
                    Some(scope) => (scope, rows, conversation, thread_message, scroll),
                    None => (Scope::UNIFIED_INBOX, None, None, None, Scroll::default()),
                };
            // A tab shown after a restart has no rows of its own; reuse the
            // loaded list when it already shows the same mailbox and search.
            let rows = rows
                .map(|mut rows| {
                    // Mail of an account removed meanwhile is gone, and tabs
                    // share every profile: mail of an account the active
                    // profile leaves out is not shown.
                    if state.using_core {
                        let accounts = &state.connected_accounts;
                        rows.messages.retain(|row| {
                            accounts.iter().any(|account| account.id == row.account_id)
                                && state.shows_account(row.account_id)
                        });
                    }
                    rows
                })
                .or_else(|| {
                    (state.using_core
                        && state.scope == scope.visible_for(&state.mail_accounts())
                        && state.query == query)
                        .then(|| cached_rows(&state))
                })
                // Rows without the conversation cannot show it at once; the
                // rows loaded below reopen it if it is still there.
                .filter(|rows| {
                    conversation.is_none_or(|id| rows.messages.iter().any(|row| row.id == id))
                });
            mail_work::invalidate(&mut state);
            state.set_scope(scope);
            state.query = query;
            state.search_filter = filter;
            state.mail_groups.clear();
            state.page = 1;
            state.checked_ids.clear();
            state.preview_closed = conversation.is_none();
            // The conversation loads again with this tab's open message, even
            // when another tab showed it last.
            state.rendered_id = None;
            state.restore_thread_message = conversation.zip(thread_message);
            let shown = rows.is_some();
            match rows {
                Some(rows) => {
                    state.messages = rows.messages;
                    state.next_cursor = rows.next_cursor;
                    state.mail_groups.collapsed = rows.collapsed;
                    state.selected_id = conversation;
                    state.pending_selection = None;
                    state.pending_selection_opens = false;
                }
                None => {
                    state.messages.clear();
                    state.next_cursor = None;
                    state.selected_id = None;
                    state.pending_selection = conversation;
                    state.pending_selection_opens = unseen && conversation.is_some();
                }
            }
            // The list shows this as its mailbox, search and filter.
            let view = format!("{}\n{}\n{}", state.scope, state.query, state.search_filter);
            (
                state.using_core,
                shown,
                view,
                conversation,
                thread_message,
                scroll,
            )
        };
        // The list scrolls back once its rows are laid out.
        let ui = app.global::<TabsUi>();
        ui.set_list_restore(scroll.list);
        ui.set_list_restore_view(view.into());
        ui.set_list_restore_request(ui.get_list_restore_request().wrapping_add(1));
        app.set_active_view("mail".into());
        let result = match conversation {
            // Showing a background tab opens its conversation, which marks it
            // read like any opened conversation; returning to a tab does not.
            Some(id) if shown && unseen => {
                app.invoke_select_email(id);
                Ok(())
            }
            Some(id) if shown => select_message(app, &context.state, &context.runtime, id),
            _ => render_current(app, &context.state, &context.runtime),
        };
        if let Err(error) = result {
            app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
        }
        if using_core {
            // Re-read every row the tab returned to, or load its first page:
            // the list pages in further rows to reach its position, and a
            // conversation further down is looked for once the page lands.
            let result = refresh_from_source(app, &context.state, &context.runtime, shown, &[]);
            if let Err(error) = result {
                app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
            }
        }
        // Even at the top: a conversation another tab showed too keeps that
        // tab's position otherwise.
        if let Some(conversation) = conversation {
            self.restore_reader(conversation, thread_message, scroll.reader);
        }
    }

    /// Scroll a shown tab's conversation back to where it was read once its
    /// body is laid out, following it while late images make it taller.
    fn restore_reader(&self, conversation: i32, message: Option<i32>, offset: f32) {
        self.reader_restore.set(Some(ReaderRestore {
            conversation,
            message,
            offset,
            settled: false,
            applied: None,
            started: Instant::now(),
        }));
        let weak = self.weak.clone();
        self.reader_timer
            .start(slint::TimerMode::Repeated, READER_POLL, move || {
                if let Some(tabs) = weak.upgrade()
                    && !tabs.step_reader_restore()
                {
                    tabs.reader_timer.stop();
                    tabs.reader_restore.set(None);
                }
            });
    }

    /// Move the reader toward its restored position; false once there is
    /// nothing more to do.
    fn step_reader_restore(&self) -> bool {
        let (Some(app), Some(mut restore)) = (self.app(), self.reader_restore.get()) else {
            return false;
        };
        if restore.started.elapsed() > READER_DEADLINE || app.get_active_view() != "mail" {
            return false;
        }
        {
            let state = self.context.state.borrow();
            // The page that reopens the conversation is still loading.
            if state.pending_selection == Some(restore.conversation) {
                return true;
            }
            // Another conversation, or none, is open now.
            if state.selected_id != Some(restore.conversation) || state.preview_closed {
                return false;
            }
            if state.conversation_owner_id != Some(restore.conversation) {
                return true;
            }
            let Some(message) = state
                .conversation_messages
                .get(state.conversation_selected_index)
            else {
                return true;
            };
            // Another message of the conversation is open now.
            if restore.message.is_some_and(|id| id != message.id) {
                return false;
            }
            if message.body_pending {
                return true;
            }
        }
        // Wait one more poll: the reader resets its position for a newly
        // shown message after this one is laid out.
        if !restore.settled {
            restore.settled = true;
            self.reader_restore.set(Some(restore));
            return true;
        }
        let current = -app.get_email_scroll_y();
        // The user scrolled.
        if restore
            .applied
            .is_some_and(|applied| (current - applied).abs() > 1.0)
        {
            return false;
        }
        let viewport = app.get_email_viewport_height();
        let bottom = (app.get_email_scroll_document_height() - viewport).max(0.0);
        let y = restore.offset.min(bottom);
        if restore.applied != Some(y) {
            app.set_email_scroll_y(-y);
            app.invoke_email_scroll(y, viewport);
            restore.applied = Some(y);
            self.reader_restore.set(Some(restore));
        }
        // Keep following the message until it is tall enough.
        y < restore.offset
    }

    /// Deleting local data also deletes saved tabs, whose titles are message
    /// subjects. A pending save is cancelled so it cannot write them back.
    fn reset(&self) {
        self.save_timer.stop();
        *self.written.borrow_mut() = None;
        self.reader_timer.stop();
        self.reader_restore.set(None);
        *self.strip.borrow_mut() = TabStrip::new(Place::default());
        {
            // The window's mail view still names the deleted mailbox and
            // search; the next save would write them back.
            let mut state = self.context.state.borrow_mut();
            state.set_scope(Scope::UNIFIED_INBOX);
            state.query.clear();
            state.search_filter = default_filter();
            state.pending_selection = None;
            state.pending_selection_opens = false;
            state.restore_thread_message = None;
        }
        // The title the next save captures, which still names the mailbox.
        if let Some(app) = self.app() {
            refresh_list_metadata(&app, &self.context.state);
        }
        self.publish();
        self.write(None, false);
    }

    /// Forget a removed account's mail in the inactive tabs, before the window
    /// drops its mailboxes: tabs on its mailboxes reopen at the inbox, and
    /// its conversations close, so none of its subjects outlive it.
    fn account_removed(&self, account_id: i64) {
        {
            let state = self.context.state.borrow();
            let owned = |place: &Place| match place.location {
                Location::Mail { scope, .. } => scope_owned_by(&state, scope, account_id),
                _ => false,
            };
            let forget = |place: &mut Place, title: &mut String, rows: Option<&mut CachedRows>| {
                if owned(place) {
                    *place = Place {
                        settings: place.settings.take(),
                        ..Place::default()
                    };
                    title.clear();
                    return true;
                }
                let conversation = place.conversation();
                let in_rows = rows.as_ref().is_some_and(|rows| {
                    rows.messages
                        .iter()
                        .any(|row| Some(row.id) == conversation && row.account_id == account_id)
                });
                if let Some(rows) = rows {
                    rows.messages.retain(|row| row.account_id != account_id);
                }
                if place.account == Some(account_id) || in_rows {
                    if let Location::Mail { conversation, .. } = &mut place.location {
                        *conversation = None;
                    }
                    place.thread_message = None;
                    place.account = None;
                    place.scroll.reader = 0.0;
                    place.unseen = false;
                    title.clear();
                }
                false
            };
            let mut strip = self.strip.borrow_mut();
            let active = strip.active;
            for (index, tab) in strip.tabs.iter_mut().enumerate() {
                // The window forgets the active tab's mail itself.
                if index != active && forget(&mut tab.place, &mut tab.title, tab.rows.as_mut()) {
                    tab.rows = None;
                }
            }
            for closed in &mut strip.closed {
                forget(&mut closed.place, &mut closed.title, None);
            }
        }
        self.changed();
    }

    /// Publish rows, then save once the tab strip has been quiet briefly.
    fn changed(&self) {
        self.publish();
        self.schedule_save();
    }

    fn schedule_save(&self) {
        let weak = self.weak.clone();
        self.save_timer
            .start(slint::TimerMode::SingleShot, SAVE_DELAY, move || {
                if let Some(tabs) = weak.upgrade() {
                    tabs.save(false);
                }
            });
    }

    /// Save every tab, the active one where it is now; `blocking` writes
    /// before returning, for an application that is about to exit.
    fn save(&self, blocking: bool) {
        if self.restored.get() {
            self.capture(false);
            self.publish();
        }
        let saved = self.strip.borrow().saved();
        if self.written.borrow().as_ref() == Some(&saved) {
            return;
        }
        *self.written.borrow_mut() = Some(saved.clone());
        self.write(Some(saved), blocking);
    }

    /// Write the tabs, or with `None` delete the file. Writes run one at a
    /// time, and one that finds a newer write done skips, so an older strip
    /// can never replace a newer one.
    fn write(&self, tabs: Option<SavedTabs>, blocking: bool) {
        let number = self.writes.get() + 1;
        self.writes.set(number);
        let newest = Arc::clone(&self.newest_write);
        let path = self.context.path.clone();
        let write = move || {
            let mut newest = newest
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *newest > number {
                return;
            }
            *newest = number;
            match tabs {
                Some(tabs) => {
                    if let Err(error) = write_file(&path, &tabs) {
                        eprintln!("could not save tabs: {error}");
                    }
                }
                None => {
                    if let Err(error) = std::fs::remove_file(&path)
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        eprintln!("could not delete saved tabs: {error}");
                    }
                }
            }
        };
        if blocking {
            write();
        } else {
            self.context.runtime.spawn_blocking(write);
        }
    }

    fn publish(&self) {
        let strip = self.strip.borrow();
        let rows = strip.tabs.iter().enumerate().map(|(index, tab)| {
            let settings = tab.place.settings.is_some();
            TabRow {
                id: tab.id,
                workspace: if settings {
                    "settings"
                } else {
                    tab.place.location.workspace()
                }
                .into(),
                title: if settings { "" } else { tab.title.as_str() }.into(),
                active: index == strip.active,
            }
        });
        sync_rows(&self.rows, rows);
        if let Some(app) = self.app() {
            app.global::<TabsUi>()
                .set_can_reopen(!strip.closed.is_empty());
        }
    }
}

/// A shown tab's mail location, with the message its reader returns to and
/// how far the list and reader were scrolled.
struct MailPlace {
    scope: Scope,
    query: String,
    filter: String,
    conversation: Option<i32>,
    thread_message: Option<i32>,
    scroll: Scroll,
}

/// Update rows in place, so each tab keeps its element.
fn sync_rows(model: &VecModel<TabRow>, rows: impl Iterator<Item = TabRow>) {
    let mut count = 0;
    for (index, row) in rows.enumerate() {
        count = index + 1;
        match model.row_data(index) {
            Some(current) if current == row => {}
            Some(_) => model.set_row_data(index, row),
            None => model.push(row),
        }
    }
    while model.row_count() > count {
        model.remove(model.row_count() - 1);
    }
}

/// The mailbox a saved tab reopens, or `None` once it is gone: a deleted
/// folder or label, or a removed account. A mailbox of an account the active
/// profile leaves out is not shown either.
fn available_scope(scope: Scope, state: &InboxState) -> Option<Scope> {
    match scope {
        Scope::Unified(_) | Scope::Important | Scope::Other => Some(scope),
        // A label can move between sections, e.g. when it becomes automatic.
        Scope::Label { id, .. } => state
            .labels
            .iter()
            .find(|label| label.id == id)
            .filter(|label| {
                label
                    .owner_account_id
                    .is_none_or(|owner| state.shows_account(owner))
            })
            .map(|label| Scope::Label {
                kind: mail_scope::LabelKind::of(label.is_auto, label.owner_account_id),
                id,
            }),
        Scope::Folder(_) => state
            .mailboxes
            .iter()
            .any(|mailbox| mailbox.scope == Some(scope) && state.shows_account(mailbox.account_id))
            .then_some(scope),
        Scope::Account { account_id, .. } => (state.shows_account(account_id)
            && state
                .connected_accounts
                .iter()
                .any(|account| account.id == account_id))
        .then_some(scope),
    }
}

/// Copy the current list rows without message bodies.
fn cached_rows(state: &InboxState) -> CachedRows {
    let mut messages = state.messages.clone();
    release_unselected_bodies(&mut messages, None);
    CachedRows {
        messages,
        next_cursor: state.next_cursor,
        collapsed: state.mail_groups.collapsed.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::software_renderer::MinimalSoftwareWindow;

    const A: Scope = Scope::Folder(1);
    const B: Scope = Scope::Folder(2);
    const C: Scope = Scope::Folder(3);
    const D: Scope = Scope::Folder(4);

    fn mail(scope: Scope) -> Place {
        Place {
            location: Location::Mail {
                scope,
                query: String::new(),
                filter: default_filter(),
                conversation: None,
            },
            ..Place::default()
        }
    }

    fn scopes(strip: &TabStrip) -> Vec<Scope> {
        strip
            .tabs
            .iter()
            .filter_map(|tab| match tab.place.location {
                Location::Mail { scope, .. } => Some(scope),
                _ => None,
            })
            .collect()
    }

    fn strip_of(places: &[Scope]) -> TabStrip {
        let mut strip = TabStrip::new(mail(places[0]));
        for (index, scope) in places.iter().enumerate().skip(1) {
            strip.insert(index, mail(*scope), String::new(), None);
        }
        strip
    }

    #[test]
    fn tabs_open_beside_the_active_tab_and_close_like_a_browser() {
        let mut strip = TabStrip::new(mail(A));
        strip.open(mail(B), String::new(), None);
        strip.open(mail(C), String::new(), None);
        // Each opens right after the active tab, which stays A.
        assert_eq!(scopes(&strip), [A, C, B]);
        assert_eq!(strip.active, 0);

        strip.active = 1;
        let c = strip.tabs[1].id;
        // The tab that takes the closed tab's place becomes active.
        assert_eq!(strip.close(c, &Place::default()), Closed::Active(1));
        assert_eq!(scopes(&strip), [A, B]);
        assert_eq!(strip.active, 1);

        // Closing the last tab in the strip activates its left neighbour.
        let b = strip.tabs[1].id;
        assert_eq!(strip.close(b, &Place::default()), Closed::Active(0));
        // The only tab stays and returns to the inbox, once.
        let a = strip.tabs[0].id;
        assert_eq!(strip.close(a, &Place::default()), Closed::Active(0));
        assert_eq!(scopes(&strip), [Scope::UNIFIED_INBOX]);
        assert_eq!(strip.close(a, &Place::default()), Closed::Missing);
        assert_eq!(strip.close(9999, &Place::default()), Closed::Missing);
        // With one account the window names the inbox by that account.
        let home = mail(Scope::Account {
            account_id: 4,
            mailbox: StandardMailbox::Inbox,
        });
        assert_eq!(strip.close(a, &home), Closed::Active(0));
        assert_eq!(strip.close(a, &home), Closed::Missing);
    }

    #[test]
    fn closing_an_inactive_tab_keeps_the_active_tab() {
        let mut strip = TabStrip::new(mail(A));
        strip.open(mail(B), String::new(), None);
        strip.active = 1;
        let a = strip.tabs[0].id;
        assert_eq!(strip.close(a, &Place::default()), Closed::Inactive);
        assert_eq!(strip.active, 0);
        assert_eq!(scopes(&strip), [B]);
    }

    #[test]
    fn closed_tabs_reopen_where_they_were() {
        let mut strip = strip_of(&[A, B, C]);
        let b = strip.tabs[1].id;
        strip.close(b, &Place::default());
        let c = strip.tabs[1].id;
        strip.close(c, &Place::default());
        assert_eq!(scopes(&strip), [A]);
        // The newest closed tab comes back first.
        assert_eq!(strip.reopen(), Some(1));
        assert_eq!(scopes(&strip), [A, C]);
        assert_eq!(strip.reopen(), Some(1));
        assert_eq!(scopes(&strip), [A, B, C]);
        assert_eq!(strip.reopen(), None);
        // Reopened tabs are new tabs.
        assert!(strip.tabs.iter().all(|tab| tab.id != b && tab.id != c));
    }

    #[test]
    fn closing_other_tabs_or_those_to_the_right_can_be_undone_one_by_one() {
        let mut strip = strip_of(&[A, B, C, D]);
        strip.active = 3;
        let c = strip.tabs[2].id;
        // The active tab D closes, so the kept tab becomes active.
        assert_eq!(strip.close_others(c), Closed::Active(0));
        assert_eq!(scopes(&strip), [C]);
        while strip.reopen().is_some() {}
        assert_eq!(scopes(&strip), [A, B, C, D]);

        strip.active = 0;
        let b = strip.tabs[1].id;
        assert_eq!(strip.close_to_right(b), Closed::Inactive);
        assert_eq!(scopes(&strip), [A, B]);
        let last = strip.tabs[1].id;
        assert_eq!(strip.close_to_right(last), Closed::Missing);
        while strip.reopen().is_some() {}
        assert_eq!(scopes(&strip), [A, B, C, D]);

        strip.active = 3;
        let a = strip.tabs[0].id;
        assert_eq!(strip.close_to_right(a), Closed::Active(0));
        assert_eq!(scopes(&strip), [A]);
    }

    #[test]
    fn moving_tabs_keeps_the_active_tab_and_cycling_wraps() {
        let mut strip = TabStrip::new(mail(A));
        strip.open(mail(C), String::new(), None);
        strip.open(mail(B), String::new(), None);
        let a = strip.tabs[0].id;
        strip.move_tab(a, 9);
        assert_eq!(scopes(&strip), [B, C, A]);
        assert_eq!(strip.active, 2);
        assert_eq!(strip.cycled(1), 0);
        assert_eq!(strip.cycled(-1), 1);
    }

    #[test]
    fn the_number_of_tabs_is_bounded() {
        let mut strip = TabStrip::new(mail(A));
        for _ in 1..MAX_TABS {
            assert!(strip.open(mail(B), String::new(), None).is_some());
        }
        assert!(strip.open(mail(B), String::new(), None).is_none());
        let first = strip.tabs[1].id;
        strip.close(first, &Place::default());
        strip.open(mail(C), String::new(), None);
        assert_eq!(strip.reopen(), None, "a full strip reopens nothing");
        assert_eq!(strip.closed.len(), 1, "and keeps the closed tab");
    }

    #[test]
    fn saved_tabs_round_trip_and_recover_from_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tabs.json");
        assert!(read_file(&path).is_none());

        let mut strip = TabStrip::new(mail(Scope::UNIFIED_INBOX));
        strip.open(
            Place {
                location: Location::Calendar {
                    date: NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(),
                    mode: "week".into(),
                },
                settings: Some("Accounts".into()),
                ..Place::default()
            },
            String::new(),
            None,
        );
        strip.open(
            Place {
                location: Location::Mail {
                    scope: Scope::Folder(7),
                    query: "invoice".into(),
                    filter: "Unread".into(),
                    conversation: Some(42),
                },
                settings: None,
                thread_message: Some(420),
                account: Some(3),
                scroll: Scroll {
                    list: 960.0,
                    reader: 1_234.5,
                },
                // A background tab is still marked read when first shown
                // after a restart.
                unseen: true,
            },
            "Invoice".into(),
            None,
        );
        strip.active = 1;
        write_file(&path, &strip.saved()).unwrap();
        let restored = TabStrip::restore(read_file(&path).unwrap());
        assert_eq!(restored.saved(), strip.saved());

        std::fs::write(&path, "{").unwrap();
        assert!(read_file(&path).is_none());
        let empty = TabStrip::restore(SavedTabs::default());
        assert_eq!(empty.tabs.len(), 1);
        let out_of_range = TabStrip::restore(SavedTabs {
            tabs: vec![SavedTab::default()],
            active: 7,
        });
        assert_eq!(out_of_range.active, 0);

        // Tabs saved before account mailboxes were keyed by id keep their
        // place and reopen at the inbox; the rest of the strip survives.
        // Files from before Settings, thread messages and scroll positions
        // were saved still load.
        std::fs::write(
            &path,
            r#"{"tabs":[
                {"location":{"workspace":"mail","scope":"Person / Sent"},"title":"Sent"},
                {"location":{"workspace":"mail","scope":"Folder:7","query":"invoice"}}
            ],"active":1}"#,
        )
        .unwrap();
        let legacy = TabStrip::restore(read_file(&path).unwrap());
        assert_eq!(scopes(&legacy), [Scope::UNIFIED_INBOX, Scope::Folder(7)]);
        assert_eq!(legacy.tabs[0].title, "Sent");
        assert_eq!(legacy.active, 1);
        assert_eq!(legacy.tabs[1].place.scroll, Scroll::default());

        // Unknown Settings pages and unusable offsets are dropped.
        std::fs::write(
            &path,
            r#"{"tabs":[{"location":{"workspace":"contacts"},"settings":"Gone",
                "scroll":{"list":-5,"reader":1e30}}]}"#,
        )
        .unwrap();
        let odd = TabStrip::restore(read_file(&path).unwrap());
        assert_eq!(odd.tabs[0].place.settings, None);
        assert_eq!(odd.tabs[0].place.scroll.list, 0.0);
        assert_eq!(odd.tabs[0].place.scroll.reader, 10_000_000.0);
    }

    #[test]
    fn commands_parse_from_ui_keys() {
        for command in [
            Command::New,
            Command::Close,
            Command::Reopen,
            Command::Next,
            Command::Previous,
            Command::Last,
            Command::Select(3),
        ] {
            assert_eq!(Command::parse(&command.key()), Some(command));
        }
        assert_eq!(Command::parse("bogus"), None);
    }

    #[test]
    fn strip_rows_update_in_place() {
        let model = VecModel::<TabRow>::default();
        let row = |id: i32, active: bool| TabRow {
            id,
            workspace: "mail".into(),
            title: "".into(),
            active,
        };
        sync_rows(&model, [row(1, true), row(2, false)].into_iter());
        sync_rows(
            &model,
            [row(1, false), row(2, true), row(3, false)].into_iter(),
        );
        assert_eq!(
            model.iter().collect::<Vec<_>>(),
            [row(1, false), row(2, true), row(3, false)]
        );
        sync_rows(&model, [row(3, true)].into_iter());
        assert_eq!(model.iter().collect::<Vec<_>>(), [row(3, true)]);
    }

    /// A window and mail state without a display server or mail core.
    fn headless() -> (
        AppWindow,
        TabContext,
        tempfile::TempDir,
        Rc<MinimalSoftwareWindow>,
    ) {
        use slint::platform::software_renderer::RepaintBufferType;
        use slint::platform::{Platform, WindowAdapter};
        struct Headless(Rc<MinimalSoftwareWindow>);
        impl Platform for Headless {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        // Tests in one process share the platform.
        let _ = slint::platform::set_platform(Box::new(Headless(window.clone())));
        let app = AppWindow::new().unwrap();
        app.set_startup_ready(true);
        let runtime = Rc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        let dir = tempfile::tempdir().unwrap();
        let (favicons, _) = bounded_ui_channel();
        let (avatars, _) = bounded_ui_channel();
        let state = Rc::new(RefCell::new(InboxState::empty(
            None,
            UiSender::new(favicons, UiWake::new(app.as_weak(), |_| {})),
            None,
            UiSender::new(avatars, UiWake::new(app.as_weak(), |_| {})),
            false,
            false,
            WarmStartCacheWriter::spawn(&runtime, dir.path().join("warm.json")),
        )));
        let context = TabContext {
            app: app.as_weak(),
            state,
            runtime,
            calendar: Rc::new(RefCell::new(LocalCalendarState::new(
                NaiveDate::from_ymd_opt(2026, 10, 10).unwrap(),
            ))),
            mail_pagination_generation: Rc::new(Cell::new(0)),
            mail_pagination_in_progress: Rc::new(Cell::new(false)),
            path: dir.path().join("tabs.json"),
        };
        (app, context, dir, window)
    }

    /// Drives the registered callbacks the way the tab strip and keyboard
    /// shortcuts do, so a strip borrow held across activation would panic.
    #[test]
    fn selecting_cycling_and_closing_tabs_through_the_ui() {
        let (app, context, _dir, _) = headless();
        let path = context.path.clone();
        register(&app, context);
        let ui = app.global::<TabsUi>();
        ui.set_enabled(true);
        // Tabs ignore input until the saved tabs are shown.
        ui.invoke_create();
        assert_eq!(ui.get_rows().row_count(), 1);
        ui.invoke_restore_active();
        let rows = ui.get_rows();
        let active = || rows.iter().position(|row| row.active).unwrap();
        let id = |index: usize| rows.row_data(index).unwrap().id;

        ui.invoke_create();
        ui.invoke_create();
        assert_eq!(rows.row_count(), 3);
        assert_eq!(active(), 2);
        ui.invoke_select(id(0));
        assert_eq!(active(), 0);
        ui.invoke_command("next".into());
        assert_eq!(active(), 1);
        ui.invoke_command("previous".into());
        ui.invoke_command("previous".into());
        assert_eq!(active(), 2, "cycling wraps around");
        ui.invoke_command("0".into());
        assert_eq!(active(), 0);
        ui.invoke_command("close".into());
        assert_eq!(rows.row_count(), 2);
        assert!(ui.get_can_reopen());
        ui.invoke_command("reopen".into());
        assert_eq!(rows.row_count(), 3);
        assert_eq!(active(), 0, "the reopened tab is shown where it was");
        assert!(!ui.get_can_reopen());

        // Settings belongs to the tab it was opened in.
        app.set_settings_tab("Data".into());
        app.set_settings_open(true);
        ui.invoke_select(id(1));
        assert!(!app.get_settings_open(), "another tab shows its own place");
        let settings_tab = rows.row_data(0).unwrap();
        assert_eq!(settings_tab.workspace, "settings");
        app.set_settings_tab("General".into());
        ui.invoke_select(id(0));
        assert!(app.get_settings_open());
        assert_eq!(app.get_settings_tab(), "Data");
        assert_eq!(app.get_active_view(), "mail");
        app.set_settings_open(false);

        // A calendar tab stays a calendar tab.
        app.set_active_view("calendar".into());
        ui.invoke_select(id(2));
        assert_eq!(app.get_active_view(), "mail");
        assert_eq!(rows.row_data(0).unwrap().workspace, "calendar");
        ui.invoke_select(id(0));
        assert_eq!(app.get_active_view(), "calendar");

        ui.invoke_close_others(id(1));
        assert_eq!(rows.row_count(), 1);
        ui.invoke_reopen();
        ui.invoke_reopen();
        assert_eq!(rows.row_count(), 3);
        ui.invoke_close_to_right(id(0));
        assert_eq!(rows.row_count(), 1);
        ui.invoke_move(id(0), 1);
        ui.invoke_close(id(0));
        assert_eq!(rows.row_count(), 1, "the last tab stays open");

        ui.invoke_flush();
        assert!(read_file(&path).is_some(), "exiting saves the tabs at once");
        ui.invoke_create();
        ui.invoke_reset();
        assert_eq!(rows.row_count(), 1, "reset leaves one Inbox tab");
        assert_eq!(rows.row_data(0).unwrap().title, "");
        assert!(!ui.get_can_reopen());
    }

    /// Presses, drags and clicks on the rendered strip, where a rebuilt row
    /// model used to drop the drag of a tab that was not active yet.
    #[test]
    fn the_strip_selects_drags_and_closes_tabs_with_the_pointer() {
        use slint::platform::{PointerEventButton, WindowEvent};
        let (app, context, _dir, window) = headless();
        window.set_size(slint::PhysicalSize::new(1320, 800));
        app.set_connected_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
            id: 1,
            ..Default::default()
        }])));
        register(&app, context);
        let ui = app.global::<TabsUi>();
        ui.invoke_restore_active();
        let draw = || {
            slint::platform::update_timers_and_animations();
            window.draw_if_needed(|renderer| {
                let mut buffer = vec![slint::Rgb8Pixel::default(); 1320 * 800];
                renderer.render(&mut buffer, 1320);
            });
        };
        ui.invoke_create();
        ui.invoke_create();
        draw();
        assert!(ui.get_enabled(), "the desktop top bar shows the tabs");
        let ids = || ui.get_rows().iter().map(|row| row.id).collect::<Vec<_>>();
        let active = || ui.get_rows().iter().position(|row| row.active).unwrap();
        let [a, b, c] = ids()[..] else {
            panic!("three tabs");
        };
        assert_eq!(active(), 2);
        // The tabs start after the profile pill. Three tabs share the bar at
        // their widest, 240px each.
        let origin = {
            use i_slint_core::items::AccessibleRole;
            let window = i_slint_core::window::WindowInner::from_pub(app.window());
            let root = i_slint_core::item_tree::ItemRc::new_root(window.component());
            let mut strip = None;
            root.visit_descendants(|item| {
                if item.accessible_role() == AccessibleRole::TabList {
                    strip = Some(item.map_to_window(item.geometry().origin).x);
                    return std::ops::ControlFlow::Break(());
                }
                std::ops::ControlFlow::Continue(())
            });
            strip.expect("the tab strip")
        };
        assert!(origin > 0.0, "the profile pill comes first");
        let at = |x: f32| slint::LogicalPosition::new(origin + x, 22.0);
        let pointer = |event| app.window().dispatch_event(event);
        let left = PointerEventButton::Left;

        pointer(WindowEvent::PointerMoved {
            position: at(120.0),
        });
        pointer(WindowEvent::PointerPressed {
            position: at(120.0),
            button: left,
        });
        assert_eq!(active(), 0, "a press selects the tab");
        pointer(WindowEvent::PointerMoved {
            position: at(130.0),
        });
        pointer(WindowEvent::PointerMoved {
            position: at(420.0),
        });
        draw();
        pointer(WindowEvent::PointerReleased {
            position: at(420.0),
            button: left,
        });
        assert_eq!(ids(), [b, a, c], "the tab lands where it was dropped");
        assert_eq!(active(), 1, "and stays active");
        draw();

        // An inactive tab shows its close button on hover; it closes the tab
        // without selecting it.
        pointer(WindowEvent::PointerMoved {
            position: at(600.0),
        });
        draw();
        let close = at(480.0 + 240.0 - 18.0);
        pointer(WindowEvent::PointerMoved { position: close });
        pointer(WindowEvent::PointerPressed {
            position: close,
            button: left,
        });
        pointer(WindowEvent::PointerReleased {
            position: close,
            button: left,
        });
        assert_eq!(ids(), [b, a]);
        assert_eq!(active(), 1);
        draw();

        // A middle click closes a tab as well.
        let middle = PointerEventButton::Middle;
        pointer(WindowEvent::PointerMoved {
            position: at(120.0),
        });
        pointer(WindowEvent::PointerPressed {
            position: at(120.0),
            button: middle,
        });
        pointer(WindowEvent::PointerReleased {
            position: at(120.0),
            button: middle,
        });
        assert_eq!(ids(), [a]);
    }

    #[test]
    fn a_shown_conversation_scrolls_back_once_its_body_is_laid_out() {
        let (app, context, _dir, _) = headless();
        let state = Rc::clone(&context.state);
        let tabs = Tabs::new(&app, context);
        app.set_active_view("mail".into());
        app.set_email_viewport_height(500.0);
        app.set_email_scroll_document_height(1000.0);
        let message = |id: i32| {
            let mut message = fixture_messages()[0].clone();
            message.id = id;
            message.body_pending = true;
            message
        };
        // The tab is restored before the page with its conversation arrives.
        state.borrow_mut().pending_selection = Some(7);
        let step = || tabs.step_reader_restore();
        tabs.restore_reader(7, Some(70), 1200.0);
        assert!(step(), "waits for the page");
        {
            let mut state = state.borrow_mut();
            state.pending_selection = None;
            state.selected_id = Some(7);
            state.preview_closed = false;
        }
        assert!(step(), "waits for the conversation");
        {
            let mut state = state.borrow_mut();
            state.conversation_owner_id = Some(7);
            state.conversation_messages = vec![message(69), message(70)];
            state.conversation_selected_index = 1;
        }
        assert!(step(), "waits for the body");
        state.borrow_mut().conversation_messages[1].body_pending = false;
        assert!(step(), "waits one more poll for the reader's own reset");
        assert_eq!(app.get_email_scroll_y(), 0.0);
        // The message is shorter than the offset for now: scroll to its end
        // and follow it while it grows.
        assert!(step());
        assert_eq!(app.get_email_scroll_y(), -500.0);
        app.set_email_scroll_document_height(3000.0);
        assert!(!step(), "the offset is reached");
        assert_eq!(app.get_email_scroll_y(), -1200.0);

        // The user's own scrolling ends the restore.
        app.set_email_scroll_document_height(1000.0);
        app.set_email_scroll_y(0.0);
        tabs.restore_reader(7, Some(70), 1200.0);
        assert!(step());
        assert!(step());
        app.set_email_scroll_y(-100.0);
        app.set_email_scroll_document_height(3000.0);
        assert!(!step());
        assert_eq!(app.get_email_scroll_y(), -100.0);

        // So does opening another message of the conversation, or another
        // conversation.
        tabs.restore_reader(7, Some(69), 1200.0);
        assert!(!step());
        tabs.restore_reader(7, Some(70), 1200.0);
        state.borrow_mut().selected_id = Some(8);
        assert!(!step());
        assert_eq!(app.get_email_scroll_y(), -100.0);
    }

    /// A folder of `account_id` in the sidebar's mailbox list.
    fn folder(account_id: i64, folder_id: i64) -> MailboxEntry {
        MailboxEntry {
            account_id,
            folder_id,
            parent_folder_id: -1,
            depth: 0,
            has_children: false,
            is_standard: false,
            is_selectable: true,
            can_create_children: true,
            can_rename: true,
            can_delete: true,
            label: "Folder".into(),
            scope: Some(Scope::Folder(folder_id)),
            context: String::new(),
            detail: String::new(),
            avatar: String::new(),
            is_account: false,
            count: String::new(),
        }
    }

    #[test]
    fn gone_mailboxes_reopen_at_the_inbox() {
        let (_app, context, _dir, _) = headless();
        let mut state = context.state.borrow_mut();
        assert_eq!(
            available_scope(Scope::UNIFIED_INBOX, &state),
            Some(Scope::UNIFIED_INBOX)
        );
        assert_eq!(available_scope(Scope::Folder(7), &state), None);
        state.mailboxes.push(folder(1, 7));
        assert_eq!(
            available_scope(Scope::Folder(7), &state),
            Some(Scope::Folder(7))
        );
        let account = Scope::Account {
            account_id: 4,
            mailbox: StandardMailbox::Inbox,
        };
        assert_eq!(available_scope(account, &state), None);
    }

    fn account(id: i64) -> Account {
        Account {
            id,
            email: format!("person{id}@example.com"),
            display_name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: flectar_mail_core::models::AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            sync_state: "idle".into(),
            sync_error: None,
            can_create_top_level_mailbox: true,
            server_keeps_mail_days: None,
        }
    }

    #[test]
    fn mailboxes_of_accounts_outside_the_profile_give_way_to_its_own() {
        let (_app, context, _dir, _) = headless();
        let mut state = context.state.borrow_mut();
        state.connected_accounts = vec![account(1), account(2), account(3)];
        state.mailboxes = vec![folder(1, 7), folder(2, 8)];
        state.profile_accounts = Some(vec![2, 3]);
        let sent = |account_id| Scope::Account {
            account_id,
            mailbox: StandardMailbox::Sent,
        };

        // Saved tabs reopen only the profile's mailboxes.
        assert_eq!(available_scope(Scope::Folder(7), &state), None);
        assert_eq!(
            available_scope(Scope::Folder(8), &state),
            Some(Scope::Folder(8))
        );
        assert_eq!(available_scope(sent(1), &state), None);
        assert_eq!(available_scope(sent(2), &state), Some(sent(2)));
        assert_eq!(
            available_scope(Scope::UNIFIED_INBOX, &state),
            Some(Scope::UNIFIED_INBOX)
        );

        // The selection moves to the profile's same mailbox.
        state.set_scope(sent(1));
        assert_eq!(state.scope, Scope::Unified(StandardMailbox::Sent));
        state.set_scope(Scope::Folder(7));
        assert_eq!(state.scope, Scope::UNIFIED_INBOX);
        state.set_scope(Scope::Folder(8));
        assert_eq!(state.scope, Scope::Folder(8));
        // With one account shown, the unified mailboxes are that account's.
        state.profile_accounts = Some(vec![3]);
        state.set_scope(sent(1));
        assert_eq!(state.scope, sent(3));
        assert!(state.shows_account(3) && !state.shows_account(2));

        state.profile_accounts = None;
        state.set_scope(sent(1));
        assert_eq!(state.scope, sent(1));
        assert_eq!(state.mail_accounts().len(), 3);
    }

    #[test]
    fn a_tab_on_a_gone_folder_reopens_at_the_top_of_the_inbox() {
        let (app, context, _dir, _) = headless();
        let tabs = Tabs::new(&app, context);
        tabs.strip.borrow_mut().open(
            Place {
                scroll: Scroll {
                    list: 2400.0,
                    reader: 0.0,
                },
                ..mail(Scope::Folder(7))
            },
            "Gone".into(),
            None,
        );
        tabs.activate(1);
        assert_eq!(tabs.context.state.borrow().scope, Scope::UNIFIED_INBOX);
        assert_eq!(app.global::<TabsUi>().get_list_restore(), 0.0);
    }

    #[test]
    fn a_removed_account_leaves_no_subject_in_the_tabs() {
        let (app, context, _dir, _) = headless();
        let state = Rc::clone(&context.state);
        state.borrow_mut().mailboxes.push(folder(4, 7));
        let tabs = Tabs::new(&app, context);
        let conversation = |scope, id, account| Place {
            location: Location::Mail {
                scope,
                query: String::new(),
                filter: default_filter(),
                conversation: Some(id),
            },
            account: Some(account),
            ..Place::default()
        };
        {
            let mut strip = tabs.strip.borrow_mut();
            strip.open(
                conversation(Scope::Folder(7), 1, 4),
                "Folder mail".into(),
                None,
            );
            strip.open(
                conversation(Scope::UNIFIED_INBOX, 2, 4),
                "Removed".into(),
                None,
            );
            strip.open(
                conversation(Scope::UNIFIED_INBOX, 3, 5),
                "Kept".into(),
                None,
            );
        }
        tabs.account_removed(4);
        let strip = tabs.strip.borrow();
        let titles = strip
            .tabs
            .iter()
            .map(|tab| tab.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles, ["", "Kept", "", ""]);
        // A tab on the account's folder starts over at the inbox; one on its
        // conversation keeps its mailbox.
        assert_eq!(strip.tabs[3].place, Place::default());
        assert_eq!(strip.tabs[2].place.conversation(), None);
        assert_eq!(strip.tabs[1].place.conversation(), Some(3));
    }

    #[cfg(desktop)]
    #[test]
    fn shortcuts_follow_browser_conventions() {
        use slint::winit_030::winit::keyboard::{Key, ModifiersState, NamedKey};
        let primary = if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        };
        let character = |text: &str| Key::Character(text.into());
        assert_eq!(shortcut(&character("t"), primary), Some(Command::New));
        assert_eq!(
            shortcut(&character("T"), primary | ModifiersState::SHIFT),
            Some(Command::Reopen)
        );
        assert_eq!(shortcut(&character("W"), primary), Some(Command::Close));
        assert_eq!(shortcut(&character("1"), primary), Some(Command::Select(0)));
        assert_eq!(shortcut(&character("9"), primary), Some(Command::Last));
        assert_eq!(shortcut(&character("t"), ModifiersState::empty()), None);
        assert_eq!(
            shortcut(&Key::Named(NamedKey::Tab), ModifiersState::CONTROL),
            Some(Command::Next)
        );
        assert_eq!(
            shortcut(
                &Key::Named(NamedKey::Tab),
                ModifiersState::CONTROL | ModifiersState::SHIFT
            ),
            Some(Command::Previous)
        );
        assert_eq!(
            shortcut(&Key::Named(NamedKey::Tab), ModifiersState::empty()),
            None
        );
    }

    #[cfg(desktop)]
    mod listed {
        slint::slint! {
            export { Shortcuts } from "../ui/shortcuts.slint";
            export component ShortcutList inherits Window {}
        }
    }

    // Settings lists the tab keys from ui/shortcuts.slint, but `shortcut`
    // handles them; the two must agree.
    #[cfg(desktop)]
    #[test]
    fn listed_tab_shortcuts_are_the_handled_ones() {
        use listed::{ShortcutList, Shortcuts};
        use slint::ComponentHandle;
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        use slint::platform::{Platform, WindowAdapter};
        use slint::winit_030::winit::keyboard::{Key, ModifiersState, NamedKey};

        struct Headless(std::rc::Rc<MinimalSoftwareWindow>);
        impl Platform for Headless {
            fn create_window_adapter(
                &self,
            ) -> Result<std::rc::Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        slint::platform::set_platform(Box::new(Headless(MinimalSoftwareWindow::new(
            RepaintBufferType::NewBuffer,
        ))))
        .unwrap();

        // Slint calls Command `Control` and Control `Meta` on macOS.
        let (control, meta) = if cfg!(target_os = "macos") {
            (ModifiersState::SUPER, ModifiersState::CONTROL)
        } else {
            (ModifiersState::CONTROL, ModifiersState::SUPER)
        };
        let handled = |keys: slint::Keys| {
            let mut modifiers = ModifiersState::empty();
            let mut key = None;
            for part in keys.to_parts() {
                match part {
                    "Control" => modifiers |= control,
                    "Meta" => modifiers |= meta,
                    "Shift" => modifiers |= ModifiersState::SHIFT,
                    "Alt" => modifiers |= ModifiersState::ALT,
                    "Shift?" | "Alt?" => {}
                    "\t" => key = Some(Key::Named(NamedKey::Tab)),
                    text => key = Some(Key::Character(text.into())),
                }
            }
            shortcut(&key.unwrap(), modifiers)
        };
        let list = ShortcutList::new().unwrap();
        let listed = list.global::<Shortcuts>();
        assert_eq!(handled(listed.get_new_tab()), Some(Command::New));
        assert_eq!(handled(listed.get_close_tab()), Some(Command::Close));
        assert_eq!(handled(listed.get_reopen_tab()), Some(Command::Reopen));
        assert_eq!(handled(listed.get_next_tab()), Some(Command::Next));
        assert_eq!(handled(listed.get_previous_tab()), Some(Command::Previous));
        assert_eq!(handled(listed.get_first_tab()), Some(Command::Select(0)));
        assert_eq!(handled(listed.get_eighth_tab()), Some(Command::Select(7)));
        assert_eq!(handled(listed.get_last_tab()), Some(Command::Last));
    }
}
