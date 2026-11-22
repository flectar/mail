//! Rules that sort new mail as it arrives (#238): their list in Settings and
//! the editor. The core runs them as sync stores new mail
//! (`flectar_mail_core::rules`), and puts them on servers that can run them.

use super::*;
use flectar_mail_core::Core;
use flectar_mail_core::events::CoreEvent;
use flectar_mail_core::models::{
    MailProtocol, MailRule, RuleAction, RuleCondition, RuleField, RuleOp, ServerRules,
    ServerRulesAnswer, ServerRulesState,
};
use slint::SharedString;
use std::future::Future;

mod choices;
use choices::*;

/// The rule being edited.
#[derive(Default)]
struct Editor {
    id: i64,
    choices: Choices,
    /// The folders and labels the editor shows now, by their place.
    folders: Vec<i64>,
    labels: Vec<i64>,
    /// The categories the editor shows, by key.
    tabs: Vec<String>,
}

struct Controller {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
    rows: Rc<VecModel<RuleRow>>,
    notices: Rc<VecModel<RulesNotice>>,
    conditions: Rc<VecModel<RuleConditionRow>>,
    actions: Rc<VecModel<RuleActionRow>>,
    rules: RefCell<Vec<MailRule>>,
    /// The accounts, and where their rules run, as last shown.
    accounts: RefCell<Vec<Account>>,
    servers: RefCell<Vec<ServerRules>>,
    editor: RefCell<Editor>,
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
    let controller = Rc::new(Controller {
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
        rows: Rc::new(VecModel::default()),
        notices: Rc::new(VecModel::default()),
        conditions: Rc::new(VecModel::default()),
        actions: Rc::new(VecModel::default()),
        rules: RefCell::default(),
        accounts: RefCell::default(),
        servers: RefCell::default(),
        editor: RefCell::default(),
    });
    let ui = app.global::<MailRules>();
    ui.set_rows(ModelRc::from(controller.rows.clone()));
    ui.set_notices(ModelRc::from(controller.notices.clone()));
    ui.set_conditions(ModelRc::from(controller.conditions.clone()));
    ui.set_actions(ModelRc::from(controller.actions.clone()));
    CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller));
    ui.on_create(|| with_controller(|controller| controller.open(None)));
    ui.on_edit(|id| with_controller(|controller| controller.open(Some(i64::from(id)))));
    ui.on_close(|| with_controller(|controller| controller.close()));
    ui.on_save(|| with_controller(|controller| controller.save()));
    ui.on_delete(|| with_controller(|controller| controller.delete()));
    ui.on_toggle(|id, enabled| {
        with_controller(|controller| controller.toggle(i64::from(id), enabled));
    });
    ui.on_move(|id, by| with_controller(|controller| controller.reorder(i64::from(id), by)));
    ui.on_notice_action(|id| with_controller(|controller| controller.answer(i64::from(id))));
    ui.on_choose_cleanup(|trash, junk| {
        with_controller(|controller| controller.choose_cleanup(trash, junk));
    });
    ui.on_choose_account(|index| with_controller(|controller| controller.choose_account(index)));
    ui.on_add_condition(|| with_controller(|controller| controller.add_condition()));
    ui.on_remove_condition(|index| {
        with_controller(|controller| remove(&controller.conditions, index));
    });
    ui.on_add_action(|| with_controller(|controller| controller.add_action()));
    ui.on_run(|| with_controller(|controller| controller.run()));
    ui.on_confirm_run(|| with_controller(|controller| controller.confirm_run()));
    ui.on_create_from_message(|| with_controller(|controller| controller.create_from_message()));
    ui.on_remove_action(|index| with_controller(|controller| remove(&controller.actions, index)));
}

/// Show the rules in Settings, once the core is there.
pub(super) fn refresh() {
    with_controller(|controller| controller.refresh());
}

/// What the editor offers: the accounts that receive mail, their folders
/// and the labels.
fn choices_of(
    accounts: Vec<flectar_mail_core::models::Account>,
    folders: Vec<flectar_mail_core::models::FolderInfo>,
    labels: Vec<flectar_mail_core::models::Label>,
) -> Choices {
    Choices {
        accounts: accounts
            .into_iter()
            .filter(|account| account.mail_protocol != MailProtocol::Local)
            .collect(),
        folders,
        labels,
    }
}

/// Show for how many days Trash and Junk keep mail.
pub(super) fn apply_cleanup(app: &AppWindow, trash_days: u32, junk_days: u32) {
    let ui = app.global::<MailRules>();
    ui.set_trash_days(cleanup_choice(trash_days));
    ui.set_junk_days(cleanup_choice(junk_days));
}

