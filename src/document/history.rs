#[derive(Debug, Clone)]
pub struct History<T> {
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
    pub fn push(&mut self, value: T) {
        self.entries.truncate(self.cursor);
        self.entries.push(value);
        self.cursor = self.entries.len();
    }

    pub fn undo(&mut self) -> bool {
        if self.cursor == 0 {
            false
        } else {
            self.cursor -= 1;
            true
        }
    }

    pub fn redo(&mut self) -> bool {
        if self.cursor == self.entries.len() {
            false
        } else {
            self.cursor += 1;
            true
        }
    }

    pub fn active(&self) -> &[T] {
        &self.entries[..self.cursor]
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor < self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.cursor = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.cursor == 0
    }

    pub fn replace_active(&mut self, index: usize, value: T) -> bool {
        if index >= self.cursor {
            return false;
        }
        self.entries.truncate(self.cursor);
        self.entries[index] = value;
        true
    }

    pub fn remove_active(&mut self, index: usize) -> bool {
        if index >= self.cursor {
            return false;
        }
        self.entries.truncate(self.cursor);
        self.entries.remove(index);
        self.cursor -= 1;
        true
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
    fn active_entries_can_be_replaced_and_removed() {
        let mut history = History::default();
        history.push(1);
        history.push(2);
        assert!(history.replace_active(0, 3));
        assert!(history.remove_active(1));
        assert_eq!(history.active(), &[3]);
    }
}
