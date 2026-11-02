//! Root view: shows sign-in until authenticated, then the PR dashboard.

use gpui::{Context, Entity, Window, div, prelude::*};

use super::diff_view::DiffView;
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
    /// The open PR's diff, keyed by PR id so opening another PR starts
    /// fresh (scroll position, collapsed files).
    diff_view: Option<(String, Entity<DiffView>)>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |this, state, cx| {
            this.sync_diff_view(&state, cx);
            cx.notify();
        })
        .detach();
        let token_input = cx.new(|cx| TextInput::new("ghp_… or github_pat_…", cx).masked());
        cx.subscribe(&token_input, |this, input, event, cx| match event {
            TextInputEvent::Submit => submit_token(&this.state, &input, cx),
        })
        .detach();
        Self {
            state,
            token_input,
            diff_view: None,
        }
    }

    /// Creates or drops the diff view to match the open PR.
    fn sync_diff_view(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        let open_id = state
            .read(cx)
            .open_pull_request
            .as_ref()
            .map(|open| open.pr.id.clone());
        match open_id {
            Some(id) if self.diff_view.as_ref().is_some_and(|(open, _)| *open == id) => {}
            Some(id) => {
                let view = cx.new(|cx| DiffView::new(state.clone(), cx));
                self.diff_view = Some((id, view));
            }
            None => self.diff_view = None,
        }
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
                .map(|el| match (&state.setup, &self.diff_view) {
                    (Some(_), _) => el.child(ProjectSetupView::new(self.state.clone())),
                    (None, Some((_, diff_view))) => el.child(diff_view.clone()),
                    (None, None) => el.child(PrList::new(self.state.clone())),
                }),
            _ => root.child(SignIn::new(self.state.clone(), self.token_input.clone())),
        }
    }
}
