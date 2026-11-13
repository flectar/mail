//! Tells people who installed Flectar Mail from a release file that a newer
//! release is out. App stores, Flathub and package managers update the app
//! themselves, so only builds made for direct download ask (see
//! [`offered`]).

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use reqwest::header::ACCEPT;
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};
use tokio::time::Instant;

/// How often the app asks.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// A start's first check waits for its mail to sync first.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);
/// After a failed check, the next one comes this much later.
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
/// While a check is due later, the app looks again this often: its timers
/// stop while the computer sleeps, but the day goes on.
const LOOK_AGAIN_AFTER: Duration = Duration::from_secs(60 * 60);
/// What the last check found, in the cache folder: losing it only means
/// asking again.
const FOUND_FILE: &str = "update-check.json";

/// Whether this build tells of new releases: only the builds made for direct
/// download, which their release workflow marks, and not the Microsoft
/// Store's, which shares the setup program's executable.
pub(crate) fn offered() -> bool {
    option_env!("FLECTAR_UPDATE_CHECKS") == Some("github") && !from_microsoft_store()
}

/// This copy came from the Microsoft Store, which updates it.
fn from_microsoft_store() -> bool {
    #[cfg(target_os = "windows")]
    {
        crate::platform::windows_install::package_family_name().is_some()
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// What the window shows about updates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UpdateView {
    /// The user wants the app to check.
    pub enabled: bool,
    pub checking: bool,
    /// The last check this run failed.
    pub failed: bool,
    /// The last check found nothing newer.
    pub current: bool,
    /// The newest release after the running one.
    pub available: Option<Release>,
    /// The user put away the notice of `available`.
    pub dismissed: bool,
}

/// What the last check found.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Found {
    /// When, in milliseconds since the Unix epoch.
    checked_at: i64,
    newest: Option<Release>,
    /// The version whose notice the user put away.
    dismissed: Option<String>,
}

/// What asks for the newest release: GitHub, or a test's stand-in.
type Fetch = Box<
    dyn Fn(Version) -> Pin<Box<dyn Future<Output = Result<Option<Release>, String>> + Send>>
        + Send
        + Sync,
>;
/// The time of day, in milliseconds since the Unix epoch: the system's, or a
/// test's stand-in.
type Clock = Box<dyn Fn() -> i64 + Send + Sync>;

struct Updates {
    file: PathBuf,
    running: Version,
    fetch: Fetch,
    clock: Clock,
    found: Mutex<Found>,
    view: Mutex<UpdateView>,
    /// Whether the user wants checks: `None` until the settings are read.
    enabled: watch::Sender<Option<bool>>,
    check_now: Notify,
    show: Box<dyn Fn(UpdateView) + Send + Sync>,
}

static UPDATES: OnceLock<Arc<Updates>> = OnceLock::new();
/// The user's choice, kept for a start that comes after the settings are
/// read: 0 until they are, then 1 for off and 2 for on.
static ENABLED: AtomicU8 = AtomicU8::new(0);

/// Start checking in the background, in builds that offer it. `show` hears
/// of every change to what the window shows.
pub(crate) fn start(
    runtime: &tokio::runtime::Handle,
    cache_dir: &Path,
    show: impl Fn(UpdateView) + Send + Sync + 'static,
) {
    if !offered() {
        return;
    }
    let Ok(running) = Version::parse(env!("CARGO_PKG_VERSION")) else {
        tracing::warn!("updates: the app's own version is not a semantic version");
        return;
    };
    let fetch: Fetch = Box::new(|running| Box::pin(async move { newest_release(&running).await }));
    let updates = Updates::new(
        cache_dir.join(FOUND_FILE),
        running,
        fetch,
        Box::new(now_ms),
        show,
    );
    match ENABLED.load(Ordering::Relaxed) {
        0 => {}
        chosen => updates.set_enabled(chosen == 2),
    }
    if UPDATES.set(Arc::clone(&updates)).is_ok() {
        runtime.spawn(updates.run());
    }
}

/// The user turned checks on or off, or the settings were read.
pub(crate) fn set_enabled(enabled: bool) {
    ENABLED.store(if enabled { 2 } else { 1 }, Ordering::Relaxed);
    if let Some(updates) = UPDATES.get() {
        updates.set_enabled(enabled);
    }
}

