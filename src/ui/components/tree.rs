//! Rows of a file tree: the file browser's directory tree, and the changed
//! files beside a diff.

use gpui::{AnyElement, App, ClickEvent, ElementId, SharedString, Window, div, prelude::*, px};

use super::TestSelector as _;
use super::button::Tooltip;
use super::icon::{Icon, IconName, IconSize};
use crate::ui::theme::{Color, Theme};

/// Every row is this tall, so trees can be virtualized (`uniform_list`).
const TREE_ROW_HEIGHT: f32 = 24.;
/// How far each level of depth is indented.
const TREE_INDENT: f32 = 12.;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A file or directory in a tree: a disclosure chevron (for directories),
/// an icon, a name, and an end slot for counts or stats.
#[derive(IntoElement)]
pub struct TreeItem {
    id: ElementId,
    selector: Option<String>,
    depth: usize,
    disclosure: Option<bool>,
    lead: Option<AnyElement>,
    icon: Option<(IconName, Color)>,
    selected: bool,
    children: Vec<AnyElement>,
    end_slot: Option<AnyElement>,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl TreeItem {
    pub fn new(id: impl Into<ElementId>, depth: usize) -> Self {
        Self {
            id: id.into(),
            selector: None,
            depth,
            disclosure: None,
            lead: None,
            icon: None,
            selected: false,
            children: Vec::new(),
            end_slot: None,
            tooltip: None,
            on_click: None,
        }
    }

    /// The test selector, when it isn't the id.
    pub fn selector(mut self, selector: impl Into<String>) -> Self {
        self.selector = Some(selector.into());
        self
    }

    /// A directory's chevron, pointing down while it's expanded.
    pub fn disclosure(mut self, expanded: bool) -> Self {
        self.disclosure = Some(expanded);
        self
    }

    /// What sits in the chevron's place instead: a spinner while a
    /// directory lists, say.
    pub fn lead(mut self, lead: impl IntoElement) -> Self {
        self.lead = Some(lead.into_any_element());
        self
    }

    /// The row's icon. A muted one brightens while the row is selected.
    pub fn icon(mut self, icon: IconName, color: Color) -> Self {
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

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
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

impl ParentElement for TreeItem {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TreeItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let hover = theme.element_hover;
        let selector = self.selector.unwrap_or_else(|| self.id.to_string());
        let muted = if self.selected {
            Color::Default
        } else {
            Color::Muted
        };
        let lead = self.lead.or_else(|| {
            let expanded = self.disclosure?;
            let chevron = if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            };
            Some(
                Icon::new(chevron)
                    .size(IconSize::XSmall)
                    .color(muted)
                    .into_any_element(),
            )
        });
        let icon = self.icon.map(|(icon, color)| {
            let color = match color {
                Color::Muted => muted,
                color => color,
            };
            Icon::new(icon).size(IconSize::Small).color(color)
        });
        div()
            .test_selector(|| selector)
            .id(self.id)
            .flex_none()
            .h(px(TREE_ROW_HEIGHT))
            .px_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .size_full()
                    .pl(px(4. + TREE_INDENT * self.depth as f32))
                    .pr_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(self.selected, |el| el.bg(theme.element_selected))
                    .when(!self.selected, |el| el.hover(move |s| s.bg(hover)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(12.))
                            .flex()
                            .items_center()
                            .children(lead),
                    )
                    .children(icon)
                    .child(div().flex_1().min_w_0().truncate().children(self.children))
                    .children(self.end_slot),
            )
            .when_some(self.tooltip, |el, tooltip| {
                el.tooltip(move |_, cx| Tooltip::view(tooltip.clone(), cx))
            })
            .when_some(self.on_click, |el, on_click| el.on_click(on_click))
    }
}
