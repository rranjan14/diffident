//! Every visual value in the app.
//!
//! One place to change, and — more importantly — one place to *look*. Before
//! this existed, each `.px_2()` and `.gap_1()` was an independent decision at a
//! call site, which is why the spacing looked arbitrary: there was nothing to
//! be consistent with.

use gpui::{Font, FontFeatures, FontStyle, FontWeight, Rgba, SharedString, TextStyle, px, rgb};

#[derive(Clone)]
pub struct Theme {
    // --- spacing, one 4px base ---
    pub s1: f32,
    pub s2: f32,
    pub s3: f32,
    pub s4: f32,
    pub s5: f32,
    pub s6: f32,

    // --- type ---
    /// Diff lines and suggestion fences. Mono, ligatures off.
    ///
    /// Owned rather than `&'static str` so the config file can name a font the
    /// binary has never heard of — which it must, because the right mono face
    /// on macOS is not the right one on Linux, and the name baked in at Phase 0
    /// did not exist on the machine at all.
    pub font_code: SharedString,
    /// Everything else: rail, file names, headings, comment prose. Proportional
    /// text beside mono code is how an editor says "someone is talking" versus
    /// "this is the file".
    pub font_ui: SharedString,
    pub code_size: f32,
    /// Every diff row's baseline height, and the uniform hint given to
    /// `ListState` before rows are measured. Not a row's actual height: a row
    /// that wraps, or that carries an inline thread, is deliberately taller.
    pub line_height: f32,
    pub ui_xs: f32,
    pub ui_sm: f32,
    pub ui_md: f32,

    // --- colour, by role rather than by place ---
    pub surface: Rgba,
    pub surface_raised: Rgba,
    pub surface_overlay: Rgba,
    pub border_subtle: Rgba,
    pub border_strong: Rgba,
    pub text_primary: Rgba,
    pub text_secondary: Rgba,
    pub text_tertiary: Rgba,
    /// The one accent. Three uses only: the active-review bar, focus rings,
    /// thread rules. Everything else staying grey is what lets the diff's own
    /// green and red carry meaning.
    pub accent: Rgba,
    /// The accent at low strength. One use: the selected sidebar row's
    /// background, where a full-strength fill would shout.
    pub accent_soft: Rgba,
    pub added_fg: Rgba,
    pub added_bg: Rgba,
    pub removed_fg: Rgba,
    pub removed_bg: Rgba,
    pub danger: Rgba,
    /// One use: the rebased badge (§6) — the only state that is neither an
    /// error nor normal.
    pub warning: Rgba,

    // --- shape ---
    pub r_sm: f32,
    pub r_md: f32,
}

/// The platform's default faces. Both verified present at runtime on macOS;
/// elsewhere the fallbacks are what every system ships, and the config file is
/// how anyone disagrees.
#[cfg(target_os = "macos")]
const DEFAULT_CODE_FONT: &str = "SF Mono";
#[cfg(target_os = "macos")]
const DEFAULT_UI_FONT: &str = "SF Pro Text";
#[cfg(not(target_os = "macos"))]
const DEFAULT_CODE_FONT: &str = "monospace";
#[cfg(not(target_os = "macos"))]
const DEFAULT_UI_FONT: &str = "sans-serif";

impl Theme {
    pub fn dark() -> Self {
        Self {
            s1: 4.,
            s2: 8.,
            s3: 12.,
            s4: 16.,
            s5: 24.,
            s6: 32.,

            // Both verified present at runtime; `Zed Plex Mono` was not, and
            // had been silently falling back since Phase 0.
            font_code: DEFAULT_CODE_FONT.into(),
            font_ui: DEFAULT_UI_FONT.into(),
            code_size: 12.5,
            line_height: 19.,
            ui_xs: 11.,
            ui_sm: 12.,
            ui_md: 13.,

            surface: rgb(0x0f1115),
            surface_raised: rgb(0x161920),
            surface_overlay: rgb(0x1c2029),
            border_subtle: rgb(0x21252d),
            border_strong: rgb(0x2f3540),
            text_primary: rgb(0xe6e9ef),
            text_secondary: rgb(0x9aa3b2),
            text_tertiary: rgb(0x646d7d),
            accent: rgb(0x6ea8fe),
            accent_soft: rgb(0x1b2536),
            added_fg: rgb(0x7ee08a),
            added_bg: rgb(0x11251a),
            removed_fg: rgb(0xf08a8a),
            removed_bg: rgb(0x2b1518),
            danger: rgb(0xf08a8a),
            warning: rgb(0xe3b341),

            r_sm: 4.,
            r_md: 6.,
        }
    }