/// The page of the newer release found, for the window to open.
pub(crate) fn release_page() -> Option<String> {
    let updates = UPDATES.get()?;
    let view = updates.view.lock().unwrap_or_else(PoisonError::into_inner);
    view.available.as_ref().map(|release| release.page.clone())
}

/// Check at once, as the user asked.
pub(crate) fn check_now() {
    if let Some(updates) = UPDATES.get() {
        updates.check_now.notify_one();
    }
}

/// Put away the notice of the release found, until a newer one comes.
pub(crate) fn dismiss() {
    if let Some(updates) = UPDATES.get() {
        updates.dismiss();
    }
}

impl Updates {
    fn new(
        file: PathBuf,
        running: Version,
        fetch: Fetch,
        clock: Clock,
        show: impl Fn(UpdateView) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let found = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Found>(&bytes).ok())
            .unwrap_or_default();
        Arc::new(Self {
            file,
            running,
            fetch,
            clock,
            found: Mutex::new(found),
            view: Mutex::new(UpdateView::default()),
            enabled: watch::Sender::new(None),
            check_now: Notify::new(),
            show: Box::new(show),
        })
    }

    fn set_enabled(&self, enabled: bool) {
        self.enabled.send_replace(Some(enabled));
    }

    fn dismiss(&self) {
        let found = {
            let mut found = self.found.lock().unwrap_or_else(PoisonError::into_inner);
            found.dismissed = found.newest.as_ref().map(|release| release.version.clone());
            found.clone()
        };
        self.save(&found);
        self.publish(|_| {});
    }

    async fn run(self: Arc<Self>) {
        let mut enabled = self.enabled.subscribe();
        let started = Instant::now();
        // After a failed check, when to try again, on the wall clock.
        let mut retry_at = None;
        loop {
            let on = *enabled.borrow_and_update() == Some(true);
            self.publish(|view| view.enabled = on);
            if !on {
                if enabled.changed().await.is_err() {
                    return;
                }
                continue;
            }
            let wait = self.wait(started, retry_at);
            tokio::select! {
                // First, so that a check asked for earlier is used up by
                // this one rather than repeating it.
                biased;
                () = self.check_now.notified() => {}
                changed = enabled.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    continue;
                }
                () = tokio::time::sleep(wait.min(LOOK_AGAIN_AFTER)) => {
                    if wait > LOOK_AGAIN_AFTER {
                        continue;
                    }
                }
            }
            if *enabled.borrow() != Some(true) {
                continue;
            }
            self.publish(|view| view.checking = true);
            match (self.fetch)(self.running.clone()).await {
                Ok(newest) => {
                    retry_at = None;
                    let found = {
                        let mut found = self.found.lock().unwrap_or_else(PoisonError::into_inner);
                        found.checked_at = (self.clock)();
                        found.newest = newest;
                        found.clone()
                    };
                    self.save(&found);
                    self.publish(|view| {
                        view.checking = false;
                        view.failed = false;
                    });
                }
                Err(error) => {
                    tracing::info!(%error, "updates: could not check");
                    retry_at = Some((self.clock)().saturating_add(millis(RETRY_AFTER)));
                    self.publish(|view| {
                        view.checking = false;
                        view.failed = true;
                    });
                }
            }
        }
    }

    /// How long until the next check: a day after the last one, or an hour
    /// after a failed one, both on the wall clock. A new start also waits
    /// for its mail to sync first.
    fn wait(&self, started: Instant, retry_at: Option<i64>) -> Duration {
        let now = (self.clock)();
        let due_at = retry_at.unwrap_or_else(|| {
            let checked_at = self
                .found
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .checked_at;
            // A clock set back past the last check makes it due.
            if checked_at > now {
                now
            } else {
                checked_at.saturating_add(millis(CHECK_EVERY))
            }
        });
        let due = Duration::from_millis(u64::try_from(due_at.saturating_sub(now)).unwrap_or(0));
        due.max((started + FIRST_CHECK_AFTER).saturating_duration_since(Instant::now()))
    }

    /// Change what the window shows, and tell it.
    fn publish(&self, change: impl FnOnce(&mut UpdateView)) {
        let view = {
            let found = self.found.lock().unwrap_or_else(PoisonError::into_inner);
            let mut view = self.view.lock().unwrap_or_else(PoisonError::into_inner);
            change(&mut view);
            // A release the app has since been updated to is no news.
            view.available = found.newest.clone().filter(|release| {
                Version::parse(&release.version).is_ok_and(|version| version > self.running)
            });
            view.current =
                found.checked_at > 0 && view.available.is_none() && !view.checking && !view.failed;
            view.dismissed = view.available.as_ref().is_some_and(|release| {
                found.dismissed.as_deref() == Some(release.version.as_str())
            });
            view.clone()
        };
        (self.show)(view);
    }

    fn save(&self, found: &Found) {
        let Ok(json) = serde_json::to_vec(found) else {
            return;
        };
        if let Err(error) = std::fs::write(&self.file, json) {
            tracing::warn!(%error, "updates: could not keep what the check found");
        }
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// The newest releases, newest first. Previews are listed too, which
/// `/releases/latest` leaves out.
const RELEASES_URL: &str = "https://api.github.com/repos/flectar/mail/releases?per_page=10";
/// Where a release's page must be, as the app opens it in the browser.
const RELEASE_PAGES: &str = "https://github.com/flectar/mail/releases/tag/";
/// Ten releases with their notes take well under this.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// How long a release's summary may run before it is cut.
const MAX_SUMMARY_CHARS: usize = 320;

/// A release as GitHub lists it.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// A release newer than the one running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Release {
    pub version: String,
    /// Its page, with the notes and the files to download.
    pub page: String,
    /// The first paragraph of its notes.
    pub summary: String,
}

