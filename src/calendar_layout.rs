//! Where events sit in the calendar's day grids: the bars of the month view
//! and of the week view's all-day strip, and the boxes in the week's hours.
//!
//! An event lasting several days is drawn as one bar per week row, cut where
//! the row ends. Bars stack in lanes, each taking the first lane that is free
//! on every day it covers, so an event keeps one lane across its row. Events
//! running several days go first, then by start, longer first, all-day before
//! timed. Timed events that overlap share their day column side by side, and
//! each widens over the lanes to its right that stay free while it runs.
//!
//! Ends are exclusive (RFC 5545 §3.6.1): an event that ends at midnight
//! stops on the day before.

use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime};
use std::cmp::Reverse;

/// Days in a week row.
pub(crate) const WEEK: usize = 7;
/// The shortest an event is drawn in the week view's hours, so a brief one
/// still shows its title and the next one is placed beside it.
pub(crate) const SHORTEST_BLOCK_MINUTES: u32 = 30;
const DAY_MINUTES: u32 = 24 * 60;

/// An event's time as the views show it: local wall-clock time, or for an
/// all-day event its dates, from midnight to the midnight after its last day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EventSpan {
    pub(crate) start: NaiveDateTime,
    /// Exclusive.
    pub(crate) end: NaiveDateTime,
    pub(crate) all_day: bool,
}

impl EventSpan {
    pub(crate) fn first_day(&self) -> NaiveDate {
        self.start.date()
    }

    /// The last day the event runs into. An event without a length still
    /// shows on its first day.
    pub(crate) fn last_day(&self) -> NaiveDate {
        if self.end <= self.start {
            self.start.date()
        } else {
            (self.end - Duration::nanoseconds(1)).date()
        }
    }

    pub(crate) fn covers(&self, day: NaiveDate) -> bool {
        self.first_day() <= day && day <= self.last_day()
    }

    /// All-day events and those lasting a day or more go in the week view's
    /// all-day strip. Shorter ones go in its hours, split at midnight.
    pub(crate) fn in_day_strip(&self) -> bool {
        self.all_day || self.end - self.start >= Duration::days(1)
    }

    /// The part of `day` the event takes, in minutes after midnight: from
    /// its start or midnight, to its end or the next midnight (1440).
    pub(crate) fn minutes_on(&self, day: NaiveDate) -> (u32, u32) {
        let midnight = day.and_time(NaiveTime::MIN);
        let start = self.start.clamp(midnight, midnight + Duration::days(1));
        let end = self.end.clamp(start, midnight + Duration::days(1));
        (minutes_after(midnight, start), minutes_after(midnight, end))
    }
}

fn minutes_after(midnight: NaiveDateTime, time: NaiveDateTime) -> u32 {
    (time - midnight)
        .num_minutes()
        .clamp(0, i64::from(DAY_MINUTES)) as u32
}

/// One event's bar in a week row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bar {
    /// The key the caller gave the event.
    pub(crate) event: usize,
    pub(crate) column: usize,
    /// Days the bar covers in this row.
    pub(crate) span: usize,
    pub(crate) lane: usize,
    /// The event began before this row.
    pub(crate) continues_before: bool,
    /// The event goes on after this row.
    pub(crate) continues_after: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) bars: Vec<Bar>,
    /// Lanes the row's bars fill.
    pub(crate) lanes: usize,
    /// For each day, `from_lane[day][k]` counts the day's bars in lane k
    /// and below it: a view with room for k lanes hides that many behind
    /// "+N more". `from_lane[day][0]` counts all of them.
    pub(crate) from_lane: [Vec<usize>; WEEK],
}

/// Lay the events out in the week that starts on `row_start`. Each event
/// comes with the caller's key for it.
pub(crate) fn lay_out_row(events: &[(usize, EventSpan)], row_start: NaiveDate) -> Row {
    let row_end = row_start + Duration::days(WEEK as i64 - 1);
    let mut bars: Vec<(Bar, &EventSpan)> = events
        .iter()
        .filter(|(_, span)| span.first_day() <= row_end && span.last_day() >= row_start)
        .map(|(key, span)| {
            let first = span.first_day().max(row_start);
            let last = span.last_day().min(row_end);
            let column = (first - row_start).num_days() as usize;
            let bar = Bar {
                event: *key,
                column,
                span: (last - first).num_days() as usize + 1,
                lane: 0,
                continues_before: span.first_day() < row_start,
                continues_after: span.last_day() > row_end,
            };
            (bar, span)
        })
        .collect();
    bars.sort_by_key(|(bar, span)| {
        (
            span.first_day() == span.last_day(),
            span.first_day(),
            Reverse(span.last_day()),
            !span.all_day,
            span.start,
            bar.event,
        )
    });

    let mut taken: Vec<[bool; WEEK]> = Vec::new();
    for (bar, _) in &mut bars {
        let days = bar.column..bar.column + bar.span;
        let lane = match taken
            .iter()
            .position(|lane| lane[days.clone()].iter().all(|taken| !taken))
        {
            Some(lane) => lane,
            None => {
                taken.push([false; WEEK]);
                taken.len() - 1
            }
        };
        taken[lane][days].fill(true);
        bar.lane = lane;
    }

    let lanes = taken.len();
    let from_lane = std::array::from_fn(|day| {
        let mut counts = vec![0; lanes + 1];
        for lane in (0..lanes).rev() {
            counts[lane] = counts[lane + 1] + usize::from(taken[lane][day]);
        }
        counts
    });
    Row {
        bars: bars.into_iter().map(|(bar, _)| bar).collect(),
        lanes,
        from_lane,
    }
}

