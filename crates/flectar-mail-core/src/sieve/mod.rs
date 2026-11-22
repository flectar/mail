//! Automatic replies (#239) and rules (#238) for accounts whose server runs
//! Sieve scripts, managed over ManageSieve (RFC 5804) or JMAP (RFC 9661).
//! The app keeps them in a script of its own and never changes the user's
//! scripts. A server runs one script at a time. When the user's own one
//! runs, the app's runs only with the user's go-ahead, and then runs theirs
//! after it with `include` (RFC 6609), so their filters keep working. With
//! no replies or rules left, the app's script steps aside, and the server
//! runs what it ran before.

pub(crate) mod managesieve;
pub(crate) mod rules;
pub(crate) mod script;

use crate::accounts::credentials::{self, Slot};
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::models::{
    AccountConfig, AutoReply, AutoReplyQuestion, AutoReplySaved, ConnectionSecurity,
    ServerRulesState, now_ms,
};
use crate::sync::engine::SyncCtx;
use managesieve::{Error, Script, Server, Session};
use rules::Section;
use std::collections::HashSet;
use std::time::Duration;

/// How long after an account starts syncing its replies are first read, so
/// the mail comes first.
const FIRST_READ_AFTER: Duration = Duration::from_secs(15);
/// How often replies are read while the server offers them, as often as
/// the folder list.
const READ_EVERY: Duration = Duration::from_secs(30 * 60);
/// How long a server without them waits to be asked again.
const ASK_AGAIN_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// At most this many of the user's scripts are read to learn what a script
/// runs.
const MOST_SCRIPTS: usize = 20;

/// The account's ManageSieve server: its IMAP host on the ManageSieve port,
/// where webmail and other apps look for it too. The password goes over
/// STARTTLS, unless the account's mail travels unencrypted as well.
pub(crate) fn server(config: &AccountConfig) -> Server<'_> {
    let connection = &config.settings.connection;
    Server {
        owner: config.proxy_owner(),
        host: &config.incoming_host,
        port: managesieve::PORT,
        security: match connection.incoming_security {
            ConnectionSecurity::None => ConnectionSecurity::None,
            ConnectionSecurity::Tls | ConnectionSecurity::Starttls => ConnectionSecurity::Starttls,
        },
        trusted_certificate_pem: &connection.trusted_certificate_pem,
        timeout: connection.server_timeout(),
    }
}

/// Where the user's Sieve scripts are kept: a ManageSieve session, or a JMAP
/// account (RFC 9661).
pub(crate) trait Scripts {
    /// Whether scripts there may use Sieve extension `name`.
    fn has_extension(&self, name: &str) -> bool;
    /// The Sieve implementation's name and version, if the server gave one.
    fn implementation(&self) -> &str;
    /// The scripts, and which one the server runs.
    async fn list(&mut self) -> Result<Vec<Script>>;
    /// The script named `name`, if there is one, once the scripts were
    /// listed.
    async fn get(&mut self, name: &str) -> Result<Option<String>>;
    /// Stores `text` as script `name`, in place of one of that name. A
    /// refused script leaves the one before.
    async fn put(&mut self, name: &str, text: &str) -> Result<()>;
    /// Makes `name` the script the server runs, or stops every script.
    async fn activate(&mut self, name: Option<&str>) -> Result<()>;
}

impl Scripts for Session {
    fn has_extension(&self, name: &str) -> bool {
        self.capabilities().has_extension(name)
    }

    fn implementation(&self) -> &str {
        &self.capabilities().implementation
    }

    async fn list(&mut self) -> Result<Vec<Script>> {
        self.list_scripts().await.map_err(core_error)
    }

    async fn get(&mut self, name: &str) -> Result<Option<String>> {
        match self.get_script(name).await {
            Ok(text) => Ok(Some(text)),
            Err(Error::Refused { .. }) => Ok(None),
            Err(error) => Err(core_error(error)),
        }
    }

    async fn put(&mut self, name: &str, text: &str) -> Result<()> {
        if let Some(warnings) = self.put_script(name, text).await.map_err(refused_script)? {
            tracing::info!(%warnings, "the server warned about a Sieve script");
        }
        Ok(())
    }

