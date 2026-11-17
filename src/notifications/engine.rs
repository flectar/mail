//! The mail store for what Android asks of the app without its window: a
//! check for new mail, or a button on a notification after the app's process
//! had ended. While the window runs, its store serves both. Otherwise one
//! opens for the errand and closes once nothing holds it. Only one store is
//! ever open, as two would sync the same accounts.

use super::{NotificationHost, Response, Window, Words, parse_payload, words::Speech};
use flectar_mail_core::{
    Core, accounts::credentials::CredentialStoreHandle, config::Paths,
    oauth::redirect::OAuthRedirectBrokerHandle,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc},
    time::Duration,
};

/// What opens the mail store without the window.
pub struct EngineServices {
    pub paths: Paths,
    pub credentials: CredentialStoreHandle,
    pub oauth_redirects: OAuthRedirectBrokerHandle,
    pub host: Arc<dyn NotificationHost>,
}

enum State {
    /// No store is open.
    Closed,
    /// The window runs, with the store it opened.
    Window,
    /// A store opened for errands, held this many times.
    Open {
        runtime: tokio::runtime::Runtime,
        held: usize,
        generation: u64,
    },
}

static STATE: Mutex<State> = Mutex::new(State::Closed);
/// Tells the stores opened for errands apart, so a late release or timer
/// of one never touches the next.
static GENERATION: Mutex<u64> = Mutex::new(0);
/// The checks in progress, by the job that runs each.
static CHECKS: Mutex<BTreeMap<i32, super::BackgroundCheck>> = Mutex::new(BTreeMap::new());

/// How long a store opened for errands stays open after the last one ends,
/// for errands that come in bursts, such as several buttons in a row.
const LINGER: Duration = Duration::from_secs(5);
/// How long the window waits for such a store to close.
const HAND_OVER: Duration = Duration::from_secs(3);
/// How long a button's receiver waits for its action to be saved, within
/// the time Android gives a receiver.
const SAVE_LIMIT: Duration = Duration::from_secs(8);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps the store an errand uses open while it lives. The window's store
/// needs no holding.
pub(crate) struct Hold(Option<u64>);

impl Drop for Hold {
    fn drop(&mut self) {
        let Some(generation) = self.0 else {
            return;
        };
        let mut state = state();
        if let State::Open {
            held,
            generation: current,
            ..
        } = &mut *state
            && *current == generation
        {
            *held -= 1;
            if *held == 0 {
                drop(state);
                // Not on a runtime thread: closing waits for the runtime.
                std::thread::spawn(move || {
                    std::thread::sleep(LINGER);
                    close_unheld(generation);
                });
            }
        }
    }
}

/// Hold whichever store is open.
pub(crate) fn hold() -> Hold {
    match &mut *state() {
        State::Open {
            held, generation, ..
        } => {
            *held += 1;
            Hold(Some(*generation))
        }
        State::Closed | State::Window => Hold(None),
    }
}

/// Open a store for errands unless one is open, and hold it.
fn open(services: impl FnOnce() -> Result<EngineServices, String>) -> Result<Hold, String> {
    let mut state = state();
    match &mut *state {
        State::Window => return Ok(Hold(None)),
        State::Open {
            held, generation, ..
        } => {
            *held += 1;
            return Ok(Hold(Some(*generation)));
        }
        State::Closed => {}
    }
    let EngineServices {
        paths,
        credentials,
        oauth_redirects,
        host,
    } = services()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let handle = runtime.handle().clone();
    runtime.spawn(async move {
        let core =
            match Core::start_mail_ui_with_platform(paths, credentials, oauth_redirects).await {
                Ok(core) => core,
                Err(error) => {
                    tracing::warn!(%error, "notifications: could not open the mail store");
                    super::drop_waiting_errands();
                    return;
                }
            };
        let speech = Speech::new(language(&core).await);
        // Names the channels, in a process the window never ran in.
        host.set_words(&speech.fixed());
        // Runs the errands that waited for the store.
        super::dispatch_through(&handle, Arc::new(core), Some(host), Headless(speech));
    });
    let generation = {
        let mut generation = GENERATION.lock().unwrap_or_else(PoisonError::into_inner);
        *generation += 1;
        *generation
    };
    *state = State::Open {
        runtime,
        held: 1,
        generation,
    };
    Ok(Hold(Some(generation)))
}

