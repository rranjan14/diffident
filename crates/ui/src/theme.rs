//! Colour tokens. One place to change, so views never hardcode a hex value.

use gpui::{Font, FontFeatures, FontStyle, FontWeight, Rgba, TextStyle, px, rgb};

#[derive(Clone)]
pub struct Theme {
    pub bg: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub row_selected: Rgba,
    pub row_hover: Rgba,
    pub added: Rgba,
    pub removed: Rgba,
    pub header_bg: Rgba,
    pub added_bg: Rgba,
    pub removed_bg: Rgba,
    pub scrollbar_thumb: Rgba,
    pub scrollbar_thumb_hover: Rgba,
    pub font_family: &'static str,
    pub font_size: f32,
    /// Every diff row is exactly this tall. `uniform_list` is fixed-height only
    /// (§3), so this value and the row element's height must never diverge.
    pub line_height: f32,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg: rgb(0x18181b),
            border: rgb(0x27272a),
            text: rgb(0xe4e4e7),
            text_muted: rgb(0x71717a),
            row_selected: rgb(0x2a2a2d),
            row_hover: rgb(0x232326),
            added: rgb(0x4ade80),
            removed: rgb(0xf87171),
            header_bg: rgb(0x202024),
            added_bg: rgb(0x14301f),
            removed_bg: rgb(0x3a1618),
            scrollbar_thumb: rgb(0x3f3f46),
            scrollbar_thumb_hover: rgb(0x52525b),
            font_family: "Zed Plex Mono",
            font_size: 13.0,
            line_height: 18.0,
        }
    }

    /// The base style `StyledText::with_default_highlights` layers syntax
    /// colours onto. Ligatures are off because a diff must show the exact
    /// characters in the file — `!=` rendering as `≠` would be a lie.
    pub fn text_style(&self) -> TextStyle {
        TextStyle {
            color: self.text.into(),
            font_family: self.font_family.into(),
            font_features: FontFeatures::disable_ligatures(),
            font_size: px(self.font_size).into(),
            line_height: px(self.line_height).into(),
            font_weight: FontWeight::NORMAL,
            font_style: FontStyle::Normal,
            ..Default::default()
        }
    }

    pub fn font(&self) -> Font {
        self.text_style().font()
    }
}
