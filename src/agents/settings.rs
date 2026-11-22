//! Settings → AI agents (ui/agent-access.slint): whether agents may read
//! mail, which accounts and folders, how to connect an agent's app, and
//! what agents did.

use crate::date_format::{self, DateStyle};
use crate::ui_message::{AppWindowMessages, UiMessage, translated};
use crate::{
    AgentAccess as AccessUi, AgentAccount, AgentActivityEntry, AgentFolder, AppWindow, InboxState,
};
use chrono::{Local, Timelike};
use flectar_mail_core::{
    Core,
    models::{AgentAccess, AgentActivity, AgentOutcome, FolderInfo, roles},
};
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, collections::HashSet, rc::Rc, sync::Arc};
use tokio::runtime::Handle;

/// The newest activity entries the page lists.
const ACTIVITY_SHOWN: i64 = 50;
/// The name agents' apps know the server by.
const SERVER_NAME: &str = "flectar-mail";

thread_local! {
    static PAGE: RefCell<Option<Rc<Page>>> = const { RefCell::new(None) };
}

struct Page {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: Handle,
    /// Accounts whose folders are open for choosing.
    open_folders: RefCell<HashSet<i64>>,
    launch: Launch,
    open: std::cell::Cell<bool>,
}

/// Wires the page. It fills itself when shown, and its activity list follows
/// what agents do while it's open.
pub(crate) fn register(app: &AppWindow, state: &Rc<RefCell<InboxState>>, runtime: &Handle) {
    let page = Rc::new(Page {
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
        open_folders: RefCell::default(),
        launch: Launch::current(),
        open: std::cell::Cell::new(false),
    });
    let ui = app.global::<AccessUi>();
    ui.set_available(true);
    let clients = Client::ALL
        .iter()
        .map(|client| SharedString::from(client.label(app)))
        .collect::<Vec<_>>();
    ui.set_clients(ModelRc::new(VecModel::from(clients)));
    page.choose_client(0);

    let callbacks = Rc::clone(&page);
    ui.on_page_shown(move |shown| {
        callbacks.open.set(shown);
        if shown {
            callbacks.refresh();
        }
    });
    let callbacks = Rc::clone(&page);
    ui.on_set_enabled(move |enabled| {
        callbacks.change(move |access| access.enabled = enabled);
    });
    let callbacks = Rc::clone(&page);
    ui.on_set_can_write_drafts(move |allowed| {
        callbacks.change(move |access| access.can_write_drafts = allowed);
    });
    let callbacks = Rc::clone(&page);
    ui.on_set_can_mark(move |allowed| {
        callbacks.change(move |access| access.can_mark = allowed);
    });
    let callbacks = Rc::clone(&page);
    ui.on_set_can_move(move |allowed| {
        callbacks.change(move |access| access.can_move = allowed);
    });
    let callbacks = Rc::clone(&page);
    ui.on_set_can_label(move |allowed| {
        callbacks.change(move |access| access.can_label = allowed);
    });
    let callbacks = Rc::clone(&page);
    ui.on_set_can_send(move |allowed| {
        callbacks.change(move |access| access.can_send = allowed);
    });
    let callbacks = Rc::clone(&page);
    ui.on_set_account_shown(move |account_id, shown| {
        let account_id = i64::from(account_id);
        callbacks.change(move |access| {
            access.hidden_accounts.retain(|id| *id != account_id);
            if !shown {
                access.hidden_accounts.push(account_id);
            }
        });
    });
    let callbacks = Rc::clone(&page);
    ui.on_toggle_folders(move |account_id| {
        let account_id = i64::from(account_id);
        {
            let mut open = callbacks.open_folders.borrow_mut();
            if !open.remove(&account_id) {
                open.insert(account_id);
            }
        }
        callbacks.refresh();
    });
    let callbacks = Rc::clone(&page);
    ui.on_set_folder_shown(move |_account_id, folder_id, shown| {
        let folder_id = i64::from(folder_id);
        let Some(core) = callbacks.core() else {
            return;
        };
        let role = callbacks
            .runtime
            .block_on(core.list_folders(None))
            .ok()
            .and_then(|folders| folders.into_iter().find(|folder| folder.id == folder_id))
            .and_then(|folder| folder.role);
        let hidden_by_role = matches!(role.as_deref(), Some(roles::SPAM | roles::TRASH));
        callbacks.change(move |access| {
            access.hidden_folders.retain(|id| *id != folder_id);
            access.shown_folders.retain(|id| *id != folder_id);
            match (hidden_by_role, shown) {
                (true, true) => access.shown_folders.push(folder_id),
                (false, false) => access.hidden_folders.push(folder_id),
                _ => {}
            }
        });
    });
    let callbacks = Rc::clone(&page);
    ui.on_choose_client(move |index| callbacks.choose_client(index));
    let callbacks = Rc::clone(&page);
    ui.on_install(move || callbacks.install());
    let callbacks = Rc::clone(&page);
    ui.on_undo_activity(move |activity_id| {
        let (Some(app), Some(core)) = (callbacks.app.upgrade(), callbacks.core()) else {
            return;
        };
        match callbacks
            .runtime
            .block_on(core.undo_agent_activity(i64::from(activity_id)))
        {
            Ok(true) => {}
            Ok(false) => app.set_sync_status(UiMessage::plain("There's nothing left to undo.")),
            Err(error) => app.set_sync_status(UiMessage::detail("Could not undo: {}", error)),
        }
        callbacks.refresh();
    });
    let callbacks = Rc::clone(&page);
    ui.on_clear_activity(move || {
        if let Some(core) = callbacks.core() {
            let _ = callbacks.runtime.block_on(core.clear_agent_activity());
        }
        callbacks.refresh();
    });

    PAGE.with(|slot| *slot.borrow_mut() = Some(page));
    super::on_activity(|| {
        let _ = slint::invoke_from_event_loop(|| {
            PAGE.with(|slot| {
                if let Some(page) = slot.borrow().as_ref()
                    && page.open.get()
                {
                    page.refresh();
                }
            });
        });
    });
}

