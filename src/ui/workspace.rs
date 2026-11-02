//! Root view: shows sign-in until authenticated, then the PR dashboard.

use gpui::{Context, Entity, Window, div, prelude::*};

use super::pr_list::PrList;
use super::sidebar::Sidebar;
use super::sign_in::SignIn;
use super::theme::Theme;
use crate::state::{AppState, AuthStatus};

pub struct Workspace {
    state: Entity<AppState>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx);
        let root = div()
            .size_full()
            .flex()
            .bg(theme.background)
            .text_color(theme.text)
            .text_sm();

        match &self.state.read(cx).auth {
            AuthStatus::SignedIn { .. } => root
                .flex_row()
                .child(Sidebar::new(self.state.clone()))
                .child(PrList::new(self.state.clone())),
            _ => root.child(SignIn::new(self.state.clone())),
        }
    }
}
