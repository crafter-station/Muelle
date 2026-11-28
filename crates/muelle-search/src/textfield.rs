//! Editing state for the single-line search box.

const MAX_CHARS: usize = 256;

#[derive(Default)]
pub struct TextField {
    chars: Vec<char>,
    caret: usize,
    /// The other end of the selection; equal to `caret` when nothing is selected.
    anchor: usize,
}

impl TextField {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    /// The caret position in UTF-16 units, as DirectWrite counts them.
    pub fn caret_utf16(&self) -> u32 {
        self.utf16_offset(self.caret)
    }

    /// The selected range in UTF-16 units, if there is one.
    pub fn selection_utf16(&self) -> Option<(u32, u32)> {
        let (start, end) = self.selection();
        (start != end).then(|| (self.utf16_offset(start), self.utf16_offset(end)))
    }

    fn utf16_offset(&self, index: usize) -> u32 {
        self.chars[..index]
            .iter()
            .map(|c| c.len_utf16() as u32)
            .sum()
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection();
        (start != end).then(|| self.chars[start..end].iter().collect())
    }

    /// Replaces the selection with `text`, dropping control characters.
    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        let room = MAX_CHARS - self.chars.len();
        // Pasted multi-line text keeps only its first line.
        let line = text
            .trim_start_matches(['\r', '\n'])
            .lines()
            .next()
            .unwrap_or("");
        let new: Vec<char> = line
            .chars()
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| !c.is_control())
            .take(room)
            .collect();
        let count = new.len();
        self.chars.splice(self.caret..self.caret, new);
        self.caret += count;
        self.anchor = self.caret;
    }

    pub fn delete_selection(&mut self) -> bool {
        let (start, end) = self.selection();
        self.chars.drain(start..end);
        self.caret = start;
        self.anchor = start;
        start != end
    }

    pub fn backspace(&mut self, word: bool) {
        if !self.delete_selection() && self.caret > 0 {
            self.anchor = if word {
                self.previous_word()
            } else {
                self.caret - 1
            };
            self.delete_selection();
        }
    }

    pub fn delete(&mut self, word: bool) {
        if !self.delete_selection() && self.caret < self.chars.len() {
            self.anchor = if word {
                self.next_word()
            } else {
                self.caret + 1
            };
            self.delete_selection();
        }
    }

    pub fn left(&mut self, word: bool, select: bool) {
        let (start, end) = self.selection();
        let target = if !select && start != end {
            start
        } else if word {
            self.previous_word()
        } else {
            self.caret.saturating_sub(1)
        };
        self.move_to(target, select);
    }

    pub fn right(&mut self, word: bool, select: bool) {
        let (start, end) = self.selection();
        let target = if !select && start != end {
            end
        } else if word {
            self.next_word()
        } else {
            (self.caret + 1).min(self.chars.len())
        };
        self.move_to(target, select);
    }

    pub fn home(&mut self, select: bool) {
        self.move_to(0, select);
    }

    pub fn end(&mut self, select: bool) {
        self.move_to(self.chars.len(), select);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.chars.len();
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn move_to(&mut self, index: usize, select: bool) {
        self.caret = index;
        if !select {
            self.anchor = index;
        }
    }

    fn previous_word(&self) -> usize {
        let mut i = self.caret;
        while i > 0 && self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn next_word(&self) -> usize {
        let mut i = self.caret;
        while i < self.chars.len() && !self.chars[i].is_whitespace() {
            i += 1;
        }
        while i < self.chars.len() && self.chars[i].is_whitespace() {
            i += 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(text: &str) -> TextField {
        let mut f = TextField::default();
        f.insert(text);
        f
    }

    #[test]
    fn edits_around_the_caret() {
        let mut f = field("helo");
        f.left(false, false);
        f.insert("l");
        assert_eq!(f.text(), "hello");
        f.backspace(false);
        assert_eq!(f.text(), "helo");
        f.delete(false);
        assert_eq!(f.text(), "hel");
    }

    #[test]
    fn word_deletion_and_selection() {
        let mut f = field("open the file");
        f.backspace(true);
        assert_eq!(f.text(), "open the ");
        f.home(false);
        f.delete(true);
        assert_eq!(f.text(), "the ");
        f.select_all();
        assert_eq!(f.selected_text().as_deref(), Some("the "));
        f.insert("new");
        assert_eq!(f.text(), "new");
    }

    #[test]
    fn counts_utf16_units() {
        let mut f = field("a😀b");
        assert_eq!(f.caret_utf16(), 4);
        f.left(false, true);
        f.left(false, true);
        assert_eq!(f.selection_utf16(), Some((1, 4)));
    }

    #[test]
    fn sanitises_and_limits_input() {
        let f = field("one\ttwo\r\nthree");
        assert_eq!(f.text(), "one two");
        assert_eq!(field(&"x".repeat(999)).text().len(), MAX_CHARS);
    }
}
