//! How each account's mail checks are going (#232). An account's sidebar
//! heading shows a spinner once a check has run for a moment, so quick
//! checks show nothing, and a marker when a problem stops its checks: a
//! sign-in the server turned down at once, any other failure once it has
//! lasted a minute or at once after a check someone asked for, so a
//! connection that drops and comes straight back marks nothing. Both clear
//! with the next successful check. The account's menu, its status panel and
//! Settings also say when it last checked mail.

use super::*;
use chrono::{DateTime, Datelike, Timelike};
use slint::SharedString;

/// A check shows as running once it has run this long.
const CHECKING_DELAY: Duration = Duration::from_secs(1);
/// A failure marks the account once it has lasted this long.
const FAILURE_GRACE: Duration = Duration::from_secs(60);
/// A failure that a check someone asked for ended in, or that its last
/// events bring this soon after it finished, marks the account at once.
const ASKED_WINDOW: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Checking,
    SignIn,
    Failed,
}

impl Phase {
    fn of(account: &Account) -> Self {
        match account.sync_state.as_str() {
            "syncing" => Self::Checking,
            "needs_reauth" => Self::SignIn,
            "offline" => Self::Failed,
            _ => Self::Idle,
        }
    }
}

/// What this run of the app has seen of each account's checks.
#[derive(Default)]
pub(super) struct AccountActivity {
    /// Each account's phase and when it began.
    phases: HashMap<i64, (Phase, Instant)>,
    /// Until when each account's failures are marked at once, after a check
    /// someone asked for.
    asked_until: HashMap<i64, Instant>,
    /// Shows the next change that comes with time alone: a spinner, a
    /// failure's mark, or another day for "Last checked".
    timer: slint::Timer,
    /// The statuses the sidebar shows, so it is built again only when one
    /// changes, and an unfolding section keeps its animation.
    shown: HashMap<i32, AccountStatus>,
}

impl AccountActivity {
    /// Notes the accounts' current states; an account whose phase changed
    /// starts its clock at `now`.
    fn observe(&mut self, accounts: &[Account], now: Instant) {
        self.phases
            .retain(|id, _| accounts.iter().any(|account| account.id == *id));
        self.asked_until.retain(|_, until| *until > now);
        for account in accounts {
            let phase = Phase::of(account);
            if self
                .phases
                .get(&account.id)
                .is_none_or(|(seen, _)| *seen != phase)
            {
                self.phases.insert(account.id, (phase, now));
            }
        }
    }

    fn asked(&mut self, accounts: impl IntoIterator<Item = i64>, now: Instant) {
        for account_id in accounts {
            self.asked_until.insert(account_id, now + ASKED_WINDOW);
        }
    }

    fn status(&self, account: &Account, now: Instant, clock: &DateTime<Local>) -> AccountStatus {
        // A state not observed yet has only just begun.
        let phase = Phase::of(account);
        let since = match self.phases.get(&account.id) {
            Some((seen, since)) if *seen == phase => *since,
            _ => now,
        };
        let lasted = now.saturating_duration_since(since);
        let (checked_day, checked_at) = last_checked(account.caught_up_at, clock);
        AccountStatus {
            checking: phase == Phase::Checking && lasted >= CHECKING_DELAY,
            problem: match phase {
                Phase::SignIn => AccountProblem::SignIn,
                Phase::Failed => AccountProblem::Failed,
                Phase::Idle | Phase::Checking => AccountProblem::None,
            },
            marked: match phase {
                Phase::SignIn => true,
                Phase::Failed => {
                    lasted >= FAILURE_GRACE
                        || self
                            .asked_until
                            .get(&account.id)
                            .is_some_and(|until| since <= *until)
                }
                Phase::Idle | Phase::Checking => false,
            },
            reason: match phase {
                Phase::Failed => account.sync_error.clone().unwrap_or_default().into(),
                _ => SharedString::new(),
            },
            checked_day,
            checked_at: checked_at.into(),
        }
    }

    /// When a status shown now changes with time alone.
    fn next_change(&self, now: Instant) -> Option<Instant> {
        self.phases
            .iter()
            .filter_map(|(account_id, (phase, since))| match phase {
                Phase::Checking => Some(*since + CHECKING_DELAY),
                Phase::Failed
                    if !self
                        .asked_until
                        .get(account_id)
                        .is_some_and(|until| since <= until) =>
                {
                    Some(*since + FAILURE_GRACE)
                }
                _ => None,
            })
            .filter(|at| *at > now)
            .min()
    }
}

