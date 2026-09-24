//! Date sections for the virtualized mailbox list.

use crate::{EmailRow, MailListEntry, mail::MailMessage, mail_view_model::same_email_row};
use chrono::{DateTime, Datelike, Duration, Local, Utc};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct MailGroupState {
    pub collapsed: HashSet<String>,
    pub opening: HashSet<String>,
    pub closing: HashSet<String>,
    generations: HashMap<String, u64>,
    next_generation: u64,
}

impl MailGroupState {
    pub fn clear(&mut self) {
        self.collapsed.clear();
        self.opening.clear();
        self.closing.clear();
        self.generations.clear();
        self.next_generation = self.next_generation.wrapping_add(1);
    }

    /// Returns the generation and whether the group is now expanded.
    pub fn toggle(&mut self, key: &str) -> (u64, bool) {
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.generations.insert(key.to_owned(), generation);
        let expanded = self.collapsed.remove(key);
        if expanded {
            self.closing.remove(key);
            self.opening.insert(key.to_owned());
        } else {
            self.collapsed.insert(key.to_owned());
            self.opening.remove(key);
            self.closing.insert(key.to_owned());
        }
        (generation, expanded)
    }

    pub fn reveal(&mut self, key: &str) {
        if self.collapsed.remove(key) {
            self.next_generation = self.next_generation.wrapping_add(1);
            self.generations.insert(key.to_owned(), self.next_generation);
            self.closing.remove(key);
            self.opening.remove(key);
        }
    }