impl Page {
    fn core(&self) -> Option<Arc<Core>> {
        self.state
            .borrow()
            .core
            .as_ref()
            .map(|core| core.file_core())
    }

    /// Saves a change to the access settings and opens or closes the
    /// endpoint to match.
    fn change(&self, change: impl FnOnce(&mut AgentAccess) + Send + 'static) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let Some(core) = self.core() else {
            app.set_sync_status(UiMessage::plain(
                "AI agents can be set up once an account is added.",
            ));
            return;
        };
        match self.runtime.block_on(core.update_agent_access(change)) {
            Ok(access) => super::set_enabled(access.enabled),
            Err(error) => app.set_sync_status(UiMessage::detail(
                "Could not save AI agent access: {}",
                error,
            )),
        }
        self.refresh();
    }

    /// Shows the saved access, the accounts and folders, and the activity.
    fn refresh(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<AccessUi>();
        let Some(core) = self.core() else {
            ui.set_accounts(ModelRc::default());
            ui.set_activity(ModelRc::default());
            return;
        };
        let (settings, accounts, local, folders, activity) = self.runtime.block_on(async {
            (
                core.get_settings().await,
                core.list_accounts().await,
                core.local_mail_account().await,
                core.list_folders(None).await,
                core.agent_activity(ACTIVITY_SHOWN).await,
            )
        });
        let access = settings
            .map(|settings| settings.agent_access)
            .unwrap_or_default();
        // The accounts as the page names them, with the mail imported to
        // this device, which account lists leave out, at the end.
        let mut accounts = accounts
            .unwrap_or_default()
            .into_iter()
            .map(|account| PageAccount {
                id: account.id,
                name: account.label().to_owned(),
                address: account.email,
            })
            .collect::<Vec<_>>();
        if let Ok(Some(account_id)) = local {
            accounts.push(PageAccount {
                id: account_id,
                name: translated(&app, &UiMessage::plain("Mail on this device")).into(),
                address: String::new(),
            });
        }
        let folders = folders
            .unwrap_or_default()
            .into_iter()
            .filter(|folder| folder.selectable)
            .collect::<Vec<_>>();
        ui.set_enabled(access.enabled);
        ui.set_can_write_drafts(access.can_write_drafts);
        ui.set_can_mark(access.can_mark);
        ui.set_can_move(access.can_move);
        ui.set_can_label(access.can_label);
        ui.set_can_send(access.can_send);
        let open = self.open_folders.borrow();
        let rows = accounts
            .iter()
            .map(|account| {
                account_row(&app, account, &folders, &access, open.contains(&account.id))
            })
            .collect::<Vec<_>>();
        ui.set_accounts(ModelRc::new(VecModel::from(rows)));
        let entries = activity
            .unwrap_or_default()
            .iter()
            .map(|entry| activity_row(&app, entry, &accounts, &folders))
            .collect::<Vec<_>>();
        ui.set_activity(ModelRc::new(VecModel::from(entries)));
    }

    fn choose_client(&self, index: i32) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let client = usize::try_from(index)
            .ok()
            .and_then(|index| Client::ALL.get(index))
            .copied()
            .unwrap_or(Client::ClaudeDesktop);
        let ui = app.global::<AccessUi>();
        ui.set_client_index(index.max(0));
        ui.set_setup_text(client.setup(&app, &self.launch).into());
        ui.set_setup_hint(translated(&app, &client.hint()));
        ui.set_install_label(match client.install_url(&self.launch) {
            Some(_) => translated(&app, &client.install_label()),
            None => SharedString::new(),
        });
    }

    fn install(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let client = Client::ALL
            .get(usize::try_from(app.global::<AccessUi>().get_client_index()).unwrap_or(0))
            .copied()
            .unwrap_or(Client::ClaudeDesktop);
        let Some(url) = client.install_url(&self.launch) else {
            return;
        };
        // An app-made link to the agent's app, not one from a message, so
        // its scheme is opened as the system knows it.
        if let Err(error) = webbrowser::open(&url) {
            app.set_sync_status(UiMessage::detail("Could not open the app: {}", error));
        }
    }
}

