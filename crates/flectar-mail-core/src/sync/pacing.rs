//! When sync asks a server again (#251): after a failure, after the server
//! asked to wait, between checks for new mail, and when many accounts start
//! at once. Also how many connections an IMAP account opens for work in the
//! background.
//!
//! A failure such as a dropped connection is tried again soon, sooner than
//! most clients do, since it is usually the network. A server that asked to
//! wait gets at least the wait it named, and without one a wait that starts
//! at a minute and grows to an hour: Gmail's suspensions last from an hour to
//! a day, and asking again during one can make it longer. Every wait varies a
//! little, so accounts that failed together don't come back together.

use crate::error::{CoreError, LimitKind};
use crate::models::{AccountSettings, CheckInterval, now_ms};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

/// The first wait after a failure that isn't the server asking to wait.
const FAILURE_FIRST: Duration = Duration::from_secs(1);
/// The longest wait after such failures.
const FAILURE_MOST: Duration = Duration::from_secs(5 * 60);
/// The first wait when a server asked to wait without saying how long.
const LIMIT_FIRST: Duration = Duration::from_secs(60);
/// The longest wait for a server that keeps asking to wait.
const LIMIT_MOST: Duration = Duration::from_secs(60 * 60);
/// The longest wait a server can name: Gmail's longest suspension.
pub(crate) const NAMED_MOST: Duration = Duration::from_secs(24 * 60 * 60);
/// How much a wait varies either way.
const SPREAD: f64 = 0.25;
/// What a named wait may grow by, at most.
const NAMED_EXTRA_MOST: Duration = Duration::from_secs(30);

/// Connections an IMAP account opens for work in the background: two
/// download message bodies and one downloads older headers. The account's
/// own connection and the one that opens messages come on top.
const BULK_CONNECTIONS: usize = 3;
/// Accounts that download their older headers at the same time.
const HISTORY_TURNS: usize = 3;
static HISTORY_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// Accounts that start together connect at once up to this many; the rest
/// take turns.
const FREE_STARTS: usize = 3;
/// An account that starts within this of the one before starts with it.
const START_TOGETHER: Duration = Duration::from_secs(5);

/// How long to wait before asking a server again, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Wait {
    pub duration: Duration,
    /// The server named the wait. A check someone asks for doesn't cut it
    /// short: asking again early would only count against the account.
    pub named: bool,
    /// The server asked to wait, named or not.
    pub limited: bool,
}

/// One account's backoff. Failures and requests to wait each grow their own
/// wait, and a success resets both.
#[derive(Debug, Clone, Default)]
pub(crate) struct Backoff {
    failures: u32,
    limits: u32,
}

impl Backoff {
    /// The account reached its server: the next failure starts over.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The wait after `error`.
    pub fn after(&mut self, error: &CoreError) -> Wait {
        self.after_with(error, rand::random::<f64>())
    }

    /// As [`Backoff::after`], with `random` in `[0, 1)` choosing the spread.
    pub(crate) fn after_with(&mut self, error: &CoreError, random: f64) -> Wait {
        match error {
            CoreError::RateLimited {
                retry_after: Some(named),
                ..
            } => {
                // A server that keeps naming short waits, or none at all,
                // still waits longer each time.
                let least = doubled(FAILURE_FIRST, FAILURE_MOST, self.limits);
                self.limits = self.limits.saturating_add(1);
                Wait {
                    duration: named_wait((*named).max(least), random),
                    named: true,
                    limited: true,
                }
            }
            CoreError::RateLimited { kind, .. } => {
                // Another session that holds the mailbox, or a sign-in too
                // soon after the last one, is over in minutes.
                let most = match kind {
                    LimitKind::Busy | LimitKind::LoginDelay => FAILURE_MOST,
                    LimitKind::Requests | LimitKind::Connections => LIMIT_MOST,
                };
                let wait = doubled(LIMIT_FIRST, most, self.limits);
                self.limits = self.limits.saturating_add(1);
                Wait {
                    duration: spread(wait, random),
                    named: false,
                    limited: true,
                }
            }
            _ => {
                let wait = doubled(FAILURE_FIRST, FAILURE_MOST, self.failures);
                self.failures = self.failures.saturating_add(1);
                Wait {
                    duration: spread(wait, random),
                    named: false,
                    limited: false,
                }
            }
        }
    }
}