/// The UI's language, as the window chooses it.
async fn language(core: &Core) -> &'static str {
    let preference = core
        .get_settings()
        .await
        .map(|settings| settings.language)
        .unwrap_or_default();
    match crate::normalized_language_preference(&preference) {
        "system" => sys_locale::get_locale()
            .as_deref()
            .and_then(crate::bundled_language_for_locale)
            .unwrap_or("en"),
        language => language,
    }
}

/// Close the store opened for errands once nothing holds it.
fn close_unheld(generation: u64) {
    let mut state = state();
    let unheld = matches!(
        &*state,
        State::Open { held: 0, generation: current, .. } if *current == generation
    );
    if !unheld {
        return;
    }
    let State::Open { runtime, .. } = std::mem::replace(&mut *state, State::Closed) else {
        return;
    };
    super::core_closed();
    drop(state);
    runtime.shutdown_timeout(HAND_OVER);
}

/// The window is about to open its own store: a store opened for errands
/// closes first, and the checks in it stop to run again later. Errands from
/// now on use the window's store.
pub fn window_starting() {
    let previous = std::mem::replace(&mut *state(), State::Window);
    if let State::Open { runtime, .. } = previous {
        for check in CHECKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
        {
            check.cancel();
        }
        super::core_closed();
        runtime.shutdown_timeout(HAND_OVER);
    }
}

/// The window closed its store.
pub fn window_ended() {
    let mut state = state();
    if matches!(*state, State::Window) {
        *state = State::Closed;
    }
}

/// Check every account for new mail and tell of it, as a job Android runs
/// does: with the window's store, or one opened for the check. Returns once
/// done, with whether every account answered: `false` when no store could
/// open or the job was stopped.
pub fn check_for_mail(services: impl FnOnce() -> Result<EngineServices, String>, job: i32) -> bool {
    let hold = match open(services) {
        Ok(hold) => hold,
        Err(error) => {
            tracing::warn!(%error, "notifications: no mail store to check");
            return false;
        }
    };
    let (sender, done) = mpsc::channel();
    let check = super::check_in_background(move |every| {
        let _ = sender.send(every);
    });
    CHECKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(job, check);
    // A store that closes under the check drops its sender.
    let every = done.recv().unwrap_or(false);
    CHECKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&job);
    drop(hold);
    every
}

/// Android wants the job's time back.
pub fn stop_check(job: i32) {
    if let Some(check) = CHECKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&job)
    {
        check.cancel();
    }
}

/// Carry out a button on a notification, as its receiver asks: returns once
/// the action is saved, or after [`SAVE_LIMIT`]. The store stays open while
/// the action goes to the server.
pub fn respond_in_background(
    services: impl FnOnce() -> Result<EngineServices, String>,
    request: &str,
) {
    let Some((route, act)) = parse_payload(request) else {
        return;
    };
    let hold = match open(services) {
        Ok(hold) => hold,
        Err(error) => {
            tracing::warn!(%error, "notifications: no mail store for the button");
            return;
        }
    };
    let (sender, saved) = mpsc::channel();
    super::respond_then(
        Response {
            route,
            act,
            activation_token: None,
        },
        move || {
            let _ = sender.send(());
        },
    );
    let _ = saved.recv_timeout(SAVE_LIMIT);
    drop(hold);
}

/// What the dispatcher asks of the window, without one: nothing is on
/// screen, and the words come from the catalogs.
struct Headless(Speech);

impl Window for Headless {
    async fn on_screen(&self, _accounts: BTreeSet<i64>) -> Option<BTreeSet<i64>> {
        Some(BTreeSet::new())
    }

    async fn words(&self, summaries: Vec<(u32, Vec<String>)>) -> Option<Words> {
        let summaries = summaries
            .into_iter()
            .map(|(count, senders)| {
                (
                    self.0
                        .new_messages(i32::try_from(count).unwrap_or(i32::MAX)),
                    super::summary_body(&senders, |named, more| self.0.more_senders(&named, more)),
                )
            })
            .collect();
        Some(Words {
            fixed: self.0.fixed(),
            summaries,
        })
    }

    async fn badge_description(&self, count: u32) -> Option<String> {
        Some(
            self.0
                .unread_count(i32::try_from(count).unwrap_or(i32::MAX)),
        )
    }
}