/// When a check last brought the Inbox up to date, for
/// `AccountStatusText.last-checked`: 0 for today and 1 for yesterday, with
/// the time, or 2 for an earlier day, with the date and time; -1 and nothing
/// before the first check.
pub(super) fn last_checked(caught_up_at: Option<i64>, now: &DateTime<Local>) -> (i32, String) {
    let Some(at) = caught_up_at
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|at| at.with_timezone(&Local))
    else {
        return (-1, String::new());
    };
    let formats = date_format::current();
    match now
        .date_naive()
        .signed_duration_since(at.date_naive())
        .num_days()
    {
        // A clock set back shows its time as today's.
        ..=0 => (0, formats.time(at.hour(), at.minute())),
        1 => (1, formats.time(at.hour(), at.minute())),
        _ => {
            let style = if at.year() == now.year() {
                DateStyle::ShortDayMonth
            } else {
                DateStyle::Short
            };
            (2, formats.date_time(style, at.naive_local()))
        }
    }
}

/// How long until the next local midnight, when "Last checked" may name
/// another day.
fn until_midnight(now: &DateTime<Local>) -> Option<Duration> {
    let midnight = now.date_naive().succ_opt()?.and_hms_opt(0, 0, 0)?;
    let midnight = Local.from_local_datetime(&midnight).earliest()?;
    (midnight - *now).to_std().ok()
}

/// Each connected account's status, for the sidebar's account headings.
pub(super) fn statuses(state: &InboxState) -> HashMap<i32, AccountStatus> {
    statuses_at(
        &state.connected_accounts,
        &state.account_activity,
        Instant::now(),
        &Local::now(),
    )
}

fn statuses_at(
    accounts: &[Account],
    activity: &AccountActivity,
    now: Instant,
    clock: &DateTime<Local>,
) -> HashMap<i32, AccountStatus> {
    accounts
        .iter()
        .filter_map(|account| {
            let id = i32::try_from(account.id).ok()?;
            Some((id, activity.status(account, now, clock)))
        })
        .collect()
}

/// A check someone asked for finished for these accounts, or for every
/// account: the failures it ended in show at once.
pub(super) fn checked_on_request(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    accounts: Option<&[i64]>,
) {
    {
        let mut inbox = state.borrow_mut();
        let ids = match accounts {
            Some(accounts) => accounts.to_vec(),
            None => inbox
                .connected_accounts
                .iter()
                .map(|account| account.id)
                .collect(),
        };
        inbox.account_activity.asked(ids, Instant::now());
    }
    refresh(app, state);
}

/// Shows every account's status again: on the sidebar's account headings,
/// in Settings and on the folders button, then waits for the next change
/// that comes with time alone.
pub(super) fn refresh(app: &AppWindow, state: &Rc<RefCell<InboxState>>) {
    let now = Instant::now();
    let clock = Local::now();
    let (marked, sidebar_changed, next) = {
        let mut inbox = state.borrow_mut();
        let InboxState {
            connected_accounts,
            account_activity,
            ..
        } = &mut *inbox;
        account_activity.observe(connected_accounts, now);
        update_account_rows(app, connected_accounts, account_activity, now, &clock);
        let statuses = statuses_at(connected_accounts, account_activity, now, &clock);
        let marked = statuses.values().any(|status| status.marked);
        let sidebar_changed = account_activity.shown != statuses;
        account_activity.shown = statuses;
        (marked, sidebar_changed, account_activity.next_change(now))
    };
    app.set_account_problem(marked);
    if sidebar_changed {
        sidebar_model::refresh_sidebar(state);
    }
    let wait = next
        .map(|at| at.saturating_duration_since(now))
        .into_iter()
        .chain(until_midnight(&clock))
        .min();
    if let Some(wait) = wait {
        let app = app.as_weak();
        let weak = Rc::downgrade(state);
        // Past the threshold itself, so the status it changes is due.
        state.borrow().account_activity.timer.start(
            slint::TimerMode::SingleShot,
            wait + Duration::from_millis(50),
            move || {
                if let (Some(app), Some(state)) = (app.upgrade(), weak.upgrade()) {
                    refresh(&app, &state);
                }
            },
        );
    }
}

