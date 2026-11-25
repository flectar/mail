//! The card above a message that carries a calendar (#228): an invitation,
//! its update or cancellation, or an answer to one. The card comes with the
//! message's body (`CoreMailSource::load_conversation`), so the message
//! doesn't move when it appears. This module shows it, answers from it,
//! takes a cancelled event out of the calendar, and keeps it current while
//! the calendar changes.

use super::*;
use crate::calendar::LocalCalendarState;
use crate::date_format::DateFormats;
use chrono::{Datelike, NaiveDateTime, Timelike, Utc};
use flectar_mail_core::models::{InvitationCard, InvitationKind};
use std::future::Future;

/// How many guests the card names before "and N more".
const NAMED_GUESTS: usize = 3;

struct Controller {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    calendar: Rc<RefCell<LocalCalendarState>>,
    runtime: tokio::runtime::Handle,
}

thread_local! {
    static CONTROLLER: RefCell<Option<Rc<Controller>>> = const { RefCell::new(None) };
    /// The message whose card each reader shows, and the card.
    static SHOWN: PerReader<(i64, InvitationCard)> = const { PerReader::new() };
}

fn with_controller(f: impl FnOnce(&Rc<Controller>)) {
    let controller = CONTROLLER.with(|slot| slot.borrow().clone());
    if let Some(controller) = controller {
        f(&controller);
    }
}

fn shown(key: ReaderKey) -> Option<(i64, InvitationCard)> {
    SHOWN.with(|shown| shown.get(key))
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    calendar: &Rc<RefCell<LocalCalendarState>>,
    runtime: &tokio::runtime::Handle,
) {
    CONTROLLER.with(|slot| {
        *slot.borrow_mut() = Some(Rc::new(Controller {
            app: app.as_weak(),
            state: Rc::clone(state),
            calendar: Rc::clone(calendar),
            runtime: runtime.clone(),
        }));
    });
    attach(&ReaderRef::main(app));
}

/// Answer from the card a reader shows.
pub(super) fn attach(reader: &ReaderRef) {
    let ui = reader.global::<Invitation>();
    let host = reader.host();
    ui.on_answer(move |response| {
        with_controller(|controller| controller.answer(&host, response.to_string()));
    });
    let host = reader.host();
    ui.on_remove(move || with_controller(|controller| controller.remove(&host)));
    let host = reader.host();
    ui.on_add(move || with_controller(|controller| controller.add(&host)));
    let host = reader.host();
    ui.on_show_in_calendar(move || {
        with_controller(|controller| controller.show_in_calendar(&host));
    });
    let host = reader.host();
    ui.on_open_link(move |url| {
        if let Some(reader) = host.upgrade() {
            reader.open_link(url);
        }
    });
}

/// Show the card of the message the reader shows, or take it away.
pub(super) fn project(reader: &ReaderRef, email: &MailMessage) {
    let message_id = i64::from(email.id);
    match &email.invitation {
        Some(card) => {
            SHOWN.with(|shown| shown.set(reader.key(), (message_id, card.clone())));
            apply(reader, card);
        }
        None => clear(reader),
    }
}

/// The reader shows no message.
pub(super) fn clear(reader: &ReaderRef) {
    SHOWN.with(|shown| shown.remove(reader.key()));
    let ui = reader.global::<Invitation>();
    ui.set_available(false);
    ui.set_working(false);
}

/// A reader's window closed.
#[cfg(desktop)]
pub(super) fn forget(key: ReaderKey) {
    SHOWN.with(|shown| shown.remove(key));
}

/// The calendar changed: the cards shown follow it.
pub(super) fn refresh() {
    with_controller(|controller| {
        for key in SHOWN.with(PerReader::keys) {
            if let Some(host) = key.host(&controller.app) {
                controller.reload(&host);
            }
        }
    });
}

impl Controller {
    fn core(&self) -> Option<CoreMailSource> {
        self.state.borrow().core.clone()
    }