/// Lay the events out in `weeks` week rows from `first_day`, as the month
/// view shows them.
pub(crate) fn lay_out_weeks(
    events: &[(usize, EventSpan)],
    first_day: NaiveDate,
    weeks: usize,
) -> Vec<Row> {
    (0..weeks)
        .map(|week| lay_out_row(events, first_day + Duration::weeks(week as i64)))
        .collect()
}

/// A timed event's box in one day column of the week view's hours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    /// The key the caller gave the event.
    pub(crate) event: usize,
    pub(crate) column: usize,
    pub(crate) start_minutes: u32,
    pub(crate) end_minutes: u32,
    /// Its place, from the left, among the `lanes` that the events
    /// overlapping it need. It widens over `lane_span` lanes from its own.
    pub(crate) lane: usize,
    pub(crate) lanes: usize,
    pub(crate) lane_span: usize,
    /// The event began on an earlier day.
    pub(crate) continues_before: bool,
    /// The event goes on into the next day.
    pub(crate) continues_after: bool,
}

impl Block {
    /// Where the box ends as drawn, at least [`SHORTEST_BLOCK_MINUTES`] down.
    fn drawn_end(&self) -> u32 {
        self.end_minutes
            .max((self.start_minutes + SHORTEST_BLOCK_MINUTES).min(DAY_MINUTES))
    }
}

/// Lay out the week's events that go in its hours (see
/// [`EventSpan::in_day_strip`]), one box per day each covers.
pub(crate) fn lay_out_hours(events: &[(usize, EventSpan)], week_start: NaiveDate) -> Vec<Block> {
    let mut blocks = Vec::new();
    for column in 0..WEEK {
        let day = week_start + Duration::days(column as i64);
        let midnight = day.and_time(NaiveTime::MIN);
        let mut pieces: Vec<Block> = events
            .iter()
            .filter(|(_, span)| !span.in_day_strip() && span.covers(day))
            .map(|(key, span)| {
                let (start_minutes, end_minutes) = span.minutes_on(day);
                Block {
                    event: *key,
                    column,
                    start_minutes,
                    end_minutes,
                    lane: 0,
                    lanes: 1,
                    lane_span: 1,
                    continues_before: span.start < midnight,
                    continues_after: span.end > midnight + Duration::days(1),
                }
            })
            .collect();
        pieces.sort_by_key(|block| (block.start_minutes, Reverse(block.end_minutes), block.event));

        // Events that overlap, directly or through others, form a group
        // whose lanes split the column.
        let mut group = 0;
        let mut group_end = 0;
        let mut lane_ends: Vec<u32> = Vec::new();
        for index in 0..pieces.len() {
            let start = pieces[index].start_minutes;
            if index > group && start >= group_end {
                settle(&mut pieces[group..index], lane_ends.len());
                group = index;
                lane_ends.clear();
            }
            let end = pieces[index].drawn_end();
            let lane = match lane_ends.iter().position(|&lane_end| lane_end <= start) {
                Some(lane) => lane,
                None => {
                    lane_ends.push(0);
                    lane_ends.len() - 1
                }
            };
            lane_ends[lane] = end;
            pieces[index].lane = lane;
            group_end = if index == group {
                end
            } else {
                group_end.max(end)
            };
        }
        settle(&mut pieces[group..], lane_ends.len());
        blocks.extend(pieces);
    }
    blocks
}

