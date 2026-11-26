//! Which messages of the open conversation are open together (#435).
//!
//! With "Keep messages open" off, opening a message closes the one before, as
//! the reader always did. With it on, the others stay open under their
//! headers, and the one opened last is active: the reader's find, zoom,
//! selection, toolbar and reply row act on it.

/// The open messages of one reader's conversation, by message id.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct OpenMessageSet {
    /// In the order they were last opened or clicked, the active one last.
    recent: Vec<i32>,
}

impl OpenMessageSet {
    /// The message the reader acts on.
    pub(crate) fn active(&self) -> Option<i32> {
        self.recent.last().copied()
    }

    pub(crate) fn is_open(&self, id: i32) -> bool {
        self.recent.contains(&id)
    }

    /// Open and not the active one, so shown under its header only.
    pub(crate) fn is_kept_open(&self, id: i32) -> bool {
        self.active() != Some(id) && self.is_open(id)
    }

    pub(crate) fn count(&self) -> usize {
        self.recent.len()
    }

    /// Open `id` as the active message. Unless messages are kept open, it is
    /// the only one open. Opening an open message makes it the active one.
    pub(crate) fn open(&mut self, id: i32, keep_open: bool) {
        if keep_open {
            self.recent.retain(|open| *open != id);
        } else {
            self.recent.clear();
        }
        self.recent.push(id);
    }

    /// Close `id`, after which the message opened before it is the active
    /// one. The last open message stays open. Returns whether it closed.
    pub(crate) fn collapse(&mut self, id: i32) -> bool {
        if self.recent.len() < 2 || !self.is_open(id) {
            return false;
        }
        self.recent.retain(|open| *open != id);
        true
    }

    /// Forget messages that left the conversation, such as one deleted or
    /// moved away. Returns whether any did.
    pub(crate) fn retain(&mut self, thread: &[i32]) -> bool {
        let before = self.recent.len();
        self.recent.retain(|open| thread.contains(open));
        self.recent.len() != before
    }

    /// Turning the setting off leaves the active message open alone.
    pub(crate) fn keep_only_active(&mut self) {
        if let Some(active) = self.active() {
            self.recent = vec![active];
        }
    }
}

/// How a thread row shows a message kept open: its body's height per unit of
/// the reader's width and its height that doesn't scale with the width, and
/// the same summed over the messages kept open above it. The pane lays out
/// the active message itself.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct KeptOpenRow {
    pub(crate) open: bool,
    pub(crate) body_aspect: f32,
    pub(crate) body_extra: f32,
    pub(crate) aspect_above: f32,
    pub(crate) extra_above: f32,
}

/// The rows of `thread`, in its order. `size` gives a kept-open message's
/// body aspect and fixed height.
pub(crate) fn kept_open_rows(
    thread: &[i32],
    open: &OpenMessageSet,
    mut size: impl FnMut(i32) -> (f32, f32),
) -> Vec<KeptOpenRow> {
    let (mut aspect_above, mut extra_above) = (0.0, 0.0);
    thread
        .iter()
        .map(|&id| {
            let mut row = KeptOpenRow {
                aspect_above,
                extra_above,
                ..KeptOpenRow::default()
            };
            if open.is_kept_open(id) {
                let (aspect, extra) = size(id);
                row.open = true;
                row.body_aspect = aspect.max(0.0);
                row.body_extra = extra.max(0.0);
                aspect_above += row.body_aspect;
                extra_above += row.body_extra;
            }
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(ids: &[i32], keep_open: bool) -> OpenMessageSet {
        let mut open = OpenMessageSet::default();
        for &id in ids {
            open.open(id, keep_open);
        }
        open
    }

    #[test]
    fn one_message_is_open_at_a_time_unless_they_are_kept_open() {
        let one = opened(&[1, 2, 3], false);
        assert_eq!((one.count(), one.active()), (1, Some(3)));
        assert!(!one.is_open(1));

        let kept = opened(&[1, 2, 3], true);
        assert_eq!((kept.count(), kept.active()), (3, Some(3)));
        assert!(kept.is_kept_open(1) && kept.is_kept_open(2) && !kept.is_kept_open(3));
    }

    #[test]
    fn opening_an_open_message_makes_it_the_active_one() {
        let mut open = opened(&[1, 2, 3], true);
        open.open(1, true);
        assert_eq!((open.count(), open.active()), (3, Some(1)));
        assert!(open.is_kept_open(3));
    }

    #[test]
    fn closing_the_active_message_returns_to_the_one_opened_before() {
        let mut open = opened(&[1, 3, 2], true);
        assert!(open.collapse(2));
        assert_eq!(open.active(), Some(3));
        assert!(open.collapse(1));
        assert_eq!(open.active(), Some(3));
        // The last open message stays open.
        assert!(!open.collapse(3));
        assert!(!open.collapse(9));
        assert_eq!((open.count(), open.active()), (1, Some(3)));
    }

    #[test]
    fn messages_that_leave_the_conversation_close() {
        let mut open = opened(&[1, 2, 3], true);
        assert!(open.retain(&[1, 2]));
        assert_eq!(open.active(), Some(2));
        assert!(!open.retain(&[1, 2]));
        open.retain(&[]);
        assert_eq!(open.active(), None);
    }

    #[test]
    fn turning_the_setting_off_keeps_the_active_message() {
        let mut open = opened(&[1, 2, 3], true);
        open.open(2, true);
        open.keep_only_active();
        assert_eq!(open, opened(&[2], false));
    }

    #[test]
    fn kept_open_rows_add_up_the_bodies_above_them() {
        // 10, 20 and 30 are kept open around 40, the active message.
        let open = opened(&[30, 10, 20, 40], true);
        let rows = kept_open_rows(&[10, 15, 20, 30, 40, 50], &open, |id| {
            (id as f32 / 10.0, 12.0)
        });
        let shown: Vec<_> = rows
            .iter()
            .map(|row| (row.open, row.aspect_above, row.extra_above))
            .collect();
        assert_eq!(
            shown,
            [
                (true, 0.0, 0.0),
                (false, 1.0, 12.0),
                (true, 1.0, 12.0),
                (true, 3.0, 24.0),
                (false, 6.0, 36.0),
                (false, 6.0, 36.0),
            ]
        );
        assert_eq!((rows[2].body_aspect, rows[2].body_extra), (2.0, 12.0));
        assert_eq!((rows[4].body_aspect, rows[4].body_extra), (0.0, 0.0));
    }
}