/// Ask GitHub for the newest release after `current` on its channel.
pub(crate) async fn newest_release(current: &Version) -> Result<Option<Release>, String> {
    let client = reqwest::Client::builder()
        // GitHub wants every request to name its client. Nothing else about
        // the app, the device or its user goes with it.
        .user_agent("Flectar Mail update check")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        // GitHub redirects the API of a moved repository: follow that only
        // within the API, over HTTPS.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            if attempt.previous().len() < 3
                && url.scheme() == "https"
                && url.host_str() == Some("api.github.com")
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|error| format!("could not create the update client: {error}"))?;
    let mut response = client
        .get(RELEASES_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|error| format!("could not reach GitHub: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("could not read the releases: {error}"))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("the release list is larger than expected".into());
        }
        body.extend_from_slice(&chunk);
    }
    let releases: Vec<GithubRelease> = serde_json::from_slice(&body)
        .map_err(|error| format!("could not read the releases: {error}"))?;
    Ok(newest_after(current, releases))
}

/// The newest release after `current` on its channel. Previews, versions
/// such as 0.2.0-alpha.1, hear of every release; stable versions only of
/// stable ones.
fn newest_after(current: &Version, releases: Vec<GithubRelease>) -> Option<Release> {
    let preview = !current.pre.is_empty();
    releases
        .into_iter()
        .filter(|release| !release.draft && (preview || !release.prerelease))
        .filter(|release| release.html_url.starts_with(RELEASE_PAGES))
        .filter_map(|release| {
            let tag = release
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&release.tag_name);
            let version = Version::parse(tag).ok()?;
            ((preview || version.pre.is_empty()) && version > *current)
                .then_some((version, release))
        })
        .max_by(|(newer, _), (older, _)| newer.cmp(older))
        .map(|(version, release)| Release {
            version: version.to_string(),
            page: release.html_url,
            summary: summary(release.body.as_deref().unwrap_or_default()),
        })
}

