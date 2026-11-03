//! The sidebars down the left of the window, one per level of what's open:
//! navigation, then the docket once a pull request or branch is open from
//! it, then the pull request's changed files on the Files tab, or the
//! branch's tree.
//!
//! Only one sidebar is expanded at a time: the deepest, unless the mouse
//! rests on another's rail, which expands that one in the deepest's place.
//! The rest collapse to rails of icons. Every sidebar is the same width
//! expanded, and the same width collapsed, so changing which one is
//! expanded never moves what's beside them.
//!
//! Where the mouse rests is all that changes it. Moving from one sidebar
//! to another leaves the expanded one open until the mouse settles on the
//! other, so reaching a rail past the edge of an expanded sidebar doesn't
//! collapse the stack out from under it. The expanded one closes only when
//! the mouse leaves the stack. The lines between sidebars belong to
//! neither, so resting on one changes nothing.
//!
//! A sidebar that expands always covers where its rail was, so the mouse
//! that rested there is still on it.

use std::time::Duration;

use gpui::{AnyElement, App, Context, Entity, Pixels, Stateful, Task, div, prelude::*, px};

use super::components::TestSelector as _;
use super::theme::Theme;

pub const SIDEBAR_WIDTH: Pixels = px(260.);
pub const RAIL_WIDTH: Pixels = px(44.);
/// The height of every sidebar's header, and of the views' beside them, so
/// their bottom borders line up.
pub const HEADER_HEIGHT: Pixels = px(48.);
/// How long the mouse rests on a rail before it expands, so crossing one on
/// the way somewhere else doesn't.
pub const PEEK_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarLevel {
    /// The signed-in user and projects.
    Navigation,
    /// The docket, beside the open pull request or branch.
    PullRequests,
    /// The open pull request's changed files, beside its diff.
    Files,
    /// The browsed branch's (or checkout's) tree, beside the open file.
    ProjectFiles,
}

impl SidebarLevel {
    fn id(self) -> &'static str {
        match self {
            Self::Navigation => "sidebar-navigation",
            Self::PullRequests => "sidebar-pull-requests",
            Self::Files => "sidebar-files",
            Self::ProjectFiles => "sidebar-project-files",
        }
    }
}

/// Which sidebar is expanded. The workspace owns it and re-renders when it
/// changes.
#[derive(Default)]
pub struct SidebarStack {
    levels: Vec<SidebarLevel>,
    /// A collapsed sidebar expanded while the mouse is on it.
    peeked: Option<SidebarLevel>,
    /// A sidebar the mouse is resting on, to peek at after `PEEK_DELAY`.
    pending: Option<(SidebarLevel, Task<()>)>,
}

impl SidebarStack {
    /// Takes this frame's levels, shallowest first. Navigating ends a peek:
    /// what was peeked at has either been left or is now the deepest.
    pub fn set_levels(&mut self, levels: &[SidebarLevel]) {
        if self.levels != levels {
            self.levels = levels.to_vec();
            self.peeked = None;
            self.pending = None;
        }
    }

    pub fn expanded(&self) -> Option<SidebarLevel> {
        self.peeked.or(self.levels.last().copied())
    }

    /// The mouse moved onto or off `level`'s sidebar.
    fn hover(&mut self, level: SidebarLevel, hovered: bool, cx: &mut Context<Self>) {
        if !hovered {
            if self.pending.as_ref().is_some_and(|(p, _)| *p == level) {
                self.pending = None;
            }
            return;
        }
        if self.expanded() == Some(level) {
            self.pending = None;
            return;
        }
        let deepest = self.levels.last().copied();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PEEK_DELAY).await;
            this.update(cx, |this, cx| {
                this.pending = None;
                this.peeked = (Some(level) != deepest).then_some(level);
                cx.notify();
            })
            .ok();
        });
        self.pending = Some((level, task));
    }

    /// The mouse left the stack: back to the deepest.
    fn leave(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        if self.peeked.take().is_some() {
            cx.notify();
        }
    }
}

/// The sidebars, side by side.
pub fn sidebar_stack(
    stack: &Entity<SidebarStack>,
    sidebars: impl IntoIterator<Item = AnyElement>,
) -> impl IntoElement {
    let stack = stack.clone();
    div()
        .id("sidebars")
        .flex()
        .flex_none()
        .h_full()
        .on_hover(move |hovered, _, cx| {
            if !hovered {
                stack.update(cx, |stack, cx| stack.leave(cx));
            }
        })
        .children(sidebars)
}

/// `level`'s sidebar, holding `content` drawn expanded or as a rail to
/// match `stack`.
pub fn sidebar(
    stack: &Entity<SidebarStack>,
    level: SidebarLevel,
    content: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    let theme = Theme::get(cx);
    let expanded = stack.read(cx).expanded() == Some(level);
    let stack = stack.clone();
    div()
        .test_selector(|| level.id().to_owned())
        .id(level.id())
        .flex_none()
        .w(if expanded { SIDEBAR_WIDTH } else { RAIL_WIDTH })
        .h_full()
        .overflow_hidden()
        .bg(theme.surface)
        .border_r_1()
        .border_color(theme.border)
        .child(
            // Inside the border, which belongs to neither sidebar. Expanded
            // or not, it's the same element, so GPUI keeps track of the
            // mouse being on it as it changes width under it.
            div()
                .id("hover")
                .size_full()
                .flex()
                .flex_col()
                .on_hover(move |hovered, _, cx| {
                    stack.update(cx, |stack, cx| stack.hover(level, *hovered, cx));
                })
                .child(content),
        )
}

/// A sidebar's header, level with the headers beside it.
pub fn sidebar_header(cx: &App) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(HEADER_HEIGHT)
        .border_b_1()
        .border_color(Theme::get(cx).border_subtle)
}

/// A rail's header: one icon or avatar standing for the sidebar.
pub fn rail_header(child: impl IntoElement, cx: &App) -> impl IntoElement {
    sidebar_header(cx).justify_center().child(child)
}

/// The column of icons in a rail, scrolling if there are more than fit.
pub fn rail_items(id: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .py_2()
}