    /// Accept, Maybe or Decline.
    fn answer(&self, host: &ReaderHost, response: String) {
        let (Some(reader), Some((message_id, card)), Some(core)) =
            (host.upgrade(), shown(host.key()), self.core())
        else {
            return;
        };
        let ui = reader.global::<Invitation>();
        if !ui.get_answerable() || ui.get_working() {
            return;
        }
        ui.set_working(true);
        // A message about one occurrence answers that occurrence alone. One
        // not in the calendar yet joins it with the answer.
        let occurrence = card.recurrence_id;
        self.spawn(
            host,
            async move {
                core.answer_invitation(message_id, &response, occurrence)
                    .await
            },
            |reader, result| {
                if let Err(error) = result {
                    reader.global::<Invitation>().set_working(false);
                    crate::undo::tell(
                        reader.app(),
                        UiMessage::detail("Could not answer: {}", error),
                    );
                }
                with_controller(|controller| controller.reload(&reader.host()));
            },
        );
    }

    /// Add an event only shared with the user to their calendar.
    fn add(&self, host: &ReaderHost) {
        let (Some(reader), Some((message_id, _)), Some(core)) =
            (host.upgrade(), shown(host.key()), self.core())
        else {
            return;
        };
        let ui = reader.global::<Invitation>();
        if !ui.get_addable() || ui.get_working() {
            return;
        }
        ui.set_working(true);
        self.spawn(
            host,
            async move { core.add_invitation(message_id).await },
            |reader, result| {
                if let Err(error) = result {
                    reader.global::<Invitation>().set_working(false);
                    crate::undo::tell(
                        reader.app(),
                        UiMessage::detail("Could not add the event: {}", error),
                    );
                }
                with_controller(|controller| controller.reload(&reader.host()));
            },
        );
    }

    /// Take the cancelled event out of the calendar.
    fn remove(&self, host: &ReaderHost) {
        let (Some(reader), Some((_, card)), Some(core)) =
            (host.upgrade(), shown(host.key()), self.core())
        else {
            return;
        };
        let ui = reader.global::<Invitation>();
        let Some(event_id) = card.event_id.filter(|_| ui.get_removable()) else {
            return;
        };
        if ui.get_working() {
            return;
        }
        ui.set_working(true);
        self.spawn(
            host,
            async move { core.delete_event(event_id).await },
            |reader, result| {
                if let Err(error) = result {
                    reader.global::<Invitation>().set_working(false);
                    crate::undo::tell(
                        reader.app(),
                        UiMessage::detail("Could not remove the event: {}", error),
                    );
                }
                with_controller(|controller| controller.reload(&reader.host()));
            },
        );
    }

    /// Open the calendar on the event, with its details, in the main window.
    fn show_in_calendar(&self, host: &ReaderHost) {
        let (Some(reader), Some((_, card))) = (host.upgrade(), shown(host.key())) else {
            return;
        };
        let Some(event_id) = card.event_id else {
            return;
        };
        let date = event_date(card.starts_at, card.all_day);
        {
            let mut calendar = self.calendar.borrow_mut();
            calendar.selected_date = date;
            calendar.visible_month = crate::calendar::first_of_month(date);
            calendar.reveal = Some(event_id);
        }
        reader.app().set_active_view("calendar".into());
        reader.raise_main();
    }

    /// Read the card a reader shows again, and keep it with its message.
    fn reload(&self, host: &ReaderHost) {
        let key = host.key();
        let (Some((message_id, _)), Some(core)) = (shown(key), self.core()) else {
            return;
        };
        let state = Rc::clone(&self.state);
        self.spawn(
            host,
            async move { Ok::<_, String>(core.invitation_card(message_id).await) },
            move |reader, result| {
                let Ok(card) = result else {
                    return;
                };
                // Kept with the message, a later projection shows it too.
                crate::reader_host::with_messages(&state, key, |messages| {
                    if let Some(message) = messages
                        .iter_mut()
                        .find(|message| i64::from(message.id) == message_id)
                    {
                        message.invitation = card.clone();
                    }
                });
                // The reader may show another message by now.
                if shown(key).is_none_or(|(shown_id, _)| shown_id != message_id) {
                    return;
                }
                match card {
                    Some(card) => {
                        SHOWN.with(|shown| shown.set(key, (message_id, card.clone())));
                        apply(&reader, &card);
                    }
                    None => clear(&reader),
                }
            },
        );
    }