/// Give a group its lane count, and widen each box over the lanes to its
/// right that nothing in the group uses while it runs.
fn settle(group: &mut [Block], lanes: usize) {
    let drawn: Vec<(usize, u32, u32)> = group
        .iter()
        .map(|block| (block.lane, block.start_minutes, block.drawn_end()))
        .collect();
    for block in group {
        let (start, end) = (block.start_minutes, block.drawn_end());
        let free = (block.lane + 1..lanes)
            .take_while(|&lane| {
                !drawn.iter().any(|&(other, other_start, other_end)| {
                    other == lane && other_start < end && start < other_end
                })
            })
            .count();
        block.lanes = lanes;
        block.lane_span = 1 + free;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, day).unwrap()
    }

    fn at(date: NaiveDate, hour: u32, minute: u32) -> NaiveDateTime {
        date.and_hms_opt(hour, minute, 0).unwrap()
    }

    /// An all-day event from `first` through `last`, as DTEND the day after.
    fn all_day(first: NaiveDate, last: NaiveDate) -> EventSpan {
        EventSpan {
            start: at(first, 0, 0),
            end: at(last + Duration::days(1), 0, 0),
            all_day: true,
        }
    }

    fn timed(start: NaiveDateTime, end: NaiveDateTime) -> EventSpan {
        EventSpan {
            start,
            end,
            all_day: false,
        }
    }

    fn keyed(spans: &[EventSpan]) -> Vec<(usize, EventSpan)> {
        spans.iter().copied().enumerate().collect()
    }

    fn bar(row: &Row, event: usize) -> &Bar {
        row.bars.iter().find(|bar| bar.event == event).unwrap()
    }

    /// The events from the report: an exam period from Monday Nov 2 through
    /// Saturday Nov 14, and a graduation on Nov 25 and 26.
    #[test]
    fn multi_day_events_cover_every_day_in_one_bar_per_week() {
        let events = keyed(&[
            all_day(day(11, 2), day(11, 14)),
            all_day(day(11, 25), day(11, 26)),
        ]);
        // November 2026's grid starts on Monday Oct 26.
        let rows = lay_out_weeks(&events, day(10, 26), 6);
        assert!(rows[0].bars.is_empty());
        assert_eq!(
            rows[1].bars,
            [Bar {
                event: 0,
                column: 0,
                span: 7,
                lane: 0,
                continues_before: false,
                continues_after: true,
            }]
        );
        assert_eq!(
            rows[2].bars,
            [Bar {
                event: 0,
                column: 0,
                span: 6,
                lane: 0,
                continues_before: true,
                continues_after: false,
            }]
        );
        assert_eq!(rows[2].from_lane[5], [1, 0]);
        assert_eq!(rows[2].from_lane[6], [0, 0]);
        let graduation = bar(&rows[4], 1);
        assert_eq!((graduation.column, graduation.span), (2, 2));
        assert!(!graduation.continues_before && !graduation.continues_after);
    }

    #[test]
    fn ends_are_exclusive() {
        let one_day = all_day(day(11, 3), day(11, 3));
        assert_eq!(one_day.last_day(), day(11, 3));
        let to_midnight = timed(at(day(11, 3), 22, 0), at(day(11, 4), 0, 0));
        assert_eq!(to_midnight.last_day(), day(11, 3));
        let overnight = timed(at(day(11, 3), 22, 0), at(day(11, 4), 0, 1));
        assert_eq!(overnight.last_day(), day(11, 4));
        let instant = timed(at(day(11, 3), 9, 0), at(day(11, 3), 9, 0));
        assert_eq!(instant.last_day(), day(11, 3));
        let row = lay_out_row(&keyed(&[one_day, to_midnight, overnight]), day(11, 2));
        assert_eq!(bar(&row, 0).span, 1);
        assert_eq!(bar(&row, 1).span, 1);
        assert_eq!(bar(&row, 2).span, 2);
    }

    #[test]
    fn events_that_began_before_the_row_start_at_its_first_day() {
        let events = keyed(&[all_day(day(10, 20), day(11, 3))]);
        let row = lay_out_row(&events, day(11, 2));
        assert_eq!(
            row.bars,
            [Bar {
                event: 0,
                column: 0,
                span: 2,
                lane: 0,
                continues_before: true,
                continues_after: false,
            }]
        );
        assert!(lay_out_row(&events, day(11, 9)).bars.is_empty());
    }

    #[test]
    fn multi_day_bars_take_the_top_lanes_and_keep_them_across_the_row() {
        let events = keyed(&[
            // Single days, listed first and starting earlier.
            timed(at(day(11, 2), 9, 0), at(day(11, 2), 10, 0)),
            all_day(day(11, 3), day(11, 3)),
            // Two bars that overlap on Wednesday and Thursday.
            all_day(day(11, 3), day(11, 5)),
            timed(at(day(11, 4), 18, 0), at(day(11, 6), 10, 0)),
            // A later single day under the first bar.
            timed(at(day(11, 4), 8, 0), at(day(11, 4), 9, 0)),
        ]);
        let row = lay_out_row(&events, day(11, 2));
        assert_eq!(bar(&row, 2).lane, 0);
        assert_eq!(bar(&row, 3).lane, 1);
        assert_eq!((bar(&row, 3).column, bar(&row, 3).span), (2, 3));
        assert_eq!(bar(&row, 0).lane, 0);
        assert_eq!(bar(&row, 1).lane, 1);
        assert_eq!(bar(&row, 4).lane, 2);
        assert_eq!(row.lanes, 3);
        // Wednesday holds both bars and the single day under them.
        assert_eq!(row.from_lane[2], [3, 2, 1, 0]);
        // Friday holds only the second bar, in lane 1.
        assert_eq!(row.from_lane[4], [1, 1, 0, 0]);
        assert_eq!(row.from_lane[6], [0, 0, 0, 0]);
    }

    #[test]
    fn longer_and_all_day_events_win_ties() {
        let events = keyed(&[
            timed(at(day(11, 3), 9, 0), at(day(11, 4), 9, 0)),
            all_day(day(11, 3), day(11, 4)),
            all_day(day(11, 3), day(11, 6)),
        ]);
        let row = lay_out_row(&events, day(11, 2));
        assert_eq!(bar(&row, 2).lane, 0);
        assert_eq!(bar(&row, 1).lane, 1);
        assert_eq!(bar(&row, 0).lane, 2);
    }

    #[test]
    fn a_day_or_more_goes_in_the_all_day_strip() {
        assert!(all_day(day(11, 3), day(11, 3)).in_day_strip());
        assert!(timed(at(day(11, 3), 9, 0), at(day(11, 4), 9, 0)).in_day_strip());
        assert!(!timed(at(day(11, 3), 23, 0), at(day(11, 4), 8, 0)).in_day_strip());
    }

    #[test]
    fn overnight_events_split_at_midnight_in_the_hours() {
        let events = keyed(&[
            timed(at(day(11, 3), 22, 0), at(day(11, 4), 2, 30)),
            // A day or more long: in the all-day strip instead.
            timed(at(day(11, 4), 9, 0), at(day(11, 5), 9, 0)),
            // Ends at midnight: one box, to the end of the day.
            timed(at(day(11, 5), 23, 0), at(day(11, 6), 0, 0)),
        ]);
        let blocks = lay_out_hours(&events, day(11, 2));
        let spans: Vec<_> = blocks
            .iter()
            .map(|block| {
                (
                    block.event,
                    block.column,
                    block.start_minutes,
                    block.end_minutes,
                    block.continues_before,
                    block.continues_after,
                )
            })
            .collect();
        assert_eq!(
            spans,
            [
                (0, 1, 22 * 60, 1440, false, true),
                (0, 2, 0, 150, true, false),
                (2, 3, 23 * 60, 1440, false, false),
            ]
        );
    }

    #[test]
    fn overlapping_events_share_their_column() {
        let monday = day(11, 2);
        let events = keyed(&[
            timed(at(monday, 9, 0), at(monday, 12, 0)),
            timed(at(monday, 9, 0), at(monday, 10, 0)),
            timed(at(monday, 9, 0), at(monday, 9, 30)),
            timed(at(monday, 10, 0), at(monday, 11, 0)),
            // Starts as the group's last event ends: a group of its own.
            timed(at(monday, 12, 0), at(monday, 13, 0)),
        ]);
        let blocks = lay_out_hours(&events, monday);
        let lanes: Vec<_> = blocks
            .iter()
            .map(|block| (block.event, block.lane, block.lanes, block.lane_span))
            .collect();
        assert_eq!(
            lanes,
            [
                (0, 0, 3, 1),
                (1, 1, 3, 1),
                (2, 2, 3, 1),
                // Lane 2 is free again from 09:30, so it widens into it.
                (3, 1, 3, 2),
                (4, 0, 1, 1),
            ]
        );
    }

    #[test]
    fn short_events_are_placed_by_their_drawn_size() {
        let monday = day(11, 2);
        let events = keyed(&[
            timed(at(monday, 9, 0), at(monday, 9, 10)),
            timed(at(monday, 9, 15), at(monday, 9, 25)),
            timed(at(monday, 9, 30), at(monday, 9, 40)),
            timed(at(monday, 23, 50), at(monday, 23, 55)),
        ]);
        let blocks = lay_out_hours(&events, monday);
        let lanes: Vec<_> = blocks
            .iter()
            .map(|block| (block.lane, block.lanes))
            .collect();
        // 09:30 still meets 09:15 as drawn, so it joins their group.
        assert_eq!(lanes, [(0, 2), (1, 2), (0, 2), (0, 1)]);
        assert_eq!(blocks[3].drawn_end(), DAY_MINUTES);
    }
}