/// `first`, doubled `times` times, at most `most`.
fn doubled(first: Duration, most: Duration, times: u32) -> Duration {
    first
        .checked_mul(1u32.checked_shl(times.min(31)).unwrap_or(u32::MAX))
        .map_or(most, |wait| wait.min(most))
}

/// `wait`, longer or shorter by up to a quarter.
fn spread(wait: Duration, random: f64) -> Duration {
    wait.mul_f64(1.0 - SPREAD + 2.0 * SPREAD * random.clamp(0.0, 1.0))
}

/// The wait a server named, never shorter, and longer by up to a tenth (at
/// most 30 s), so accounts it stopped together don't return together.
fn named_wait(named: Duration, random: f64) -> Duration {
    let named = named.min(NAMED_MOST);
    let extra = named.mul_f64(0.1).min(NAMED_EXTRA_MOST);
    named + extra.mul_f64(random.clamp(0.0, 1.0))
}

/// The time to the next check, varied by up to 5% either way and by at
/// least 1 s and at most 30 s, as Thunderbird varies its check timers, so
/// accounts on one server don't check together.
pub(crate) fn varied(interval: Duration) -> Duration {
    varied_with(interval, rand::random::<f64>())
}

pub(crate) fn varied_with(interval: Duration, random: f64) -> Duration {
    let spread = interval
        .mul_f64(0.05)
        .clamp(Duration::from_secs(1), Duration::from_secs(30))
        .min(interval / 2);
    let random = random.clamp(0.0, 1.0);
    (interval - spread) + spread.mul_f64(2.0 * random)
}

/// How long the account `index` places down a start waits before its first
/// connection, so a start with many accounts doesn't open every connection
/// at once. The first few go at once, the rest 2 s apart, for at most half a
/// minute.
pub(crate) fn start_delay_with(index: usize, random: f64) -> Duration {
    const STEP: Duration = Duration::from_secs(2);
    const MOST: Duration = Duration::from_secs(30);
    let steps = index.saturating_sub(FREE_STARTS - 1);
    if steps == 0 {
        return Duration::ZERO;
    }
    let steps = u32::try_from(steps).unwrap_or(u32::MAX);
    STEP.saturating_mul(steps).min(MOST) + Duration::from_secs(1).mul_f64(random.clamp(0.0, 1.0))
}

/// How long an account waits between checks for new mail, or `None` when
/// it checks only when asked. `app` is the interval Settings chose for every
/// account.
pub(crate) fn check_interval(settings: &AccountSettings, app: Duration) -> Option<Duration> {
    match settings.check_interval {
        CheckInterval::App => Some(app),
        CheckInterval::Minutes(minutes) => {
            Some(Duration::from_secs(u64::from(minutes.clamp(1, 60)) * 60))
        }
        CheckInterval::Manual => None,
    }
}

/// Whether the server tells the account of new mail as it arrives (IMAP
/// IDLE, JMAP's event source). An account that checks only when asked
/// doesn't listen either.
pub(crate) fn push(settings: &AccountSettings) -> bool {
    settings.push && settings.check_interval != CheckInterval::Manual
}

/// A wait the server asked for, which every connection of the account keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pause {
    pub until: Instant,
    /// The same moment in milliseconds since the Unix epoch, for the app.
    pub until_ms: i64,
    /// The server named the wait, so a check someone asks for doesn't end it.
    pub named: bool,
    /// Who asked to wait, and what they said.
    pub reason: String,
}

impl Pause {
    pub(crate) fn after(wait: Wait, reason: String) -> Self {
        let millis = i64::try_from(wait.duration.as_millis()).unwrap_or(i64::MAX);
        Self {
            until: Instant::now() + wait.duration,
            until_ms: now_ms().saturating_add(millis),
            named: wait.named,
            reason,
        }
    }
}