    async fn activate(&mut self, name: Option<&str>) -> Result<()> {
        // An empty name stops every script (RFC 5804 section 2.8).
        self.set_active(name.unwrap_or_default())
            .await
            .map_err(core_error)
    }
}

/// The automatic replies the app's script among `scripts` holds, off unless
/// the server runs it.
pub(crate) async fn read_from(scripts: &mut impl Scripts) -> Result<AutoReply> {
    let state = State::read(scripts).await?;
    let runs_ours = state.runs_ours();
    let mut reply = state.ours.map(|ours| ours.reply).unwrap_or_default();
    reply.enabled &= runs_ours;
    Ok(reply)
}

/// Saves `reply` in the app's script, sent for mail to any of `addresses`.
/// When saving would change which script the server runs, or replace
/// changes made elsewhere, nothing changes unless the user `allowed` it.
pub(crate) async fn save(
    server: &Server<'_>,
    user: &str,
    password: &str,
    reply: &AutoReply,
    addresses: &[String],
    allowed: Option<&AutoReplyQuestion>,
) -> Result<AutoReplySaved> {
    let Some(mut session) = open(server, user, password, &["vacation"]).await? else {
        return Err(CoreError::Other(
            "This account's server can't send automatic replies".into(),
        ));
    };
    let saved = save_to(&mut session, reply, addresses, allowed).await;
    let _ = session.logout().await;
    saved
}

/// Saves `reply` in the app's script among `scripts`, as [`save`] does,
/// keeping the rules there.
pub(crate) async fn save_to(
    scripts: &mut impl Scripts,
    reply: &AutoReply,
    addresses: &[String],
    allowed: Option<&AutoReplyQuestion>,
) -> Result<AutoReplySaved> {
    let state = State::read(scripts).await?;
    let rules = state.ours.as_ref().and_then(|ours| ours.rules.clone());
    let allowed = allowed.map(|question| match question {
        AutoReplyQuestion::RunBefore(theirs) => Question::RunBefore(theirs.clone()),
        AutoReplyQuestion::ReplaceChanges => Question::ReplaceChanges,
    });
    let refused = match write(
        scripts,
        state,
        reply,
        addresses,
        rules.as_ref(),
        allowed.as_ref(),
    )
    .await?
    {
        Written::Done => {
            return Ok(AutoReplySaved::Done(AutoReply {
                contacts_only: false,
                domain_only: false,
                ..reply.clone()
            }));
        }
        Written::Ask(Question::RunBefore(theirs)) => {
            return Ok(AutoReplySaved::Ask(AutoReplyQuestion::RunBefore(theirs)));
        }
        Written::Ask(Question::ReplaceChanges) => {
            return Ok(AutoReplySaved::Ask(AutoReplyQuestion::ReplaceChanges));
        }
        Written::Refused(refused) => refused,
    };
    Err(CoreError::Other(match refused {
        Refused::TheirsReply(theirs) => format!(
            "Your filter script \u{201c}{theirs}\u{201d} on the server sends automatic \
             replies of its own. Change them where you set them up, such as your webmail"
        ),
        Refused::NoInclude(theirs) => format!(
            "Your server runs your filter script \u{201c}{theirs}\u{201d} and can't \
             run automatic replies beside it"
        ),
        Refused::Missing("include") => {
            "Your server can't run automatic replies beside your filter script".into()
        }
        Refused::Missing(_) => {
            "Your server can't limit automatic replies to certain days. Leave the days empty".into()
        }
    }))
}

/// What putting rules in the app's script came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mirrored {
    /// The server runs them, or has no rules to drop.
    Done,
    /// The app's script holds other rules, which another device wrote.
    Others,
    /// Nothing changed: the user must allow this first.
    Ask(Question),
    Refused(Refused),
}

