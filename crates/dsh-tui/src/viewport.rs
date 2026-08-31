//! Scrollback over a rendered line list.
//!
//! The viewport tracks a distance from the **bottom** rather than a top offset, because a
//! conversation grows at the bottom: anchoring to the top would make every appended line
//! shift what the reader is looking at.
//!
//! While the viewport is at the bottom it *follows* — new lines keep it pinned there. Once
//! the reader scrolls up it stops following, so an arriving answer never yanks the view
//! away mid-read. Scrolling back to the bottom resumes following.

/// Scroll state for one scrollable region.
#[derive(Debug, Clone, Default)]
pub struct Viewport {
    /// Lines hidden below the visible window. Zero means pinned to the newest line.
    offset: usize,
    /// Total lines available at the last layout.
    total: usize,
    /// Visible height at the last layout.
    height: usize,
}

impl Viewport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the layout and clamp the offset to what is now scrollable.
    ///
    /// Called every frame: a shrinking transcript or a resized pane must not leave the
    /// offset pointing past the end.
    pub fn layout(&mut self, total: usize, height: usize) {
        self.total = total;
        self.height = height;
        self.offset = self.offset.min(self.max_offset());
    }

    /// The greatest distance from the bottom that still shows content.
    pub fn max_offset(&self) -> usize {
        self.total.saturating_sub(self.height)
    }

    /// Whether the newest line is visible, so new content should keep it in view.
    pub fn is_following(&self) -> bool {
        self.offset == 0
    }

    /// Whether the oldest held line is visible — the moment to request more history.
    pub fn is_at_top(&self) -> bool {
        self.total <= self.height || self.offset >= self.max_offset()
    }

    /// Whether a layout has been recorded.
    ///
    /// A real pane always has a height of at least one, so a zero height means `layout`
    /// has not run yet.
    pub fn is_measured(&self) -> bool {
        self.height > 0
    }

    /// The slice of lines to render, oldest-first.
    ///
    /// An unmeasured viewport returns everything rather than nothing: drawing an empty
    /// conversation because a layout pass was missed is far worse than drawing too much,
    /// which the renderer clips anyway.
    pub fn window(&self, total: usize) -> (usize, usize) {
        if !self.is_measured() {
            return (0, total);
        }
        let end = total.saturating_sub(self.offset);
        let start = end.saturating_sub(self.height);
        (start, end)
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.offset = (self.offset + lines).min(self.max_offset());
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.offset = self.offset.saturating_sub(lines);
    }

    pub fn page_up(&mut self) {
        self.scroll_up(self.height.max(1));
    }

    pub fn page_down(&mut self) {
        self.scroll_down(self.height.max(1));
    }

    /// Jump to the newest line and resume following.
    pub fn to_bottom(&mut self) {
        self.offset = 0;
    }

    pub fn to_top(&mut self) {
        self.offset = self.max_offset();
    }

    /// Keep a specific line visible, scrolling the least distance needed.
    ///
    /// Used to follow a search match without losing the reader's place any further than
    /// the jump requires.
    pub fn reveal(&mut self, line: usize) {
        let (start, end) = self.window(self.total);
        if line < start {
            self.offset = self.total.saturating_sub(line + self.height).min(self.max_offset());
        } else if line >= end {
            self.offset = self.total.saturating_sub(line + 1);
        }
    }
}

/// Line indices whose text contains `query`, case-insensitively.
pub fn matches(lines: &[String], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&needle))
        .map(|(index, _)| index)
        .collect()
}

/// The match at or after `from`, wrapping to the first.
pub fn next_match(matches: &[usize], from: usize) -> Option<usize> {
    matches
        .iter()
        .copied()
        .find(|line| *line > from)
        .or_else(|| matches.first().copied())
}