/// What sync keeps about each account's server while the app runs: when it
/// asked to be left alone until, and how many connections it takes.
#[derive(Default)]
pub struct Pace {
    accounts: Mutex<HashMap<i64, Arc<AccountPace>>>,
    /// When the last account started, and how many started with it.
    starts: Mutex<Option<(Instant, usize)>>,
}

impl std::fmt::Debug for Pace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pace").finish_non_exhaustive()
    }
}

impl Pace {
    pub(crate) fn account(&self, account_id: i64) -> Arc<AccountPace> {
        lock(&self.accounts)
            .entry(account_id)
            .or_insert_with(|| Arc::new(AccountPace::new()))
            .clone()
    }

    /// The account was removed, or now uses another server.
    pub(crate) fn forget(&self, account_id: i64) {
        lock(&self.accounts).remove(&account_id);
    }

    /// The account's worker starts: it waits its turn before it first
    /// connects when many accounts start together.
    pub(crate) fn starting(&self, account_id: i64) {
        let now = Instant::now();
        let index = {
            let mut starts = lock(&self.starts);
            let index = match *starts {
                Some((last, index)) if now.duration_since(last) < START_TOGETHER => index + 1,
                _ => 0,
            };
            *starts = Some((now, index));
            index
        };
        let delay = start_delay_with(index, rand::random::<f64>());
        lock(&self.account(account_id).state).start = (!delay.is_zero()).then(|| now + delay);
    }
}

/// One account's share of [`Pace`].
pub(crate) struct AccountPace {
    state: Mutex<PaceState>,
    bulk: Arc<Semaphore>,
}

struct PaceState {
    /// How waits grow while the server keeps asking to wait.
    limits: Backoff,
    pause: Option<Pause>,
    /// Connections allowed for background work, and permits to retire when
    /// they come back.
    bulk_allowed: usize,
    bulk_owed: usize,
    /// The account's first connection waits until then.
    start: Option<Instant>,
}

impl AccountPace {
    fn new() -> Self {
        Self {
            state: Mutex::new(PaceState {
                limits: Backoff::default(),
                pause: None,
                bulk_allowed: BULK_CONNECTIONS,
                bulk_owed: 0,
                start: None,
            }),
            bulk: Arc::new(Semaphore::new(BULK_CONNECTIONS)),
        }
    }

    /// The server asked to wait, with `error`: every connection of the
    /// account waits as long. Too many connections also leaves the account
    /// one fewer for work in the background, until the app starts again, as
    /// Evolution does.
    pub(crate) fn limited(&self, error: &CoreError) -> Pause {
        self.limited_with(error, rand::random::<f64>())
    }

    pub(crate) fn limited_with(&self, error: &CoreError, random: f64) -> Pause {
        let mut state = lock(&self.state);
        let mut pause = Pause::after(state.limits.after_with(error, random), error.to_string());
        if let Some(current) = state
            .pause
            .as_ref()
            .filter(|pause| pause.until > Instant::now())
            && current.until > pause.until
        {
            pause = Pause {
                named: current.named || pause.named,
                ..current.clone()
            };
        }
        state.pause = Some(pause.clone());
        if matches!(
            error,
            CoreError::RateLimited {
                kind: LimitKind::Connections,
                ..
            }
        ) {
            self.fewer_connections(&mut state);
        }
        pause
    }

    /// Sync reached the server: waits start over.
    pub(crate) fn reached(&self) {
        let mut state = lock(&self.state);
        state.limits.reset();
        state.pause = None;
    }

    /// The wait the server asked for, while it lasts.
    pub(crate) fn paused(&self) -> Option<Pause> {
        let mut state = lock(&self.state);
        if state
            .pause
            .as_ref()
            .is_some_and(|pause| pause.until <= Instant::now())
        {
            state.pause = None;
        }
        state.pause.clone()
    }

    /// Someone asked for a check: a wait sync chose ends, one the server
    /// named goes on. Returns the wait that goes on.
    pub(crate) fn asked(&self) -> Option<Pause> {
        let pause = self.paused()?;
        if pause.named {
            return Some(pause);
        }
        lock(&self.state).pause = None;
        None
    }

    /// How long the account's first connection still waits for its turn.
    /// Asking for a check ends the wait.
    pub(crate) fn take_start(&self) -> Option<Instant> {
        lock(&self.state)
            .start
            .take()
            .filter(|start| *start > Instant::now())
    }

