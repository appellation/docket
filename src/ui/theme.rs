//! Minimal color palette. Swap for a proper theme system later.

use gpui::{App, Global, Hsla, rgb};

pub struct Theme {
    pub background: Hsla,
    pub surface: Hsla,
    pub surface_hover: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub accent: Hsla,
    pub success: Hsla,
    pub danger: Hsla,
}

impl Global for Theme {}

impl Theme {
    pub fn dark() -> Self {
        Self {
            background: rgb(0x0d1117).into(),
            surface: rgb(0x161b22).into(),
            surface_hover: rgb(0x21262d).into(),
            border: rgb(0x30363d).into(),
            text: rgb(0xe6edf3).into(),
            text_muted: rgb(0x8b949e).into(),
            accent: rgb(0x2f81f7).into(),
            success: rgb(0x3fb950).into(),
            danger: rgb(0xf85149).into(),
        }
    }

    pub fn init(cx: &mut App) {
        cx.set_global(Self::dark());
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}
