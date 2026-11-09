//! The time picker Snooze, Schedule send and reply reminders share.
//!
//! Snooze: mail leaves the inbox until a time the user picks, then comes back
//! to the top of it. Send later: a message waits until a time, then goes out.
//! Remind me if no reply: unless someone replies by a time, the conversation
//! comes back to the top of the inbox. With Flectar Mail Sync all three
//! follow the user to every device, and any of the user's devices sends a
//! scheduled message.

use super::*;
use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike, Weekday};
use slint::SharedString;

fn at_hour(date: NaiveDate, hour: u32) -> Option<chrono::DateTime<Local>> {
    Local
        .from_local_datetime(&date.and_hms_opt(hour, 0, 0)?)
        .earliest()
}

/// The preset snooze times, as Gmail offers them: later today while the day
/// is young, tomorrow morning, Saturday morning, and next Monday morning.
fn snooze_presets(now: chrono::DateTime<Local>) -> Vec<(&'static str, chrono::DateTime<Local>)> {
    let today = now.date_naive();
    let days = |count: u32| today + chrono::Duration::days(i64::from(count));
    let mut presets = Vec::new();
    if now.hour() < 17
        && let Some(evening) = at_hour(today, 18)
    {
        presets.push(("later-today", evening));
    }
    if let Some(tomorrow) = at_hour(days(1), 8) {
        presets.push(("tomorrow", tomorrow));
    }
    let weekday = today.weekday().num_days_from_monday();
    // Saturday, unless it is today or tomorrow already.
    let to_saturday = (Weekday::Sat.num_days_from_monday() + 7 - weekday) % 7;
    if to_saturday >= 2
        && let Some(saturday) = at_hour(days(to_saturday), 8)
    {
        presets.push(("weekend", saturday));
    }
    // The coming Monday, unless that is tomorrow.
    let to_monday = 7 - weekday;
    if to_monday >= 2
        && let Some(monday) = at_hour(days(to_monday), 8)
    {
        presets.push(("next-week", monday));
    }
    presets
}

/// The preset send times, as Gmail offers them: tomorrow morning and
/// afternoon, and Monday morning unless that is tomorrow.
fn send_presets(now: chrono::DateTime<Local>) -> Vec<(&'static str, chrono::DateTime<Local>)> {
    let today = now.date_naive();
    let tomorrow = today + chrono::Duration::days(1);
    let mut presets = Vec::new();
    if let Some(morning) = at_hour(tomorrow, 8) {
        presets.push(("tomorrow-morning", morning));
    }
    if let Some(afternoon) = at_hour(tomorrow, 13) {
        presets.push(("tomorrow-afternoon", afternoon));
    }
    let to_monday = 7 - today.weekday().num_days_from_monday();
    if to_monday >= 2
        && let Some(monday) = at_hour(today + chrono::Duration::days(i64::from(to_monday)), 8)
    {
        presets.push(("monday-morning", monday));
    }
    presets
}

/// The preset reminder times: tomorrow, in three days and in a week, each
/// in the morning.
fn remind_presets(now: chrono::DateTime<Local>) -> Vec<(&'static str, chrono::DateTime<Local>)> {
    let today = now.date_naive();
    [("tomorrow", 1), ("in-3-days", 3), ("in-a-week", 7)]
        .into_iter()
        .filter_map(|(kind, days)| Some((kind, at_hour(today + chrono::Duration::days(days), 8)?)))
        .collect()
}

/// When a preset or picked time is, as the picker and the status line say
/// it: a time today, otherwise a date and a time.
pub(super) fn when_label(at: chrono::DateTime<Local>, now: chrono::DateTime<Local>) -> String {
    mail_view_model::upcoming_time(at.timestamp_millis(), &now)
}

/// A picked date (YYYY-MM-DD) and time in this device's format, as a local
/// time; `Err` names what is wrong with it.
fn picked(
    date: &str,
    time: &str,
    now: chrono::DateTime<Local>,
) -> Result<chrono::DateTime<Local>, &'static str> {
    let date = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").map_err(|_| "invalid")?;
    let (hour, minute) = date_format::current()
        .parse_time(time.trim())
        .ok_or("invalid")?;
    let at = Local
        .from_local_datetime(&date.and_hms_opt(hour, minute, 0).ok_or("invalid")?)
        .earliest()
        .ok_or("invalid")?;
    if at <= now + chrono::Duration::minutes(1) {
        return Err("past");
    }
    Ok(at)
}

