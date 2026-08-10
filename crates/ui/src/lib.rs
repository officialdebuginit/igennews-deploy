//! `meridian-ui` — the Meridian design system.
//!
//! Step 1 of the design-system migration (see MERIDIAN.md §4): own the token
//! layer, add dark mode, and give the vendored components somewhere to land.
//! The app depends on this crate for its stylesheets rather than carrying a
//! `:root` block of its own, so the components and the app can never drift onto
//! two different palettes.
//!
//! Components arrive here next, wrapped one per file: route files import the
//! Meridian wrapper, never `dioxus-primitives` directly, so an upstream API
//! change stays a one-file fix.

use dioxus::prelude::*;

/// The design-system token layer. Must be linked **before** the app stylesheet
/// so app rules can override a token-derived default.
pub const TOKENS_CSS: Asset = asset!("/assets/tokens.css");

/// Aliases upstream component variable names onto Meridian tokens.
pub const DX_ADAPTER_CSS: Asset = asset!("/assets/dx-adapter.css");

/// Which palette the app is rendering in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    /// Follow the operating system.
    System,
    Light,
    /// Soft, readable dark (default dark) — slate, not pure black.
    Slate,
    /// True near-black dark.
    Midnight,
    /// Warm paper (sepia), for long-form reading.
    Sepia,
}

impl Theme {
    /// The `data-theme` value, or `None` when following the OS — in which case
    /// the attribute is removed so the `prefers-color-scheme` rule applies.
    #[must_use]
    pub const fn attribute(self) -> Option<&'static str> {
        match self {
            Theme::System => None,
            Theme::Light => Some("light"),
            Theme::Slate => Some("slate"),
            Theme::Midnight => Some("midnight"),
            Theme::Sepia => Some("sepia"),
        }
    }

    /// Parses a stored preference; anything unrecognised falls back to
    /// following the system rather than guessing a palette. `dark` maps to
    /// [`Theme::Slate`] for backward compatibility with the old two-way toggle,
    /// and `system` is stored explicitly so "follow the OS" survives a reload
    /// instead of being re-defaulted (see [`apply_theme_script`]).
    #[must_use]
    pub fn from_stored(value: &str) -> Self {
        match value {
            "light" => Theme::Light,
            "slate" | "dark" => Theme::Slate,
            "midnight" => Theme::Midnight,
            "sepia" => Theme::Sepia,
            "system" => Theme::System,
            _ => Theme::System,
        }
    }

    /// The next theme in the cycle: System → Light → Slate → Midnight → Sepia → System.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Theme::System => Theme::Light,
            Theme::Light => Theme::Slate,
            Theme::Slate => Theme::Midnight,
            Theme::Midnight => Theme::Sepia,
            Theme::Sepia => Theme::System,
        }
    }

    /// The label shown on the toggle.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Theme::System => "System theme",
            Theme::Light => "Light theme",
            Theme::Slate => "Slate (dark)",
            Theme::Midnight => "Midnight",
            Theme::Sepia => "Sepia",
        }
    }
}

/// The JavaScript that applies a theme to `<html>` and remembers the choice.
///
/// Written as a snippet rather than a `web-sys` call so the crate stays
/// dependency-free and usable from the server target, where it is inert.
#[must_use]
pub fn apply_theme_script(theme: Theme) -> String {
    match theme.attribute() {
        Some(value) => format!(
            "document.documentElement.setAttribute('data-theme','{value}');\
             try{{localStorage.setItem('meridian.theme','{value}');}}catch(e){{}}"
        ),
        // "System" removes the attribute so the OS preference applies, but records
        // the choice as `system` so it survives a reload instead of falling back to
        // the app default theme.
        None => "document.documentElement.removeAttribute('data-theme');\
                 try{localStorage.setItem('meridian.theme','system');}catch(e){}"
            .to_owned(),
    }
}

/// The app-wide default palette when a user has expressed no preference. Warm paper
/// (sepia) — chosen as the house default; a user may still switch to any theme.
pub const DEFAULT_THEME: Theme = Theme::Sepia;

/// A minimal boot snippet that reflects a theme onto `<html>` on first paint
/// **without** writing storage — used to apply the resolved default/stored theme
/// before content draws, so there is no flash of the wrong palette on reload.
#[must_use]
pub fn boot_theme_script(theme: Theme) -> String {
    match theme.attribute() {
        Some(value) => format!("document.documentElement.setAttribute('data-theme','{value}');"),
        None => "document.documentElement.removeAttribute('data-theme');".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::Theme;

    #[test]
    fn the_toggle_cycles_back_to_following_the_system() {
        let mut theme = Theme::System;
        for expected in [Theme::Light, Theme::Slate, Theme::Midnight, Theme::Sepia, Theme::System] {
            theme = theme.next();
            assert_eq!(theme, expected);
        }
    }

    #[test]
    fn system_removes_the_attribute_so_the_os_preference_applies() {
        assert_eq!(Theme::System.attribute(), None);
        assert_eq!(Theme::Slate.attribute(), Some("slate"));
        assert_eq!(Theme::Midnight.attribute(), Some("midnight"));
        assert_eq!(Theme::Light.attribute(), Some("light"));
    }

    #[test]
    fn stored_values_parse_and_dark_stays_backward_compatible() {
        assert_eq!(Theme::from_stored("dark"), Theme::Slate);
        assert_eq!(Theme::from_stored("slate"), Theme::Slate);
        assert_eq!(Theme::from_stored("sepia"), Theme::Sepia);
        assert_eq!(Theme::from_stored("light"), Theme::Light);
        assert_eq!(Theme::from_stored(""), Theme::System);
        assert_eq!(Theme::from_stored("bogus"), Theme::System);
    }

    #[test]
    fn the_script_sets_and_clears_the_attribute() {
        assert!(super::apply_theme_script(Theme::Slate).contains("setAttribute('data-theme','slate')"));
        assert!(super::apply_theme_script(Theme::System).contains("removeAttribute('data-theme')"));
    }
}