/// An account as the page lists it.
struct PageAccount {
    id: i64,
    name: String,
    /// Empty for the mail on this device, which has no address.
    address: String,
}

fn account_row(
    app: &AppWindow,
    account: &PageAccount,
    folders: &[FolderInfo],
    access: &AgentAccess,
    folders_open: bool,
) -> AgentAccount {
    let shown = !access.hidden_accounts.contains(&account.id);
    let own = folders
        .iter()
        .filter(|folder| folder.account_id == account.id)
        .collect::<Vec<_>>();
    // Folder access as if this account were shown, so the summary reads the
    // same whether or not agent access is on.
    let as_shown = AgentAccess {
        enabled: true,
        hidden_accounts: Vec::new(),
        ..access.clone()
    };
    let rows = if folders_open {
        own.iter()
            .map(|folder| AgentFolder {
                id: i32::try_from(folder.id).unwrap_or_default(),
                name: folder.display_name.as_str().into(),
                depth: folder_depth(folder, &own),
                shown: as_shown.shows_folder(folder),
            })
            .collect()
    } else {
        Vec::new()
    };
    AgentAccount {
        id: i32::try_from(account.id).unwrap_or_default(),
        name: account.name.as_str().into(),
        address: account.address.as_str().into(),
        shown,
        folders_summary: translated(app, &folders_summary(&own, &as_shown)),
        folders_open,
        folders: ModelRc::new(VecModel::from(rows)),
    }
}

/// How deep `folder` sits under the account's other folders.
fn folder_depth(folder: &FolderInfo, folders: &[&FolderInfo]) -> i32 {
    let mut depth = 0;
    let mut parent = folder.parent_id;
    while let Some(id) = parent {
        let Some(above) = folders.iter().find(|folder| folder.id == id) else {
            break;
        };
        depth += 1;
        parent = above.parent_id;
        if depth > 16 {
            break;
        }
    }
    depth
}

