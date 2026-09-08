//! Batched updates for virtualized workspace navigation.
use slint::Model;
use std::cell::RefCell;

/// Keep one model for all responsive presentations. A splice emits range
/// notifications so expanding thousands of descendants moves the tail once,
/// without resetting the ListView or doing one Vec insertion per folder.
#[derive(Default)]
pub(crate) struct RetainedModel<T> {
    rows: RefCell<Vec<T>>,
    notify: slint::ModelNotify,
}

impl<T: Clone + 'static> Model for RetainedModel<T> {
    type Data = T;

    fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<T> {
        self.rows.borrow().get(row).cloned()
    }

    fn model_tracker(&self) -> &dyn slint::ModelTracker {
        &self.notify
    }
}

impl<T: Clone + 'static> RetainedModel<T> {
    pub(crate) fn reconcile_by<K: Eq>(
        &self,
        rows: Vec<T>,
        key: impl Fn(&T) -> K,
        equal: impl Fn(&T, &T) -> bool,
    ) {
        let current = self.rows.borrow();
        let prefix = current
            .iter()
            .zip(&rows)
            .take_while(|(a, b)| key(a) == key(b))
            .count();
        let suffix = current[prefix..]
            .iter()
            .rev()
            .zip(rows[prefix..].iter().rev())
            .take_while(|(a, b)| key(a) == key(b))
            .count();
        let removed = current.len() - prefix - suffix;
        let added = rows.len() - prefix - suffix;
        drop(current);
        if removed > 0 {
            self.rows.borrow_mut().drain(prefix..prefix + removed);
            self.notify.row_removed(prefix, removed);
        }
        if added > 0 {
            self.rows
                .borrow_mut()
                .splice(prefix..prefix, rows[prefix..prefix + added].iter().cloned());
            self.notify.row_added(prefix, added);
        }
        for (index, row) in rows.into_iter().enumerate() {
            if (prefix..prefix + added).contains(&index) {
                continue;
            }
            if !equal(&self.rows.borrow()[index], &row) {
                self.rows.borrow_mut()[index] = row;
                self.notify.row_changed(index);
            }
        }
    }
}
