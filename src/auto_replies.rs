//! Automatic replies (#239): the Automatic replies tab of an account's mail
//! preferences, and the strip over the mail list while replies are on. The
//! core saves them to the account's server and reads them back on each
//! sync, so this only shows and edits them.

use crate::date_format::{self, DateStyle};
use crate::ui_message::{AppWindowMessages, UiMessage, translated};
use crate::{AccountMailPreferences, AppWindow, AutoReplies, InboxState, oauth_browser};
use chrono::{Local, NaiveDate, TimeZone};
use flectar_mail_core::{
    Core,
    events::CoreEvent,
    models::{AccountConfig, AutoReply, MailProtocol, Provider, now_ms},
};
use slint::{ComponentHandle, Weak};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{runtime::Handle, sync::broadcast::error::RecvError};

/// Connects the tab's Save and the strip's buttons.
pub(crate) fn register(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Handle,
    opener: crate::ExternalUrlOpener,
) {
    let saver = Rc::new(Saver {
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
        opener,
    });
    let save = Rc::clone(&saver);
    app.global::<AccountMailPreferences>()
        .on_save_auto_reply(move || save.save_form());
    let turn_off = Rc::clone(&saver);
    app.global::<AutoReplies>()
        .on_turn_off(move |account_id| turn_off.turn_off(i64::from(account_id)));
    let weak = app.as_weak();
    app.global::<AutoReplies>().on_open_settings(move || {
        if let Some(app) = weak.upgrade() {
            app.set_settings_tab("Accounts".into());
            app.set_settings_open(true);
        }
    });
}

/// Keeps the strip over the mail list up to date once the core is ready:
/// when replies change, and when the last day of some ends.
pub(crate) fn core_ready(app: &AppWindow, runtime: &Handle, core: Arc<Core>) {
    let weak = app.as_weak();
    let mut events = core.bus.subscribe();
    runtime.spawn(async move {
        loop {
            let next_end = show_strip(&weak, &core).await;
            let until_end = next_end.map_or(Duration::from_secs(24 * 60 * 60), |end| {
                Duration::from_millis(u64::try_from(end.saturating_sub(now_ms())).unwrap_or(0))
            });
            let sleep = tokio::time::sleep(until_end);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        Ok(CoreEvent::AutoReplyChanged { .. } | CoreEvent::AccountState { .. })
                        | Err(RecvError::Lagged(_)) => break,
                        Ok(_) => {}
                        Err(RecvError::Closed) => return,
                    },
                    () = &mut sleep => break,
                }
            }
        }
    });
}

/// Shows on the strip which accounts' replies are on, and gives when the
/// first of them ends.
async fn show_strip(app: &Weak<AppWindow>, core: &Core) -> Option<i64> {
    let replies = core.auto_replies().await.unwrap_or_default();
    let accounts = core.list_accounts().await.unwrap_or_default();
    let now = now_ms();
    let on = replies
        .into_iter()
        .filter(|(_, reply)| reply.on_at(now))
        .filter_map(|(account_id, reply)| {
            let account = accounts.iter().find(|account| account.id == account_id)?;
            Some((account_id, account.email.clone(), reply))
        })
        .collect::<Vec<_>>();
    let next_end = on.iter().filter_map(|(_, _, reply)| reply.ends_at).min();
    let _ = app.upgrade_in_event_loop(move |app| {
        let ui = app.global::<AutoReplies>();
        ui.set_on(!on.is_empty());
        if let [(account_id, email, reply)] = on.as_slice() {
            ui.set_account_id(i32::try_from(*account_id).unwrap_or_default());
            ui.set_detail(match strip_detail(email, reply, now_ms()) {
                Some(detail) => translated(&app, &detail),
                None => email.as_str().into(),
            });
        } else {
            ui.set_account_id(0);
            ui.set_detail(translated(
                &app,
                &UiMessage::detail("{} accounts", on.len()),
            ));
        }
    });
    next_end
}

/// The account's address with the day replies start, or else end. None
/// without either, when the address alone says it.
fn strip_detail(email: &str, reply: &AutoReply, now: i64) -> Option<UiMessage> {
    let day = |at: i64| {
        local_day(at).map(|day| date_format::current().date(DateStyle::WeekdayDayMonth, day))
    };
    match (
        reply
            .starts_at
            .filter(|starts_at| *starts_at > now)
            .and_then(day),
        reply.ends_at.and_then(day),
    ) {
        (Some(start), _) => Some(UiMessage::arguments("{} · from {}", email, start)),
        (None, Some(end)) => Some(UiMessage::arguments("{} · until {}", email, end)),
        (None, None) => None,
    }
}