/// Puts `rules` in the app's script among `scripts`, keeping its replies,
/// which go out for mail to any of `addresses`. Rules found there that this
/// device didn't write, as their digest isn't `written`, stay unless
/// `replace` says otherwise.
pub(crate) async fn put_rules(
    scripts: &mut impl Scripts,
    rules: Option<&Section>,
    written: Option<&str>,
    addresses: &[String],
    replace: bool,
    allowed: Option<&Question>,
) -> Result<Mirrored> {
    let state = State::read(scripts).await?;
    let runs_ours = state.runs_ours();
    let found = state
        .ours
        .as_ref()
        .and_then(|ours| ours.rules.as_ref())
        .map(|found| found.digest.clone());
    let wanted = rules.map(|rules| rules.digest.clone());
    if found == wanted && (wanted.is_none() || runs_ours) {
        return Ok(Mirrored::Done);
    }
    // Rules already as this device wants them are its own, also when it
    // wrote them without getting to run them.
    if found.is_some() && found.as_deref() != written && found != wanted && !replace {
        return Ok(Mirrored::Others);
    }
    // The replies stay as the app shows them, off unless the server ran
    // them, so a reply turned off elsewhere doesn't start again.
    let mut reply = state
        .ours
        .as_ref()
        .map(|ours| ours.reply.clone())
        .unwrap_or_default();
    reply.enabled &= runs_ours;
    Ok(
        match write(scripts, state, &reply, addresses, rules, allowed).await? {
            Written::Done => Mirrored::Done,
            Written::Ask(question) => Mirrored::Ask(question),
            Written::Refused(refused) => Mirrored::Refused(refused),
        },
    )
}

/// What writing the app's script would change on the server, beyond the
/// script, which the user must allow first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Question {
    /// The server runs the user's own filter script, named here. The app's
    /// would run first, then that script, unchanged.
    RunBefore(String),
    /// The app's script was changed elsewhere since the app wrote it.
    ReplaceChanges,
}

/// Why the app's script can't be written as wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The user's script, named here, sends automatic replies of its own,
    /// and two would fail the whole run (RFC 5230 section 4.7).
    TheirsReply(String),
    /// The server runs the user's script, named here, and can't run the
    /// app's before it.
    NoInclude(String),
    /// The server lacks an extension the replies need.
    Missing(&'static str),
}

enum Written {
    Done,
    Ask(Question),
    Refused(Refused),
}

/// Writes the app's script among `scripts`, which `state` describes, with
/// `reply`, sent for mail to any of `addresses`, and `rules`. When writing
/// would change which script the server runs, or replace changes made
/// elsewhere, nothing changes unless the user `allowed` it.
async fn write(
    scripts: &mut impl Scripts,
    state: State,
    reply: &AutoReply,
    addresses: &[String],
    rules: Option<&Section>,
    allowed: Option<&Question>,
) -> Result<Written> {
    let runs_ours = state.runs_ours();
    let ours_active = state.active.as_deref() == Some(script::NAME);
    let ours = state.ours.unwrap_or_default();
    let wanted = reply.enabled || rules.is_some();
    // The user's script the app's runs after its own, and the script the
    // server runs once written, when that changes.
    let (mut beside, then) = if !wanted {
        // With nothing left to run, the app's script steps aside for what
        // ran before it, if that's still there.
        let before = ours
            .beside
            .clone()
            .filter(|theirs| state.scripts.contains(theirs));
        (None, ours_active.then_some(Activate(before)))
    } else if runs_ours {
        // Their script may run the app's already.
        (ours.beside.clone().filter(|_| ours_active), None)
    } else {
        (
            state.active.clone(),
            Some(Activate(Some(script::NAME.into()))),
        )
    };
    let activate = then.is_some() && wanted;
    if wanted && let Some(theirs) = beside.clone() {
        let runs = match state.theirs {
            Some(runs) if activate => runs,
            _ => Runs::read(scripts, &theirs).await?,
        };
        if reply.enabled && runs.replies {
            return Ok(Written::Refused(Refused::TheirsReply(theirs)));
        }
        if activate {
            if !scripts.has_extension("include") {
                return Ok(Written::Refused(Refused::NoInclude(theirs)));
            }
            let question = Question::RunBefore(theirs);
            if allowed != Some(&question) {
                return Ok(Written::Ask(question));
            }
        } else if !runs.found {
            // They deleted it.
            beside = None;
        }
    }
    if runs_ours && ours.edited && allowed != Some(&Question::ReplaceChanges) {
        return Ok(Written::Ask(Question::ReplaceChanges));
    }
    if let Some(missing) = script::needs(reply, beside.as_deref())
        .into_iter()
        .find(|extension| !scripts.has_extension(extension))
    {
        return Ok(Written::Refused(Refused::Missing(missing)));
    }
    let text = script::generate(reply, addresses, rules, beside.as_deref());
    scripts.put(script::NAME, &text).await?;
    if let Some(Activate(name)) = then {
        scripts.activate(name.as_deref()).await?;
    }
    Ok(Written::Done)
}

