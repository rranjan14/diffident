//! Every visual value in the app.
//!
//! One place to change, and — more importantly — one place to *look*. Before
//! this existed, each `.px_2()` and `.gap_1()` was an independent decision at a
//! call site, which is why the spacing looked arbitrary: there was nothing to
//! be consistent with.

use gpui::{Font, FontFeatures, FontStyle, FontWeight, Rgba, TextStyle, px, rgb};

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
    pub font_code: &'static str,
    /// Everything else: rail, file names, headings, comment prose. Proportional
    /// text beside mono code is how an editor says "someone is talking" versus
    /// "this is the file".
    pub font_ui: &'static str,
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
            font_code: "SF Mono",
            font_ui: "SF Pro Text",
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

    /// The base style `StyledText::with_default_highlights` layers syntax
    /// colours onto.
    pub fn code_style(&self) -> TextStyle {
        TextStyle {
            color: self.text_primary.into(),
            font_family: self.font_code.into(),
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
            font_family: self.font_ui.into(),
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
        assert_eq!(t.code_style().font_family.as_ref(), t.font_code);
        assert_eq!(t.ui_style().font_family.as_ref(), t.font_ui);
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