/// Fills the Automatic replies tab for `config`'s account, from what the app
/// last read from its server.
pub(crate) fn load_tab(app: &AppWindow, runtime: &Handle, core: Arc<Core>, config: &AccountConfig) {
    let ui = app.global::<AccountMailPreferences>();
    ui.set_auto_reply_status("".into());
    ui.set_auto_reply_saving(false);
    let cant = if config.mail_protocol == MailProtocol::Pop3 {
        UiMessage::plain(
            "POP3 accounts can't have automatic replies from Flectar Mail. Your provider's webmail may offer them.",
        )
    } else {
        UiMessage::plain(
            "Flectar Mail can't set this account's automatic replies yet. Your provider's webmail can.",
        )
    };
    let kind = match (config.provider, config.mail_protocol) {
        (Provider::Gmail, _) => Server::Gmail,
        (Provider::Microsoft, _) => Server::Outlook,
        (_, MailProtocol::Jmap) => Server::Jmap,
        _ => {
            ui.set_auto_reply_supported(false);
            ui.set_auto_reply_unsupported_text(translated(app, &cant));
            return;
        }
    };
    // A JMAP server offers them when a sync could read them.
    ui.set_auto_reply_supported(kind != Server::Jmap);
    ui.set_auto_reply_unsupported_text("".into());
    ui.set_auto_reply_note(translated(
        app,
        &match kind {
            Server::Gmail => UiMessage::plain(
                "Gmail answers each sender once, and again after four days. Mailing lists and spam get no reply.",
            ),
            Server::Outlook => {
                UiMessage::plain("Outlook answers each sender once while replies are on.")
            }
            Server::Jmap => UiMessage::plain(
                "Your server answers each sender once every few days, and never mailing lists.",
            ),
        },
    ));
    // Outlook's replies have no subject of their own, and its days need an end.
    ui.set_auto_reply_subject_available(kind != Server::Outlook);
    ui.set_auto_reply_needs_last_day(kind == Server::Outlook);
    // JMAP replies go to every sender. Work accounts can keep Gmail's and
    // Outlook's inside the organization, and Outlook always answers it.
    let work = !personal_account(&config.email);
    ui.set_auto_reply_contacts_available(kind != Server::Jmap);
    ui.set_auto_reply_domain_available(work && kind != Server::Jmap);
    let contacts = if work && kind == Server::Outlook {
        UiMessage::plain(
            "Senders outside your organization who aren't in your contacts get no reply.",
        )
    } else {
        UiMessage::plain("Senders who aren't in your contacts get no reply.")
    };
    ui.set_auto_reply_contacts_note(translated(app, &contacts));
    show_form(app, &AutoReply::default());
    let account_id = config.id;
    let weak = app.as_weak();
    let task = runtime.spawn(async move { core.auto_reply(account_id).await });
    let _ = slint::spawn_local(async move {
        let Ok(Ok(reply)) = task.await else {
            return;
        };
        let Some(app) = weak.upgrade() else {
            return;
        };
        let ui = app.global::<AccountMailPreferences>();
        if i64::from(ui.get_account_id()) != account_id {
            return;
        }
        match reply {
            Some(reply) => {
                ui.set_auto_reply_supported(true);
                show_form(&app, &reply);
            }
            None if kind == Server::Jmap => {
                ui.set_auto_reply_unsupported_text(translated(&app, &cant));
            }
            None => {}
        }
    });
}

/// Where an account's automatic replies are kept.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Server {
    /// Gmail's vacation responder.
    Gmail,
    /// A Microsoft mailbox's settings.
    Outlook,
    /// JMAP's VacationResponse.
    Jmap,
}

/// Whether `email` is a personal Google or Microsoft account, which has no
/// organization to keep replies inside.
fn personal_account(email: &str) -> bool {
    let domain = email
        .rsplit_once('@')
        .map_or("", |(_, domain)| domain)
        .to_ascii_lowercase();
    let first = domain.split('.').next().unwrap_or_default();
    matches!(
        domain.as_str(),
        "gmail.com" | "googlemail.com" | "msn.com" | "passport.com"
    ) || matches!(first, "outlook" | "hotmail" | "live" | "windowslive")
}

