//! The workspace directory browser.
//!
//! Three rules come from the contract rather than from convenience:
//!
//! - **Clients never join path segments themselves.** Every row and crumb carries an
//!   absolute host path; navigation uses those, so a client cannot get separators, symlink
//!   resolution, or a UNC root wrong on the host's behalf.
//! - `hidden` is the host platform's convention (dot-prefixed on POSIX). The **client owns**
//!   whether to show those rows.
//! - `truncated` means the backend cut the listing at its bound and the missing rows are
//!   the name-sorted tail. Hidden rows count toward that bound, so revealing hidden entries
//!   does not recover what was cut.

use serde::Deserialize;

/// One directory row or breadcrumb.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Entry {
    /// Base name; a root crumb carries its full path.
    pub name: String,
    /// Absolute host path — the only thing navigation should use.
    pub path: String,
    #[serde(default)]
    pub hidden: bool,
}

/// One directory level plus its ancestry.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Listing {
    #[serde(default)]
    pub path: String,
    /// The host account's home, for breadcrumb rooting.
    #[serde(default)]
    pub home: String,
    /// Ancestors from the filesystem root to this directory, inclusive.
    #[serde(default)]
    pub crumbs: Vec<Entry>,
    /// Direct child directories, name-sorted.
    #[serde(default)]
    pub entries: Vec<Entry>,
    /// The backend cut the listing; the missing rows are the name-sorted tail.
    #[serde(default)]
    pub truncated: bool,
}

/// The browser's state: a listing plus the client-owned hidden-row preference.
#[derive(Debug, Default)]
pub struct Browser {
    pub listing: Listing,
    /// Whether host-hidden rows are shown. The client owns this, not the host.
    pub show_hidden: bool,
    pub selected: usize,
}