    fn fewer_connections(&self, state: &mut PaceState) {
        if state.bulk_allowed <= 1 {
            return;
        }
        state.bulk_allowed -= 1;
        if self.bulk.forget_permits(1) == 0 {
            state.bulk_owed += 1;
        }
        tracing::info!(
            allowed = state.bulk_allowed,
            "the server refused a connection for having too many; opening fewer"
        );
    }

    /// Connections the account may open for background work right now.
    #[cfg(test)]
    pub(crate) fn bulk_allowed(&self) -> usize {
        lock(&self.state).bulk_allowed
    }

    /// A connection for work in the background, such as downloading message
    /// bodies. Waits while the account has as many open as it may.
    pub(crate) async fn bulk(self: &Arc<Self>) -> BulkPermit {
        let permit = self
            .bulk
            .clone()
            .acquire_owned()
            .await
            .expect("connection budgets are never closed");
        BulkPermit {
            permit: Some(permit),
            _history: None,
            pace: Arc::clone(self),
        }
    }

    /// A connection for downloading older headers: one of the turns all
    /// accounts share, so a start with many accounts doesn't download every
    /// account's history at once, and one of the account's own background
    /// connections.
    pub(crate) async fn history(self: &Arc<Self>) -> BulkPermit {
        let slot = HISTORY_SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(HISTORY_TURNS)))
            .clone()
            .acquire_owned()
            .await
            .expect("history turns are never closed");
        let mut permit = self.bulk().await;
        permit._history = Some(slot);
        permit
    }
}

/// A background connection's place in its account's budget, held while the
/// connection is open.
pub(crate) struct BulkPermit {
    permit: Option<OwnedSemaphorePermit>,
    _history: Option<OwnedSemaphorePermit>,
    pace: Arc<AccountPace>,
}

impl Drop for BulkPermit {
    fn drop(&mut self) {
        let mut state = lock(&self.pace.state);
        if state.bulk_owed > 0
            && let Some(permit) = self.permit.take()
        {
            state.bulk_owed -= 1;
            permit.forget();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limited(retry_after: Option<Duration>) -> CoreError {
        CoreError::RateLimited {
            service: "imap.example.com".into(),
            kind: LimitKind::Requests,
            retry_after,
            detail: "NO [LIMIT] Rate limit hit.".into(),
        }
    }

    fn too_many_connections() -> CoreError {
        CoreError::RateLimited {
            service: "imap.example.com".into(),
            kind: LimitKind::Connections,
            retry_after: None,
            detail: "NO [UNAVAILABLE] Maximum number of connections from user+IP exceeded".into(),
        }
    }

    #[test]
    fn failures_double_from_a_second_to_five_minutes() {
        let mut backoff = Backoff::default();
        let failure = CoreError::Network("connection reset".into());
        let waits: Vec<u64> = (0..12)
            .map(|_| backoff.after_with(&failure, 0.5).duration.as_secs())
            .collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300, 300]);
        backoff.reset();
        assert_eq!(backoff.after_with(&failure, 0.5).duration, FAILURE_FIRST);
    }

    #[test]
    fn waits_vary_by_a_quarter_either_way() {
        let failure = CoreError::Network("connection reset".into());
        let mut low = Backoff::default();
        let mut high = Backoff::default();
        for _ in 0..4 {
            low.after_with(&failure, 0.0);
            high.after_with(&failure, 0.999_999);
        }
        assert_eq!(
            low.after_with(&failure, 0.0).duration,
            Duration::from_secs(12)
        );
        assert!(high.after_with(&failure, 0.999_999).duration > Duration::from_millis(19_999));
    }

    #[test]
    fn unnamed_limits_start_at_a_minute_and_grow_to_an_hour() {
        let mut backoff = Backoff::default();
        let waits: Vec<u64> = (0..9)
            .map(|_| backoff.after_with(&limited(None), 0.5).duration.as_secs())
            .collect();
        assert_eq!(waits, [60, 120, 240, 480, 960, 1920, 3600, 3600, 3600]);
        let wait = backoff.after_with(&limited(None), 0.5);
        assert!(wait.limited && !wait.named);
    }