/// Which of an account's folders agents can read, in a few words.
fn folders_summary(folders: &[&FolderInfo], access: &AgentAccess) -> UiMessage {
    let (shown, hidden): (Vec<&&FolderInfo>, Vec<&&FolderInfo>) = folders
        .iter()
        .partition(|folder| access.shows_folder(folder));
    let names = |folders: &[&&FolderInfo]| {
        folders
            .iter()
            .map(|folder| folder.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match (shown.len(), hidden.len()) {
        (0, _) => UiMessage::plain("No folders"),
        (_, 0) => UiMessage::plain("All folders"),
        (_, 1..=3) => UiMessage::detail("All folders except {}", names(&hidden)),
        (1..=3, _) => UiMessage::detail("Only {}", names(&shown)),
        (shown, hidden) => UiMessage::arguments("{} of {} folders", shown, shown + hidden),
    }
}

fn activity_row(
    app: &AppWindow,
    entry: &AgentActivity,
    accounts: &[PageAccount],
    folders: &[FolderInfo],
) -> AgentActivityEntry {
    let action = match entry.tool.as_str() {
        "list_accounts" => UiMessage::plain("Listed accounts"),
        "list_folders" => UiMessage::plain("Listed folders"),
        "list_labels" => UiMessage::plain("Listed labels"),
        "list_messages" => UiMessage::plain("Listed messages"),
        "search_messages" => UiMessage::plain("Searched messages"),
        "read_message" => UiMessage::plain("Read a message"),
        "read_conversation" => UiMessage::plain("Read a conversation"),
        "search_contacts" => UiMessage::plain("Searched contacts"),
        "list_events" => UiMessage::plain("Listed calendar events"),
        "create_draft" => UiMessage::plain("Wrote a draft"),
        "update_draft" => UiMessage::plain("Changed a draft"),
        "mark_messages" => UiMessage::plain("Marked messages"),
        "move_messages" => UiMessage::plain("Moved messages"),
        "label_messages" => UiMessage::plain("Labeled messages"),
        "send_draft" => UiMessage::plain("Asked to send a draft"),
        _ => UiMessage::detail("Used {}", &entry.tool),
    };
    let mut detail = Vec::new();
    if let Some(account) = entry
        .account_id
        .and_then(|id| accounts.iter().find(|account| account.id == id))
    {
        detail.push(account.name.clone());
    }
    if let Some(folder) = entry
        .folder_id
        .and_then(|id| folders.iter().find(|folder| folder.id == id))
    {
        detail.push(folder.display_name.clone());
    }
    match entry.outcome {
        AgentOutcome::Refused => {
            let refused = if changes_mail(&entry.tool) {
                UiMessage::plain("Refused, outside what agents can do")
            } else {
                UiMessage::plain("Refused, outside what agents can read")
            };
            detail.push(translated(app, &refused).into());
        }
        AgentOutcome::Failed => detail.push(translated(app, &UiMessage::plain("Failed")).into()),
        // A draft counts its recipients, a list its results.
        AgentOutcome::Done if entry.tool.ends_with("_draft") => {
            if entry.items > 0 {
                let recipients = UiMessage::detail("To {} people", entry.items);
                detail.push(translated(app, &recipients).into());
            }
        }
        AgentOutcome::Done if changes_mail(&entry.tool) => {
            let messages = UiMessage::detail("{} messages", entry.items);
            detail.push(translated(app, &messages).into());
        }
        AgentOutcome::Done if !entry.tool.starts_with("read_") => {
            let results = UiMessage::detail("{} results", entry.items);
            detail.push(translated(app, &results).into());
        }
        AgentOutcome::Done => {}
    }
    AgentActivityEntry {
        id: i32::try_from(entry.id).unwrap_or_default(),
        undoable: entry.undoable,
        undone: entry.undone,
        time: activity_time(entry.occurred_at).into(),
        agent: entry.agent.as_str().into(),
        action: translated(app, &action),
        detail: detail.join(" · ").into(),
        refused: entry.outcome == AgentOutcome::Refused,
        failed: entry.outcome == AgentOutcome::Failed,
    }
}

/// Whether `tool` changes mail rather than reading it.
fn changes_mail(tool: &str) -> bool {
    matches!(
        tool,
        "create_draft"
            | "update_draft"
            | "send_draft"
            | "mark_messages"
            | "move_messages"
            | "label_messages"
    )
}

/// The time today, or the day and time before that.
fn activity_time(occurred_at: i64) -> String {
    let Some(time) =
        chrono::DateTime::from_timestamp_millis(occurred_at).map(|time| time.with_timezone(&Local))
    else {
        return String::new();
    };
    let formats = date_format::current();
    let clock = formats.time(time.hour(), time.minute());
    if time.date_naive() == Local::now().date_naive() {
        clock
    } else {
        format!(
            "{} {clock}",
            formats.date(DateStyle::ShortDayMonth, time.date_naive())
        )
    }
}

/// How an agent's app starts the bridge, for the setup the page shows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Launch {
    command: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl Launch {
    fn current() -> Self {
        if cfg!(feature = "flatpak") {
            // A host app starts the Flatpak, which finds the running one only
            // in the same runtime directory. Hosts often leave the variable
            // out, so it's spelled out.
            #[cfg(unix)]
            let runtime_dir = format!("/run/user/{}", unsafe { libc::geteuid() });
            #[cfg(not(unix))]
            let runtime_dir = String::new();
            return Self {
                command: "flatpak".into(),
                args: vec!["run".into(), "com.flectar.mail".into(), "mcp".into()],
                env: vec![("XDG_RUNTIME_DIR".into(), runtime_dir)],
            };
        }
        // An AppImage runs from a temporary mount, so the image itself is
        // what to start.
        let command = std::env::var_os("APPIMAGE")
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::current_exe().ok())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| SERVER_NAME.into());
        Self {
            command,
            args: vec!["mcp".into()],
            env: Vec::new(),
        }
    }

    fn env_json(&self) -> serde_json::Map<String, Value> {
        self.env
            .iter()
            .map(|(name, value)| (name.clone(), Value::String(value.clone())))
            .collect()
    }

    /// The server entry most apps take: command, arguments and environment.
    fn entry(&self, typed: bool) -> Value {
        let mut entry = serde_json::Map::new();
        if typed {
            entry.insert("type".into(), "stdio".into());
        }
        entry.insert("command".into(), self.command.clone().into());
        entry.insert("args".into(), json!(self.args));
        if !self.env.is_empty() {
            entry.insert("env".into(), Value::Object(self.env_json()));
        }
        Value::Object(entry)
    }
}