/// What the open picker's time is for.
#[derive(Default)]
enum Picking {
    #[default]
    Nothing,
    /// Snooze these list rows.
    Snooze(Vec<i32>),
    /// When a composer's message goes out, or reminds the user.
    Composer(composer::Asking),
    /// Move the open scheduled draft.
    Reschedule(i32),
    /// Remind the user about the open conversation.
    RemindOpen(i32),
}

#[derive(Default)]
pub(super) struct Picker {
    picking: Picking,
    /// The preset times; `None` drops the reminder already set.
    presets: Vec<Option<chrono::DateTime<Local>>>,
}

/// A window's picker: what the question it shows is for. The main window
/// has one, and so does each message in a window of its own.
pub(super) type PickerState = Rc<RefCell<Picker>>;

thread_local! {
    static MAIN_PICKER: RefCell<Option<PickerState>> = const { RefCell::new(None) };
}

/// The main window's picker, which its composer asks in too.
pub(super) fn main_picker() -> Option<PickerState> {
    MAIN_PICKER.with(|slot| slot.borrow().clone())
}

/// Open the picker `ui` shows for `picking` with its preset times, and with
/// "Don't remind me" last when `droppable`. `time_placeholder` shows how a
/// time is typed here.
fn open(
    ui: &WhenUi<'_>,
    time_placeholder: SharedString,
    picker: &RefCell<Picker>,
    picking: Picking,
    presets: Vec<(&'static str, chrono::DateTime<Local>)>,
    droppable: bool,
) {
    let now = Local::now();
    ui.set_purpose(
        match &picking {
            Picking::Snooze(_) => "snooze",
            Picking::RemindOpen(_) => "remind",
            Picking::Composer(asking) if asking.reminds() => "remind",
            _ => "send",
        }
        .into(),
    );
    let mut choices = presets
        .iter()
        .map(|(kind, at)| WhenChoice {
            kind: (*kind).into(),
            when: when_label(*at, now).into(),
        })
        .collect::<Vec<_>>();
    let mut times = presets
        .into_iter()
        .map(|(_, at)| Some(at))
        .collect::<Vec<_>>();
    if droppable {
        choices.push(WhenChoice {
            kind: "none".into(),
            when: Default::default(),
        });
        times.push(None);
    }
    ui.set_choices(ModelRc::new(VecModel::from(choices)));
    let tomorrow = now.date_naive() + chrono::Duration::days(1);
    ui.set_date(tomorrow.format("%Y-%m-%d").to_string().into());
    ui.set_time(date_format::current().time(8, 0).into());
    ui.set_time_placeholder(time_placeholder);
    ui.set_error(Default::default());
    *picker.borrow_mut() = Picker {
        picking,
        presets: times,
    };
    ui.set_open(true);
}

/// Ask in `ui` when a composer's message goes out, or when it reminds the
/// user, with "Don't remind me" last when `droppable`.
pub(super) fn ask(
    ui: &WhenUi<'_>,
    time_placeholder: SharedString,
    picker: &PickerState,
    asking: composer::Asking,
    droppable: bool,
) {
    let now = Local::now();
    let presets = if asking.reminds() {
        remind_presets(now)
    } else {
        send_presets(now)
    };
    open(
        ui,
        time_placeholder,
        picker,
        Picking::Composer(asking),
        presets,
        droppable,
    );
}

/// The picker's answer to its open question: the preset at `index`, or
/// "Don't remind me" as `None`. The question is answered only once.
fn chosen(
    picker: &RefCell<Picker>,
    index: i32,
) -> Option<(Picking, Option<chrono::DateTime<Local>>)> {
    let Picker { picking, presets } = std::mem::take(&mut *picker.borrow_mut());
    let at = usize::try_from(index)
        .ok()
        .and_then(|index| presets.get(index).copied())?;
    Some((picking, at))
}

/// The picker of a message's window of its own, which only its composer
/// asks in.
#[cfg(desktop)]
pub(super) fn attach(window: &ComposerWindow) -> PickerState {
    let picker = PickerState::default();
    let ui = window.global::<WhenUi>();
    let choose_picker = Rc::clone(&picker);
    ui.on_choose(move |index| {
        if let Some((Picking::Composer(asking), at)) = chosen(&choose_picker, index) {
            composer::answered(asking, at);
        }
    });
    let weak = window.as_weak();
    let custom_picker = Rc::clone(&picker);
    ui.on_choose_custom(move |date, time| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let ui = window.global::<WhenUi>();
        match picked(&date, &time, Local::now()) {
            Ok(at) => {
                ui.set_open(false);
                let Picker { picking, .. } = std::mem::take(&mut *custom_picker.borrow_mut());
                if let Picking::Composer(asking) = picking {
                    composer::answered(asking, Some(at));
                }
            }
            Err(error) => ui.set_error(error.into()),
        }
    });
    picker
}

