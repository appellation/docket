//! Semantic color tokens with light and dark palettes.
//!
//! Views style themselves from these tokens, never from raw colors, so both
//! appearances stay consistent. The active palette follows the system
//! appearance unless the user picks one from the View menu.

use gpui::{App, Global, Hsla, Rgba, WindowAppearance, rgb};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Appearance {
    Light,
    Dark,
}

impl From<WindowAppearance> for Appearance {
    fn from(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
        }
    }
}

/// The user's appearance preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

pub struct Theme {
    mode: ThemeMode,

    /// Main content area.
    pub background: Hsla,
    /// Chrome around the content: sidebar, headers.
    pub surface: Hsla,
    /// Controls and cards that sit on top of a surface: inputs, lists.
    pub elevated: Hsla,
    /// Translucent overlays, so they work on any surface.
    pub element_hover: Hsla,
    pub element_selected: Hsla,

    pub border: Hsla,
    pub border_subtle: Hsla,
    pub border_focused: Hsla,

    pub text: Hsla,
    pub text_muted: Hsla,
    /// Placeholders, disabled text, tertiary detail.
    pub text_subtle: Hsla,
    pub text_on_accent: Hsla,

    /// Fill for primary actions and selection indicators.
    pub accent: Hsla,
    pub accent_hover: Hsla,
    /// Accent for text and icons, tuned for contrast on `background`.
    pub text_accent: Hsla,
    pub accent_subtle: Hsla,

    /// Status colors are for text and icons; `*_subtle` are their fills.
    pub success: Hsla,
    pub success_subtle: Hsla,
    pub warning: Hsla,
    pub warning_subtle: Hsla,
    pub danger: Hsla,
    pub danger_subtle: Hsla,
    pub merged: Hsla,
    pub merged_subtle: Hsla,
}

impl Global for Theme {}

/// Opacity of status colors when used as a fill behind text.
const SUBTLE: f32 = 0.14;

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

fn alpha(color: Rgba, a: f32) -> Hsla {
    Hsla::from(color).alpha(a)
}

impl Theme {
    pub fn light() -> Self {
        let accent = hex(0x2563eb);
        let success = hex(0x1a7f37);
        let warning = hex(0x9a6700);
        let danger = hex(0xcf222e);
        let merged = hex(0x8250df);
        Self {
            mode: ThemeMode::System,
            background: hex(0xffffff),
            surface: hex(0xf6f7f9),
            elevated: hex(0xffffff),
            element_hover: alpha(rgb(0x0f1729), 0.05),
            element_selected: alpha(rgb(0x0f1729), 0.08),
            border: hex(0xdcdfe4),
            border_subtle: hex(0xe9ebef),
            border_focused: accent,
            text: hex(0x1a1d23),
            text_muted: hex(0x5b616e),
            text_subtle: hex(0x8a909c),
            text_on_accent: hex(0xffffff),
            accent,
            accent_hover: hex(0x1d4ed8),
            text_accent: hex(0x1d5bd6),
            accent_subtle: accent.alpha(0.10),
            success,
            success_subtle: success.alpha(SUBTLE * 0.8),
            warning,
            warning_subtle: warning.alpha(SUBTLE * 0.8),
            danger,
            danger_subtle: danger.alpha(SUBTLE * 0.7),
            merged,
            merged_subtle: merged.alpha(SUBTLE * 0.8),
        }
    }

    pub fn dark() -> Self {
        let accent = hex(0x2f6ae8);
        let success = hex(0x46b75a);
        let warning = hex(0xd4a23a);
        let danger = hex(0xef6b63);
        let merged = hex(0xa57cf5);
        Self {
            mode: ThemeMode::System,
            background: hex(0x111316),
            surface: hex(0x17191d),
            elevated: hex(0x1c1f24),
            element_hover: alpha(rgb(0xffffff), 0.05),
            element_selected: alpha(rgb(0xffffff), 0.08),
            border: hex(0x2b2f36),
            border_subtle: hex(0x22252b),
            border_focused: hex(0x4a84f0),
            text: hex(0xe6e8eb),
            text_muted: hex(0x9aa0aa),
            text_subtle: hex(0x6c727d),
            text_on_accent: hex(0xffffff),
            accent,
            accent_hover: hex(0x4078ee),
            text_accent: hex(0x74a5fb),
            accent_subtle: accent.alpha(0.18),
            success,
            success_subtle: success.alpha(SUBTLE),
            warning,
            warning_subtle: warning.alpha(SUBTLE),
            danger,
            danger_subtle: danger.alpha(SUBTLE),
            merged,
            merged_subtle: merged.alpha(SUBTLE),
        }
    }

    fn for_appearance(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Light => Self::light(),
            Appearance::Dark => Self::dark(),
        }
    }

    pub fn init(cx: &mut App) {
        let theme = Self::for_appearance(cx.window_appearance().into());
        cx.set_global(theme);
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// Applies the user's preference and redraws every window.
    pub fn set_mode(mode: ThemeMode, cx: &mut App) {
        let appearance = match mode {
            ThemeMode::System => cx.window_appearance().into(),
            ThemeMode::Light => Appearance::Light,
            ThemeMode::Dark => Appearance::Dark,
        };
        let mut theme = Self::for_appearance(appearance);
        theme.mode = mode;
        cx.set_global(theme);
        cx.refresh_windows();
    }

    /// Call when the system appearance changes; no-op unless following it.
    pub fn sync_system_appearance(cx: &mut App) {
        if Self::get(cx).mode == ThemeMode::System {
            Self::set_mode(ThemeMode::System, cx);
        }
    }
}

/// A semantic color for text and icons, resolved against the active theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Muted,
    Subtle,
    Accent,
    OnAccent,
    Success,
    Warning,
    Danger,
    Merged,
}

impl Color {
    pub fn hsla(self, theme: &Theme) -> Hsla {
        match self {
            Self::Default => theme.text,
            Self::Muted => theme.text_muted,
            Self::Subtle => theme.text_subtle,
            Self::Accent => theme.text_accent,
            Self::OnAccent => theme.text_on_accent,
            Self::Success => theme.success,
            Self::Warning => theme.warning,
            Self::Danger => theme.danger,
            Self::Merged => theme.merged,
        }
    }

    /// The tinted fill that pairs with this color, for badges and callouts.
    pub fn subtle_fill(self, theme: &Theme) -> Hsla {
        match self {
            Self::Accent | Self::OnAccent => theme.accent_subtle,
            Self::Success => theme.success_subtle,
            Self::Warning => theme.warning_subtle,
            Self::Danger => theme.danger_subtle,
            Self::Merged => theme.merged_subtle,
            Self::Default | Self::Muted | Self::Subtle => theme.element_selected,
        }
    }
}