impl Browser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn replace(&mut self, listing: Listing) {
        self.listing = listing;
        self.selected = 0;
    }

    /// Rows to display, honoring the hidden preference.
    pub fn rows(&self) -> Vec<&Entry> {
        self.listing
            .entries
            .iter()
            .filter(|entry| self.show_hidden || !entry.hidden)
            .collect()
    }

    /// Whether the cursor is on the synthetic "use this directory" row.
    ///
    /// That row is index 0 and is not a listing entry: it stands for the directory being
    /// browsed, so the current directory is something the cursor can point at. Without it
    /// `enter` had to mean "take the parent" while the cursor sat on a child, which is
    /// the one thing a cursor must never do.
    pub fn on_use_row(&self) -> bool {
        self.selected == 0
    }

    /// The absolute path of the highlighted *entry*, for descending.
    ///
    /// `None` on the synthetic row, which has no entry behind it.
    ///
    /// Returns the host's own string; nothing is joined client-side.
    pub fn selected_path(&self) -> Option<&str> {
        if self.on_use_row() {
            return None;
        }
        self.rows()
            .get(self.selected - 1)
            .map(|entry| entry.path.as_str())
    }

    /// The path `enter` would take: the current directory on the synthetic row, otherwise
    /// the highlighted entry. Always the row under the cursor.
    pub fn chosen_path(&self) -> Option<&str> {
        if self.on_use_row() {
            let path = self.listing.path.as_str();
            return (!path.is_empty()).then_some(path);
        }
        self.selected_path()
    }

    /// Rows including the synthetic one, which is what the cursor indexes.
    pub fn cursor_len(&self) -> usize {
        self.rows().len() + 1
    }

    /// The parent to navigate up to: the second-to-last crumb.
    ///
    /// `None` at the filesystem root, where the only crumb is the current directory.
    pub fn parent_path(&self) -> Option<&str> {
        self.ancestor(0)
    }

    /// The ancestor `levels` above the parent, from the host's own crumb chain.
    ///
    /// Climbing is driven off the crumbs rather than the listed path so that pressing
    /// `←` repeatedly keeps rising while the listings are still in flight. Recomputing
    /// the parent from stale state would send every press to the same directory.
    pub fn ancestor(&self, levels: usize) -> Option<&str> {
        let crumbs = &self.listing.crumbs;
        crumbs
            .len()
            .checked_sub(2 + levels)
            .and_then(|at| crumbs.get(at))
            .map(|crumb| crumb.path.as_str())
    }

    /// Breadcrumb label, abbreviating the home prefix the way the web client does.
    pub fn breadcrumb(&self) -> String {
        let path = &self.listing.path;
        let home = &self.listing.home;
        if !home.is_empty() && path.starts_with(home.as_str()) {
            return format!("~{}", &path[home.len()..]);
        }
        path.clone()
    }

    pub fn select_next(&mut self) {
        self.selected = (self.selected + 1).min(self.cursor_len() - 1);
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move by a screenful, clamped to the list.
    ///
    /// `page` is the visible row count, measured from the frame rather than assumed: a
    /// guessed page size either overshoots on a short terminal or crawls on a tall one.
    pub fn page_down(&mut self, page: usize) {
        self.selected = (self.selected + page.max(1)).min(self.cursor_len() - 1);
    }

    pub fn page_up(&mut self, page: usize) {
        self.selected = self.selected.saturating_sub(page.max(1));
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.cursor_len() - 1;
    }

    /// Toggle hidden rows, keeping the cursor inside the resulting list.
    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.selected = self.selected.min(self.cursor_len() - 1);
    }

    /// The note explaining an incomplete listing, if it is one.
    ///
    /// Revealing hidden rows does not recover the cut tail — hidden rows counted toward the
    /// bound — so the note must not suggest that toggling them would.
    pub fn truncation_note(&self) -> Option<&'static str> {
        self.listing
            .truncated
            .then_some("more directories exist here than the host will list")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing() -> Listing {
        serde_json::from_value(serde_json::json!({
            "path": "/home/acp/projects",
            "home": "/home/acp",
            "crumbs": [
                { "name": "/", "path": "/", "hidden": false },
                { "name": "home", "path": "/home", "hidden": false },
                { "name": "acp", "path": "/home/acp", "hidden": false },
                { "name": "projects", "path": "/home/acp/projects", "hidden": false }
            ],
            "entries": [
                { "name": ".cache", "path": "/home/acp/projects/.cache", "hidden": true },
                { "name": "dsh-tui", "path": "/home/acp/projects/dsh-tui", "hidden": false },
                { "name": "harness", "path": "/home/acp/projects/harness", "hidden": false }
            ],
            "truncated": true
        }))
        .expect("listing")
    }

    fn browser() -> Browser {
        let mut browser = Browser::new();
        browser.replace(listing());
        browser
    }

    #[test]
    fn hidden_rows_are_the_clients_choice() {
        let mut browser = browser();
        assert_eq!(browser.rows().len(), 2);
        browser.toggle_hidden();
        assert_eq!(browser.rows().len(), 3);
        assert_eq!(browser.rows()[0].name, ".cache");
    }

    #[test]
    fn navigation_uses_the_hosts_absolute_paths() {
        let mut browser = browser();
        // The cursor opens on the synthetic row; the first entry is one below it.
        browser.select_next();
        // Nothing is joined client-side; the host's own string is used.
        assert_eq!(browser.selected_path(), Some("/home/acp/projects/dsh-tui"));
    }

    #[test]
    fn the_cursor_opens_on_the_use_this_directory_row() {
        let browser = browser();
        assert!(browser.on_use_row());
        // There is no entry under it, so nothing to descend into...
        assert_eq!(browser.selected_path(), None);
        // ...but `enter` still has a target: the directory being browsed.
        assert_eq!(browser.chosen_path(), Some("/home/acp/projects"));
    }

    #[test]
    fn enter_always_takes_the_row_under_the_cursor() {
        // The whole point of the synthetic row: one rule, so `enter` can never act on
        // something other than what is highlighted.
        let mut browser = browser();
        browser.select_next();
        assert!(!browser.on_use_row());
        assert_eq!(browser.chosen_path(), Some("/home/acp/projects/dsh-tui"));
        browser.select_next();
        assert_eq!(browser.chosen_path(), Some("/home/acp/projects/harness"));
        // And the cursor stops at the last entry rather than running off it.
        browser.select_next();
        assert_eq!(browser.chosen_path(), Some("/home/acp/projects/harness"));
        browser.select_prev();
        browser.select_prev();
        assert!(browser.on_use_row(), "prev returns to the synthetic row");
    }

    #[test]
    fn the_parent_comes_from_the_crumb_chain() {
        let browser = browser();
        assert_eq!(browser.parent_path(), Some("/home/acp"));
    }

    #[test]
    fn there_is_no_parent_at_the_filesystem_root() {
        let mut browser = Browser::new();
        browser.replace(
            serde_json::from_value(serde_json::json!({
                "path": "/", "home": "/home/acp",
                "crumbs": [{ "name": "/", "path": "/", "hidden": false }],
                "entries": []
            }))
            .unwrap(),
        );
        assert_eq!(browser.parent_path(), None);
    }

    #[test]
    fn the_breadcrumb_abbreviates_home() {
        assert_eq!(browser().breadcrumb(), "~/projects");

        let mut elsewhere = Browser::new();
        elsewhere.replace(
            serde_json::from_value(serde_json::json!({
                "path": "/var/log", "home": "/home/acp", "crumbs": [], "entries": []
            }))
            .unwrap(),
        );
        assert_eq!(elsewhere.breadcrumb(), "/var/log");
    }

    #[test]
    fn toggling_hidden_keeps_the_cursor_in_range() {
        let mut browser = browser();
        browser.toggle_hidden();
        // Three entries plus the synthetic row: the cursor can reach index 3.
        for _ in 0..3 {
            browser.select_next();
        }
        assert_eq!(browser.selected, 3);
        // Hiding rows again must not leave the cursor past the end.
        browser.toggle_hidden();
        assert_eq!(browser.selected, 2);
        assert!(browser.selected_path().is_some());
    }

    #[test]
    fn a_truncated_listing_says_so_without_promising_a_fix() {
        let browser = browser();
        let note = browser.truncation_note().expect("a note");
        // Hidden rows counted toward the bound, so revealing them recovers nothing.
        assert!(!note.contains("hidden"));
        assert!(note.contains("more directories"));
    }
}