pub(super) fn register(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    let picker = PickerState::default();
    MAIN_PICKER.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&picker)));
    let snooze_ui = app.global::<SnoozeUi>();
    let send_ui = app.global::<SendLaterUi>();
    let when_ui = app.global::<WhenUi>();
    let reminder_ui = app.global::<ReminderUi>();

    let weak = app.as_weak();
    let request_state = state.clone();
    let request_picker = picker.clone();
    snooze_ui.on_request(move |source, id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let ids = {
            let state = request_state.borrow();
            if !state.using_core {
                return;
            }
            move_picker::picker_ids(&state, &source, id)
        };
        if ids.is_empty() {
            return;
        }
        open(
            &app.global::<WhenUi>(),
            app.get_time_format_sample(),
            &request_picker,
            Picking::Snooze(ids),
            snooze_presets(Local::now()),
            false,
        );
    });

    let weak = app.as_weak();
    let unsnooze_state = state.clone();
    snooze_ui.on_unsnooze(move |source, id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let ids = {
            let state = unsnooze_state.borrow();
            move_picker::picker_ids(&state, &source, id)
        };
        snooze(&app, &unsnooze_state, &ids, None);
    });

    let weak = app.as_weak();
    let change_state = state.clone();
    let change_picker = picker.clone();
    send_ui.on_change_time(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let Some(id) = change_state.borrow().selected_id else {
            return;
        };
        open(
            &app.global::<WhenUi>(),
            app.get_time_format_sample(),
            &change_picker,
            Picking::Reschedule(id),
            send_presets(Local::now()),
            false,
        );
    });

    let weak = app.as_weak();
    let remind_state = state.clone();
    let remind_open_picker = picker.clone();
    reminder_ui.on_remind_open(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let Some(id) = remind_state.borrow().selected_id else {
            return;
        };
        let set = !app.global::<ReminderUi>().get_open_remind_time().is_empty();
        open(
            &app.global::<WhenUi>(),
            app.get_time_format_sample(),
            &remind_open_picker,
            Picking::RemindOpen(id),
            remind_presets(Local::now()),
            set,
        );
    });

    let weak = app.as_weak();
    let dismiss_state = state.clone();
    let dismiss_runtime = runtime.clone();
    reminder_ui.on_dismiss(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let Some((core, row)) = open_row(&dismiss_state) else {
            return;
        };
        dismiss_reminder(&app, &dismiss_state, &dismiss_runtime, &core, &row);
    });

    let weak = app.as_weak();
    let now_state = state.clone();
    let now_runtime = runtime.clone();
    send_ui.on_send_now(move || {
        let Some((core, row)) = open_row(&now_state) else {
            return;
        };
        change_send(
            &weak,
            &now_state,
            &now_runtime,
            Box::pin(async move { core.send_scheduled_row_now(&row).await }),
            |app, sent| {
                app.set_render_status(match sent {
                    Ok(true) => UiMessage::plain("Sending message…"),
                    // Sent, or cancelled on another device, meanwhile.
                    Ok(false) => UiMessage::plain("This message is no longer scheduled."),
                    Err(error) => UiMessage::detail("Could not send the message: {}", error),
                })
            },
        );
    });

    // As in Gmail, the message opens as a draft to edit: Continue editing
    // cancels the send first.
    let weak = app.as_weak();
    send_ui.on_cancel(move || {
        if let Some(app) = weak.upgrade() {
            app.invoke_compose_message_action("edit_draft".into());
        }
    });

    let weak = app.as_weak();
    let choose_state = state.clone();
    let choose_runtime = runtime.clone();
    let choose_picker = picker.clone();
    when_ui.on_choose(move |index| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let Some((picking, at)) = chosen(&choose_picker, index) else {
            return;
        };
        match at {
            Some(at) => picked_for(&app, &choose_state, &choose_runtime, picking, at),
            None => dropped_for(&app, &choose_state, &choose_runtime, picking),
        }
    });

    let weak = app.as_weak();
    let custom_state = state.clone();
    let custom_runtime = runtime.clone();
    when_ui.on_choose_custom(move |date, time| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let ui = app.global::<WhenUi>();
        match picked(&date, &time, Local::now()) {
            Ok(at) => {
                ui.set_open(false);
                let Picker { picking, .. } = std::mem::take(&mut *picker.borrow_mut());
                picked_for(&app, &custom_state, &custom_runtime, picking, at);
            }
            Err(error) => ui.set_error(error.into()),
        }
    });
}