    /// Run `work` on the runtime and `apply` its result to the reader on the
    /// UI thread, while its window is open.
    fn spawn<T: Send + 'static>(
        &self,
        host: &ReaderHost,
        work: impl Future<Output = Result<T, String>> + Send + 'static,
        apply: impl FnOnce(ReaderRef, Result<T, String>) + 'static,
    ) {
        let host = host.clone();
        let task = self.runtime.spawn(work);
        let abort = task.abort_handle();
        if slint::spawn_local(async move {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            if let Some(reader) = host.upgrade() {
                apply(reader, result);
            }
        })
        .is_err()
        {
            abort.abort();
        }
    }
}

/// The day an event is on: all-day events keep their date in UTC.
fn event_date(starts_at: i64, all_day: bool) -> NaiveDate {
    if all_day {
        Utc.timestamp_millis_opt(starts_at)
            .single()
            .map(|value| value.date_naive())
    } else {
        Local
            .timestamp_millis_opt(starts_at)
            .single()
            .map(|value| value.date_naive())
    }
    .unwrap_or_else(|| Local::now().date_naive())
}

fn moment(at: i64, all_day: bool) -> Option<NaiveDateTime> {
    if all_day {
        Utc.timestamp_millis_opt(at)
            .single()
            .map(|value| value.naive_utc())
    } else {
        Local
            .timestamp_millis_opt(at)
            .single()
            .map(|value| value.naive_local())
    }
}

/// When the event takes place, as the calendar puts it: its day, and its
/// times or "All day".
fn when(app: &AppWindow, formats: &DateFormats, card: &InvitationCard) -> String {
    let Some(start) = moment(card.starts_at, card.all_day) else {
        return String::new();
    };
    let end = card
        .ends_at
        .filter(|end| *end > card.starts_at)
        .and_then(|end| moment(end, card.all_day));
    if card.all_day {
        // An all-day event ends at the start of the day after its last.
        let last = end.map_or(start.date(), |end| {
            (end - ChronoDuration::nanoseconds(1)).date()
        });
        if last > start.date() {
            return formats.date_range(start.date(), last);
        }
        return format!(
            "{} · {}",
            formats.date(DateStyle::WeekdayMedium, start.date()),
            app.global::<I18n>().invoke_calendar_all_day()
        );
    }
    let start_time = formats.time(start.hour(), start.minute());
    match end {
        Some(end) if end.date() == start.date() => format!(
            "{} · {}",
            formats.date(DateStyle::WeekdayMedium, start.date()),
            formats.range(&start_time, &formats.time(end.hour(), end.minute()))
        ),
        Some(end) => formats.range(
            &formats.date_time(DateStyle::WeekdayMedium, start),
            &formats.date_time(DateStyle::WeekdayMedium, end),
        ),
        None => format!(
            "{} · {}",
            formats.date(DateStyle::WeekdayMedium, start.date()),
            start_time
        ),
    }
}

fn kind(kind: InvitationKind) -> &'static str {
    match kind {
        InvitationKind::Request => "request",
        InvitationKind::Cancel => "cancel",
        InvitationKind::Reply => "reply",
        InvitationKind::Counter => "counter",
        InvitationKind::DeclineCounter => "declinecounter",
        InvitationKind::Publish => "publish",
        InvitationKind::Refresh => "refresh",
    }
}

/// A person as the card names them: by name, else by address.
fn named(name: Option<&str>, email: &str) -> String {
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(email)
        .to_owned()
}

/// A meeting link the card opens: a web address.
fn web_link(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "https" | "http"))
}

