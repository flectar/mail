//! Rules on the server (#238). Where an account's server runs Sieve scripts,
//! over ManageSieve or JMAP, the app puts the account's rules in its own
//! script there, as far as the server runs them as the app does, so they
//! sort mail while Flectar Mail is closed. The app keeps trying every rule on
//! new mail while it's open, and finds the work of those the server ran
//! done.
//!
//! Each device keeps its own rules. The script holds the rules of the device
//! that last wrote them, and a device leaves rules another one put there
//! until the user says to replace them, so that two devices with different
//! rules don't take turns. Devices with the same rules agree.

use crate::Core;
use crate::accounts::credentials::{self, Slot};
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::models::{
    AccountConfig, MailProtocol, Provider, RuleAction, RuleHere, RuleHereWhy, ServerRules,
    ServerRulesAnswer, ServerRulesState, now_ms, roles,
};
use crate::sieve::rules::{Action, Engine, Folder, Rule, Why, compile};
use crate::sieve::{self, Mirrored, Question, Refused, Scripts};
use crate::sync::engine::SyncCtx;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

/// How long after a rule changes the servers hear of it, so that a run of
/// changes, such as moving a rule up a few places, goes as one.
const SETTLE: Duration = Duration::from_millis(1500);

/// One writer at a time for each account's script on its server.
static WRITERS: LazyLock<Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);
/// The accounts whose servers are about to hear of changed rules.
static PENDING: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(Default::default);

/// Held while the app's script on account `account_id`'s server is read and
/// written, for rules or for automatic replies.
pub(crate) fn writer(account_id: i64) -> Arc<tokio::sync::Mutex<()>> {
    WRITERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(account_id)
        .or_default()
        .clone()
}

/// Whether the app can put account `config`'s rules on its server, as a
/// generic IMAP or JMAP account. A POP3 account's server files mail into
/// folders the app never downloads.
fn on_sieve(config: &AccountConfig) -> bool {
    config.provider == Provider::Generic
        && matches!(
            config.mail_protocol,
            MailProtocol::Imap | MailProtocol::Jmap
        )
}