/// Send a scheduled message now, or move it, off the UI thread: the mail
/// server may be holding it, and letting go can take a while. `done` reports
/// the result, and the list shows the change. One change runs at a time.
fn change_send<T: Send + 'static>(
    app: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    change: documents::DocumentFuture<T>,
    done: impl FnOnce(&AppWindow, Result<T, String>) + 'static,
) {
    if !state.borrow_mut().start_send_change() {
        return;
    }
    let app = app.clone();
    let state = state.clone();
    let list_runtime = runtime.clone();
    documents::run_request(runtime, change, move |result| {
        state.borrow_mut().changing_send = false;
        let Some(app) = app.upgrade() else {
            return;
        };
        let changed = result.is_ok();
        done(&app, result);
        if changed {
            refresh(&app, &state, &list_runtime);
        }
    });
}

/// The open message's row, and the core to act on it with.
fn open_row(state: &Rc<RefCell<InboxState>>) -> Option<(CoreMailSource, MailMessage)> {
    let id = state.borrow().selected_id?;
    row_by_id(state, id)
}

fn refresh(app: &AppWindow, state: &Rc<RefCell<InboxState>>, runtime: &tokio::runtime::Handle) {
    let _ = refresh_from_source(app, state, runtime, true, &[]);
}

/// A row of the open conversation, and the core to act on it with.
fn row_by_id(state: &Rc<RefCell<InboxState>>, id: i32) -> Option<(CoreMailSource, MailMessage)> {
    let state = state.borrow();
    let row = state
        .messages
        .iter()
        .find(|message| message.id == id)?
        .clone();
    Some((state.core.clone()?, row))
}

fn dismiss_reminder(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    core: &CoreMailSource,
    row: &MailMessage,
) {
    match runtime.block_on(core.dismiss_reply_reminder(row)) {
        Ok(()) => {
            app.set_render_status(UiMessage::plain("Reminder removed."));
            refresh(app, state, runtime);
        }
        Err(error) => {
            app.set_render_status(UiMessage::detail("Could not set the reminder: {}", error));
        }
    }
}

/// Act on "Don't remind me" for `picking`.
fn dropped_for(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    picking: Picking,
) {
    match picking {
        Picking::Composer(asking) => composer::answered(asking, None),
        Picking::RemindOpen(id) => {
            if let Some((core, row)) = row_by_id(state, id) {
                dismiss_reminder(app, state, runtime, &core, &row);
            }
        }
        _ => {}
    }
}

/// Act on the time picked for `picking`.
fn picked_for(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    picking: Picking,
    at: chrono::DateTime<Local>,
) {
    match picking {
        Picking::Nothing => {}
        Picking::Snooze(ids) => snooze(app, state, &ids, Some(at)),
        Picking::Composer(asking) => composer::answered(asking, Some(at)),
        Picking::RemindOpen(id) => {
            let Some((core, row)) = row_by_id(state, id) else {
                return;
            };
            match runtime.block_on(core.set_reply_reminder(&row, at.timestamp_millis())) {
                Ok(()) => {
                    app.set_render_status(UiMessage::detail(
                        "Reminds you {} if nobody replies",
                        when_label(at, Local::now()),
                    ));
                    refresh(app, state, runtime);
                }
                Err(error) => {
                    app.set_render_status(UiMessage::detail(
                        "Could not set the reminder: {}",
                        error,
                    ));
                }
            }
        }
        Picking::Reschedule(id) => {
            let Some((core, row)) = row_by_id(state, id) else {
                return;
            };
            change_send(
                &app.as_weak(),
                state,
                runtime,
                Box::pin(async move { core.reschedule_row(&row, at.timestamp_millis()).await }),
                move |app, moved| {
                    app.set_render_status(match moved {
                        Ok(()) => {
                            UiMessage::detail("Scheduled to send {}", when_label(at, Local::now()))
                        }
                        Err(error) => {
                            UiMessage::detail("Could not schedule the message: {}", error)
                        }
                    })
                },
            );
        }
    }
}