/// The extensions that let the app's script send automatic replies or sort
/// mail.
pub(crate) const USEFUL: [&str; 3] = ["vacation", "fileinto", "imap4flags"];

/// Opens a session and signs in, or None when the server has no ManageSieve
/// that offers any of the extensions the app `wants`.
pub(crate) async fn open(
    server: &Server<'_>,
    user: &str,
    password: &str,
    wants: &[&str],
) -> Result<Option<Session>> {
    let mut session = match Session::open(server).await {
        Ok(session) => session,
        Err(error) if absent(&error) => return Ok(None),
        Err(error) => return Err(core_error(error)),
    };
    tracing::debug!(
        implementation = %session.capabilities().implementation,
        "ManageSieve server found"
    );
    // Asking first keeps the password away from servers that can't use it.
    if !wants
        .iter()
        .any(|extension| session.capabilities().has_extension(extension))
    {
        let _ = session.logout().await;
        return Ok(None);
    }
    session.sign_in(user, password).await.map_err(core_error)?;
    Ok(Some(session))
}

/// Whether `error` means that nothing on the port speaks ManageSieve.
fn absent(error: &Error) -> bool {
    match error {
        Error::Io(error) => matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::UnexpectedEof
        ),
        Error::Timeout(_) | Error::Protocol(_) => true,
        _ => false,
    }
}

fn core_error(error: Error) -> CoreError {
    match error {
        Error::Io(_) | Error::Timeout(_) => CoreError::Network(error.to_string()),
        Error::Tls(text) => CoreError::Tls(text),
        Error::Refused {
            command: "AUTHENTICATE",
            text,
            ..
        } => CoreError::Auth(if text.is_empty() {
            "the server refused the password".into()
        } else {
            text
        }),
        Error::NoSignIn => CoreError::Auth(error.to_string()),
        error => CoreError::Other(error.to_string()),
    }
}

/// The server's word on the app's script.
fn refused_script(error: Error) -> CoreError {
    match error {
        Error::Refused { code, text, .. } => {
            let quota = code.is_some_and(|code| code.to_ascii_uppercase().starts_with("QUOTA"));
            CoreError::Other(if quota {
                "Your server has no room for another filter script".into()
            } else {
                format!("Your server refused Flectar Mail's script. It said \u{201c}{text}\u{201d}")
            })
        }
        error => core_error(error),
    }
}

/// The script to make the one the server runs, or none to stop every
/// script.
struct Activate(Option<String>);

/// What the server runs.
struct State {
    /// The user's scripts, by name.
    scripts: Vec<String>,
    active: Option<String>,
    /// The app's script, if there.
    ours: Option<script::Found>,
    /// What the active script runs, unless it's the app's.
    theirs: Option<Runs>,
}

impl State {
    async fn read(scripts: &mut impl Scripts) -> Result<Self> {
        let listed = scripts.list().await?;
        let active = listed
            .iter()
            .find(|found| found.active)
            .map(|found| found.name.clone());
        let ours = if listed.iter().any(|found| found.name == script::NAME) {
            scripts
                .get(script::NAME)
                .await?
                .map(|text| script::read(&text))
        } else {
            None
        };
        let theirs = match active.as_deref() {
            None | Some(script::NAME) => None,
            Some(theirs) => Some(Runs::read(scripts, theirs).await?),
        };
        Ok(Self {
            scripts: listed.into_iter().map(|found| found.name).collect(),
            active,
            ours,
            theirs,
        })
    }