/// Settings' account cards keep their other fields; replacing the model
/// would build every card again.
fn update_account_rows(
    app: &AppWindow,
    accounts: &[Account],
    activity: &AccountActivity,
    now: Instant,
    clock: &DateTime<Local>,
) {
    let model = app.get_connected_accounts();
    for index in 0..model.row_count() {
        let Some(mut row) = model.row_data(index) else {
            continue;
        };
        let Some(account) = accounts
            .iter()
            .find(|account| i64::from(row.id) == account.id)
        else {
            continue;
        };
        let status = activity.status(account, now, clock);
        let sync_error: SharedString = account.sync_error.clone().unwrap_or_default().into();
        if row.status.as_str() != account.sync_state
            || row.sync_error != sync_error
            || row.checked_day != status.checked_day
            || row.checked_at != status.checked_at
        {
            row.status = account.sync_state.as_str().into();
            row.sync_error = sync_error;
            row.checked_day = status.checked_day;
            row.checked_at = status.checked_at;
            model.set_row_data(index, row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(state: &str, error: Option<&str>) -> Account {
        Account {
            id: 1,
            email: "alex@example.com".into(),
            display_name: None,
            avatar_url: None,
            provider: flectar_mail_core::models::Provider::Generic,
            auth_kind: flectar_mail_core::models::AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            sync_state: state.into(),
            sync_error: error.map(str::to_owned),
            can_create_top_level_mailbox: true,
            server_keeps_mail_days: None,
            caught_up_at: None,
        }
    }

    fn observed(account: &Account, at: Instant) -> AccountActivity {
        let mut activity = AccountActivity::default();
        activity.observe(std::slice::from_ref(account), at);
        activity
    }

    #[test]
    fn a_check_shows_once_it_has_run_for_a_moment() {
        let start = Instant::now();
        let checking = account("syncing", None);
        let activity = observed(&checking, start);
        let clock = Local::now();
        assert!(!activity.status(&checking, start, &clock).checking);
        assert_eq!(activity.next_change(start), Some(start + CHECKING_DELAY));
        assert!(
            activity
                .status(&checking, start + CHECKING_DELAY, &clock)
                .checking
        );
    }

    #[test]
    fn a_refused_sign_in_marks_the_account_at_once() {
        let start = Instant::now();
        let refused = account("needs_reauth", None);
        let status = observed(&refused, start).status(&refused, start, &Local::now());
        assert_eq!(status.problem, AccountProblem::SignIn);
        assert!(status.marked);
    }

    #[test]
    fn a_failure_marks_the_account_once_it_has_lasted() {
        let start = Instant::now();
        let failed = account("offline", Some("network error: refused"));
        let activity = observed(&failed, start);
        let clock = Local::now();
        let status = activity.status(&failed, start, &clock);
        assert_eq!(status.problem, AccountProblem::Failed);
        assert_eq!(status.reason, "network error: refused");
        assert!(!status.marked);
        assert_eq!(activity.next_change(start), Some(start + FAILURE_GRACE));
        assert!(
            activity
                .status(&failed, start + FAILURE_GRACE, &clock)
                .marked
        );
    }

    #[test]
    fn a_check_someone_asked_for_marks_its_failures_at_once() {
        let start = Instant::now();
        let failed = account("offline", Some("network error: refused"));
        let mut activity = observed(&failed, start);
        activity.asked([1], start + Duration::from_secs(2));
        let clock = Local::now();
        assert!(
            activity
                .status(&failed, start + Duration::from_secs(2), &clock)
                .marked
        );
        assert_eq!(activity.next_change(start + Duration::from_secs(2)), None);
        // A failure after the account recovered waits again.
        let later = start + Duration::from_secs(30);
        activity.observe(&[account("idle", None)], later);
        activity.observe(std::slice::from_ref(&failed), later);
        assert!(!activity.status(&failed, later, &clock).marked);
    }

    #[test]
    fn the_same_state_keeps_its_clock() {
        let start = Instant::now();
        let failed = account("offline", Some("first"));
        let mut activity = observed(&failed, start);
        activity.observe(
            &[account("offline", Some("second"))],
            start + Duration::from_secs(59),
        );
        assert!(
            activity
                .status(&failed, start + FAILURE_GRACE, &Local::now())
                .marked
        );
    }

    #[test]
    fn last_checked_names_today_yesterday_or_the_date() {
        let now = Local.with_ymd_and_hms(2026, 11, 14, 9, 30, 0).unwrap();
        let at = |day, hour, minute| {
            Some(
                Local
                    .with_ymd_and_hms(2026, 11, day, hour, minute, 0)
                    .unwrap()
                    .timestamp_millis(),
            )
        };
        assert_eq!(last_checked(None, &now), (-1, String::new()));
        assert_eq!(last_checked(at(14, 9, 5), &now), (0, "09:05".into()));
        assert_eq!(last_checked(at(13, 18, 2), &now), (1, "18:02".into()));
        assert_eq!(
            last_checked(at(12, 18, 2), &now),
            (2, "Nov 12 at 18:02".into())
        );
        let last_year = Local.with_ymd_and_hms(2025, 12, 30, 8, 0, 0).unwrap();
        assert_eq!(
            last_checked(Some(last_year.timestamp_millis()), &now),
            (2, "Dec 30, 2025 at 08:00".into())
        );
    }

    #[test]
    fn midnight_is_the_start_of_the_next_day() {
        let now = Local.with_ymd_and_hms(2026, 11, 14, 23, 59, 0).unwrap();
        assert_eq!(until_midnight(&now), Some(Duration::from_secs(60)));
    }
}
