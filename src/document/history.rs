#[derive(Debug, Clone)]
pub(super) struct History<T> {
    entries: Vec<T>,
    cursor: usize,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
        }
    }
}

impl<T> History<T> {
    pub(super) fn push(&mut self, value: T) {
        self.entries.truncate(self.cursor);
        self.entries.push(value);
        self.cursor = self.entries.len();
    }

    pub(super) fn undo(&mut self) -> bool {
        if self.cursor == 0 {
            false
        } else {
            self.cursor -= 1;
            true
        }
    }

    pub(super) fn redo(&mut self) -> bool {
        if self.cursor == self.entries.len() {
            false
        } else {
            self.cursor += 1;
            true
        }
    }

    pub(super) fn active(&self) -> &[T] {
        &self.entries[..self.cursor]
    }

    pub(super) fn replace_last(&mut self, value: T) -> bool {
        if self.cursor == 0 || self.cursor != self.entries.len() {
            return false;
        }
        self.entries[self.cursor - 1] = value;
        true
    }

    pub(super) fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub(super) fn can_redo(&self) -> bool {
        self.cursor < self.entries.len()
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::History;

    #[test]
    fn adding_after_undo_discards_redo_branch() {
        let mut history = History::default();
        history.push(1);
        history.push(2);
        assert!(history.undo());
        history.push(3);
        assert_eq!(history.active(), &[1, 3]);
        assert!(!history.redo());
    }

    #[test]
    fn replacement_requires_the_tip_of_history() {
        let mut history = History::default();
        assert!(!history.replace_last(1));
        history.push(1);
        assert!(history.replace_last(2));
        assert_eq!(history.active(), &[2]);
        history.push(3);
        assert!(history.undo());
        assert!(!history.replace_last(4));
        assert_eq!(history.active(), &[2]);
    }
}