    pub fn finish_transition(&mut self, key: &str, generation: u64) -> bool {
        if self.generations.get(key).copied() != Some(generation) {
            return false;
        }
        self.opening.remove(key) | self.closing.remove(key)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DateGroup {
    key: String,
    kind: &'static str,
    month: i32,
    year: i32,
}

fn date_group(timestamp_ms: i64, now: DateTime<Local>) -> DateGroup {
    let Some(date) = DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .map(|date| date.with_timezone(&Local))
        .filter(|_| timestamp_ms > 0)
    else {
        return DateGroup {
            key: "unknown".into(),
            kind: "unknown",
            month: 0,
            year: 0,
        };
    };
    let day = date.date_naive();
    let today = now.date_naive();
    let yesterday = today - Duration::days(1);
    let this_week = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let last_week = this_week - Duration::days(7);
    let (previous_year, previous_month) = if now.month() == 1 {
        (now.year() - 1, 12)
    } else {
        (now.year(), now.month() - 1)
    };

    let (key, kind) = if day >= today {
        (format!("day:{today}"), "today")
    } else if day == yesterday {
        (format!("day:{yesterday}"), "yesterday")
    } else if day >= this_week {
        (format!("week:{this_week}"), "this-week")
    } else if day >= last_week {
        (format!("week:{last_week}"), "last-week")
    } else if date.year() == now.year() && date.month() == now.month() {
        (format!("month:{}-{:02}", date.year(), date.month()), "this-month")
    } else if date.year() == previous_year && date.month() == previous_month {
        (format!("month:{}-{:02}", date.year(), date.month()), "last-month")
    } else {
        (format!("month:{}-{:02}", date.year(), date.month()), "month")
    };
    DateGroup {
        key,
        kind,
        month: date.month() as i32,
        year: if kind == "month" && date.year() != now.year() {
            date.year()
        } else {
            0
        },
    }
}

pub(super) fn mail_group_key(timestamp_ms: i64, now: DateTime<Local>) -> String {
    date_group(timestamp_ms, now).key
}

pub(super) fn project_mail_list(
    messages: &[MailMessage],
    rows: &[EmailRow],
    groups: &MailGroupState,
    group_by_date: bool,
    now: DateTime<Local>,
) -> Vec<MailListEntry> {
    debug_assert_eq!(messages.len(), rows.len());
    // Legacy warm-start snapshots have display text but no timestamps. Keep
    // their short-lived preview flat until the core supplies real dates.
    if !group_by_date || messages.iter().all(|message| message.date_ms <= 0) {
        return rows
            .iter()
            .enumerate()
            .map(|(index, email)| message_entry(email, index, "", true, false))
            .collect();
    }

    // Keep source order within each section. A late sync can briefly deliver
    // rows out of date order, so gather by key and sort the sections by their
    // latest message before flattening.
    let mut ordered = Vec::<(DateGroup, i64, Vec<&EmailRow>)>::new();
    let mut group_indices = HashMap::<String, usize>::new();
    for (message, row) in messages.iter().zip(rows) {
        let group = date_group(message.date_ms, now);
        if let Some(&index) = group_indices.get(&group.key) {
            let (_, latest, group_rows) = &mut ordered[index];
            *latest = (*latest).max(message.date_ms);
            group_rows.push(row);
        } else {
            group_indices.insert(group.key.clone(), ordered.len());
            ordered.push((group, message.date_ms, vec![row]));
        }
    }
    ordered.sort_by_key(|(_, latest, _)| std::cmp::Reverse(*latest));

    let mut entries = Vec::with_capacity(rows.len() + ordered.len());
    let mut display_index = 0;
    for (group, _, group_rows) in ordered {
        let collapsed = groups.collapsed.contains(&group.key);
        entries.push(MailListEntry {
            is_header: true,
            group_key: group.key.clone().into(),
            group_kind: group.kind.into(),
            group_month: group.month,
            group_year: group.year,
            group_count: i32::try_from(group_rows.len()).unwrap_or(i32::MAX),
            expanded: !collapsed,
            show_row: false,
            reveal_row: false,
            email_index: -1,
            email: EmailRow::default(),
        });
        for row in group_rows {
            if !collapsed || groups.closing.contains(&group.key) {
                entries.push(message_entry(
                    row,
                    display_index,
                    &group.key,
                    !collapsed,
                    groups.opening.contains(&group.key),
                ));
            }
            display_index += 1;
        }
    }
    entries
}

fn message_entry(
    row: &EmailRow,
    index: usize,
    group_key: &str,
    show_row: bool,
    reveal_row: bool,
) -> MailListEntry {
    MailListEntry {
        is_header: false,
        group_key: group_key.into(),
        group_kind: "".into(),
        group_month: 0,
        group_year: 0,
        group_count: 0,
        expanded: false,
        show_row,
        reveal_row,
        email_index: i32::try_from(index).unwrap_or(i32::MAX),
        email: row.clone(),
    }
}

pub(super) fn same_mail_list_entry(a: &MailListEntry, b: &MailListEntry) -> bool {
    a.is_header == b.is_header
        && a.group_key == b.group_key
        && a.group_kind == b.group_kind
        && a.group_month == b.group_month
        && a.group_year == b.group_year
        && a.group_count == b.group_count
        && a.expanded == b.expanded
        && a.show_row == b.show_row
        && a.reveal_row == b.reveal_row
        && a.email_index == b.email_index
        && (a.is_header || same_email_row(&a.email, &b.email))
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum MailListEntryKey {
    Section(slint::SharedString),
    Message(i32),
}

pub(super) fn mail_list_entry_key(entry: &MailListEntry) -> MailListEntryKey {
    if entry.is_header {
        MailListEntryKey::Section(entry.group_key.clone())
    } else {
        MailListEntryKey::Message(entry.email.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    fn local_day(year: i32, month: u32, day: u32) -> DateTime<Local> {
        let naive = NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        Local.from_local_datetime(&naive).single().unwrap()
    }

    #[test]
    fn calendar_boundaries_keep_yesterday_and_last_week_distinct() {
        let now = local_day(2026, 1, 5); // Monday, after a year boundary.
        assert_eq!(date_group(local_day(2026, 1, 5).timestamp_millis(), now).kind, "today");
        assert_eq!(date_group(local_day(2026, 1, 4).timestamp_millis(), now).kind, "yesterday");
        assert_eq!(date_group(local_day(2025, 12, 30).timestamp_millis(), now).kind, "last-week");
        assert_eq!(date_group(local_day(2025, 12, 20).timestamp_millis(), now).kind, "last-month");
        assert_eq!(date_group(local_day(2025, 11, 20).timestamp_millis(), now).year, 2025);
    }

    #[test]
    fn fast_reversal_does_not_finish_a_newer_transition() {
        let mut groups = MailGroupState::default();
        let (first, expanded) = groups.toggle("day:2026-01-05");
        assert!(!expanded);
        let (second, expanded) = groups.toggle("day:2026-01-05");
        assert!(expanded);
        assert!(!groups.finish_transition("day:2026-01-05", first));
        assert!(groups.opening.contains("day:2026-01-05"));
        assert!(groups.finish_transition("day:2026-01-05", second));

        let (before_scope_change, _) = groups.toggle("day:2026-01-05");
        groups.clear();
        let (after_scope_change, _) = groups.toggle("day:2026-01-05");
        assert_ne!(before_scope_change, after_scope_change);
        assert!(!groups.finish_transition("day:2026-01-05", before_scope_change));
        assert!(groups.finish_transition("day:2026-01-05", after_scope_change));
    }

    #[test]
    fn collapsed_sections_keep_headers_and_other_messages_in_order() {
        let now = local_day(2026, 1, 5);
        let mut messages = crate::mail::fixture_messages();
        messages.truncate(3);
        messages[0].date_ms = now.timestamp_millis();
        messages[1].date_ms = now.timestamp_millis();
        messages[2].date_ms = local_day(2025, 12, 30).timestamp_millis();
        let rows = messages
            .iter()
            .map(|message| EmailRow { id: message.id, ..Default::default() })
            .collect::<Vec<_>>();
        let today_key = mail_group_key(messages[0].date_ms, now);
        let mut groups = MailGroupState::default();

        let expanded = project_mail_list(&messages, &rows, &groups, true, now);
        assert_eq!(expanded.iter().filter(|entry| entry.is_header).count(), 2);
        assert_eq!(expanded[0].group_count, 2);
        assert_eq!(expanded[1].email_index, 0);
        assert_eq!(expanded[2].email_index, 1);
        assert_eq!(expanded[4].email.id, messages[2].id);

        let (generation, is_open) = groups.toggle(&today_key);
        assert!(!is_open);
        let closing = project_mail_list(&messages, &rows, &groups, true, now);
        assert!(!closing[1].show_row);
        assert_eq!(closing.len(), 5);

        assert!(groups.finish_transition(&today_key, generation));
        let closed = project_mail_list(&messages, &rows, &groups, true, now);
        assert_eq!(closed.len(), 3);
        assert_eq!(closed[0].group_count, 2);
        assert!(!closed[0].expanded);
        assert_eq!(closed[2].email_index, 2);

        groups.reveal(&today_key);
        let revealed = project_mail_list(&messages, &rows, &groups, true, now);
        assert_eq!(revealed.len(), 5);
        assert!(revealed[0].expanded);
        assert!(!revealed[1].reveal_row);

        let reordered_messages = vec![messages[2].clone(), messages[0].clone(), messages[1].clone()];
        let reordered_rows = vec![rows[2].clone(), rows[0].clone(), rows[1].clone()];
        let reordered = project_mail_list(&reordered_messages, &reordered_rows, &groups, true, now);
        assert_eq!(reordered[1].email.id, messages[0].id);
        assert_eq!(reordered[1].email_index, 0);
        assert_eq!(reordered[4].email.id, messages[2].id);
        assert_eq!(reordered[4].email_index, 2);
    }

    #[test]
    fn ungrouped_mode_ignores_section_state() {
        let now = local_day(2026, 1, 5);
        let mut messages = crate::mail::fixture_messages();
        messages.truncate(2);
        let mut groups = MailGroupState::default();
        groups.toggle(&mail_group_key(messages[0].date_ms, now));
        let rows = messages
            .iter()
            .map(|message| EmailRow { id: message.id, ..Default::default() })
            .collect::<Vec<_>>();
        let entries = project_mail_list(&messages, &rows, &groups, false, now);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| !entry.is_header && entry.show_row));
    }
}