fn apply(reader: &ReaderRef, card: &InvitationCard) {
    let app = reader.app();
    let ui = reader.global::<Invitation>();
    let formats = date_format::current();
    let when = when(app, &formats, card);
    let tile = (card.starts_at != 0)
        .then(|| moment(card.starts_at, card.all_day))
        .flatten();
    ui.set_kind(kind(card.kind).into());
    ui.set_title(
        card.title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .map(Into::into)
            .unwrap_or_else(|| app.global::<I18n>().invoke_untitled_event()),
    );
    ui.set_when(when.into());
    ui.set_tile_month(
        tile.map(|tile| formats.month(tile.month0(), true).to_uppercase())
            .unwrap_or_default()
            .into(),
    );
    ui.set_tile_day(
        tile.map(|tile| tile.day().to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.set_location(card.location.clone().unwrap_or_default().into());
    ui.set_join_url(
        card.join_url
            .clone()
            .filter(|url| web_link(url))
            .unwrap_or_default()
            .into(),
    );
    ui.set_organizer(
        card.organizer
            .as_ref()
            .map(|organizer| named(organizer.name.as_deref(), &organizer.email))
            .unwrap_or_default()
            .into(),
    );
    // Answers, proposals and requests for the latest version come from one
    // guest, the only one they list.
    let from_guest = matches!(
        card.kind,
        InvitationKind::Reply | InvitationKind::Counter | InvitationKind::Refresh
    );
    let guest = card.answer.as_ref().or(card.attendees.first());
    ui.set_answered_by(
        guest
            .filter(|_| from_guest)
            .map(|guest| named(guest.name.as_deref(), &guest.email))
            .unwrap_or_default()
            .into(),
    );
    ui.set_their_answer(
        card.answer
            .as_ref()
            .and_then(|answer| answer.partstat.clone())
            .unwrap_or_default()
            .into(),
    );
    // The other guests by name. The organizer is named above, and the user
    // knows they are invited.
    let organizer = card
        .organizer
        .as_ref()
        .map(|organizer| organizer.email.as_str());
    let others: Vec<String> = card
        .attendees
        .iter()
        .filter(|_| !from_guest)
        .filter(|guest| {
            !organizer.is_some_and(|organizer| organizer.eq_ignore_ascii_case(&guest.email))
                && !card
                    .invited_as
                    .as_deref()
                    .is_some_and(|me| me.eq_ignore_ascii_case(&guest.email))
        })
        .map(|guest| named(guest.name.as_deref(), &guest.email))
        .collect();
    ui.set_guest_names(
        others
            .iter()
            .take(NAMED_GUESTS)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
            .into(),
    );
    ui.set_more_guests(i32::try_from(others.len().saturating_sub(NAMED_GUESTS)).unwrap_or(0));
    let accepted = card
        .attendees
        .iter()
        .filter(|guest| guest.partstat.as_deref() == Some("ACCEPTED"))
        .count();
    ui.set_accepted(if from_guest {
        0
    } else {
        i32::try_from(accepted).unwrap_or(i32::MAX)
    });
    ui.set_conflict(card.conflicts.first().cloned().unwrap_or_default().into());
    ui.set_more_conflicts(i32::try_from(card.conflicts.len().saturating_sub(1)).unwrap_or(0));
    ui.set_my_answer(card.my_answer.clone().unwrap_or_default().into());
    ui.set_organized(card.organized);
    ui.set_invited(card.invited_as.is_some());
    ui.set_out_of_date(card.out_of_date);
    ui.set_cancelled(card.cancelled);
    ui.set_kept_out(card.kept_out);
    ui.set_in_calendar(card.event_id.is_some());
    ui.set_repeats(card.repeats);
    ui.set_occurrence(card.occurrence);
    // Local mail has no account to answer from. An invitation that isn't
    // in the calendar yet joins it when answered.
    let local_mail = reader.global::<LocalMailUi>().get_selected();
    let answerable = card.kind == InvitationKind::Request
        && !card.organized
        && !card.cancelled
        && !card.out_of_date
        && !card.kept_out
        && !local_mail;
    ui.set_answerable(answerable);
    ui.set_held(answerable && card.event_id.is_none());
    // An event only shared with the user, which they add themselves.
    ui.set_addable(
        card.kind == InvitationKind::Publish
            && card.event_id.is_none()
            && !card.cancelled
            && !card.kept_out,
    );
    // Taking a cancelled occurrence out would take the whole series.
    ui.set_removable(
        card.cancelled && card.event_id.is_some() && !card.organized && !card.occurrence,
    );
    ui.set_working(false);
    ui.set_available(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_links_are_web_addresses() {
        assert!(web_link("https://meet.example.com/abc-defg-hij"));
        assert!(web_link("http://zoom.example/j/1"));
        assert!(!web_link("javascript:alert(1)"));
        assert!(!web_link("file:///etc/passwd"));
        assert!(!web_link("Room 4"));
    }

    #[test]
    fn people_are_named_by_name_else_address() {
        assert_eq!(named(Some(" Alice "), "alice@example.com"), "Alice");
        assert_eq!(named(Some(""), "alice@example.com"), "alice@example.com");
        assert_eq!(named(None, "alice@example.com"), "alice@example.com");
    }
}