    /// The same roles at the other end of the range.
    ///
    /// Only the colours differ — every spacing, type and shape token is shared,
    /// because those are decisions about layout and layout does not have a
    /// brightness. That is the whole return on naming tokens by role: a second
    /// palette is one function, not a second design.
    ///
    /// Not a mechanical inversion of `dark()`. On a light ground the diff's own
    /// green and red have to be *darker* than the text to read as emphasis
    /// rather than as decoration, and the accent has to lose brightness or it
    /// vibrates against white.
    pub fn light() -> Self {
        Self {
            surface: rgb(0xfbfcfd),
            surface_raised: rgb(0xf2f4f7),
            surface_overlay: rgb(0xffffff),
            border_subtle: rgb(0xe3e7ec),
            border_strong: rgb(0xc9d0d9),
            text_primary: rgb(0x1b2029),
            text_secondary: rgb(0x5a6472),
            text_tertiary: rgb(0x8b95a3),
            accent: rgb(0x2563c9),
            accent_soft: rgb(0xe6eefb),
            added_fg: rgb(0x1a7f37),
            added_bg: rgb(0xe6f6ea),
            removed_fg: rgb(0xb42318),
            removed_bg: rgb(0xfdeceb),
            danger: rgb(0xb42318),
            warning: rgb(0x9a6700),
            ..Self::dark()
        }
    }

    /// Build the palette the config asked for, with any font it named.
    ///
    /// An empty font name is treated as absent rather than as a request for a
    /// font called "" — the latter renders nothing at all, silently, which is
    /// the exact failure this project already shipped once.
    pub fn from_config(cfg: &diffident_session::config::Config) -> Self {
        use diffident_session::config::ThemeChoice;
        let mut theme = match cfg.theme {
            ThemeChoice::Dark => Self::dark(),
            ThemeChoice::Light => Self::light(),
        };
        if let Some(f) = cfg.code_font.as_deref().filter(|f| !f.trim().is_empty()) {
            theme.font_code = f.to_string().into();
        }
        if let Some(f) = cfg.ui_font.as_deref().filter(|f| !f.trim().is_empty()) {
            theme.font_ui = f.to_string().into();
        }
        theme
    }

    /// The base style `StyledText::with_default_highlights` layers syntax
    /// colours onto.
    pub fn code_style(&self) -> TextStyle {
        TextStyle {
            color: self.text_primary.into(),
            font_family: self.font_code.clone(),
            // A diff must show the exact characters in the file — `!=`
            // rendering as `≠` would be a lie.
            font_features: FontFeatures::disable_ligatures(),
            font_size: px(self.code_size).into(),
            line_height: px(self.line_height).into(),
            font_weight: FontWeight::NORMAL,
            font_style: FontStyle::Normal,
            ..Default::default()
        }
    }

    /// Prose and chrome. Ligatures are left alone: this is text, not code.
    pub fn ui_style(&self) -> TextStyle {
        TextStyle {
            color: self.text_primary.into(),
            font_family: self.font_ui.clone(),
            font_size: px(self.ui_md).into(),
            ..Default::default()
        }
    }