fn show_form(app: &AppWindow, reply: &AutoReply) {
    let ui = app.global::<AccountMailPreferences>();
    ui.set_auto_reply_on(reply.enabled);
    ui.set_auto_reply_subject(reply.subject.as_str().into());
    ui.set_auto_reply_text(reply.text.as_str().into());
    let day = |at: Option<i64>| {
        at.and_then(local_day)
            .map(|day| day.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    };
    ui.set_auto_reply_first_day(day(reply.starts_at).into());
    ui.set_auto_reply_last_day(day(reply.ends_at).into());
    ui.set_auto_reply_contacts_only(reply.contacts_only);
    ui.set_auto_reply_domain_only(reply.domain_only);
}

/// The replies the tab's fields describe, or what to correct.
fn form_reply(ui: &AccountMailPreferences<'_>) -> Result<AutoReply, UiMessage> {
    let first = parse_day(&ui.get_auto_reply_first_day())
        .map_err(|()| UiMessage::plain("Write the first day as YYYY-MM-DD."))?;
    let last = parse_day(&ui.get_auto_reply_last_day())
        .map_err(|()| UiMessage::plain("Write the last day as YYYY-MM-DD."))?;
    if let (Some(first), Some(last)) = (first, last)
        && last < first
    {
        return Err(UiMessage::plain("The last day comes before the first."));
    }
    if ui.get_auto_reply_needs_last_day() && first.is_some() && last.is_none() {
        return Err(UiMessage::plain(
            "Outlook needs a last day with a first day.",
        ));
    }
    let enabled = ui.get_auto_reply_on();
    let text = ui.get_auto_reply_text().trim().to_owned();
    if enabled && text.is_empty() {
        return Err(UiMessage::plain("Write the message to send."));
    }
    Ok(AutoReply {
        enabled,
        subject: if ui.get_auto_reply_subject_available() {
            ui.get_auto_reply_subject().trim().to_owned()
        } else {
            String::new()
        },
        text,
        // From the start of the first day to the end of the last, here.
        starts_at: first.and_then(local_midnight),
        ends_at: last
            .and_then(|day| day.succ_opt())
            .and_then(local_midnight)
            .map(|midnight| midnight - 1),
        contacts_only: ui.get_auto_reply_contacts_available() && ui.get_auto_reply_contacts_only(),
        domain_only: ui.get_auto_reply_domain_available() && ui.get_auto_reply_domain_only(),
    })
}

/// A day typed as YYYY-MM-DD, or none when left empty.
fn parse_day(text: &str) -> Result<Option<NaiveDate>, ()> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map(Some)
        .map_err(|_| ())
}

fn local_midnight(day: NaiveDate) -> Option<i64> {
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()
        .map(|time| time.timestamp_millis())
}

fn local_day(at: i64) -> Option<NaiveDate> {
    chrono::DateTime::from_timestamp_millis(at).map(|time| time.with_timezone(&Local).date_naive())
}

/// Saves replies to an account's server, asking Google for permission in
/// the browser the first time.
struct Saver {
    app: Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: Handle,
    opener: crate::ExternalUrlOpener,
}

impl Saver {
    fn core(&self) -> Option<Arc<Core>> {
        self.state
            .borrow()
            .core
            .as_ref()
            .map(|core| core.account_preferences_core())
    }

    fn save_form(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ui = app.global::<AccountMailPreferences>();
        match form_reply(&ui) {
            Ok(reply) => self.save(i64::from(ui.get_account_id()), reply),
            Err(problem) => ui.set_auto_reply_status(translated(&app, &problem)),
        }
    }

    fn turn_off(&self, account_id: i64) {
        let Some(core) = self.core() else {
            return;
        };
        let current = self
            .runtime
            .block_on(core.auto_reply(account_id))
            .ok()
            .flatten()
            .unwrap_or_default();
        self.save(
            account_id,
            AutoReply {
                enabled: false,
                ..current
            },
        );
    }

