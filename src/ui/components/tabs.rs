//! A row of tabs under a view's header, like a pull request's Conversation
//! and Files.

use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, SharedString, Window, div, prelude::*, px,
};

use super::TestSelector as _;
use super::icon::{Icon, IconName};
use crate::ui::theme::{Color, Theme};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// The row the tabs sit in, along the bottom border of the area above.
#[derive(IntoElement, Default)]
pub struct TabBar {
    children: Vec<AnyElement>,
}

impl TabBar {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ParentElement for TabBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TabBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .items_end()
            .gap_4()
            .h(px(36.))
            .px_4()
            .border_b_1()
            .border_color(Theme::get(cx).border_subtle)
            .children(self.children)
    }
}

/// One tab: an icon, a label, and optionally a count, underlined in the
/// accent color when selected.
#[derive(IntoElement)]
pub struct TabBarItem {
    id: ElementId,
    icon: IconName,
    label: SharedString,
    count: Option<SharedString>,
    selected: bool,
    on_click: Option<ClickHandler>,
}

impl TabBarItem {
    pub fn new(id: impl Into<ElementId>, icon: IconName, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon,
            label: label.into(),
            count: None,
            selected: false,
            on_click: None,
        }
    }

    pub fn count(mut self, count: Option<impl Into<SharedString>>) -> Self {
        self.count = count.map(Into::into);
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
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

impl RenderOnce for TabBarItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let selected = self.selected;
        div()
            .test_selector(|| self.id.to_string())
            .id(self.id)
            .flex()
            .items_center()
            .gap_1p5()
            .h_full()
            .px_1()
            .border_b_2()
            .border_color(if selected {
                theme.accent
            } else {
                gpui::transparent_black()
            })
            .text_color(if selected {
                theme.text
            } else {
                theme.text_muted
            })
            .when(selected, |el| el.font_weight(FontWeight::MEDIUM))
            .cursor_pointer()
            .hover(|style| style.text_color(theme.text))
            .child(Icon::new(self.icon).color(if selected {
                Color::Default
            } else {
                Color::Muted
            }))
            .child(self.label)
            .children(self.count.map(|count| {
                div()
                    .px_1p5()
                    .rounded_full()
                    .text_xs()
                    .bg(theme.element_selected)
                    .child(count)
            }))
            .when_some(self.on_click, |el, on_click| el.on_click(on_click))
    }
}