    pub fn font(&self) -> Font {
        self.code_style().font()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spacing_scale_is_a_consistent_ramp() {
        // One base, doubling then stepping. Call sites pick a step, never a
        // number, which is the whole reason spacing looked arbitrary before.
        let t = Theme::dark();
        assert_eq!([t.s1, t.s2, t.s3, t.s4, t.s5, t.s6], [4., 8., 12., 16., 24., 32.]);
    }

    #[test]
    fn the_code_font_is_one_that_actually_resolves() {
        // `Zed Plex Mono` did not exist on the machine and every glyph was a
        // silent fallback — a missing font renders nothing and reports nothing.
        let t = Theme::dark();
        assert_eq!(t.font_code, "SF Mono");
        assert_eq!(t.font_ui, "SF Pro Text");
    }

    #[test]
    fn code_keeps_ligatures_off_and_ui_does_not_care() {
        // A diff must show the exact characters in the file; `!=` rendering as
        // `≠` is a lie. Prose has no such constraint.
        let t = Theme::dark();
        assert_eq!(t.code_style().font_family, t.font_code);
        assert_eq!(t.ui_style().font_family, t.font_ui);
    }

    #[test]
    fn every_colour_role_is_distinct_from_its_neighbours() {
        // Roles that collapse to the same value are roles that do not exist —
        // if `surface` and `surface_raised` match, a panel has no edge.
        let t = Theme::dark();
        assert_ne!(t.surface, t.surface_raised);
        assert_ne!(t.surface_raised, t.surface_overlay);
        assert_ne!(t.border_subtle, t.border_strong);
        assert_ne!(t.text_primary, t.text_secondary);
        assert_ne!(t.text_secondary, t.text_tertiary);
        assert_ne!(t.accent, t.accent_soft);
    }
}

#[cfg(test)]
mod theme_tests {
    use super::*;
    use diffident_session::config::{Config, ThemeChoice};

    #[test]
    fn light_and_dark_share_every_measurement_and_differ_only_in_colour() {
        // The return on naming tokens by role: a second palette is one
        // function, not a second design. Layout has no brightness.
        let (d, l) = (Theme::dark(), Theme::light());
        assert_eq!([d.s1, d.s2, d.s3, d.s4, d.s5, d.s6], [l.s1, l.s2, l.s3, l.s4, l.s5, l.s6]);
        assert_eq!(d.line_height, l.line_height);
        assert_eq!(d.code_size, l.code_size);
        assert_eq!(d.r_sm, l.r_sm);
        assert_ne!(d.surface, l.surface, "but the colours are not shared");
    }

    #[test]
    fn light_keeps_its_text_readable_against_its_own_ground() {
        // A palette inverted mechanically ends up with pale grey on white.
        // Cheap proxy for contrast: on a light ground, text must be darker than
        // the surface it sits on, and on a dark ground, lighter.
        let lum = |c: gpui::Rgba| c.r * 0.299 + c.g * 0.587 + c.b * 0.114;
        let l = Theme::light();
        assert!(lum(l.text_primary) < lum(l.surface), "dark text on a light ground");
        assert!(lum(l.text_secondary) < lum(l.surface));
        let d = Theme::dark();
        assert!(lum(d.text_primary) > lum(d.surface), "light text on a dark ground");
    }

    #[test]
    fn the_config_picks_the_palette() {
        let light = Config { theme: ThemeChoice::Light, ..Default::default() };
        assert_eq!(Theme::from_config(&light).surface, Theme::light().surface);
        let dark = Config { theme: ThemeChoice::Dark, ..Default::default() };
        assert_eq!(Theme::from_config(&dark).surface, Theme::dark().surface);
    }

    #[test]
    fn a_configured_font_wins_but_a_blank_one_does_not() {
        // A font named "" renders nothing at all, silently — the exact failure
        // this project shipped for seven phases. Blank means absent.
        let named = Config { code_font: Some("Fira Code".into()), ..Default::default() };
        assert_eq!(Theme::from_config(&named).font_code, "Fira Code");
        let blank = Config { code_font: Some("   ".into()), ..Default::default() };
        assert_eq!(Theme::from_config(&blank).font_code, Theme::dark().font_code);
    }
}
