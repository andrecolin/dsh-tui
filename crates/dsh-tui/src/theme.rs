//! Terminal palette derived from the web client's `--dsw-*` design tokens.
//!
//! Values are taken from `packages/client/ui-theme/src/styles/base.css` so the two front
//! ends agree on color rather than approximating each other.

use ratatui::style::Color;

/// `--dsw-static-*` values, verbatim.
mod token {
    use ratatui::style::Color;
    pub const DEEPSEEK_450: Color = Color::Rgb(86, 134, 254);
    pub const DEEPSEEK_500: Color = Color::Rgb(65, 118, 230);
    pub const BLUISH_00: Color = Color::Rgb(255, 255, 255);
    pub const BLUISH_50: Color = Color::Rgb(249, 250, 251);
    pub const BLUISH_100: Color = Color::Rgb(235, 238, 242);
    pub const BLUISH_500: Color = Color::Rgb(151, 157, 166);
    pub const BLUISH_900: Color = Color::Rgb(27, 27, 28);
    pub const BLUISH_950: Color = Color::Rgb(21, 21, 23);
    pub const BLUISH_1000: Color = Color::Rgb(15, 17, 21);
}

/// Which token set is active. The web client persists this in the `ui-theme` settings
/// namespace as `light`, `dark`, or `system`; the terminal reads the same field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

/// The stored preference, including the one the terminal cannot answer directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preference {
    Light,
    Dark,
    System,
}

impl Preference {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "light" => Some(Preference::Light),
            "dark" => Some(Preference::Dark),
            "system" => Some(Preference::System),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Preference::Light => "light",
            Preference::Dark => "dark",
            Preference::System => "system",
        }
    }
}

/// How a preference was turned into a concrete mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub mode: Mode,
    /// Human-readable explanation, shown on the Appearance row.
    pub reason: &'static str,
}

/// Resolve a stored preference into the mode to render with.
///
/// A browser answers `system` with `prefers-color-scheme`. A terminal has no equivalent:
/// the closest signal is `COLORFGBG`, which some terminals export as `fg;bg` colour
/// indices. When it is absent or unreadable the mode falls back to dark, and the
/// Appearance row says so rather than implying the terminal was asked.
pub fn resolve(preference: Preference, colorfgbg: Option<&str>) -> Resolved {
    match preference {
        Preference::Light => Resolved { mode: Mode::Light, reason: "set to light" },
        Preference::Dark => Resolved { mode: Mode::Dark, reason: "set to dark" },
        Preference::System => match colorfgbg.and_then(background_is_light) {
            Some(true) => Resolved {
                mode: Mode::Light,
                reason: "from your terminal's COLORFGBG",
            },
            Some(false) => Resolved {
                mode: Mode::Dark,
                reason: "from your terminal's COLORFGBG",
            },
            None => Resolved {
                mode: Mode::Dark,
                reason: "terminal does not report a theme; using dark",
            },
        },
    }
}

/// Read the background colour index out of `COLORFGBG` (`"15;0"` — foreground, background).
///
/// Indices 0-6 and 8 are the dark half of the ANSI palette; 7 and 9-15 are the light half.
fn background_is_light(colorfgbg: &str) -> Option<bool> {
    let background = colorfgbg.rsplit(';').next()?.trim();
    let index: u8 = background.parse().ok()?;
    match index {
        0..=6 | 8 => Some(false),
        7 | 9..=15 => Some(true),
        _ => None,
    }
}

/// The `--dsw-alias-*` layer: semantic roles resolved from the static tokens.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Read by the settings pane's theme row.
    #[allow(dead_code)]
    pub mode: Mode,
    /// `--dsw-alias-bg-base`
    pub bg_base: Color,
    /// `--dsw-alias-bg-layer-1`, the raised surface panes sit on.
    pub bg_layer: Color,
    /// Primary body text.
    pub text: Color,
    /// `--dsw-alias-text-2`, for secondary and disabled copy.
    pub text_dim: Color,
    /// `--dsw-alias-border-l1`, approximated: terminals have no alpha compositing.
    pub border: Color,
    /// Border of the focused pane.
    pub border_focus: Color,
    /// `--dsw-alias-brand-primary-new-color`, the interactive accent.
    pub accent: Color,
    pub danger: Color,
    pub success: Color,
    pub warning: Color,
}

impl Theme {
    pub fn new(mode: Mode) -> Self {
        match mode {
            Mode::Dark => Self {
                mode,
                bg_base: token::BLUISH_950,
                bg_layer: token::BLUISH_900,
                text: token::BLUISH_50,
                text_dim: token::BLUISH_500,
                // `rgba(255,255,255,0.06)` over `bluish-950` has no terminal equivalent;
                // the nearest opaque step keeps the seam visible without a halo.
                border: Color::Rgb(48, 48, 52),
                border_focus: token::DEEPSEEK_450,
                accent: token::DEEPSEEK_450,
                danger: Color::Rgb(232, 92, 92),
                success: Color::Rgb(90, 186, 130),
                warning: Color::Rgb(219, 165, 74),
            },
            Mode::Light => Self {
                mode,
                bg_base: token::BLUISH_00,
                bg_layer: token::BLUISH_50,
                text: token::BLUISH_1000,
                text_dim: token::BLUISH_500,
                border: token::BLUISH_100,
                border_focus: token::DEEPSEEK_500,
                accent: token::DEEPSEEK_500,
                danger: Color::Rgb(200, 60, 60),
                success: Color::Rgb(46, 150, 96),
                warning: Color::Rgb(176, 124, 30),
            },
        }
    }

    /// Border color for a pane, by focus.
    pub fn pane_border(&self, focused: bool) -> Color {
        if focused {
            self.border_focus
        } else {
            self.border
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(Mode::Dark)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_preferences_ignore_the_terminal() {
        assert_eq!(resolve(Preference::Light, Some("0;15")).mode, Mode::Light);
        assert_eq!(resolve(Preference::Dark, Some("0;15")).mode, Mode::Dark);
    }

    #[test]
    fn system_reads_the_terminal_background_when_it_is_reported() {
        assert_eq!(resolve(Preference::System, Some("15;0")).mode, Mode::Dark);
        assert_eq!(resolve(Preference::System, Some("0;15")).mode, Mode::Light);
        assert_eq!(resolve(Preference::System, Some("0;7")).mode, Mode::Light);
        // Index 8 is bright black — the dark half despite being "bright".
        assert_eq!(resolve(Preference::System, Some("15;8")).mode, Mode::Dark);
    }

    #[test]
    fn system_says_so_when_the_terminal_reports_nothing() {
        let resolved = resolve(Preference::System, None);
        assert_eq!(resolved.mode, Mode::Dark);
        // The row must not imply the terminal was consulted and answered.
        assert!(resolved.reason.contains("does not report"));

        let garbage = resolve(Preference::System, Some("not-a-pair"));
        assert_eq!(garbage.mode, Mode::Dark);
        assert!(garbage.reason.contains("does not report"));
    }

    #[test]
    fn preferences_round_trip_through_their_wire_strings() {
        for preference in [Preference::Light, Preference::Dark, Preference::System] {
            assert_eq!(Preference::parse(preference.as_str()), Some(preference));
        }
        assert_eq!(Preference::parse("solarized"), None);
    }
}