/// The apps the page has setup for, in the order it lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Client {
    ClaudeDesktop,
    ClaudeCode,
    VsCode,
    Cursor,
    Zed,
    LmStudio,
    Codex,
    Other,
}

impl Client {
    const ALL: [Self; 8] = [
        Self::ClaudeDesktop,
        Self::ClaudeCode,
        Self::VsCode,
        Self::Cursor,
        Self::Zed,
        Self::LmStudio,
        Self::Codex,
        Self::Other,
    ];

    fn label(self, app: &AppWindow) -> String {
        match self {
            Self::ClaudeDesktop => "Claude Desktop".into(),
            Self::ClaudeCode => "Claude Code".into(),
            Self::VsCode => "VS Code".into(),
            Self::Cursor => "Cursor".into(),
            Self::Zed => "Zed".into(),
            Self::LmStudio => "LM Studio".into(),
            Self::Codex => "Codex".into(),
            Self::Other => translated(app, &UiMessage::plain("Other apps")).into(),
        }
    }

    fn hint(self) -> UiMessage {
        match self {
            Self::ClaudeDesktop => UiMessage::plain(
                "In Claude Desktop, open Settings, Developer, Edit Config, and add flectar-mail to the mcpServers in that file. Then restart Claude Desktop.",
            ),
            Self::ClaudeCode => UiMessage::plain("Run this in a terminal."),
            Self::VsCode => UiMessage::plain(
                "Click Add to VS Code to review it there, or add flectar-mail to the mcpServers in ~/.copilot/mcp-config.json.",
            ),
            Self::Cursor => {
                UiMessage::plain("Add flectar-mail to the mcpServers in ~/.cursor/mcp.json.")
            }
            Self::Zed => {
                UiMessage::plain("Add flectar-mail to the context_servers in Zed's settings.json.")
            }
            Self::LmStudio => UiMessage::plain(
                "Click Add to LM Studio to review it there, or add flectar-mail to the mcpServers in LM Studio's mcp.json.",
            ),
            Self::Codex => UiMessage::plain("Add this to ~/.codex/config.toml."),
            Self::Other => UiMessage::plain(
                "Apps that use local MCP servers ask for the command that starts one. Give them this one.",
            ),
        }
    }

    fn install_label(self) -> UiMessage {
        match self {
            Self::VsCode => UiMessage::plain("Add to VS Code"),
            _ => UiMessage::plain("Add to LM Studio"),
        }
    }

