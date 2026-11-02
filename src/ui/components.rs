//! Small shared UI pieces.

use std::path::Path;

use gpui::{App, ClickEvent, ElementId, SharedString, Window, div, prelude::*};

use super::theme::Theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Primary,
    Secondary,
    Danger,
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    style: ButtonStyle,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            style: ButtonStyle::Primary,
            disabled: false,
            on_click: None,
        }
    }

    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let base = div()
            .id(self.id)
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .child(self.label);
        let base = match self.style {
            ButtonStyle::Primary => base.bg(theme.accent).border_color(theme.accent),
            ButtonStyle::Secondary => base.bg(theme.surface).border_color(theme.border),
            ButtonStyle::Danger => base
                .bg(theme.surface)
                .border_color(theme.border)
                .text_color(theme.danger),
        };
        if self.disabled {
            return base.opacity(0.5);
        }
        let hover = theme.surface_hover;
        let style = self.style;
        base.cursor_pointer()
            .hover(move |s| match style {
                ButtonStyle::Primary => s.opacity(0.9),
                ButtonStyle::Secondary | ButtonStyle::Danger => s.bg(hover),
            })
            .when_some(self.on_click, |el, on_click| el.on_click(on_click))
    }
}

/// Formats a path for display, abbreviating the home directory to `~`.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return Path::new("~").join(rest).display().to_string();
    }
    path.display().to_string()
}