/// Snooze the mail until `at`, or end its snooze; the rows leave the list
/// at once.
fn snooze(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    ids: &[i32],
    at: Option<chrono::DateTime<Local>>,
) {
    let operations = {
        let state = state.borrow();
        ids.iter()
            .filter_map(|id| state.messages.iter().find(|message| message.id == *id))
            .filter_map(|message| Some((message.id, MailTarget::of(message)?)))
            .map(|(id, target)| {
                (
                    id,
                    target,
                    mail_work::Operation::Snooze(at.map(|at| at.timestamp_millis())),
                )
            })
            .collect::<Vec<_>>()
    };
    let Some(core) = state.borrow().core.clone() else {
        return;
    };
    match mail_work::enqueue(state, core, operations) {
        Ok(()) => {
            if let Some(at) = at {
                app.set_render_status(UiMessage::detail(
                    "Snoozed until {}",
                    when_label(at, Local::now()),
                ));
            }
        }
        Err(error) => {
            app.set_render_status(UiMessage::detail("Could not snooze mail: {}", error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(year: i32, month: u32, day: u32, hour: u32) -> chrono::DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, 0, 0)
            .unwrap()
    }

    fn kinds(presets: Vec<(&'static str, chrono::DateTime<Local>)>) -> Vec<&'static str> {
        presets.into_iter().map(|(kind, _)| kind).collect()
    }

    #[test]
    fn snooze_presets_follow_the_day_and_week() {
        // Wednesday morning: all four.
        assert_eq!(
            kinds(snooze_presets(local(2026, 11, 4, 9))),
            ["later-today", "tomorrow", "weekend", "next-week"]
        );
        let wednesday = snooze_presets(local(2026, 11, 4, 9));
        assert_eq!(wednesday[2].1, local(2026, 11, 7, 8));
        assert_eq!(wednesday[3].1, local(2026, 11, 9, 8));
        // Friday evening: Saturday is tomorrow already.
        assert_eq!(
            kinds(snooze_presets(local(2026, 11, 6, 19))),
            ["tomorrow", "next-week"]
        );
        // Sunday: Monday is tomorrow.
        assert_eq!(
            kinds(snooze_presets(local(2026, 11, 8, 10))),
            ["later-today", "tomorrow", "weekend"]
        );
    }

    #[test]
    fn send_presets_follow_gmail() {
        // Wednesday: tomorrow morning and afternoon, and Monday morning.
        let wednesday = send_presets(local(2026, 11, 4, 9));
        assert_eq!(
            kinds(send_presets(local(2026, 11, 4, 9))),
            ["tomorrow-morning", "tomorrow-afternoon", "monday-morning"]
        );
        assert_eq!(wednesday[1].1, local(2026, 11, 5, 13));
        assert_eq!(wednesday[2].1, local(2026, 11, 9, 8));
        // Sunday: Monday morning is tomorrow morning.
        assert_eq!(
            kinds(send_presets(local(2026, 11, 8, 10))),
            ["tomorrow-morning", "tomorrow-afternoon"]
        );
    }

    #[test]
    fn reminder_presets_wait_a_day_three_days_and_a_week() {
        let presets = remind_presets(local(2026, 11, 4, 9));
        assert_eq!(
            kinds(remind_presets(local(2026, 11, 4, 9))),
            ["tomorrow", "in-3-days", "in-a-week"]
        );
        assert_eq!(presets[0].1, local(2026, 11, 5, 8));
        assert_eq!(presets[1].1, local(2026, 11, 7, 8));
        assert_eq!(presets[2].1, local(2026, 11, 11, 8));
    }

    #[test]
    fn a_picked_time_must_be_ahead() {
        let now = local(2026, 11, 4, 9);
        assert_eq!(
            picked("2026-11-05", "08:30", now),
            Ok(local(2026, 11, 5, 8) + chrono::Duration::minutes(30))
        );
        assert_eq!(picked("2026-11-03", "08:00", now), Err("past"));
        assert_eq!(picked("11/05/2026", "08:00", now), Err("invalid"));
        assert_eq!(picked("2026-11-05", "later", now), Err("invalid"));
    }
}