    /// Whether the server runs the app's script: it's the active script, or
    /// the active one runs it.
    fn runs_ours(&self) -> bool {
        self.active.as_deref() == Some(script::NAME)
            || self.theirs.as_ref().is_some_and(|theirs| theirs.ours)
    }
}

/// What the user's script runs, with the scripts it includes.
#[derive(Debug, Default, PartialEq, Eq)]
struct Runs {
    /// The script exists.
    found: bool,
    /// It runs the app's script.
    ours: bool,
    /// It sends automatic replies.
    replies: bool,
}

impl Runs {
    async fn read(scripts: &mut impl Scripts, name: &str) -> Result<Self> {
        let mut runs = Self::default();
        let mut seen = HashSet::new();
        let mut waiting = vec![name.to_owned()];
        while let Some(name) = waiting.pop() {
            if !seen.insert(name.clone()) || seen.len() > MOST_SCRIPTS {
                continue;
            }
            if name == script::NAME {
                runs.ours = true;
                continue;
            }
            // Scripts included as optional may not exist.
            let Some(text) = scripts.get(&name).await? else {
                continue;
            };
            runs.found |= seen.len() == 1;
            runs.replies |= script::requires(&text)
                .iter()
                .any(|extension| extension.starts_with("vacation"));
            waiting.extend(script::includes(&text));
        }
        Ok(runs)
    }
}

/// Reads an IMAP account's automatic replies back from its server, and
/// puts its rules there, while the account syncs: soon after it starts,
/// then every half hour while the server runs Sieve scripts, so a change
/// made on another device shows. A server without them is asked again a day
/// later. A refused password ends the reading until the account starts
/// again, as repeated failed sign-ins can get the user's address blocked.
pub(crate) async fn watch(ctx: SyncCtx, config: AccountConfig) {
    tokio::time::sleep(FIRST_READ_AFTER).await;
    loop {
        let wait = match read_back(&ctx, &config).await {
            Ok(true) => READ_EVERY,
            Ok(false) => ASK_AGAIN_AFTER,
            Err(error @ CoreError::Auth(_)) => {
                tracing::warn!(account_id = config.id, %error, "Sieve: sign-in refused");
                return;
            }
            Err(error) => {
                tracing::warn!(account_id = config.id, %error, "Sieve failed");
                READ_EVERY
            }
        };
        tokio::time::sleep(wait).await;
    }
}

/// Reads the account's automatic replies from its server and keeps them,
/// then puts the account's rules there. Whether the server runs Sieve
/// scripts.
pub(crate) async fn read_back(ctx: &SyncCtx, config: &AccountConfig) -> Result<bool> {
    let account_id = config.id;
    let password =
        credentials::load_async(ctx.credentials.clone(), account_id, Slot::Password).await?;
    let saved_at = ctx
        .db
        .read(move |conn| repo::auto_replies::saved_at(conn, account_id))
        .await?;
    let writer = crate::server_rules::writer(account_id);
    let _writing = writer.lock().await;
    let Some(mut session) = open(&server(config), &config.username, &password, &USEFUL).await?
    else {
        crate::server_rules::store_state(ctx, account_id, ServerRulesState::Unavailable).await?;
        return Ok(false);
    };
    let reply = if session.has_extension("vacation") {
        Some(read_from(&mut session).await)
    } else {
        None
    };
    let rules = crate::server_rules::mirror(ctx, account_id, &mut session, None).await;
    let _ = session.logout().await;
    if let Err(error) = rules {
        tracing::warn!(account_id, %error, "rules on the server failed");
        let failed = ServerRulesState::Failed(error.to_string());
        crate::server_rules::store_state(ctx, account_id, failed).await?;
    }
    if let Some(reply) = reply {
        let reply = reply?;
        let changed = ctx
            .db
            .write(move |conn| {
                let read = repo::auto_replies::Origin::Read { saved_at };
                repo::auto_replies::store(conn, account_id, &reply, now_ms(), read)
            })
            .await?;
        if changed {
            ctx.bus.emit(CoreEvent::AutoReplyChanged { account_id });
        }
    }
    Ok(true)
}

#[cfg(test)]
pub(crate) mod test_server;
#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