    fn save(&self, account_id: i64, reply: AutoReply) {
        let (Some(app), Some(core)) = (self.app.upgrade(), self.core()) else {
            return;
        };
        let ui = app.global::<AccountMailPreferences>();
        if app.get_oauth_in_progress() {
            app.set_sync_status(UiMessage::plain(
                "Another browser authorization is already in progress; finish it or choose Cancel.",
            ));
            return;
        }
        ui.set_auto_reply_saving(true);
        ui.set_auto_reply_status("".into());
        // Google asks for permission only the first time, in the browser.
        let consenting = Arc::new(AtomicBool::new(false));
        let asked = Arc::clone(&consenting);
        let browser_app = app.as_weak();
        let opener = self.opener.clone();
        let task = self.runtime.spawn(async move {
            core.set_auto_reply(account_id, reply, move |url| {
                asked.store(true, Ordering::Relaxed);
                let _ = browser_app.upgrade_in_event_loop(|app| {
                    app.set_oauth_in_progress(true);
                    app.set_oauth_browser_error(false);
                    app.set_sync_status(UiMessage::plain(
                        "Allow Flectar Mail to change Gmail settings in your browser.",
                    ));
                });
                oauth_browser::begin(browser_app, url, opener)
            })
            .await
        });
        let weak = app.as_weak();
        let _ = slint::spawn_local(async move {
            let result = task.await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            if consenting.load(Ordering::Relaxed) {
                app.set_oauth_in_progress(false);
                app.set_sync_status(UiMessage::EMPTY);
            }
            let ui = app.global::<AccountMailPreferences>();
            ui.set_auto_reply_saving(false);
            let shown = i64::from(ui.get_account_id()) == account_id;
            match result {
                Ok(Ok(saved)) => {
                    if shown {
                        show_form(&app, &saved);
                        ui.set_auto_reply_status(translated(&app, &UiMessage::plain("Saved.")));
                    }
                }
                Ok(Err(error)) => {
                    let problem = UiMessage::detail(
                        "Could not save automatic replies: {}",
                        error.to_string(),
                    );
                    if shown {
                        ui.set_auto_reply_status(translated(&app, &problem));
                    } else {
                        app.set_sync_status(problem);
                    }
                }
                Err(error) => app.set_sync_status(UiMessage::detail(
                    "Could not save automatic replies: {}",
                    error.to_string(),
                )),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_accounts_have_no_organization() {
        for email in [
            "a@gmail.com",
            "a@outlook.com",
            "a@hotmail.co.uk",
            "a@live.de",
            "a@msn.com",
        ] {
            assert!(personal_account(email), "{email}");
        }
        for email in [
            "a@flectar.com",
            "a@contoso.onmicrosoft.com",
            "a@liverpool.ac.uk",
        ] {
            assert!(!personal_account(email), "{email}");
        }
    }

    #[test]
    fn days_are_typed_as_iso_dates_or_left_empty() {
        assert_eq!(parse_day(""), Ok(None));
        assert_eq!(
            parse_day(" 2026-11-20 "),
            Ok(NaiveDate::from_ymd_opt(2026, 11, 20))
        );
        assert_eq!(parse_day("20/11/2026"), Err(()));
        assert_eq!(parse_day("2026-02-30"), Err(()));
    }

    #[test]
    fn the_last_day_lasts_until_its_end() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let start = local_midnight(day).unwrap();
        let next = local_midnight(day.succ_opt().unwrap()).unwrap();
        assert_eq!(local_day(start), Some(day));
        assert_eq!(local_day(next - 1), Some(day));
        assert_eq!(local_day(next), day.succ_opt());
    }

    #[test]
    fn the_strip_names_the_account_and_the_day_that_matters() {
        let reply = |starts_at, ends_at| AutoReply {
            enabled: true,
            starts_at,
            ends_at,
            ..AutoReply::default()
        };
        let english =
            |reply| strip_detail("a@example.com", &reply, 5).map(|detail| detail.english());
        assert_eq!(english(reply(None, None)), None, "the address alone");
        assert!(
            english(reply(None, Some(10)))
                .unwrap()
                .starts_with("a@example.com · until ")
        );
        // Before the first day, the strip says when replies start.
        assert!(
            english(reply(Some(9), Some(10)))
                .unwrap()
                .starts_with("a@example.com · from ")
        );
        assert!(english(reply(Some(1), Some(10))).unwrap().contains("until"));
    }
}