    /// A link that opens the app's own review of the server, where it has
    /// one.
    fn install_url(self, launch: &Launch) -> Option<String> {
        match self {
            Self::VsCode => {
                let mut entry = launch.entry(true);
                entry["name"] = SERVER_NAME.into();
                Some(format!(
                    "vscode:mcp/install?{}",
                    uri_component(&entry.to_string())
                ))
            }
            Self::LmStudio => {
                use base64::Engine;
                let config = base64::engine::general_purpose::STANDARD
                    .encode(launch.entry(false).to_string());
                Some(format!(
                    "lmstudio://add_mcp?name={SERVER_NAME}&config={}",
                    uri_component(&config)
                ))
            }
            _ => None,
        }
    }

    /// What to paste into the app.
    fn setup(self, app: &AppWindow, launch: &Launch) -> String {
        let servers = |key: &str, entry: Value| {
            let file = json!({ key: { SERVER_NAME: entry } });
            serde_json::to_string_pretty(&file).unwrap_or_default()
        };
        match self {
            Self::ClaudeDesktop | Self::LmStudio => servers("mcpServers", launch.entry(false)),
            Self::VsCode | Self::Cursor => servers("mcpServers", launch.entry(true)),
            Self::Zed => servers("context_servers", launch.entry(false)),
            Self::ClaudeCode => {
                let mut command = vec!["claude".into(), "mcp".into(), "add".into()];
                command.extend(["--scope".into(), "user".into()]);
                for (name, value) in &launch.env {
                    command.extend(["--env".into(), shell_word(&format!("{name}={value}"))]);
                }
                command.extend([SERVER_NAME.into(), "--".into(), shell_word(&launch.command)]);
                command.extend(launch.args.iter().map(|arg| shell_word(arg)));
                command.join(" ")
            }
            Self::Codex => {
                let mut lines = vec![
                    format!("[mcp_servers.{SERVER_NAME}]"),
                    format!("command = {}", toml_string(&launch.command)),
                    format!(
                        "args = [{}]",
                        launch
                            .args
                            .iter()
                            .map(|arg| toml_string(arg))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ];
                if !launch.env.is_empty() {
                    lines.push(format!(
                        "env = {{ {} }}",
                        launch
                            .env
                            .iter()
                            .map(|(name, value)| format!("{name} = {}", toml_string(value)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                lines.join("\n")
            }
            Self::Other => {
                let mut lines = vec![
                    translated(app, &UiMessage::detail("Command: {}", &launch.command)).to_string(),
                    translated(
                        app,
                        &UiMessage::detail("Arguments: {}", launch.args.join(" ")),
                    )
                    .to_string(),
                ];
                for (name, value) in &launch.env {
                    lines.push(
                        translated(
                            app,
                            &UiMessage::detail("Environment: {}", format!("{name}={value}")),
                        )
                        .to_string(),
                    );
                }
                lines.join("\n")
            }
        }
    }
}

/// `text` as JavaScript's encodeURIComponent writes it, as the apps' link
/// formats expect.
fn uri_component(text: &str) -> String {
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// A word for the user's shell: quoted when it needs to be.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-=:@+,".contains(c));
    if plain {
        word.to_owned()
    } else if cfg!(windows) {
        // Windows paths can't hold quotes, so double quotes always do.
        format!("\"{word}\"")
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn toml_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(command: &str) -> Launch {
        Launch {
            command: command.into(),
            args: vec!["mcp".into()],
            env: Vec::new(),
        }
    }

    fn flatpak() -> Launch {
        Launch {
            command: "flatpak".into(),
            args: vec!["run".into(), "com.flectar.mail".into(), "mcp".into()],
            env: vec![("XDG_RUNTIME_DIR".into(), "/run/user/1000".into())],
        }
    }

    #[test]
    fn links_open_each_apps_own_review() {
        let launch = launch("/opt/Flectar Mail/flectar-mail");
        let vscode = Client::VsCode.install_url(&launch).unwrap();
        let json = vscode.strip_prefix("vscode:mcp/install?").unwrap();
        assert!(!json.contains(' ') && !json.contains('{'));
        assert!(json.contains("%22name%22%3A%22flectar-mail%22"));
        assert!(json.contains("%2Fopt%2FFlectar%20Mail%2Fflectar-mail"));

        let lm_studio = Client::LmStudio.install_url(&launch).unwrap();
        let config = lm_studio
            .strip_prefix("lmstudio://add_mcp?name=flectar-mail&config=")
            .unwrap()
            .replace("%2B", "+")
            .replace("%2F", "/")
            .replace("%3D", "=");
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(config)
            .unwrap();
        let entry: Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(
            entry,
            json!({ "command": "/opt/Flectar Mail/flectar-mail", "args": ["mcp"] })
        );
        assert_eq!(Client::Zed.install_url(&launch), None);
    }

    #[test]
    fn the_flatpak_keeps_its_runtime_directory() {
        let entry = flatpak().entry(true);
        assert_eq!(
            entry,
            json!({
                "type": "stdio",
                "command": "flatpak",
                "args": ["run", "com.flectar.mail", "mcp"],
                "env": { "XDG_RUNTIME_DIR": "/run/user/1000" },
            })
        );
    }

    #[test]
    fn shell_words_and_toml_strings_survive_odd_paths() {
        assert_eq!(shell_word("/usr/bin/flectar-mail"), "/usr/bin/flectar-mail");
        if !cfg!(windows) {
            assert_eq!(
                shell_word("/Applications/Flectar Mail.app/x"),
                "'/Applications/Flectar Mail.app/x'"
            );
            assert_eq!(shell_word("it's"), r"'it'\''s'");
        }
        assert_eq!(
            toml_string(r#"C:\Program Files\Flectar "Mail"\flectar-mail.exe"#),
            r#""C:\\Program Files\\Flectar \"Mail\"\\flectar-mail.exe""#
        );
    }

    fn folder(id: i64, name: &str, role: Option<&str>) -> FolderInfo {
        FolderInfo {
            id,
            account_id: 1,
            parent_id: None,
            display_name: name.into(),
            is_jmap: false,
            path: name.into(),
            delimiter: None,
            role: role.map(str::to_owned),
            selectable: true,
            can_create_children: true,
            can_rename: true,
            can_delete: true,
        }
    }

    #[test]
    fn folder_summaries_name_the_exceptions() {
        let folders = [
            folder(1, "Inbox", Some(roles::INBOX)),
            folder(2, "Sent", Some(roles::SENT)),
            folder(3, "Archive", Some(roles::ARCHIVE)),
            folder(4, "Receipts", None),
            folder(5, "Spam", Some(roles::SPAM)),
            folder(6, "Trash", Some(roles::TRASH)),
        ];
        let folders = folders.iter().collect::<Vec<_>>();
        let mut access = AgentAccess {
            enabled: true,
            ..AgentAccess::default()
        };
        assert_eq!(
            folders_summary(&folders, &access),
            UiMessage::detail("All folders except {}", "Spam, Trash")
        );
        access.shown_folders = vec![5, 6];
        assert_eq!(
            folders_summary(&folders, &access),
            UiMessage::plain("All folders")
        );
        // New folders show unless hidden, so a tie names what's hidden.
        access.hidden_folders = vec![4, 5, 6];
        access.shown_folders.clear();
        assert_eq!(
            folders_summary(&folders, &access),
            UiMessage::detail("All folders except {}", "Receipts, Spam, Trash")
        );
        access.hidden_folders = vec![3, 4, 5, 6];
        assert_eq!(
            folders_summary(&folders, &access),
            UiMessage::detail("Only {}", "Inbox, Sent")
        );
        access.hidden_folders = vec![1, 2, 3, 4];
        assert_eq!(
            folders_summary(&folders, &access),
            UiMessage::plain("No folders")
        );
    }

    #[test]
    fn counts_take_the_language_s_plural_forms() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        struct Headless(Rc<MinimalSoftwareWindow>);
        impl slint::platform::Platform for Headless {
            fn create_window_adapter(
                &self,
            ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let app = AppWindow::new().unwrap();
        let text = |message: UiMessage| translated(&app, &message).to_string();

        assert_eq!(text(UiMessage::detail("{} results", 1)), "1 result");
        assert_eq!(text(UiMessage::detail("{} results", 12)), "12 results");
        assert_eq!(text(UiMessage::detail("To {} people", 1)), "To 1 person");
        assert_eq!(text(UiMessage::detail("To {} people", 3)), "To 3 people");
        assert_eq!(
            text(UiMessage::arguments("{} of {} folders", 3, 12)),
            "3 of 12 folders"
        );
    }
}
