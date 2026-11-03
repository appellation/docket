//! Root view: shows sign-in until authenticated, then the PR dashboard.

use gpui::{Context, Entity, Window, div, prelude::*};

use super::diff_view::DiffView;
use super::file_browser::FileBrowserView;
use super::pr_detail::PrDetailView;
use super::pr_list::PrList;
use super::project_setup::ProjectSetupView;
use super::sidebar::Sidebar;
use super::sign_in::{SignIn, submit_token};
use super::text_input::{TextInput, TextInputEvent};
use super::theme::Theme;
use crate::projects::ProjectId;
use crate::state::{AppState, AuthStatus};

pub struct Workspace {
    state: Entity<AppState>,
    /// The sign-in screen's personal access token field. Views are rebuilt
    /// each frame, so the field's entity lives here.
    token_input: Entity<TextInput>,
    /// The open PR's Files tab, keyed by PR id so opening another PR starts
    /// fresh (scroll position, collapsed files).
    diff_view: Option<(String, Entity<DiffView>)>,
    /// The open project's files, keyed by project so browsing another
    /// starts fresh (expanded directories, selection).
    file_browser: Option<(ProjectId, Entity<FileBrowserView>)>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |this, state, cx| {
            this.sync_diff_view(&state, cx);
            this.sync_file_browser(&state, cx);
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
            file_browser: None,
        }
    }

    /// Creates or drops the diff view to match the open PR.
    fn sync_diff_view(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        let open_id = state
            .read(cx)
            .detail
            .as_ref()
            .map(|detail| detail.summary.id.clone());
        match open_id {
            Some(id) if self.diff_view.as_ref().is_some_and(|(open, _)| *open == id) => {}
            Some(id) => {
                let view = cx.new(|cx| DiffView::new(state.clone(), cx));
                self.diff_view = Some((id, view));
            }
            None => self.diff_view = None,
        }
    }

    /// Creates or drops the file browser to match the browsed project.
    fn sync_file_browser(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        let open = state
            .read(cx)
            .browser
            .as_ref()
            .map(|browser| browser.project);
        match open {
            Some(id)
                if self
                    .file_browser
                    .as_ref()
                    .is_some_and(|(open, _)| *open == id) => {}
            Some(id) => {
                let view = cx.new(|cx| FileBrowserView::new(state.clone(), cx));
                self.file_browser = Some((id, view));
            }
            None => self.file_browser = None,
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        crate::test_support::start_frame();
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
                .map(
                    |el| match (&state.setup, &state.detail, &self.file_browser) {
                        (Some(_), ..) => el.child(ProjectSetupView::new(self.state.clone())),
                        (None, Some(_), _) => el.child(PrDetailView::new(
                            self.state.clone(),
                            self.diff_view.as_ref().map(|(_, view)| view.clone()),
                        )),
                        (None, None, Some((_, browser))) => el.child(browser.clone()),
                        (None, None, None) => el.child(PrList::new(self.state.clone())),
                    },
                ),
            _ => root.child(SignIn::new(self.state.clone(), self.token_input.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::TestAppContext;

    use crate::github::FakeGithub;
    use crate::state::SectionKind;
    use crate::test_support::{VisualTestContextExt as _, detail, open_docket, pull_request};

    #[gpui::test]
    fn switches_inboxes_from_the_sidebar(cx: &mut TestAppContext) {
        let github = Arc::new(FakeGithub::new());
        let review = pull_request("acme/widgets", 7, "Add sprockets");
        let mine = pull_request("acme/widgets", 8, "Fix the gears");
        github.set_search(
            SectionKind::ReviewRequested.search_query(),
            vec![review.clone()],
        );
        github.set_search(SectionKind::Authored.search_query(), vec![mine.clone()]);
        let (_, cx) = open_docket(cx, github, Vec::new());

        assert!(cx.is_drawn(&review.id));
        assert!(!cx.is_drawn(&mine.id));

        cx.click_on(SectionKind::Authored.label());
        assert!(cx.is_drawn(&mine.id));
        assert!(!cx.is_drawn(&review.id));
    }

    #[gpui::test]
    fn opens_a_pull_request_and_escape_closes_it(cx: &mut TestAppContext) {
        let github = Arc::new(FakeGithub::new());
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        github.set_search(
            SectionKind::ReviewRequested.search_query(),
            vec![pr.clone()],
        );
        github.set_detail(pr.repository.parse().unwrap(), detail(&pr));
        let (state, cx) = open_docket(cx, github, Vec::new());

        cx.click_on(&pr.id);
        state.read_with(cx, |state, _| {
            let detail = state.detail.as_ref().expect("the pull request is open");
            assert_eq!(detail.number, 7);
            assert!(detail.loaded.is_some(), "{:?}", detail.error);
        });
        assert!(!cx.is_drawn(&pr.id), "the detail replaces the list");

        cx.simulate_keystrokes("escape");
        assert!(state.read_with(cx, |state, _| state.detail.is_none()));
        assert!(cx.is_drawn(&pr.id));
    }

    #[gpui::test]
    fn refresh_shows_new_pull_requests(cx: &mut TestAppContext) {
        let github = Arc::new(FakeGithub::new());
        let (_, cx) = open_docket(cx, github.clone(), Vec::new());

        let pr = pull_request("acme/widgets", 9, "Oil the hinges");
        github.set_search(
            SectionKind::ReviewRequested.search_query(),
            vec![pr.clone()],
        );
        assert!(!cx.is_drawn(&pr.id));

        cx.simulate_keystrokes("cmd-r");
        assert!(cx.is_drawn(&pr.id));
    }
}
