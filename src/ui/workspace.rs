//! Root view: shows sign-in until authenticated, then the PR dashboard.

use gpui::{Context, Entity, Window, div, prelude::*};

use super::pr_detail::PrDetailView;
use super::pr_list::PrList;
use super::project_setup::ProjectSetupView;
use super::sidebar::Sidebar;
use super::sign_in::{SignIn, submit_token};
use super::text_input::{TextInput, TextInputEvent};
use super::theme::Theme;
use crate::state::{AppState, AuthStatus};

pub struct Workspace {
    state: Entity<AppState>,
    /// The sign-in screen's personal access token field. Views are rebuilt
    /// each frame, so the field's entity lives here.
    token_input: Entity<TextInput>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let token_input = cx.new(|cx| TextInput::new("ghp_… or github_pat_…", cx).masked());
        cx.subscribe(&token_input, |this, input, event, cx| match event {
            TextInputEvent::Submit => submit_token(&this.state, &input, cx),
        })
        .detach();
        Self { state, token_input }
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

        let state = self.state.read(cx);
        match &state.auth {
            AuthStatus::SignedIn { .. } => root
                .flex_row()
                .child(Sidebar::new(self.state.clone()))
                .map(|el| match (&state.setup, &state.detail) {
                    (Some(_), _) => el.child(ProjectSetupView::new(self.state.clone())),
                    (None, Some(_)) => el.child(PrDetailView::new(self.state.clone())),
                    (None, None) => el.child(PrList::new(self.state.clone())),
                }),
            _ => root.child(SignIn::new(self.state.clone(), self.token_input.clone())),
        }
    }
}