    #[test]
    fn a_mailbox_in_use_is_tried_again_at_least_every_five_minutes() {
        for kind in [LimitKind::Busy, LimitKind::LoginDelay] {
            let busy = CoreError::RateLimited {
                service: "pop.example.com".into(),
                kind,
                retry_after: None,
                detail: "-ERR [IN-USE] Mailbox locked".into(),
            };
            let mut backoff = Backoff::default();
            let waits: Vec<u64> = (0..5)
                .map(|_| backoff.after_with(&busy, 0.5).duration.as_secs())
                .collect();
            assert_eq!(waits, [60, 120, 240, 300, 300], "{kind:?}");
        }
    }

    #[test]
    fn a_named_wait_is_never_cut_short() {
        let mut backoff = Backoff::default();
        let wait = backoff.after_with(&limited(Some(Duration::from_secs(165))), 0.0);
        assert_eq!(wait.duration, Duration::from_secs(165));
        assert!(wait.named && wait.limited);
        // Longer by up to a tenth, and at most 30 s.
        assert_eq!(
            backoff
                .after_with(&limited(Some(Duration::from_secs(100))), 0.999_999)
                .duration
                .as_secs(),
            109
        );
        assert!(
            backoff
                .after_with(&limited(Some(Duration::from_secs(3600))), 0.999_999)
                .duration
                <= Duration::from_secs(3630)
        );
        // A day at most.
        assert_eq!(
            backoff
                .after_with(&limited(Some(Duration::from_secs(10 * 86_400))), 0.0)
                .duration,
            NAMED_MOST
        );
    }

