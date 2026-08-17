//! The user's config file (§9 Phase 8).
//!
//! Lives here rather than in a crate of its own because this crate already owns
//! "state that outlives the process, kept in files under the user's home" — a
//! config is that, just authored by a human instead of by the app.
//!
//! **Nothing here can fail loudly.** A missing file, a typo, a value out of
//! range: each falls back to a working default and says why, because a review
//! tool that refuses to start over a stray comma is worse than one that ignores
//! it. The reason travels back with the config so the UI can mention it once;
//! it is never a reason not to open.

use serde::Deserialize;
use std::path::PathBuf;

/// Which palette to build. Not the palette itself — that lives in the UI crate,
/// which is the only place that knows what a colour is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub theme: ThemeChoice,
    pub sidebar_width: f32,
    pub wrap: bool,
    /// `None` means "whatever the platform's default is". Deliberately not a
    /// baked-in name: the font that shipped in Phase 0 did not exist on the
    /// machine and every glyph was a silent fallback for seven phases. A name
    /// that is right on macOS is wrong on Linux, so the default belongs to the
    /// UI, not to this file.
    pub code_font: Option<String>,
    pub ui_font: Option<String>,
}

/// The width the sidebar is clamped to, matching the drag handle's limits —
/// a config that could set a width the mouse cannot is a config that lies.
const MIN_SIDEBAR: f32 = 200.;
const MAX_SIDEBAR: f32 = 480.;

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            sidebar_width: 280.,
            wrap: true,
            code_font: None,
            ui_font: None,
        }
    }
}

/// The file's shape. Every field optional, so a config naming one setting keeps
/// the defaults for everything else — the alternative is a file you must write
/// in full to change one line.
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct File {
    theme: Option<String>,
    sidebar_width: Option<f32>,
    wrap: Option<bool>,
    code_font: Option<String>,
    ui_font: Option<String>,
}

/// Parse config text. Separate from reading the file so it is testable with no
/// disk and no home directory, the same split `threads::parse_page` makes.
///
/// Returns the config to use and, when something was wrong, one sentence
/// saying what — never an `Err`, because there is always a usable answer.
pub fn parse(text: &str) -> (Config, Option<String>) {
    let file: File = match toml::from_str(text) {
        Ok(f) => f,
        Err(e) => {
            // The whole file is unusable, so every default applies. Reporting
            // the first line is enough to find it; the full parser error is
            // several lines of span art nobody wants in a rail.
            let why = e.message().lines().next().unwrap_or("could not be read").to_string();
            return (Config::default(), Some(format!("config ignored: {why}")));
        }
    };

    let mut cfg = Config::default();
    let mut complaint = None;

    if let Some(theme) = file.theme {
        match theme.as_str() {
            "dark" => cfg.theme = ThemeChoice::Dark,
            "light" => cfg.theme = ThemeChoice::Light,
            other => {
                complaint = Some(format!("unknown theme {other:?}, using dark"));
            }
        }
    }
    if let Some(w) = file.sidebar_width {
        // Clamped rather than refused: a width outside the drag handle's range
        // is a number to bring into range, not a reason to fail.
        cfg.sidebar_width = w.clamp(MIN_SIDEBAR, MAX_SIDEBAR);
    }
    if let Some(wrap) = file.wrap {
        cfg.wrap = wrap;
    }
    cfg.code_font = file.code_font;
    cfg.ui_font = file.ui_font;

    (cfg, complaint)
}

/// Where the config file lives. Same convention as `store::default_root`.
pub fn default_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| {
            if cfg!(target_os = "macos") {
                h.join("Library/Application Support/diffident/config.toml")
            } else {
                h.join(".config/diffident/config.toml")
            }
        })
        .unwrap_or_else(|| std::env::temp_dir().join("diffident-config.toml"))
}

/// Read the config, or the defaults.
///
/// A missing file is the normal case, not an error — most people never write
/// one — so it produces no complaint at all.
pub fn load(path: &std::path::Path) -> (Config, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
        Err(e) => (
            Config::default(),
            Some(format!("config at {} could not be read: {e}", path.display())),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_config_is_all_defaults_and_no_complaint() {
        let (cfg, why) = parse("");
        assert_eq!(cfg, Config::default());
        assert!(why.is_none(), "an empty file is not a mistake");
    }

    #[test]
    fn naming_one_setting_keeps_the_defaults_for_the_rest() {
        // Otherwise you would have to write the file in full to change one line.
        let (cfg, why) = parse(r#"theme = "light""#);
        assert_eq!(cfg.theme, ThemeChoice::Light);
        assert_eq!(cfg.sidebar_width, Config::default().sidebar_width);
        assert!(cfg.wrap);
        assert!(why.is_none());
    }

    #[test]
    fn a_broken_file_still_starts_the_app_and_says_why() {
        // A review tool that refuses to open over a stray comma is worse than
        // one that ignores it.
        let (cfg, why) = parse("theme = \nnot toml at all [[[");
        assert_eq!(cfg, Config::default());
        assert!(why.is_some_and(|w| w.starts_with("config ignored:")));
    }

    #[test]
    fn an_unknown_theme_falls_back_and_names_itself() {
        let (cfg, why) = parse(r#"theme = "solarized""#);
        assert_eq!(cfg.theme, ThemeChoice::Dark);
        assert!(why.is_some_and(|w| w.contains("solarized")), "say which word was wrong");
    }

    #[test]
    fn a_sidebar_width_out_of_range_is_clamped_not_refused() {
        // The drag handle clamps to the same range; a config that could set a
        // width the mouse cannot would be a config that lies.
        assert_eq!(parse("sidebar_width = 10000").0.sidebar_width, MAX_SIDEBAR);
        assert_eq!(parse("sidebar_width = 1").0.sidebar_width, MIN_SIDEBAR);
    }

    #[test]
    fn a_misspelled_key_is_reported_rather_than_silently_ignored() {
        // `deny_unknown_fields`: silently ignoring `sidbar_width` would leave
        // someone editing a file that does nothing, with no way to tell.
        let (cfg, why) = parse("sidbar_width = 300");
        assert_eq!(cfg, Config::default());
        assert!(why.is_some(), "a typo the user cannot see is worse than an error");
    }

    #[test]
    fn fonts_default_to_the_platforms_choice_rather_than_a_baked_in_name() {
        // The font baked in at Phase 0 did not exist on the machine and every
        // glyph was a silent fallback for seven phases. A name that is right on
        // macOS is wrong on Linux.
        assert_eq!(parse("").0.code_font, None);
        assert_eq!(
            parse(r#"code_font = "Fira Code""#).0.code_font.as_deref(),
            Some("Fira Code")
        );
    }

    #[test]
    fn a_missing_file_is_the_normal_case_and_says_nothing() {
        let (cfg, why) = load(std::path::Path::new("/nonexistent/diffident/config.toml"));
        assert_eq!(cfg, Config::default());
        assert!(why.is_none(), "most people never write one");
    }
}