/// Shows where the rules run again whenever that changes.
pub(super) fn core_ready(runtime: &tokio::runtime::Handle, core: Arc<Core>) {
    let mut events = core.bus.subscribe();
    runtime.spawn(async move {
        loop {
            match events.recv().await {
                Ok(CoreEvent::ServerRulesChanged { .. })
                | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let _ = slint::invoke_from_event_loop(refresh);
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

/// Remove row `index`, keeping one: a rule needs a condition and an action.
fn remove<T: Clone + 'static>(model: &VecModel<T>, index: i32) {
    if let Ok(index) = usize::try_from(index)
        && index < model.row_count()
        && model.row_count() > 1
    {
        model.remove(index);
    }
}

impl Controller {
    fn core(&self) -> Option<CoreMailSource> {
        self.state.borrow().core.clone()
    }

    fn refresh(&self) {
        let Some(core) = self.core() else {
            return;
        };
        self.spawn(
            async move {
                Ok((
                    core.mail_rules().await?,
                    core.rule_choices().await?,
                    core.server_rules().await?,
                ))
            },
            |_app, result| {
                let Ok((rules, (accounts, folders, labels), servers)) = result else {
                    return;
                };
                with_controller(|controller| {
                    let rows = rules
                        .iter()
                        .map(|rule| RuleRow {
                            id: i32::try_from(rule.id).unwrap_or_default(),
                            name: rule.name.as_str().into(),
                            account: rule
                                .account_id
                                .and_then(|id| accounts.iter().find(|account| account.id == id))
                                .map(|account| account.email.as_str())
                                .unwrap_or_default()
                                .into(),
                            enabled: rule.enabled,
                            problem: !targets_exist(rule, &folders, &labels),
                            place: place(rule, &accounts, &servers).0.into(),
                        })
                        .collect::<Vec<_>>();
                    controller.notices.set_vec(
                        notices(&accounts, &servers, &rules)
                            .into_iter()
                            .map(|notice| RulesNotice {
                                account_id: i32::try_from(notice.account_id).unwrap_or_default(),
                                account: notice.account.into(),
                                kind: notice.kind.into(),
                                detail: notice.detail.into(),
                                has_rules: notice.has_rules,
                            })
                            .collect::<Vec<_>>(),
                    );
                    *controller.accounts.borrow_mut() = accounts;
                    *controller.servers.borrow_mut() = servers;
                    // In place, so a switch that was just turned keeps its
                    // row.
                    if controller.rows.row_count() == rows.len() {
                        for (index, row) in rows.into_iter().enumerate() {
                            if controller.rows.row_data(index).as_ref() != Some(&row) {
                                controller.rows.set_row_data(index, row);
                            }
                        }
                    } else {
                        controller.rows.set_vec(rows);
                    }
                    *controller.rules.borrow_mut() = rules;
                });
            },
        );
    }

    /// Open the editor on rule `id`, or on a new rule.
    fn open(&self, id: Option<i64>) {
        let Some(core) = self.core() else {
            return;
        };
        let rule = id.and_then(|id| {
            self.rules
                .borrow()
                .iter()
                .find(|rule| rule.id == id)
                .cloned()
        });
        if id.is_some() && rule.is_none() {
            return;
        }
        self.spawn(
            async move { core.rule_choices().await },
            move |app, result| {
                let Ok((accounts, folders, labels)) = result else {
                    return;
                };
                let choices = choices_of(accounts, folders, labels);
                with_controller(|controller| controller.show(&app, choices, rule));
            },
        );
    }

    /// The message menu: Settings opens on a new rule for mail like the
    /// open message, for its mailing list or its sender.
    fn create_from_message(&self) {
        let (core, target) = {
            let state = self.state.borrow();
            let target = state
                .selected_id
                .and_then(|id| state.messages.iter().find(|message| message.id == id))
                .and_then(crate::mail::MailTarget::of);
            (state.core.clone(), target)
        };
        let (Some(core), Some(target)) = (core, target) else {
            return;
        };
        self.spawn(
            async move { Ok((core.rule_from(target).await?, core.rule_choices().await?)) },
            |app, result| match result {
                Ok((rule, (accounts, folders, labels))) => {
                    app.set_settings_tab("Rules".into());
                    app.invoke_open_settings();
                    let choices = choices_of(accounts, folders, labels);
                    with_controller(|controller| controller.show(&app, choices, Some(rule)));
                }
                Err(error) => crate::undo::tell(
                    &app,
                    UiMessage::detail("Could not save the rule: {}", error),
                ),
            },
        );
    }

    /// Run on Inbox: how much the rule as written matches there now, to ask
    /// before running it.
    fn run(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<MailRules>();
        if ui.get_busy() {
            return;
        }
        let rule = match self.written(&app) {
            Ok(rule) => rule,
            Err(message) => {
                ui.set_error(translated(&app, &message));
                return;
            }
        };
        let Some(core) = self.core() else {
            return;
        };
        ui.set_busy(true);
        ui.set_error(SharedString::new());
        self.spawn(
            async move { core.mail_rule_matches(rule).await },
            |app, result| {
                let ui = app.global::<MailRules>();
                ui.set_busy(false);
                match result {
                    Ok(found) if found.message_ids.is_empty() => ui.set_error(translated(
                        &app,
                        &UiMessage::plain("This rule matches no mail in the Inbox now."),
                    )),
                    Ok(found) => {
                        ui.set_run_count(
                            i32::try_from(found.message_ids.len()).unwrap_or(i32::MAX),
                        );
                        ui.set_run_unknown(i32::try_from(found.unknown).unwrap_or(i32::MAX));
                        ui.set_run_open(true);
                    }
                    Err(error) => ui.set_error(translated(
                        &app,
                        &UiMessage::detail("Could not run the rule: {}", error),
                    )),
                }
            },
        );
    }

    /// The question's Run: the rule as written runs on the Inbox mail it
    /// matches, with Undo.
    fn confirm_run(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<MailRules>();
        ui.set_run_open(false);
        let rule = match self.written(&app) {
            Ok(rule) => rule,
            Err(message) => {
                ui.set_error(translated(&app, &message));
                return;
            }
        };
        let Some(core) = self.core() else {
            return;
        };
        let count = ui.get_run_count();
        ui.set_busy(true);
        self.spawn(
            async move { core.run_mail_rule(rule).await },
            move |app, result| {
                let ui = app.global::<MailRules>();
                ui.set_busy(false);
                match result {
                    Ok(action_ids) => crate::undo::ruled(&app, count, action_ids),
                    Err(error) => ui.set_error(translated(
                        &app,
                        &UiMessage::detail("Could not run the rule: {}", error),
                    )),
                }
            },
        );
    }

    fn show(&self, app: &AppWindow, choices: Choices, rule: Option<MailRule>) {
        let ui = app.global::<MailRules>();
        let mut names = vec![translated(app, &UiMessage::plain("All accounts"))];
        names.extend(
            choices
                .accounts
                .iter()
                .map(|account| SharedString::from(account.email.as_str())),
        );
        // A new rule is for the first account, whose folders it can use.
        let account = match &rule {
            Some(rule) => rule.account_id.map_or(0, |id| {
                choices
                    .accounts
                    .iter()
                    .position(|account| account.id == id)
                    .map_or(0, |index| index + 1)
            }),
            None => usize::from(!choices.accounts.is_empty()),
        };
        let account = i32::try_from(account).unwrap_or_default();
        {
            let mut editor = self.editor.borrow_mut();
            editor.id = rule.as_ref().map_or(0, |rule| rule.id);
            editor.choices = choices;
        }
        ui.set_accounts(ModelRc::new(VecModel::from(names)));
        ui.set_account(account);
        self.offer(app, account);
        let editor = self.editor.borrow();
        let rule = rule.unwrap_or_else(|| MailRule {
            id: 0,
            account_id: None,
            name: String::new(),
            enabled: true,
            match_all: true,
            conditions: vec![RuleCondition {
                field: RuleField::From,
                header: String::new(),
                op: RuleOp::Contains,
                value: String::new(),
                negate: false,
            }],
            actions: vec![RuleAction::Move { folder: 0 }],
            stop: false,
        });
        ui.set_editing_id(i32::try_from(rule.id).unwrap_or_default());
        let (place, why) = if rule.id == 0 {
            ("", "")
        } else {
            place(&rule, &self.accounts.borrow(), &self.servers.borrow())
        };
        ui.set_place(place.into());
        ui.set_place_why(why.into());
        ui.set_name(rule.name.as_str().into());
        ui.set_match(if rule.match_all { 0 } else { 1 });
        ui.set_stop(rule.stop);
        ui.set_error(SharedString::new());
        self.conditions.set_vec(
            rule.conditions
                .iter()
                .map(|condition| {
                    let field = condition.field;
                    RuleConditionRow {
                        field: index_of(&FIELDS, &field),
                        header: condition.header.as_str().into(),
                        op: index_of(&ops(field), &(condition.op, condition.negate)),
                        value: condition.value.as_str().into(),
                    }
                })
                .collect::<Vec<_>>(),
        );
        self.actions.set_vec(
            rule.actions
                .iter()
                .map(|action| RuleActionRow {
                    kind: index_of(&KINDS, &kind_of(action)),
                    target: match action {
                        RuleAction::Move { folder } | RuleAction::Copy { folder } => {
                            index_of(&editor.folders, folder)
                        }
                        RuleAction::Label { label } => index_of(&editor.labels, label),
                        RuleAction::ShowIn { tab } => index_of(&editor.tabs, tab),
                        _ => 0,
                    },
                })
                .collect::<Vec<_>>(),
        );
        ui.set_editing(true);
    }

    /// Offer what account choice `index` can use: its folders, the labels
    /// it can add, and copying where it keeps a message in several folders.
    fn offer(&self, app: &AppWindow, index: i32) {
        let ui = app.global::<MailRules>();
        let mut editor = self.editor.borrow_mut();
        let account = editor.choices.account(index);
        let folders = editor.choices.folders(account);
        let labels = editor.choices.labels(account);
        let copies = account.is_some_and(|account| account.mail_protocol == MailProtocol::Jmap);
        ui.set_folders(ModelRc::new(VecModel::from(
            folders
                .iter()
                .map(|(_, place)| SharedString::from(place.as_str()))
                .collect::<Vec<_>>(),
        )));
        ui.set_labels(ModelRc::new(VecModel::from(
            labels
                .iter()
                .map(|(_, name)| SharedString::from(name.as_str()))
                .collect::<Vec<_>>(),
        )));
        ui.set_can_copy(copies);
        let tabs = editor.choices.tabs();
        let words = app.global::<MailboxWords>();
        ui.set_tabs(ModelRc::new(VecModel::from(
            tabs.iter()
                .map(|(_, name)| match name {
                    TabName::Role(role) => words.invoke_name(*role),
                    TabName::Text(name) => SharedString::from(name.as_str()),
                })
                .collect::<Vec<_>>(),
        )));
        editor.folders = folders.into_iter().map(|(id, _)| id).collect();
        editor.labels = labels.into_iter().map(|(id, _)| id).collect();
        editor.tabs = tabs.into_iter().map(|(key, _)| key).collect();
    }

    /// Another account: what it can file into, by the same names where it
    /// has them.
    fn choose_account(&self, index: i32) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let (folders, labels) = {
            let editor = self.editor.borrow();
            (editor.folders.clone(), editor.labels.clone())
        };
        self.offer(&app, index);
        let editor = self.editor.borrow();
        for row in 0..self.actions.row_count() {
            let Some(mut action) = self.actions.row_data(row) else {
                continue;
            };
            let target = usize::try_from(action.target).ok();
            action.target = match KINDS.get(usize::try_from(action.kind).unwrap_or(0)) {
                Some(Kind::Move | Kind::Copy) => target
                    .and_then(|target| folders.get(target))
                    .map_or(0, |id| index_of(&editor.folders, id)),
                Some(Kind::Label) => target
                    .and_then(|target| labels.get(target))
                    .map_or(0, |id| index_of(&editor.labels, id)),
                _ => 0,
            };
            self.actions.set_row_data(row, action);
        }
    }

    fn add_condition(&self) {
        self.conditions.push(RuleConditionRow {
            field: 0,
            header: SharedString::new(),
            op: 0,
            value: SharedString::new(),
        });
    }

    fn add_action(&self) {
        // Marking read is something every account can do.
        self.actions.push(RuleActionRow {
            kind: index_of(&KINDS, &Kind::MarkRead),
            target: 0,
        });
    }

    /// Do what the notice for account `account_id`'s server offers: run the
    /// rules there, replace what is there, or try again.
    fn answer(&self, account_id: i64) {
        let Some(core) = self.core() else {
            return;
        };
        let answer = self
            .servers
            .borrow()
            .iter()
            .find(|server| server.account_id == account_id)
            .and_then(|server| match &server.state {
                ServerRulesState::RunBefore(theirs) => {
                    Some(ServerRulesAnswer::RunBefore(theirs.clone()))
                }
                ServerRulesState::Others => Some(ServerRulesAnswer::ReplaceOthers),
                ServerRulesState::Edited => Some(ServerRulesAnswer::ReplaceChanges),
                _ => None,
            });
        self.spawn(
            async move { core.put_rules_on_server(account_id, answer).await },
            move |_app, result| {
                if let Err(error) = result {
                    tracing::warn!(account_id, %error, "rules on the server failed");
                }
                with_controller(|controller| controller.refresh());
            },
        );
    }

    /// Keep mail in Trash and Junk for the days of cleanup choices `trash`
    /// and `junk`.
    fn choose_cleanup(&self, trash: i32, junk: i32) {
        let Some(core) = self.core() else {
            return;
        };
        let days = |choice: i32| {
            usize::try_from(choice)
                .ok()
                .and_then(|choice| CLEANUP_DAYS.get(choice))
                .copied()
                .unwrap_or(0)
        };
        let (trash, junk) = (days(trash), days(junk));
        self.spawn(
            async move { core.set_folder_cleanup(trash, junk).await },
            |app, result| match result {
                Ok(()) => app.set_sync_status(UiMessage::plain("Cleanup preference saved.")),
                Err(error) => app.set_sync_status(UiMessage::detail(
                    "Could not save the cleanup preference: {}",
                    error,
                )),
            },
        );
    }

    fn close(&self) {
        if let Some(app) = self.app.upgrade() {
            let ui = app.global::<MailRules>();
            if !ui.get_busy() {
                ui.set_editing(false);
            }
        }
    }

    /// The rule as the editor shows it, or what to fix first.
    fn written(&self, app: &AppWindow) -> Result<MailRule, UiMessage> {
        let ui = app.global::<MailRules>();
        let editor = self.editor.borrow();
        let name = ui.get_name().trim().to_owned();
        if name.is_empty() {
            return Err(UiMessage::plain("Name this rule."));
        }
        let account = editor.choices.account(ui.get_account());
        let mut conditions = Vec::new();
        for (index, row) in self.conditions.iter().enumerate() {
            let number = index + 1;
            let field = FIELDS[usize::try_from(row.field)
                .unwrap_or(0)
                .min(FIELDS.len() - 1)];
            let ops = ops(field);
            let (op, negate) = ops[usize::try_from(row.op).unwrap_or(0).min(ops.len() - 1)];
            let value = row.value.trim().to_owned();
            let header = row.header.trim().to_owned();
            if field == RuleField::Header
                && !flectar_mail_core::rules::is_valid_header_name(&header)
            {
                return Err(UiMessage::detail(
                    "Condition {}: name a header, such as X-Spam-Flag.",
                    number,
                ));
            }
            match op {
                RuleOp::Exists => {}
                RuleOp::Over | RuleOp::Under => {
                    if value.parse::<u32>().is_err() {
                        return Err(UiMessage::detail(
                            "Condition {}: give a size in KB.",
                            number,
                        ));
                    }
                }
                RuleOp::Matches => {
                    if value.is_empty() || !flectar_mail_core::rules::is_valid_pattern(&value) {
                        return Err(UiMessage::detail(
                            "Condition {}: this isn't a valid regular expression.",
                            number,
                        ));
                    }
                }
                _ => {
                    if value.is_empty() {
                        return Err(UiMessage::detail(
                            "Condition {}: fill in what to look for.",
                            number,
                        ));
                    }
                }
            }
            conditions.push(RuleCondition {
                field,
                header: if field == RuleField::Header {
                    header
                } else {
                    String::new()
                },
                op,
                // A pattern is kept as written; spaces can matter in it.
                value: if op == RuleOp::Matches {
                    row.value.to_string()
                } else {
                    value
                },
                negate,
            });
        }
        let mut actions = Vec::new();
        for (index, row) in self.actions.iter().enumerate() {
            let number = index + 1;
            let kind = KINDS
                .get(usize::try_from(row.kind).unwrap_or(0))
                .copied()
                .unwrap_or(Kind::MarkRead);
            let target = usize::try_from(row.target).unwrap_or(0);
            let folder = || {
                if account.is_none() {
                    return Err(UiMessage::plain(
                        "Choose one account to move or copy mail into its folders.",
                    ));
                }
                editor
                    .folders
                    .get(target)
                    .copied()
                    .ok_or_else(|| UiMessage::detail("Action {}: choose a folder.", number))
            };
            actions.push(match kind {
                Kind::Move => RuleAction::Move { folder: folder()? },
                Kind::Copy => {
                    if account.is_none_or(|account| account.mail_protocol != MailProtocol::Jmap)
                    {
                        return Err(UiMessage::detail(
                            "Action {}: this account keeps each message in one folder, so it can't copy mail.",
                            number,
                        ));
                    }
                    RuleAction::Copy { folder: folder()? }
                }
                Kind::Label => RuleAction::Label {
                    label: editor
                        .labels
                        .get(target)
                        .copied()
                        .ok_or_else(|| UiMessage::detail("Action {}: choose a label.", number))?,
                },
                Kind::MarkRead => RuleAction::MarkRead,
                Kind::Star => RuleAction::Star,
                Kind::Archive => RuleAction::Archive,
                Kind::Trash => RuleAction::Trash,
                Kind::Junk => RuleAction::Junk,
                Kind::ShowIn => RuleAction::ShowIn {
                    tab: editor.tabs.get(target).cloned().unwrap_or_else(|| "important".into()),
                },
            });
        }
        if actions
            .iter()
            .filter(|action| flectar_mail_core::rules::files_away(action))
            .count()
            > 1
        {
            return Err(UiMessage::plain(
                "A rule can move mail out of the Inbox only once. Keep one of moving, archiving, trash and spam.",
            ));
        }
        let enabled = self
            .rules
            .borrow()
            .iter()
            .find(|rule| rule.id == editor.id)
            .is_none_or(|rule| rule.enabled);
        Ok(MailRule {
            id: editor.id,
            account_id: account.map(|account| account.id),
            name,
            enabled,
            match_all: ui.get_match() == 0,
            conditions,
            actions,
            stop: ui.get_stop(),
        })
    }

    fn save(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<MailRules>();
        if ui.get_busy() {
            return;
        }
        let rule = match self.written(&app) {
            Ok(rule) => rule,
            Err(message) => {
                ui.set_error(translated(&app, &message));
                return;
            }
        };
        let Some(core) = self.core() else {
            return;
        };
        ui.set_busy(true);
        ui.set_error(SharedString::new());
        self.spawn(
            async move { core.save_mail_rule(rule).await },
            |app, result| {
                let ui = app.global::<MailRules>();
                ui.set_busy(false);
                match result {
                    Ok(_) => {
                        ui.set_editing(false);
                        with_controller(|controller| controller.refresh());
                    }
                    Err(error) => ui.set_error(translated(
                        &app,
                        &UiMessage::detail("Could not save the rule: {}", error),
                    )),
                }
            },
        );
    }

    fn delete(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<MailRules>();
        let id = self.editor.borrow().id;
        if ui.get_busy() || id == 0 {
            return;
        }
        let Some(core) = self.core() else {
            return;
        };
        ui.set_busy(true);
        self.spawn(
            async move { core.delete_mail_rule(id).await },
            |app, result| {
                let ui = app.global::<MailRules>();
                ui.set_busy(false);
                match result {
                    Ok(()) => {
                        ui.set_editing(false);
                        with_controller(|controller| controller.refresh());
                    }
                    Err(error) => ui.set_error(translated(
                        &app,
                        &UiMessage::detail("Could not delete the rule: {}", error),
                    )),
                }
            },
        );
    }

    fn toggle(&self, id: i64, enabled: bool) {
        let Some(core) = self.core() else {
            return;
        };
        self.spawn(
            async move { core.set_mail_rule_enabled(id, enabled).await },
            |app, result| {
                if let Err(error) = result {
                    crate::undo::tell(
                        &app,
                        UiMessage::detail("Could not save the rule: {}", error),
                    );
                }
                with_controller(|controller| controller.refresh());
            },
        );
    }

    /// Move rule `id` up (-1) or down (1) the list.
    fn reorder(&self, id: i64, by: i32) {
        let Some(core) = self.core() else {
            return;
        };
        let mut ids = self
            .rules
            .borrow()
            .iter()
            .map(|rule| rule.id)
            .collect::<Vec<_>>();
        let Some(from) = ids.iter().position(|candidate| *candidate == id) else {
            return;
        };
        let Some(to) = from
            .checked_add_signed(isize::try_from(by).unwrap_or(0))
            .filter(|to| *to < ids.len())
        else {
            return;
        };
        ids.swap(from, to);
        self.spawn(
            async move { core.reorder_mail_rules(ids).await },
            |app, result| {
                if let Err(error) = result {
                    crate::undo::tell(
                        &app,
                        UiMessage::detail("Could not save the rule: {}", error),
                    );
                }
                with_controller(|controller| controller.refresh());
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
