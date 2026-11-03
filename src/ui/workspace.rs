//! Root view: shows sign-in until authenticated, then the PR dashboard: the
//! sidebars for what's open (`sidebar_stack`), and the view beside them.

use gpui::{AnyElement, App, Context, Entity, Window, div, prelude::*};

use super::diff_view::{DiffView, files_sidebar};
use super::file_browser::FileBrowserView;
use super::pr_detail::PrDetailView;
use super::pr_list::{PrList, PrSidebar};
use super::project_setup::ProjectSetupView;
use super::sidebar::Sidebar;
use super::sidebar_stack::{SidebarLevel, SidebarStack, sidebar, sidebar_stack};
use super::sign_in::{SignIn, submit_token};
use super::text_input::{TextInput, TextInputEvent};
use super::theme::Theme;
use crate::state::{AppState, AuthStatus, BrowserKey, PrTab};

pub struct Workspace {
    state: Entity<AppState>,
    /// The sign-in screen's personal access token field. Views are rebuilt
    /// each frame, so the field's entity lives here.
    token_input: Entity<TextInput>,
    /// The open PR's Files tab, keyed by PR id so opening another PR starts
    /// fresh (scroll position, collapsed files).
    diff_view: Option<(String, Entity<DiffView>)>,
    /// The open project's files, keyed by project and branch so browsing
    /// another starts fresh (expanded directories, selection).
    file_browser: Option<(BrowserKey, Entity<FileBrowserView>)>,
    /// Which of the sidebars is expanded.
    sidebars: Entity<SidebarStack>,
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
        let sidebars = cx.new(|_| SidebarStack::default());
        cx.observe(&sidebars, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            token_input,
            diff_view: None,
            file_browser: None,
            sidebars,
        }
    }

    /// The sidebars for what's open, shallowest first.
    fn sidebar_levels(&self, cx: &App) -> Vec<SidebarLevel> {
        let state = self.state.read(cx);
        let mut levels = vec![SidebarLevel::Navigation];
        if state.setup.is_some() {
            return levels;
        }
        if let Some(detail) = &state.detail {
            levels.push(SidebarLevel::PullRequests);
            if detail.tab == PrTab::Files && self.diff_view.is_some() {
                levels.push(SidebarLevel::Files);
            }
        } else if self.file_browser.is_some() {
            levels.push(SidebarLevel::PullRequests);
            levels.push(SidebarLevel::ProjectFiles);
        }
        levels
    }

    fn sidebar_content(&self, level: SidebarLevel, collapsed: bool, cx: &mut App) -> AnyElement {
        match level {
            SidebarLevel::Navigation => {
                Sidebar::new(self.state.clone(), collapsed).into_any_element()
            }
            SidebarLevel::PullRequests => {
                PrSidebar::new(self.state.clone(), collapsed).into_any_element()
            }
            SidebarLevel::Files => match &self.diff_view {
                Some((_, view)) => files_sidebar(view, collapsed, cx),
                None => div().into_any_element(),
            },
            SidebarLevel::ProjectFiles => match &self.file_browser {
                Some((_, view)) => view.update(cx, |view, cx| view.tree_sidebar(collapsed, cx)),
                None => div().into_any_element(),
            },
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

    /// Creates or drops the file browser to match the browsed branch.
    fn sync_file_browser(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        let open = state.read(cx).browser.as_ref().map(|browser| browser.key());
        match open {
            Some(key)
                if self
                    .file_browser
                    .as_ref()
                    .is_some_and(|(open, _)| *open == key) => {}
            Some(key) => {
                let view = cx.new(|cx| FileBrowserView::new(state.clone(), cx));
                self.file_browser = Some((key, view));
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

        let levels = self.sidebar_levels(cx);
        self.sidebars
            .update(cx, |sidebars, _| sidebars.set_levels(&levels));
        let expanded = self.sidebars.read(cx).expanded();
        let mut sidebars = Vec::new();
        for level in levels {
            let content = self.sidebar_content(level, expanded != Some(level), cx);
            sidebars.push(sidebar(&self.sidebars, level, content, cx).into_any_element());
        }

        let state = self.state.read(cx);
        match &state.auth {
            AuthStatus::SignedIn { .. } => root
                .flex_row()
                .child(sidebar_stack(&self.sidebars, sidebars))
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

    use chrono::{TimeZone as _, Utc};
    use gpui::{Entity, Modifiers, Pixels, Point, TestAppContext, VisualTestContext, point, px};

    use crate::github::{FakeGithub, PullRequest};
    use crate::state::{AppState, PrTab, SectionKind};
    use crate::test_support::{
        VisualTestContextExt as _, detail, git, git_checkout, git_commit, open_docket, project,
        pull_request,
    };
    use crate::ui::sidebar_stack::{PEEK_DELAY, RAIL_WIDTH, SIDEBAR_WIDTH};

    fn top_of(cx: &mut VisualTestContext, selector: &str) -> Pixels {
        cx.bounds_of(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .top()
    }

    #[gpui::test]
    fn review_requests_come_first_then_the_rest_by_update(cx: &mut TestAppContext) {
        let github = Arc::new(FakeGithub::new());
        let updated = |mut pr: PullRequest, day| {
            pr.updated_at = Utc.with_ymd_and_hms(2026, 1, day, 0, 0, 0).unwrap();
            pr
        };
        let old_review = updated(pull_request("acme/widgets", 1, "Add sprockets"), 1);
        let new_review = updated(pull_request("acme/widgets", 2, "Oil the hinges"), 3);
        let mine = updated(pull_request("acme/widgets", 3, "Fix the gears"), 2);
        let involved = updated(pull_request("acme/widgets", 4, "Paint it"), 5);
        github.set_search(
            SectionKind::ReviewRequested.search_query(),
            vec![old_review.clone(), new_review.clone()],
        );
        github.set_search(SectionKind::Authored.search_query(), vec![mine.clone()]);
        // A review request you're also involved in shows once, as a review.
        github.set_search(
            SectionKind::Involved.search_query(),
            vec![involved.clone(), old_review.clone()],
        );
        let (state, cx) = open_docket(cx, github, Vec::new());

        assert!(!cx.is_drawn("Inbox"), "the sidebar doesn't filter");
        let order = [
            "Review requested",
            &new_review.id,
            &old_review.id,
            "Recently updated",
            &involved.id,
            &mine.id,
        ]
        .map(|selector| top_of(cx, selector));
        assert!(order.is_sorted(), "{order:?}");
        assert_eq!(
            state.read_with(cx, |state, _| state.docket().items().count()),
            4
        );
    }

    #[gpui::test]
    fn a_project_is_removed_from_its_connection_settings(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        git_checkout(checkout.path(), &[("README.md", "# Widgets\n")]);
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);

        cx.click_on("project-1");
        assert!(cx.is_drawn("reveal-project"), "the path shows it in Finder");
        assert!(!cx.is_drawn("remove-project"));

        cx.click_on("edit-project");
        assert!(state.read_with(cx, |state, _| state.setup.is_some()));
        cx.click_on("remove-project");
        cx.simulate_prompt_answer("Remove");
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert!(state.projects.is_empty());
            assert!(state.setup.is_none());
        });
    }

    #[gpui::test]
    fn a_missing_project_can_still_be_removed(cx: &mut TestAppContext) {
        let gone = tempfile::tempdir().unwrap().path().to_owned();
        let projects = vec![project(1, &gone, "acme/widgets")];
        let (state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);

        cx.click_on("project-1");
        assert!(!cx.is_drawn("reveal-project"), "there's nothing to show");
        cx.click_on("edit-project");
        assert!(state.read_with(cx, |state, _| state.setup.is_some()));
        assert!(cx.is_drawn("change-folder"));
        assert!(cx.is_drawn("remove-project"));
    }

    #[gpui::test]
    fn one_group_has_no_heading(cx: &mut TestAppContext) {
        let github = Arc::new(FakeGithub::new());
        let mine = pull_request("acme/widgets", 3, "Fix the gears");
        github.set_search(SectionKind::Authored.search_query(), vec![mine.clone()]);
        github.set_detail(mine.repository.parse().unwrap(), detail(&mine));
        let (_, cx) = open_docket(cx, github, Vec::new());

        assert!(cx.is_drawn(&mine.id));
        assert!(!cx.is_drawn("Recently updated"));
        assert!(!cx.is_drawn("Review requested"));

        // Nor beside an open pull request.
        cx.click_on(&mine.id);
        assert!(cx.is_drawn(&mine.id));
        assert!(!cx.is_drawn("Recently updated"));
    }

    #[gpui::test]
    fn branches_without_pull_requests_open_their_files(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        let dir = checkout.path();
        let commit = |path: &str, message: &str, date: &str| {
            std::fs::write(dir.join(path), message).unwrap();
            git_commit(dir, message, date);
        };
        git(dir, &["init", "--quiet", "-b", "main"]);
        commit("README.md", "# Widgets", "2025-12-01T00:00:00Z");
        git(dir, &["checkout", "--quiet", "-b", "sprockets"]);
        commit("sprockets.md", "Sprockets", "2026-01-02T00:00:00Z");
        git(dir, &["checkout", "--quiet", "-b", "gears", "main"]);
        commit("gears.md", "Gears", "2026-01-03T00:00:00Z");
        git(dir, &["checkout", "--quiet", "main"]);

        let github = Arc::new(FakeGithub::new());
        let mut with_branch = pull_request("acme/widgets", 7, "Fix the gears");
        with_branch.head_ref_name = "gears".into();
        let older = pull_request("acme/widgets", 8, "Paint it");
        let mut newer = pull_request("acme/widgets", 9, "Oil the hinges");
        newer.updated_at = Utc.with_ymd_and_hms(2026, 1, 5, 0, 0, 0).unwrap();
        github.set_search(
            SectionKind::Authored.search_query(),
            vec![with_branch.clone(), older.clone(), newer.clone()],
        );
        let projects = vec![project(1, dir, "acme/widgets")];
        let (state, cx) = open_docket(cx, github, projects);

        assert!(
            !cx.is_drawn("branch-1-gears"),
            "its pull request stands for it"
        );
        let order = [&newer.id, "branch-1-sprockets", &older.id].map(|id| top_of(cx, id));
        assert!(order.is_sorted(), "interspersed by when they were updated");
        assert!(
            top_of(cx, "branch-1-main") < top_of(cx, &newer.id),
            "the default branch is pinned above them"
        );

        cx.click_on("branch-1-sprockets");
        state.read_with(cx, |state, _| {
            let browser = state.browser.as_ref().expect("the browser is open");
            assert_eq!(browser.branch, "sprockets");
            assert_eq!(browser.working_copy(), None, "it isn't checked out");
        });
        assert!(
            cx.is_drawn("browser-tree-sprockets.md"),
            "the branch's files"
        );
        assert_eq!(width_of(cx, "sidebar-navigation"), RAIL_WIDTH);
        assert_eq!(width_of(cx, "sidebar-pull-requests"), RAIL_WIDTH);
        assert_eq!(width_of(cx, "sidebar-project-files"), SIDEBAR_WIDTH);
        assert!(cx.is_drawn("rail-branch-1-sprockets"));

        cx.click_on("browser-tree-sprockets.md");
        assert!(state.read_with(cx, |state, _| {
            let file = state.browser.as_ref().unwrap().file.as_ref().unwrap();
            matches!(&file.content, crate::state::FileLoad::Ready(_))
        }));

        // The default branch is checked out, so it's browsed on disk.
        rest_mouse_on(cx, "sidebar-pull-requests");
        cx.click_on("branch-1-main");
        state.read_with(cx, |state, _| {
            let browser = state.browser.as_ref().expect("the browser is open");
            assert_eq!(browser.branch, "main");
            assert!(browser.working_copy().is_some());
        });

        // The docket beside it opens a pull request in its place.
        rest_mouse_on(cx, "sidebar-pull-requests");
        cx.click_on(&older.id);
        state.read_with(cx, |state, _| {
            assert!(state.browser.is_none());
            assert_eq!(state.detail.as_ref().map(|d| d.number), Some(8));
        });
    }

    #[gpui::test]
    fn the_default_branch_is_pinned_below_review_requests(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        git_checkout(checkout.path(), &[("README.md", "# Widgets\n")]);
        let github = Arc::new(FakeGithub::new());
        let review = pull_request("acme/widgets", 1, "Add sprockets");
        let mut mine = pull_request("acme/widgets", 2, "Fix the gears");
        mine.updated_at = Utc.with_ymd_and_hms(2025, 12, 1, 0, 0, 0).unwrap();
        github.set_search(
            SectionKind::ReviewRequested.search_query(),
            vec![review.clone()],
        );
        github.set_search(SectionKind::Authored.search_query(), vec![mine.clone()]);
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (_, cx) = open_docket(cx, github, projects);

        let order = [
            &review.id,
            "branch-1-main",
            "Recently updated",
            "branch-1-feature",
            &mine.id,
        ]
        .map(|selector| top_of(cx, selector));
        assert!(order.is_sorted(), "{order:?}");
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
        assert!(cx.is_drawn(&pr.id), "the list moves to a sidebar");
        assert!(cx.is_drawn("rail-project-all"), "navigation collapses");
        assert!(!cx.is_drawn("project-all"));

        cx.simulate_keystrokes("escape");
        assert!(state.read_with(cx, |state, _| state.detail.is_none()));
        assert!(cx.is_drawn(&pr.id));
        assert!(cx.is_drawn("project-all"));
        assert!(!cx.is_drawn("rail-project-all"));
    }

    /// Opens `pr` from the review requests, with its detail to load.
    fn open_pull_request<'a>(
        cx: &'a mut TestAppContext,
        pr: &PullRequest,
    ) -> (Entity<AppState>, &'a mut VisualTestContext) {
        let github = Arc::new(FakeGithub::new());
        github.set_search(
            SectionKind::ReviewRequested.search_query(),
            vec![pr.clone()],
        );
        github.set_detail(pr.repository.parse().unwrap(), detail(pr));
        let (state, cx) = open_docket(cx, github, Vec::new());
        cx.click_on(&pr.id);
        (state, cx)
    }

    fn rest_mouse_on(cx: &mut VisualTestContext, selector: &str) {
        let bounds = cx.bounds_of(selector).expect("it's drawn");
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
        cx.executor().advance_clock(PEEK_DELAY);
        cx.run_until_parked();
    }

    fn width_of(cx: &mut VisualTestContext, selector: &str) -> Pixels {
        cx.bounds_of(selector).expect("it's drawn").size.width
    }

    /// The height of the first PR's row in the sidebar, with `count` PRs
    /// in the docket.
    fn sidebar_row_height(cx: &mut TestAppContext, count: u64) -> Pixels {
        let prs: Vec<_> = (1..=count)
            .map(|n| pull_request("acme/widgets", n, "Add sprockets"))
            .collect();
        let github = Arc::new(FakeGithub::new());
        github.set_search(SectionKind::Authored.search_query(), prs.clone());
        github.set_detail("acme/widgets".parse().unwrap(), detail(&prs[0]));
        let (_, cx) = open_docket(cx, github, Vec::new());
        cx.click_on(&prs[0].id);
        cx.bounds_of(&prs[0].id).unwrap().size.height
    }

    #[gpui::test]
    fn sidebar_rows_keep_their_height_when_they_overflow(cx: &mut TestAppContext) {
        let fits = sidebar_row_height(cx, 2);
        let overflows = sidebar_row_height(cx, 60);
        assert_eq!(overflows, fits, "the rows aren't squeezed to fit");
    }

    #[gpui::test]
    fn hovering_a_rail_expands_it_in_place_of_the_deepest(cx: &mut TestAppContext) {
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        let (_, cx) = open_pull_request(cx, &pr);
        let rail = format!("rail-{}", pr.id);
        let content = cx.bounds_of("pr-back").unwrap().origin;
        assert_eq!(width_of(cx, "sidebar-navigation"), RAIL_WIDTH);
        assert_eq!(width_of(cx, "sidebar-pull-requests"), SIDEBAR_WIDTH);

        rest_mouse_on(cx, "sidebar-navigation");
        assert!(cx.is_drawn("project-all"), "navigation expands");
        assert!(cx.is_drawn(&rail), "the docket collapses");
        assert!(!cx.is_drawn(&pr.id));
        assert_eq!(width_of(cx, "sidebar-navigation"), SIDEBAR_WIDTH);
        assert_eq!(width_of(cx, "sidebar-pull-requests"), RAIL_WIDTH);
        assert_eq!(
            cx.bounds_of("pr-back").unwrap().origin,
            content,
            "the pull request doesn't move"
        );

        rest_mouse_on(cx, "pr-back");
        assert!(cx.is_drawn(&pr.id), "leaving restores the deepest");
        assert!(cx.is_drawn("rail-project-all"));
    }

    #[gpui::test]
    fn crossing_a_rail_leaves_it_collapsed(cx: &mut TestAppContext) {
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        let (_, cx) = open_pull_request(cx, &pr);

        let rail = cx.bounds_of("sidebar-navigation").unwrap().center();
        cx.simulate_mouse_move(rail, None, Modifiers::none());
        rest_mouse_on(cx, "pr-back");
        assert!(cx.is_drawn("rail-project-all"));
        assert!(cx.is_drawn(&pr.id));
    }

    #[gpui::test]
    fn rail_icons_act_without_expanding(cx: &mut TestAppContext) {
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        let (state, cx) = open_pull_request(cx, &pr);

        cx.click_on("rail-project-all");
        state.read_with(cx, |state, _| {
            assert!(state.detail.is_none());
            assert_eq!(state.selected_project, None);
        });
    }

    #[gpui::test]
    fn files_tab_adds_a_sidebar_and_collapses_the_docket(cx: &mut TestAppContext) {
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        let (state, cx) = open_pull_request(cx, &pr);
        let content = cx.bounds_of("pr-back").unwrap().origin;

        state.update(cx, |state, cx| state.select_pr_tab(PrTab::Files, cx));
        cx.run_until_parked();
        assert_eq!(width_of(cx, "sidebar-navigation"), RAIL_WIDTH);
        assert_eq!(width_of(cx, "sidebar-pull-requests"), RAIL_WIDTH);
        assert_eq!(width_of(cx, "sidebar-files"), SIDEBAR_WIDTH);
        assert_eq!(
            cx.bounds_of("pr-back").unwrap().origin.x,
            content.x + RAIL_WIDTH,
            "one more rail"
        );

        // Peeking at the docket swaps it with the files.
        rest_mouse_on(cx, "sidebar-pull-requests");
        assert!(cx.is_drawn(&pr.id));
        assert_eq!(width_of(cx, "sidebar-files"), RAIL_WIDTH);
        assert_eq!(
            cx.bounds_of("pr-back").unwrap().origin.x,
            content.x + RAIL_WIDTH
        );
    }

    /// Moves the mouse to `at` without resting there.
    fn move_mouse(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_mouse_move(at, None, Modifiers::none());
        cx.run_until_parked();
    }

    fn rest(cx: &mut VisualTestContext) {
        cx.executor().advance_clock(PEEK_DELAY);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn moves_left_to_right_through_three_sidebars(cx: &mut TestAppContext) {
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        let (state, cx) = open_pull_request(cx, &pr);
        state.update(cx, |state, cx| state.select_pr_tab(PrTab::Files, cx));
        cx.run_until_parked();
        let content = cx.bounds_of("pr-back").unwrap().origin;

        rest_mouse_on(cx, "sidebar-navigation");
        assert_eq!(width_of(cx, "sidebar-navigation"), SIDEBAR_WIDTH);

        // Heading right, past the edge of navigation and onto the docket's
        // rail, navigation stays open until the mouse rests there.
        let docket = cx.bounds_of("sidebar-pull-requests").unwrap().center();
        move_mouse(cx, docket);
        assert_eq!(width_of(cx, "sidebar-navigation"), SIDEBAR_WIDTH);
        rest(cx);
        assert_eq!(width_of(cx, "sidebar-navigation"), RAIL_WIDTH);
        assert_eq!(width_of(cx, "sidebar-pull-requests"), SIDEBAR_WIDTH);
        assert!(cx.is_drawn(&pr.id));

        rest_mouse_on(cx, "sidebar-files");
        assert_eq!(width_of(cx, "sidebar-pull-requests"), RAIL_WIDTH);
        assert_eq!(width_of(cx, "sidebar-files"), SIDEBAR_WIDTH);
        assert_eq!(cx.bounds_of("pr-back").unwrap().origin, content);
    }

    #[gpui::test]
    fn dividers_between_sidebars_are_neutral(cx: &mut TestAppContext) {
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        let (_, cx) = open_pull_request(cx, &pr);
        let divider = |cx: &mut VisualTestContext, selector: &str| {
            let bounds = cx.bounds_of(selector).unwrap();
            point(bounds.right() - px(0.5), bounds.center().y)
        };

        // Resting on the line between navigation's rail and the docket
        // doesn't open navigation.
        let at = divider(cx, "sidebar-navigation");
        move_mouse(cx, at);
        rest(cx);
        assert_eq!(width_of(cx, "sidebar-navigation"), RAIL_WIDTH);
        assert!(cx.is_drawn(&pr.id));

        // Nor does it close navigation once it's open.
        rest_mouse_on(cx, "sidebar-navigation");
        let at = divider(cx, "sidebar-navigation");
        move_mouse(cx, at);
        rest(cx);
        assert_eq!(width_of(cx, "sidebar-navigation"), SIDEBAR_WIDTH);
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