    #[test]
    fn short_named_waits_still_grow() {
        // A server that answers every request with "Retry-After: 0".
        let mut backoff = Backoff::default();
        let waits: Vec<u64> = (0..10)
            .map(|_| {
                backoff
                    .after_with(&limited(Some(Duration::ZERO)), 0.0)
                    .duration
                    .as_secs()
            })
            .collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 64, 128, 256, 300]);
    }

    #[test]
    fn limits_and_failures_grow_apart() {
        let mut backoff = Backoff::default();
        backoff.after_with(&limited(None), 0.5);
        backoff.after_with(&limited(None), 0.5);
        let failure = CoreError::Network("connection reset".into());
        assert_eq!(backoff.after_with(&failure, 0.5).duration, FAILURE_FIRST);
        assert_eq!(
            backoff.after_with(&limited(None), 0.5).duration,
            Duration::from_secs(240)
        );
    }

    #[test]
    fn check_timers_vary_by_five_percent_within_a_second_and_half_a_minute() {
        let five = Duration::from_secs(300);
        assert_eq!(varied_with(five, 0.0), Duration::from_secs(285));
        assert_eq!(varied_with(five, 0.5), five);
        assert!(varied_with(five, 0.999_999) < Duration::from_secs(315));
        // At least a second either way, and at most 30 s.
        assert_eq!(
            varied_with(Duration::from_secs(10), 0.0),
            Duration::from_secs(9)
        );
        assert_eq!(
            varied_with(Duration::from_secs(3600), 0.0),
            Duration::from_secs(3570)
        );
    }

    #[test]
    fn many_accounts_start_two_seconds_apart_for_at_most_half_a_minute() {
        assert_eq!(start_delay_with(0, 0.9), Duration::ZERO);
        assert_eq!(start_delay_with(2, 0.9), Duration::ZERO);
        assert_eq!(start_delay_with(3, 0.0), Duration::from_secs(2));
        assert_eq!(start_delay_with(5, 0.5), Duration::from_millis(6_500));
        assert_eq!(start_delay_with(40, 0.0), Duration::from_secs(30));
    }

    #[test]
    fn accounts_check_at_their_own_interval_or_the_apps() {
        let app = Duration::from_secs(5 * 60);
        let mut settings = crate::db::testutil::account_settings();
        assert_eq!(check_interval(&settings, app), Some(app));
        assert!(push(&settings));
        settings.check_interval = CheckInterval::Minutes(30);
        assert_eq!(
            check_interval(&settings, app),
            Some(Duration::from_secs(1800))
        );
        settings.check_interval = CheckInterval::Minutes(0);
        assert_eq!(
            check_interval(&settings, app),
            Some(Duration::from_secs(60))
        );
        // Only when asked: no timer, and no push either.
        settings.check_interval = CheckInterval::Manual;
        assert_eq!(check_interval(&settings, app), None);
        assert!(!push(&settings));
        settings.check_interval = CheckInterval::App;
        settings.push = false;
        assert!(!push(&settings));
    }

    #[tokio::test(start_paused = true)]
    async fn every_connection_of_an_account_keeps_the_servers_wait() {
        let pace = Pace::default();
        let account = pace.account(7);
        assert_eq!(account.paused(), None);
        let pause = account.limited_with(&limited(Some(Duration::from_secs(120))), 0.0);
        assert!(pause.named);
        assert_eq!(
            pause.reason,
            "imap.example.com asked to wait: NO [LIMIT] Rate limit hit."
        );
        // Another connection of the same account sees it.
        assert_eq!(pace.account(7).paused(), Some(pause.clone()));
        assert_eq!(pace.account(8).paused(), None);
        // A check someone asks for doesn't end a wait the server named.
        assert_eq!(account.asked(), Some(pause.clone()));
        // A shorter wait asked for later doesn't end it early either.
        let later = account.limited_with(&limited(None), 0.0);
        assert_eq!(later.until, pause.until);
        assert!(later.named);
        tokio::time::advance(Duration::from_secs(121)).await;
        assert_eq!(account.paused(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_check_someone_asks_for_ends_a_wait_sync_chose() {
        let account = Pace::default().account(1);
        let pause = account.limited_with(&limited(None), 0.5);
        assert!(!pause.named);
        assert_eq!(pause.until, Instant::now() + Duration::from_secs(60));
        assert_eq!(account.asked(), None);
        assert_eq!(account.paused(), None);
        // The wait still grows the next time: only reaching the server
        // starts it over.
        let pause = account.limited_with(&limited(None), 0.5);
        assert_eq!(pause.until, Instant::now() + Duration::from_secs(120));
        account.reached();
        assert_eq!(account.paused(), None);
        let pause = account.limited_with(&limited(None), 0.5);
        assert_eq!(pause.until, Instant::now() + Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn too_many_connections_leaves_one_fewer_down_to_one() {
        let account = Pace::default().account(1);
        assert_eq!(account.bulk_allowed(), BULK_CONNECTIONS);
        // All three in use: the one to retire goes when it comes back.
        let permits = vec![
            account.bulk().await,
            account.bulk().await,
            account.bulk().await,
        ];
        account.limited_with(&too_many_connections(), 0.0);
        assert_eq!(account.bulk_allowed(), 2);
        drop(permits);
        let _first = account.bulk().await;
        let _second = account.bulk().await;
        assert!(
            tokio::time::timeout(Duration::from_secs(1), account.bulk())
                .await
                .is_err()
        );
        drop((_first, _second));
        account.limited_with(&too_many_connections(), 0.0);
        account.limited_with(&too_many_connections(), 0.0);
        assert_eq!(account.bulk_allowed(), 1);
        let _only = account.bulk().await;
        assert!(
            tokio::time::timeout(Duration::from_secs(1), account.bulk())
                .await
                .is_err()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_accounts_of_a_start_go_at_once_and_the_rest_take_turns() {
        let pace = Pace::default();
        for id in 0..5 {
            pace.starting(id);
        }
        for id in 0..3 {
            assert_eq!(pace.account(id).take_start(), None, "account {id}");
        }
        let fourth = pace.account(3).take_start().expect("the fourth waits");
        let fifth = pace.account(4).take_start().expect("the fifth waits");
        assert!(fourth >= Instant::now() + Duration::from_secs(2));
        assert!(fifth >= Instant::now() + Duration::from_secs(4));
        // Taken once.
        assert_eq!(pace.account(3).take_start(), None);
        // An account added later starts at once.
        tokio::time::advance(START_TOGETHER).await;
        pace.starting(9);
        assert_eq!(pace.account(9).take_start(), None);
    }
}
