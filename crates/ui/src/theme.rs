//! Colour tokens. One place to change, so views never hardcode a hex value.

use gpui::{Rgba, rgb};

pub struct Theme {
    pub bg: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub row_selected: Rgba,
    pub row_hover: Rgba,
    pub added: Rgba,
    pub removed: Rgba,
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
        }
    }
}
