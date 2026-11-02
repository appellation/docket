//! Navigation rows, section headings, badges, and callouts.

use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, SharedString, Window, div, prelude::*, px,
};

use super::icon::{Icon, IconName, IconSize};
use crate::ui::theme::{Color, Theme};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A selectable row, as in the sidebar: optional leading icon, content, and
/// an end slot for counts or status.
#[derive(IntoElement)]
pub struct ListItem {
    id: ElementId,
    icon: Option<(IconName, Color)>,
    selected: bool,
    children: Vec<AnyElement>,
    end_slot: Option<AnyElement>,
    on_click: Option<ClickHandler>,
}

impl ListItem {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            selected: false,
            children: Vec::new(),
            end_slot: None,
            on_click: None,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some((icon, Color::Muted));
        self
    }

    pub fn icon_color(mut self, icon: IconName, color: Color) -> Self {
        self.icon = Some((icon, color));
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn end_slot(mut self, end_slot: impl IntoElement) -> Self {
        self.end_slot = Some(end_slot.into_any_element());
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

impl ParentElement for ListItem {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ListItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let hover = theme.element_hover;
        div()
            .id(self.id)
            .flex()
            .items_center()
            .gap_2()
            .min_h(px(28.))
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .when(self.selected, |el| el.bg(theme.element_selected))
            .when(!self.selected, |el| el.hover(move |s| s.bg(hover)))
            .children(self.icon.map(|(icon, color)| {
                let color = match (self.selected, color) {
                    (true, Color::Muted) => Color::Default,
                    (_, color) => color,
                };
                Icon::new(icon).size(IconSize::Medium).color(color)
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .children(self.children),
            )
            .children(self.end_slot)
            .when_some(self.on_click, |el, on_click| el.on_click(on_click))
    }
}

/// A small label above a group of rows.
pub fn section_heading(label: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .px_2()
        .pt_3()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(Theme::get(cx).text_subtle)
        .child(label.into())
}

/// A numeric count for a navigation row. Renders nothing for zero.
pub fn count_badge(count: usize, selected: bool, cx: &App) -> impl IntoElement {
    let theme = Theme::get(cx);
    div()
        .flex_none()
        .text_xs()
        .text_color(if selected {
            theme.text
        } else {
            theme.text_muted
        })
        .when(count > 0, |el| el.child(count.to_string()))
}

/// A tinted pill pairing a status icon with a short label.
#[derive(IntoElement)]
pub struct Badge {
    label: SharedString,
    icon: Option<IconName>,
    color: Color,
}

impl Badge {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            color: Color::Muted,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(20.))
            .px_1p5()
            .rounded_md()
            .bg(self.color.subtle_fill(theme))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(self.color.hsla(theme))
            .children(
                self.icon
                    .map(|icon| Icon::new(icon).size(IconSize::XSmall).color(self.color)),
            )
            .child(self.label)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalloutKind {
    Warning,
    Error,
}

/// An inline message block: errors, warnings, and notes that need attention.
#[derive(IntoElement)]
pub struct Callout {
    kind: CalloutKind,
    title: Option<SharedString>,
    message: SharedString,
    actions: Vec<AnyElement>,
}

impl Callout {
    pub fn new(kind: CalloutKind, message: impl Into<SharedString>) -> Self {
        Self {
            kind,
            title: None,
            message: message.into(),
            actions: Vec::new(),
        }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// A button (or other control) below the message that resolves it.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }
}

impl RenderOnce for Callout {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let (icon, color) = match self.kind {
            CalloutKind::Warning => (IconName::TriangleAlert, Color::Warning),
            CalloutKind::Error => (IconName::CircleX, Color::Danger),
        };
        div()
            .flex()
            .items_start()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(color.hsla(theme).alpha(0.3))
            .bg(color.subtle_fill(theme))
            .child(
                div()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .child(Icon::new(icon).color(color)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    // With a title, the message is supporting detail.
                    .text_color(if self.title.is_some() {
                        theme.text_muted
                    } else {
                        theme.text
                    })
                    .children(self.title.map(|title| {
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(title)
                    }))
                    .child(self.message)
                    .when(!self.actions.is_empty(), |el| {
                        el.child(div().flex().gap_2().pt_1().children(self.actions))
                    }),
            )
    }
}
