//! The composer and its input triggers.
//!
//! Mirrors `ui-input-trigger`: `/` and `@` are detected *under the caret*, not merely
//! present in the text, so an `@` the user already moved past stops offering candidates
//! and a `/` in the middle of a sentence is prose rather than a command.

/// Which source a trigger routes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// `/` — commands, skills, and the model and permission pickers.
    Slash,
    /// `@` — file and session references, and subagents.
    At,
}

/// An active trigger under the caret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// Byte offset of the sigil.
    pub start: usize,
    /// Text between the sigil and the caret.
    pub query: String,
}

/// Text and caret state for the message input.
#[derive(Debug, Default, Clone)]
pub struct Composer {
    text: String,
    /// Byte offset of the caret. Always on a char boundary.
    caret: usize,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Take the text, leaving the composer empty.
    pub fn take(&mut self) -> String {
        self.caret = 0;
        std::mem::take(&mut self.text)
    }

    /// Put text back, caret at the end.
    ///
    /// Used to re-seat a prompt that was typed before its session existed, so the send
    /// path stays the single one rather than being duplicated for the deferred case.
    pub fn set(&mut self, text: String) {
        self.caret = text.len();
        self.text = text;
    }

    pub fn insert(&mut self, ch: char) {
        self.text.insert(self.caret, ch);
        self.caret += ch.len_utf8();
    }

    /// Delete the character before the caret.
    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let previous = self.text[..self.caret]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.text.replace_range(previous..self.caret, "");
        self.caret = previous;
    }

    pub fn move_left(&mut self) {
        if let Some((index, _)) = self.text[..self.caret].char_indices().next_back() {
            self.caret = index;
        }
    }

    pub fn move_right(&mut self) {
        if let Some(ch) = self.text[self.caret..].chars().next() {
            self.caret += ch.len_utf8();
        }
    }

    pub fn move_home(&mut self) {
        self.caret = 0;
    }

    pub fn move_end(&mut self) {
        self.caret = self.text.len();
    }

    /// The trigger under the caret, if any.
    ///
    /// `/` counts only as the first character of the input: a slash inside a sentence, or
    /// a path like `src/lex.rs`, is prose. `@` counts at any word boundary. Either stops
    /// matching once its query would span whitespace, so a completed reference does not
    /// keep the menu open.
    pub fn active_trigger(&self) -> Option<Trigger> {
        let before = &self.text[..self.caret];

        if let Some(rest) = before.strip_prefix('/') {
            if !rest.contains(char::is_whitespace) {
                return Some(Trigger {
                    kind: TriggerKind::Slash,
                    start: 0,
                    query: rest.to_string(),
                });
            }
        }

        // The nearest `@` that starts a word, with no whitespace between it and the caret.
        let at = before.char_indices().rev().find_map(|(index, ch)| {
            if ch != '@' {
                return None;
            }
            let starts_word = index == 0
                || before[..index]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace);
            starts_word.then_some(index)
        })?;

        let query = &before[at + 1..];
        if query.contains(char::is_whitespace) {
            return None;
        }
        Some(Trigger {
            kind: TriggerKind::At,
            start: at,
            query: query.to_string(),
        })
    }

    /// Replace the active trigger's span with a picked value.
    ///
    /// The replacement covers the sigil through the caret, so picking twice does not stack
    /// two sigils, and the caret lands after the inserted text.
    pub fn apply_pick(&mut self, replacement: &str) {
        let Some(trigger) = self.active_trigger() else {
            return;
        };
        self.text.replace_range(trigger.start..self.caret, replacement);
        self.caret = trigger.start + replacement.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_end(text: &str) -> Composer {
        let mut composer = Composer::new();
        for ch in text.chars() {
            composer.insert(ch);
        }
        composer
    }

    #[test]
    fn a_leading_slash_opens_the_command_menu() {
        let composer = at_end("/mod");
        let trigger = composer.active_trigger().expect("a slash trigger");
        assert_eq!(trigger.kind, TriggerKind::Slash);
        assert_eq!(trigger.query, "mod");
    }

    #[test]
    fn a_slash_inside_a_path_is_prose() {
        // `src/lex.rs` must not open the command menu.
        assert!(at_end("open src/lex.rs").active_trigger().is_none());
    }

    #[test]
    fn a_slash_command_closes_once_an_argument_starts() {
        assert!(at_end("/model deepseek").active_trigger().is_none());
    }

    #[test]
    fn an_at_opens_the_reference_menu_at_a_word_boundary() {
        let trigger = at_end("look at @src/le").active_trigger().expect("an at trigger");
        assert_eq!(trigger.kind, TriggerKind::At);
        assert_eq!(trigger.query, "src/le");
    }

    #[test]
    fn an_email_like_at_does_not_trigger() {
        // The `@` is mid-word, so it is not a reference sigil.
        assert!(at_end("mail me@example.com").active_trigger().is_none());
    }

    #[test]
    fn moving_past_a_reference_closes_the_menu() {
        let mut composer = at_end("@src/lex.rs and more");
        assert!(composer.active_trigger().is_none());
        // Back inside the reference, it opens again.
        for _ in 0.."and more".len() + 1 {
            composer.move_left();
        }
        assert!(composer.active_trigger().is_some());
    }

    #[test]
    fn picking_replaces_the_whole_trigger_span() {
        let mut composer = at_end("see @src/le");
        composer.apply_pick("@src/lex.rs");
        assert_eq!(composer.text(), "see @src/lex.rs");
        assert_eq!(composer.caret(), composer.text().len());
        // Picking again must not stack a second sigil.
        assert!(composer.active_trigger().is_some());
        composer.apply_pick("@src/lexer.rs");
        assert_eq!(composer.text(), "see @src/lexer.rs");
    }

    #[test]
    fn editing_multibyte_text_stays_on_char_boundaries() {
        let mut composer = at_end("你好 @文件");
        assert_eq!(composer.active_trigger().unwrap().query, "文件");
        composer.backspace();
        assert_eq!(composer.text(), "你好 @文");
        composer.move_left();
        composer.move_left();
        assert_eq!(composer.caret(), "你好 ".len());
    }

    #[test]
    fn taking_the_text_resets_the_caret() {
        let mut composer = at_end("hello");
        assert_eq!(composer.take(), "hello");
        assert!(composer.is_empty());
        assert_eq!(composer.caret(), 0);
    }
}