/// Account `account_id`'s enabled rules, as its server would run them, with
/// their folders by name and labels by keyword. A folder or label that is
/// gone has no name, which keeps the rule in the app.
fn resolve(conn: &Connection, account_id: i64) -> Result<Vec<Rule>> {
    let folder = |found: Option<repo::folders::Folder>| {
        found
            .filter(|folder| folder.account_id == account_id)
            .map(|folder| Folder {
                path: folder.path,
                jmap_id: folder.jmap_id,
            })
            .unwrap_or(Folder {
                path: String::new(),
                jmap_id: None,
            })
    };
    let role = |role: &str| repo::folders::by_role(conn, account_id, role);
    repo::rules::enabled_for(conn, account_id)?
        .into_iter()
        .map(|rule| {
            // Tabs are this device's alone, and the app puts mail in them as
            // it sees it, so the rest of the rule can still go to the server.
            let actions = rule
                .actions
                .iter()
                .filter(|action| !matches!(action, RuleAction::ShowIn { .. }))
                .map(|action| {
                    Ok(match action {
                        RuleAction::Move { folder: id } => Action::Move {
                            folder: folder(repo::folders::get(conn, *id)?),
                        },
                        RuleAction::Copy { folder: id } => Action::Copy {
                            folder: folder(repo::folders::get(conn, *id)?),
                        },
                        RuleAction::Label { label } => Action::Label {
                            keyword: repo::labels::get(conn, *label)?
                                .map(|label| label.keyword)
                                .unwrap_or_default(),
                        },
                        RuleAction::MarkRead => Action::MarkRead,
                        RuleAction::Star => Action::Star,
                        // As archiving does in the app, into All Mail when
                        // there's no Archive.
                        RuleAction::Archive => Action::Move {
                            folder: folder(match role(roles::ARCHIVE)? {
                                Some(archive) => Some(archive),
                                None => role(roles::ALL)?,
                            }),
                        },
                        RuleAction::Trash => Action::Move {
                            folder: folder(role(roles::TRASH)?),
                        },
                        RuleAction::Junk => Action::Move {
                            folder: folder(role(roles::SPAM)?),
                        },
                        RuleAction::ShowIn { .. } => unreachable!("left out above"),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Rule {
                id: rule.id,
                name: rule.name,
                match_all: rule.match_all,
                conditions: rule.conditions,
                actions,
                stop: rule.stop,
            })
        })
        .collect()
}

/// The addresses automatic replies go out for: the account's own.
pub(crate) fn reply_addresses(conn: &Connection, account_id: i64) -> Result<Vec<String>> {
    Ok(repo::sender_identities::list(conn, account_id)?
        .into_iter()
        .filter(|identity| identity.is_verified() && !identity.is_wildcard())
        .map(|identity| identity.email)
        .collect())
}

fn why(why: Why) -> RuleHereWhy {
    match why {
        Why::Pattern => RuleHereWhy::Pattern,
        Why::Attachments => RuleHereWhy::Attachments,
        Why::Case => RuleHereWhy::Case,
        Why::Name => RuleHereWhy::Name,
        Why::Folder => RuleHereWhy::Folder,
        Why::Label => RuleHereWhy::Label,
        Why::Extension(_) | Why::Value => RuleHereWhy::Unsupported,
        Why::After => RuleHereWhy::After,
    }
}

/// Puts account `account_id`'s rules in the app's script among `scripts`,
/// as far as that server runs them as the app does, and keeps where they
/// run. `answer` is the user's go-ahead for what the last try asked.
pub(crate) async fn mirror(
    ctx: &SyncCtx,
    account_id: i64,
    scripts: &mut impl Scripts,
    answer: Option<&ServerRulesAnswer>,
) -> Result<ServerRules> {
    let (rules, written, addresses) = ctx
        .db
        .read(move |conn| {
            Ok((
                resolve(conn, account_id)?,
                repo::server_rules::written(conn, account_id)?,
                reply_addresses(conn, account_id)?,
            ))
        })
        .await?;
    let compiled = {
        let offers = |extension: &str| scripts.has_extension(extension);
        compile(
            &rules,
            &Engine {
                offers: &offers,
                implementation: scripts.implementation(),
            },
        )
    };
    let allowed = match answer {
        Some(ServerRulesAnswer::RunBefore(theirs)) => Some(Question::RunBefore(theirs.clone())),
        Some(ServerRulesAnswer::ReplaceChanges) => Some(Question::ReplaceChanges),
        _ => None,
    };
    let replace = answer == Some(&ServerRulesAnswer::ReplaceOthers);
    let mirrored = sieve::put_rules(
        scripts,
        compiled.section.as_ref(),
        written.as_deref(),
        &addresses,
        replace,
        allowed.as_ref(),
    )
    .await?;
    let state = match mirrored {
        Mirrored::Done => ServerRulesState::Runs,
        Mirrored::Others => ServerRulesState::Others,
        Mirrored::Ask(Question::RunBefore(theirs)) => ServerRulesState::RunBefore(theirs),
        Mirrored::Ask(Question::ReplaceChanges) => ServerRulesState::Edited,
        Mirrored::Refused(Refused::NoInclude(theirs)) => ServerRulesState::Failed(format!(
            "Your server runs your filter script \u{201c}{theirs}\u{201d} and can't run \
             Flectar Mail's rules beside it"
        )),
        Mirrored::Refused(Refused::TheirsReply(theirs)) => ServerRulesState::Failed(format!(
            "Your filter script \u{201c}{theirs}\u{201d} on the server sends automatic replies \
             as Flectar Mail does. Turn one of them off"
        )),
        Mirrored::Refused(Refused::Missing(extension)) => ServerRulesState::Failed(format!(
            "Your server lacks the Sieve extension \u{201c}{extension}\u{201d}"
        )),
    };
    let runs = state == ServerRulesState::Runs;
    let checked = ServerRules {
        account_id,
        state,
        on_server: if runs { compiled.on_server } else { Vec::new() },
        here: if runs {
            compiled
                .here
                .into_iter()
                .map(|(rule_id, reason)| RuleHere {
                    rule_id,
                    why: why(reason),
                })
                .collect()
        } else {
            Vec::new()
        },
        checked_at: now_ms(),
    };
    let digest = compiled.section.map(|section| section.digest);
    store(ctx, &checked, runs.then_some(digest)).await?;
    Ok(checked)
}

/// Keeps where account `checked.account_id`'s rules run, and the digest of
/// the rules this device put there when it wrote them, and tells the app
/// when that changed.
async fn store(
    ctx: &SyncCtx,
    checked: &ServerRules,
    written: Option<Option<String>>,
) -> Result<()> {
    let checked_copy = checked.clone();
    let changed = ctx
        .db
        .write(move |conn| {
            repo::server_rules::store(conn, &checked_copy, written.as_ref().map(Option::as_deref))
        })
        .await?;
    if changed {
        ctx.bus.emit(CoreEvent::ServerRulesChanged {
            account_id: checked.account_id,
        });
    }
    Ok(())
}

/// Keeps that account `account_id`'s rules can't run on its server, for
/// `state`'s reason.
pub(crate) async fn store_state(
    ctx: &SyncCtx,
    account_id: i64,
    state: ServerRulesState,
) -> Result<()> {
    store(
        ctx,
        &ServerRules {
            account_id,
            state,
            on_server: Vec::new(),
            here: Vec::new(),
            checked_at: now_ms(),
        },
        None,
    )
    .await
}

/// Puts account `config`'s rules on its server now, as [`mirror`] does,
/// signing in for it. None when the account's server can't run rules.
pub(crate) async fn mirror_now(
    ctx: &SyncCtx,
    config: &AccountConfig,
    answer: Option<&ServerRulesAnswer>,
) -> Result<Option<ServerRules>> {
    if !on_sieve(config) {
        return Ok(None);
    }
    let writer = writer(config.id);
    let _writing = writer.lock().await;
    let password =
        credentials::load_async(ctx.credentials.clone(), config.id, Slot::Password).await?;
    let mirrored = if config.mail_protocol == MailProtocol::Jmap {
        let connected = crate::jmap::client::connect(config, &password).await?;
        match crate::jmap::sieve::scripts(&connected) {
            Some(mut scripts) => Some(mirror(ctx, config.id, &mut scripts, answer).await),
            None => None,
        }
    } else {
        match sieve::open(
            &sieve::server(config),
            &config.username,
            &password,
            &sieve::USEFUL,
        )
        .await?
        {
            Some(mut session) => {
                let mirrored = mirror(ctx, config.id, &mut session, answer).await;
                let _ = session.logout().await;
                Some(mirrored)
            }
            None => None,
        }
    };
    match mirrored {
        Some(Ok(checked)) => Ok(Some(checked)),
        Some(Err(error)) => {
            store_state(ctx, config.id, ServerRulesState::Failed(error.to_string())).await?;
            Err(error)
        }
        None => {
            store_state(ctx, config.id, ServerRulesState::Unavailable).await?;
            Ok(None)
        }
    }
}

impl Core {
    /// Where each account's rules run, as last checked. Accounts never
    /// checked, and those whose servers the app can't put rules on, aren't
    /// there: their rules run in the app.
    pub async fn server_rules(&self) -> Result<Vec<ServerRules>> {
        self.db.read(|conn| repo::server_rules::all(conn)).await
    }

    /// Puts account `account_id`'s rules on its server now, with the user's
    /// `answer` to what the last try asked, if any, and gives where they
    /// run. None when its server can't run rules.
    pub async fn put_rules_on_server(
        &self,
        account_id: i64,
        answer: Option<ServerRulesAnswer>,
    ) -> Result<Option<ServerRules>> {
        let config = self
            .db
            .read(move |conn| repo::accounts::get_config(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;
        mirror_now(&self.sync_ctx(), &config, answer.as_ref()).await
    }

    /// Tells the servers of the accounts whose rules changed, `account_id`'s
    /// or, for None, every account's, shortly, once for a run of changes.
    pub(crate) fn rules_changed(&self, account_id: Option<i64>) {
        let ctx = self.sync_ctx();
        tokio::spawn(async move {
            let configs = match ctx.db.read(|conn| repo::accounts::list_configs(conn)).await {
                Ok(configs) => configs,
                Err(error) => {
                    tracing::warn!(%error, "rules on the server: accounts unreadable");
                    return;
                }
            };
            let unavailable = ctx
                .db
                .read(|conn| repo::server_rules::all(conn))
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|checked| checked.state == ServerRulesState::Unavailable)
                .map(|checked| checked.account_id)
                .collect::<HashSet<_>>();
            for config in configs {
                // A server without Sieve is asked again by the daily check.
                if account_id.is_some_and(|id| id != config.id)
                    || !on_sieve(&config)
                    || unavailable.contains(&config.id)
                {
                    continue;
                }
                if !PENDING
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(config.id)
                {
                    continue;
                }
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(SETTLE).await;
                    PENDING
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&config.id);
                    if let Err(error) = mirror_now(&ctx, &config, None).await {
                        tracing::warn!(account_id = config.id, %error, "rules on the server failed");
                    }
                });
            }
        });
    }
}

#[cfg(test)]
#[path = "server_rules_tests.rs"]
mod tests;