/// The first paragraph of release notes, as plain text.
fn summary(notes: &str) -> String {
    let mut text = String::new();
    let mut in_paragraph = false;
    for event in Parser::new(notes) {
        match event {
            Event::Start(Tag::Paragraph) => in_paragraph = true,
            Event::End(TagEnd::Paragraph) => {
                if !text.trim().is_empty() {
                    break;
                }
                in_paragraph = false;
            }
            Event::Text(part) | Event::Code(part) if in_paragraph => text.push_str(&part),
            Event::SoftBreak | Event::HardBreak if in_paragraph => text.push(' '),
            _ => {}
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= MAX_SUMMARY_CHARS {
        return text;
    }
    let cut = text
        .char_indices()
        .nth(MAX_SUMMARY_CHARS)
        .map_or(text.len(), |(index, _)| index);
    let cut = text[..cut].rfind(' ').unwrap_or(cut);
    format!("{}…", text[..cut].trim_end_matches([',', ';', ':', '.']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering::SeqCst};

    type Views = Arc<Mutex<Vec<UpdateView>>>;

    /// A wall clock that moves with the paused test clock, and further on
    /// when a test skips time, as for a computer that slept.
    #[derive(Clone)]
    struct TestClock {
        start: Instant,
        skipped: Arc<AtomicI64>,
    }

    impl TestClock {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                skipped: Arc::default(),
            }
        }

        fn now(&self) -> i64 {
            1_700_000_000_000 + millis(self.start.elapsed()) + self.skipped.load(SeqCst)
        }

        fn skip(&self, time: Duration) {
            self.skipped.fetch_add(millis(time), SeqCst);
        }
    }

    /// A controller whose checks give `answers` in turn, and what it showed
    /// and how often it asked.
    fn controller(
        dir: &Path,
        clock: &TestClock,
        running: &str,
        answers: Vec<Result<Option<Release>, String>>,
    ) -> (Arc<Updates>, Views, Arc<AtomicUsize>) {
        let views = Views::default();
        let asked = Arc::new(AtomicUsize::new(0));
        let answers = Mutex::new(VecDeque::from(answers));
        let counted = Arc::clone(&asked);
        let fetch: Fetch = Box::new(move |_| {
            counted.fetch_add(1, SeqCst);
            let answer = answers.lock().unwrap().pop_front().unwrap_or(Ok(None));
            Box::pin(async move { answer })
        });
        let shown = Arc::clone(&views);
        let clock = clock.clone();
        let updates = Updates::new(
            dir.join(FOUND_FILE),
            version(running),
            fetch,
            Box::new(move || clock.now()),
            move |view| shown.lock().unwrap().push(view),
        );
        (updates, views, asked)
    }

    fn found(version: &str) -> Release {
        Release {
            version: version.into(),
            page: format!("{RELEASE_PAGES}v{version}"),
            summary: "Notes.".into(),
        }
    }

    fn last(views: &Views) -> UpdateView {
        views.lock().unwrap().last().cloned().unwrap()
    }

    /// Let the controller run until it waits again.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_check_waits_for_the_start_and_the_next_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let (updates, views, asked) = controller(
            dir.path(),
            &clock,
            "0.1.0-alpha.7",
            vec![Ok(Some(found("0.1.0-alpha.8"))), Ok(None)],
        );
        updates.set_enabled(true);
        tokio::spawn(Arc::clone(&updates).run());
        tokio::time::sleep(FIRST_CHECK_AFTER - Duration::from_secs(1)).await;
        assert_eq!(asked.load(SeqCst), 0);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(asked.load(SeqCst), 1);
        let view = last(&views);
        assert_eq!(view.available, Some(found("0.1.0-alpha.8")));
        assert!(view.enabled && !view.checking && !view.failed && !view.current && !view.dismissed);
        // What it found outlives the run.
        let kept: Found =
            serde_json::from_slice(&std::fs::read(dir.path().join(FOUND_FILE)).unwrap()).unwrap();
        assert_eq!(kept.newest, Some(found("0.1.0-alpha.8")));

        tokio::time::sleep(CHECK_EVERY - Duration::from_secs(5)).await;
        assert_eq!(asked.load(SeqCst), 1);
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(asked.load(SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_start_soon_after_a_check_shows_its_find_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let kept = Found {
            checked_at: clock.now() - 23 * 60 * 60 * 1000,
            newest: Some(found("0.1.0-alpha.8")),
            dismissed: None,
        };
        std::fs::write(
            dir.path().join(FOUND_FILE),
            serde_json::to_vec(&kept).unwrap(),
        )
        .unwrap();
        let (updates, views, asked) = controller(dir.path(), &clock, "0.1.0-alpha.7", Vec::new());
        updates.set_enabled(true);
        tokio::spawn(Arc::clone(&updates).run());
        settle().await;
        assert_eq!(last(&views).available, Some(found("0.1.0-alpha.8")));
        assert_eq!(asked.load(SeqCst), 0);
        // A day after the last check, not a day after this start.
        tokio::time::sleep(Duration::from_secs(60 * 60 + 1)).await;
        assert_eq!(asked.load(SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_asked_while_checks_are_off() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let (updates, views, asked) = controller(dir.path(), &clock, "0.1.0-alpha.7", Vec::new());
        tokio::spawn(Arc::clone(&updates).run());
        // Not before the settings are read either.
        tokio::time::sleep(2 * CHECK_EVERY).await;
        updates.set_enabled(false);
        updates.check_now.notify_one();
        tokio::time::sleep(2 * CHECK_EVERY).await;
        assert_eq!(asked.load(SeqCst), 0);
        assert!(!last(&views).enabled);

        updates.set_enabled(true);
        settle().await;
        // The check asked for while they were off comes now.
        assert_eq!(asked.load(SeqCst), 1);
        assert!(last(&views).current);
    }

    #[tokio::test(start_paused = true)]
    async fn checking_now_does_not_wait() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let (updates, views, asked) = controller(dir.path(), &clock, "1.0.0", Vec::new());
        updates.set_enabled(true);
        tokio::spawn(Arc::clone(&updates).run());
        settle().await;
        updates.check_now.notify_one();
        settle().await;
        assert_eq!(asked.load(SeqCst), 1);
        let view = last(&views);
        assert!(view.current && view.available.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_check_is_tried_again_an_hour_later() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let (updates, views, asked) = controller(
            dir.path(),
            &clock,
            "0.1.0-alpha.7",
            vec![Err("offline".into()), Ok(Some(found("0.1.0-alpha.8")))],
        );
        updates.set_enabled(true);
        tokio::spawn(Arc::clone(&updates).run());
        tokio::time::sleep(FIRST_CHECK_AFTER + Duration::from_secs(1)).await;
        assert!(last(&views).failed);
        assert!(!dir.path().join(FOUND_FILE).exists());
        tokio::time::sleep(RETRY_AFTER - Duration::from_secs(5)).await;
        assert_eq!(asked.load(SeqCst), 1);
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(asked.load(SeqCst), 2);
        let view = last(&views);
        assert!(!view.failed);
        assert_eq!(view.available, Some(found("0.1.0-alpha.8")));
    }

    #[tokio::test(start_paused = true)]
    async fn a_put_away_notice_stays_away_until_a_newer_release() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let (updates, views, _) = controller(
            dir.path(),
            &clock,
            "0.1.0-alpha.7",
            vec![
                Ok(Some(found("0.1.0-alpha.8"))),
                Ok(Some(found("0.1.0-alpha.8"))),
                Ok(Some(found("0.1.0-alpha.9"))),
            ],
        );
        updates.set_enabled(true);
        tokio::spawn(Arc::clone(&updates).run());
        updates.check_now.notify_one();
        settle().await;
        updates.dismiss();
        assert!(last(&views).dismissed);
        updates.check_now.notify_one();
        settle().await;
        assert!(last(&views).dismissed);
        updates.check_now.notify_one();
        settle().await;
        let view = last(&views);
        assert_eq!(view.available, Some(found("0.1.0-alpha.9")));
        assert!(!view.dismissed);
    }

    #[test]
    fn a_release_the_app_was_updated_to_is_no_news() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let kept = Found {
            checked_at: clock.now(),
            newest: Some(found("0.1.0-alpha.8")),
            dismissed: None,
        };
        std::fs::write(
            dir.path().join(FOUND_FILE),
            serde_json::to_vec(&kept).unwrap(),
        )
        .unwrap();
        let (updates, views, _) = controller(dir.path(), &clock, "0.1.0-alpha.8", Vec::new());
        updates.publish(|_| {});
        assert_eq!(last(&views).available, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_computer_that_slept_through_the_day_checks_within_the_hour() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let (updates, _, asked) = controller(dir.path(), &clock, "0.1.0-alpha.7", Vec::new());
        updates.set_enabled(true);
        tokio::spawn(Arc::clone(&updates).run());
        tokio::time::sleep(FIRST_CHECK_AFTER + Duration::from_secs(1)).await;
        assert_eq!(asked.load(SeqCst), 1);
        // Three days asleep: the day went on, the timers didn't.
        clock.skip(3 * CHECK_EVERY);
        tokio::time::sleep(LOOK_AGAIN_AFTER + Duration::from_secs(1)).await;
        assert_eq!(asked.load(SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_clock_set_back_before_the_last_check_checks_again() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let kept = Found {
            checked_at: clock.now() + 2 * millis(CHECK_EVERY),
            newest: None,
            dismissed: None,
        };
        std::fs::write(
            dir.path().join(FOUND_FILE),
            serde_json::to_vec(&kept).unwrap(),
        )
        .unwrap();
        let (updates, _, asked) = controller(dir.path(), &clock, "0.1.0-alpha.7", Vec::new());
        updates.set_enabled(true);
        tokio::spawn(Arc::clone(&updates).run());
        tokio::time::sleep(FIRST_CHECK_AFTER + Duration::from_secs(1)).await;
        assert_eq!(asked.load(SeqCst), 1);
    }

    fn release(tag: &str, prerelease: bool) -> GithubRelease {
        GithubRelease {
            tag_name: tag.into(),
            html_url: format!("{RELEASE_PAGES}{tag}"),
            body: Some(format!("Notes for {tag}.")),
            draft: false,
            prerelease,
        }
    }

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn a_preview_hears_of_the_newest_release_of_any_kind() {
        let newest = newest_after(
            &version("0.1.0-alpha.7"),
            vec![
                release("v0.1.0-alpha.9", true),
                release("v0.1.0-alpha.10", false),
                release("v0.1.0-alpha.8", false),
            ],
        )
        .unwrap();
        // alpha.10 sorts after alpha.9, as numbers.
        assert_eq!(newest.version, "0.1.0-alpha.10");
        assert_eq!(newest.page, format!("{RELEASE_PAGES}v0.1.0-alpha.10"));
        assert_eq!(newest.summary, "Notes for v0.1.0-alpha.10.");
    }

    #[test]
    fn a_stable_version_hears_only_of_stable_releases() {
        let current = version("1.0.0");
        let newest = newest_after(
            &current,
            vec![
                release("v1.1.0-beta.1", false),
                release("v1.2.0", true),
                release("v1.0.1", false),
            ],
        )
        .unwrap();
        assert_eq!(newest.version, "1.0.1");
        assert_eq!(
            newest_after(&current, vec![release("v1.1.0-beta.1", false)]),
            None
        );
    }

    #[test]
    fn the_running_version_or_older_ones_are_no_news() {
        let current = version("0.1.0-alpha.7");
        assert_eq!(
            newest_after(
                &current,
                vec![
                    release("v0.1.0-alpha.7", false),
                    release("v0.1.0-alpha.6", false)
                ]
            ),
            None
        );
        // A final release follows its previews.
        assert_eq!(
            newest_after(&current, vec![release("v0.1.0", false)]).map(|release| release.version),
            Some("0.1.0".into())
        );
    }

    #[test]
    fn drafts_odd_tags_and_pages_elsewhere_are_left_out() {
        let current = version("0.1.0-alpha.7");
        let mut draft = release("v0.1.0-alpha.8", false);
        draft.draft = true;
        let mut elsewhere = release("v0.1.0-alpha.9", false);
        elsewhere.html_url = "https://example.com/flectar-mail".into();
        assert_eq!(
            newest_after(&current, vec![draft, elsewhere, release("nightly", false)]),
            None
        );
    }

    #[test]
    fn github_s_listing_is_read() {
        let listing = br#"[{"tag_name":"v0.1.0-alpha.8","html_url":"https://github.com/flectar/mail/releases/tag/v0.1.0-alpha.8","body":"Alpha 8 brings **calm** [mail](https://flectar.com).\n\n## More\n\n- Item","draft":false,"prerelease":false,"assets":[]}]"#;
        let releases: Vec<GithubRelease> = serde_json::from_slice(listing).unwrap();
        let newest = newest_after(&version("0.1.0-alpha.7"), releases).unwrap();
        assert_eq!(newest.summary, "Alpha 8 brings calm mail.");
    }

    #[test]
    fn the_summary_is_the_first_paragraph_in_plain_text() {
        assert_eq!(
            summary("## Heading\n\nFirst line\nsame paragraph with `code`.\n\nSecond paragraph."),
            "First line same paragraph with code."
        );
        assert_eq!(summary("- only\n- a list"), "");
        assert_eq!(summary(""), "");
        let long = format!("{}.", "word ".repeat(100).trim_end());
        let cut = summary(&long);
        assert!(cut.ends_with("word…"), "{cut}");
        assert!(cut.chars().count() <= MAX_SUMMARY_CHARS + 1);
    }
}
