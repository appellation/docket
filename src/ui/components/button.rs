//! Text buttons and icon-only buttons.

use gpui::{
    AnyView, App, ClickEvent, ElementId, FontWeight, SharedString, Window, div, prelude::*, px,
};

use super::TestSelector as _;
use super::icon::{Icon, IconName, IconSize};
use crate::ui::theme::{Color, Theme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonStyle {
    /// The one main action in a view.
    #[default]
    Primary,
    /// Bordered; for everything else.
    Secondary,
    /// No chrome until hovered; for toolbars and dense rows.
    Ghost,
    /// Secondary with destructive intent.
    Danger,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonSize {
    #[default]
    Default,
    Compact,
}

impl ButtonSize {
    fn height(self) -> gpui::Pixels {
        match self {
            Self::Default => px(28.),
            Self::Compact => px(24.),
        }
    }
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// The chrome shared by `Button` and `IconButton`.
fn base(
    id: ElementId,
    style: ButtonStyle,
    size: ButtonSize,
    disabled: bool,
    on_click: Option<ClickHandler>,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let theme = Theme::get(cx);
    let el = div()
        .test_selector(|| id.to_string())
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap_1p5()
        .h(size.height())
        .rounded_md()
        .border_1()
        .border_color(gpui::transparent_black())
        .font_weight(FontWeight::MEDIUM)
        .when(size == ButtonSize::Compact, |el| el.text_xs());
    let el = match style {
        ButtonStyle::Primary => el
            .bg(theme.accent)
            .border_color(theme.accent)
            .text_color(theme.text_on_accent),
        ButtonStyle::Secondary => el
            .bg(theme.elevated)
            .border_color(theme.border)
            .text_color(theme.text),
        ButtonStyle::Ghost => el.text_color(theme.text_muted),
        ButtonStyle::Danger => el
            .bg(theme.elevated)
            .border_color(theme.border)
            .text_color(theme.danger),
    };
    if disabled {
        return el.opacity(0.5);
    }
    let (accent_hover, element_hover, danger_subtle, text) = (
        theme.accent_hover,
        theme.element_hover,
        theme.danger_subtle,
        theme.text,
    );
    el.cursor_pointer()
        .hover(move |s| match style {
            ButtonStyle::Primary => s.bg(accent_hover).border_color(accent_hover),
            ButtonStyle::Secondary => s.bg(element_hover),
            ButtonStyle::Ghost => s.bg(element_hover).text_color(text),
            ButtonStyle::Danger => s.bg(danger_subtle),
        })
        .when_some(on_click, |el, on_click| el.on_click(on_click))
}

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    icon: Option<IconName>,
    style: ButtonStyle,
    size: ButtonSize,
    full_width: bool,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            style: ButtonStyle::default(),
            size: ButtonSize::default(),
            full_width: false,
            disabled: false,
            on_click: None,
        }
    }

    /// A leading icon.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Stretch to fill the container, as in stacked forms.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
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
        let icon_color = match self.style {
            ButtonStyle::Primary => Color::OnAccent,
            ButtonStyle::Secondary => Color::Muted,
            ButtonStyle::Ghost => Color::Muted,
            ButtonStyle::Danger => Color::Danger,
        };
        base(
            self.id,
            self.style,
            self.size,
            self.disabled,
            self.on_click,
            cx,
        )
        .map(|el| match self.size {
            ButtonSize::Default => el.px_3(),
            ButtonSize::Compact => el.px_2(),
        })
        .when(self.full_width, |el| el.w_full())
        .children(self.icon.map(|icon| {
            Icon::new(icon)
                .size(match self.size {
                    ButtonSize::Default => IconSize::Small,
                    ButtonSize::Compact => IconSize::XSmall,
                })
                .color(icon_color)
        }))
        .child(self.label)
    }
}

/// A square, icon-only button. Always give it a tooltip naming the action.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    tooltip: Option<SharedString>,
    style: ButtonStyle,
    size: ButtonSize,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, icon: IconName) -> Self {
        Self {
            id: id.into(),
            icon,
            tooltip: None,
            style: ButtonStyle::Ghost,
            size: ButtonSize::default(),
            disabled: false,
            on_click: None,
        }
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
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

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = match self.style {
            ButtonStyle::Primary => Color::OnAccent,
            ButtonStyle::Danger => Color::Danger,
            ButtonStyle::Secondary | ButtonStyle::Ghost => Color::Muted,
        };
        let size = self.size;
        base(
            self.id,
            self.style,
            self.size,
            self.disabled,
            self.on_click,
            cx,
        )
        .w(size.height())
        .child(Icon::new(self.icon).color(color).size(match size {
            ButtonSize::Default => IconSize::Medium,
            ButtonSize::Compact => IconSize::Small,
        }))
        .when_some(self.tooltip, |el, text| {
            el.tooltip(move |_, cx| Tooltip::view(text.clone(), cx))
        })
    }
}

/// A small floating label shown on hover.
pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    pub fn view(text: SharedString, cx: &mut App) -> AnyView {
        cx.new(|_| Self { text }).into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx);
        // Tooltips are positioned at the cursor; the padding keeps them clear
        // of it.
        div().pl_2().pt_2p5().child(
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.elevated)
                .shadow_md()
                .text_xs()
                .text_color(theme.text)
                .child(self.text.clone()),
        )
    }
}