/// The match before `from`, wrapping to the last.
pub fn prev_match(matches: &[usize], from: usize) -> Option<usize> {
    matches
        .iter()
        .rev()
        .copied()
        .find(|line| *line < from)
        .or_else(|| matches.last().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized(total: usize, height: usize) -> Viewport {
        let mut viewport = Viewport::new();
        viewport.layout(total, height);
        viewport
    }

    #[test]
    fn a_fresh_viewport_follows_the_newest_line() {
        let viewport = sized(100, 10);
        assert!(viewport.is_following());
        assert_eq!(viewport.window(100), (90, 100));
    }

    #[test]
    fn scrolling_up_stops_the_view_from_following() {
        let mut viewport = sized(100, 10);
        viewport.scroll_up(5);
        assert!(!viewport.is_following());
        assert_eq!(viewport.window(100), (85, 95));
        // Returning to the bottom resumes following, so live output is seen again.
        viewport.to_bottom();
        assert!(viewport.is_following());
    }

    #[test]
    fn appended_lines_do_not_move_a_scrolled_reader() {
        let mut viewport = sized(100, 10);
        viewport.scroll_up(20);
        let before = viewport.window(100);
        // Ten lines arrive while the reader is scrolled up.
        viewport.layout(110, 10);
        let after = viewport.window(110);
        // The same lines stay on screen: the offset is measured from the bottom.
        assert_eq!(after.0 - before.0, 10);
        assert!(!viewport.is_following());
    }

    #[test]
    fn a_following_viewport_stays_pinned_as_content_arrives() {
        let mut viewport = sized(100, 10);
        viewport.layout(140, 10);
        assert!(viewport.is_following());
        assert_eq!(viewport.window(140), (130, 140));
    }

    #[test]
    fn the_offset_is_clamped_when_content_shrinks_or_the_pane_grows() {
        let mut viewport = sized(100, 10);
        viewport.to_top();
        assert_eq!(viewport.offset, 90);
        // A replaced, shorter transcript must not leave the offset past the end.
        viewport.layout(20, 10);
        assert_eq!(viewport.offset, 10);
        // A taller pane reduces what is scrollable.
        viewport.layout(20, 20);
        assert_eq!(viewport.offset, 0);
        assert!(viewport.is_following());
    }

    #[test]
    fn reaching_the_top_is_detectable_for_backfill() {
        let mut viewport = sized(100, 10);
        assert!(!viewport.is_at_top());
        viewport.to_top();
        assert!(viewport.is_at_top());
        // A transcript shorter than the pane is entirely visible, so it is also "at top".
        let short_view = sized(4, 10);
        assert!(short_view.is_at_top());
    }

    #[test]
    fn paging_moves_by_the_visible_height() {
        let mut viewport = sized(100, 10);
        viewport.page_up();
        assert_eq!(viewport.window(100), (80, 90));
        viewport.page_down();
        assert!(viewport.is_following());
    }

    #[test]
    fn revealing_a_line_scrolls_the_least_distance_needed() {
        let mut viewport = sized(100, 10);
        // A line above the window scrolls up just enough to show it.
        viewport.reveal(50);
        let (start, end) = viewport.window(100);
        assert!((start..end).contains(&50));

        // A line already visible does not move the view.
        let before = viewport.window(100);
        viewport.reveal(start + 1);
        assert_eq!(viewport.window(100), before);
    }

    #[test]
    fn an_unmeasured_viewport_shows_everything_rather_than_nothing() {
        let viewport = Viewport::new();
        assert!(!viewport.is_measured());
        // A missed layout pass must not blank the conversation.
        assert_eq!(viewport.window(40), (0, 40));
    }

    #[test]
    fn search_finds_matches_case_insensitively() {
        let lines = vec![
            "Checking the tokenizer".to_string(),
            "read_file src/lex.rs".to_string(),
            "TOKENIZER bounds".to_string(),
        ];
        assert_eq!(matches(&lines, "tokenizer"), vec![0, 2]);
        // Whitespace is not a query.
        assert!(matches(&lines, "   ").is_empty());
        assert!(matches(&lines, "absent").is_empty());
    }

    #[test]
    fn match_navigation_wraps_in_both_directions() {
        let found = vec![2usize, 7, 11];
        assert_eq!(next_match(&found, 0), Some(2));
        assert_eq!(next_match(&found, 7), Some(11));
        // Past the last match, wrap to the first.
        assert_eq!(next_match(&found, 11), Some(2));
        assert_eq!(prev_match(&found, 11), Some(7));
        // Before the first, wrap to the last.
        assert_eq!(prev_match(&found, 2), Some(11));
        assert_eq!(next_match(&[], 0), None);
    }
}
