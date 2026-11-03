//! Root view of a window (a native tab, on macOS): shows sign-in until
//! authenticated, then what the window's [`Tab`] shows: the sidebars for
//! what's open (`sidebar_stack`), and the view beside them.
//!
//! Actions that act on what a window shows (Refresh, Escape, the project
//! shortcuts, Close Tab, following a link) are handled here, so they reach
//! the window they came from. The workspace takes focus whenever nothing in
//! it has it, so they always have somewhere to dispatch from. The window's title is the
//! tab's, which names it in the tab bar and the Window menu.

use gpui::{Action, AnyElement, App, Context, Entity, FocusHandle, Window, div, prelude::*};

use super::branch_view::BranchView;
use super::code::Definition;
use super::diff_view::{DiffView, files_sidebar};
use super::file_browser::FileBrowserView;
use super::pr_detail::PrDetailView;
use super::pr_list::{PrList, PrSidebar, branch_id};
use super::project_setup::ProjectSetupView;
use super::sidebar::Sidebar;
use super::sidebar_stack::{SidebarLevel, SidebarStack, sidebar, sidebar_stack};
use super::sign_in::{SignIn, submit_token};
use super::text_input::{TextInput, TextInputEvent};
use super::theme::Theme;
use crate::state::{AuthStatus, BranchTab, BrowserKey, PrTab, Tab};
use crate::{
    AddProject, Cancel, CloseTab, Minimize, NextProject, OpenLink, PreviousProject, Refresh, Zoom,
};

pub struct Workspace {
    tab: Entity<Tab>,
    /// The sign-in screen's personal access token field. Views are rebuilt
    /// each frame, so the field's entity lives here.
    token_input: Entity<TextInput>,
    /// The open PR's Files tab or branch's changes, keyed by PR id or
    /// branch so opening another starts fresh (scroll position, collapsed
    /// files).
    diff_view: Option<(String, Entity<DiffView>)>,
    /// The default branch's files, keyed by project and branch so browsing
    /// another starts fresh (expanded directories, selection).
    file_browser: Option<(BrowserKey, Entity<FileBrowserView>)>,
    /// Which of the sidebars is expanded.
    sidebars: Entity<SidebarStack>,
    /// The window title last set, so it's only set when it changes.
    title: Option<String>,
    focus_handle: FocusHandle,
}

impl Workspace {
    pub fn new(tab: Entity<Tab>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        cx.on_focus_lost(window, |this, window, _| window.focus(&this.focus_handle))
            .detach();
        cx.observe(&tab, |this, _, cx| {
            this.sync_diff_view(cx);
            this.sync_file_browser(cx);
            cx.notify();
        })
        .detach();
        let token_input = cx.new(|cx| TextInput::new("ghp_… or github_pat_…", cx).masked());
        cx.subscribe(&token_input, |this, input, event, cx| match event {
            TextInputEvent::Submit => {
                let app = this.tab.read(cx).app.clone();
                submit_token(&app, &input, cx);
            }
        })
        .detach();
        let sidebars = cx.new(|_| SidebarStack::default());
        cx.observe(&sidebars, |_, _, cx| cx.notify()).detach();
        let mut this = Self {
            tab,
            token_input,
            diff_view: None,
            file_browser: None,
            sidebars,
            title: None,
            focus_handle,
        };
        this.sync_diff_view(cx);
        this.sync_file_browser(cx);
        this
    }

    pub fn tab(&self) -> &Entity<Tab> {
        &self.tab
    }

    /// Shows a definition in the browsed project's files, which the tab
    /// must already be browsing.
    pub fn show_definition(&mut self, definition: Definition, cx: &mut Context<Self>) {
        self.sync_file_browser(cx);
        if let Some((_, browser)) = &self.file_browser {
            browser.update(cx, |browser, cx| browser.show_definition(definition, cx));
        }
    }

    /// The sidebars for what's open, shallowest first.
    fn sidebar_levels(&self, cx: &App) -> Vec<SidebarLevel> {
        let tab = self.tab.read(cx);
        let mut levels = vec![SidebarLevel::Navigation];
        if tab.setup.is_some() {
            return levels;
        }
        if let Some(detail) = &tab.detail {
            levels.push(SidebarLevel::PullRequests);
            if detail.tab == PrTab::Files && self.diff_view.is_some() {
                levels.push(SidebarLevel::Files);
            }
        } else if let Some(open) = &tab.branch {
            levels.push(SidebarLevel::PullRequests);
            if open.tab == BranchTab::Files && self.diff_view.is_some() {
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
                Sidebar::new(self.tab.clone(), collapsed).into_any_element()
            }
            SidebarLevel::PullRequests => {
                PrSidebar::new(self.tab.clone(), collapsed).into_any_element()
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

    /// Creates or drops the diff view to match the open PR or branch.
    fn sync_diff_view(&mut self, cx: &mut Context<Self>) {
        let tab = self.tab.read(cx);
        let open_id = match (&tab.detail, &tab.branch) {
            (Some(detail), _) => Some(format!("{}#{}", detail.repo, detail.number)),
            (None, Some(open)) => Some(branch_id(open.project, &open.branch.name)),
            (None, None) => None,
        };
        match open_id {
            Some(id) if self.diff_view.as_ref().is_some_and(|(open, _)| *open == id) => {}
            Some(id) => {
                let tab = self.tab.clone();
                let view = cx.new(|cx| DiffView::new(tab, cx));
                self.diff_view = Some((id, view));
            }
            None => self.diff_view = None,
        }
    }

    /// Creates or drops the file browser to match the browsed default
    /// branch.
    fn sync_file_browser(&mut self, cx: &mut Context<Self>) {
        let open = self
            .tab
            .read(cx)
            .browser
            .as_ref()
            .map(|browser| browser.key());
        match open {
            Some(key)
                if self
                    .file_browser
                    .as_ref()
                    .is_some_and(|(open, _)| *open == key) => {}
            Some(key) => {
                let tab = self.tab.clone();
                let view = cx.new(|cx| FileBrowserView::new(tab, cx));
                self.file_browser = Some((key, view));
            }
            None => self.file_browser = None,
        }
    }

    fn update_title(&mut self, window: &mut Window, cx: &Context<Self>) {
        let title = self.tab.read(cx).title(cx);
        if self.title.as_ref() != Some(&title) {
            window.set_window_title(&title);
            self.title = Some(title);
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        crate::test_support::start_frame(window.window_handle().window_id());
        self.update_title(window, cx);
        let theme = Theme::get(cx);
        let tab = &self.tab;
        let root = div()
            .size_full()
            .flex()
            .bg(theme.background)
            .text_color(theme.text)
            .text_sm()
            .on_action(on_tab::<Refresh>(tab, |tab, cx| tab.refresh(cx)))
            .on_action(on_tab::<AddProject>(tab, |tab, cx| tab.add_project(cx)))
            .on_action(on_tab::<NextProject>(tab, |tab, cx| {
                tab.select_adjacent_project(1, cx)
            }))
            .on_action(on_tab::<PreviousProject>(tab, |tab, cx| {
                tab.select_adjacent_project(-1, cx)
            }))
            .on_action(on_tab::<Cancel>(tab, |tab, cx| tab.cancel(cx)))
            .on_action({
                let tab = tab.clone();
                move |link: &OpenLink, _, cx| tab.update(cx, |tab, cx| tab.open_link(&link.url, cx))
            })
            .on_action(|_: &CloseTab, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            // Where focus falls back to, inside the root so its actions are
            // reachable. Not the root itself: GPUI focuses a tracked element
            // on any click in it, after the views inside have taken focus.
            .child(div().absolute().size_0().track_focus(&self.focus_handle));

        let levels = self.sidebar_levels(cx);
        self.sidebars
            .update(cx, |sidebars, _| sidebars.set_levels(&levels));
        let expanded = self.sidebars.read(cx).expanded();
        let mut sidebars = Vec::new();
        for level in levels {
            let content = self.sidebar_content(level, expanded != Some(level), cx);
            sidebars.push(sidebar(&self.sidebars, level, content, cx).into_any_element());
        }

        let tab = self.tab.read(cx);
        let app = tab.app.read(cx);
        match &app.auth {
            AuthStatus::SignedIn { .. } => root
                .flex_row()
                .child(sidebar_stack(&self.sidebars, sidebars))
                .map(|el| {
                    let diff_view = self.diff_view.as_ref().map(|(_, view)| view.clone());
                    match (&tab.setup, &tab.detail, &tab.branch, &self.file_browser) {
                        (Some(_), ..) => el.child(ProjectSetupView::new(self.tab.clone())),
                        (None, Some(_), ..) => {
                            el.child(PrDetailView::new(self.tab.clone(), diff_view))
                        }
                        (None, None, Some(_), _) => {
                            el.child(BranchView::new(self.tab.clone(), diff_view))
                        }
                        (None, None, None, Some((_, browser))) => el.child(browser.clone()),
                        (None, None, None, None) => el.child(PrList::new(self.tab.clone())),
                    }
                }),
            _ => root.child(SignIn::new(tab.app.clone(), self.token_input.clone())),
        }
    }
}

/// Handles `A` by running `run` on the window's tab.
fn on_tab<A: Action>(
    tab: &Entity<Tab>,
    run: fn(&mut Tab, &mut Context<Tab>),
) -> impl Fn(&A, &mut Window, &mut App) + 'static {
    let tab = tab.clone();
    move |_, _, cx| tab.update(cx, run)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone as _, Utc};
    use gpui::{Entity, Modifiers, Pixels, Point, TestAppContext, VisualTestContext, point, px};

    use crate::github::{Commit, FakeGithub, PullRequest, TimelineItem};
    use crate::lsp::{Location, Position, PositionEncoding};
    use crate::projects::ProjectId;
    use crate::state::{BranchTab, CommitsLoad, DiffLoad, PrTab, SectionKind, Tab};
    use crate::test_support::{
        VisualTestContextExt as _, detail, docket_windows, git, git_checkout, git_commit,
        open_docket, project, pull_request, window_context,
    };
    use crate::ui::code::{CodeDefinition as _, Definition};
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
            state.read_with(cx, |tab, cx| tab.docket(cx).items().count()),
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
        state.read_with(cx, |tab, cx| {
            assert!(tab.app.read(cx).projects.is_empty());
            assert!(tab.setup.is_none());
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
    fn older_branches_show_a_week_at_a_time(cx: &mut TestAppContext) {
        // The tests' clock is 2026-01-07, so the first week starts Dec 31.
        let checkout = tempfile::tempdir().unwrap();
        let dir = checkout.path();
        git(dir, &["init", "--quiet", "-b", "main"]);
        std::fs::write(dir.join("README.md"), "# Widgets\n").unwrap();
        git_commit(dir, "Start", "2025-11-01T00:00:00Z");
        for (branch, date) in [
            ("this-week", "2026-01-05T00:00:00Z"),
            ("last-week", "2025-12-27T00:00:00Z"),
            ("before", "2025-12-20T00:00:00Z"),
        ] {
            git(dir, &["checkout", "--quiet", "-b", branch, "main"]);
            std::fs::write(dir.join(format!("{branch}.md")), branch).unwrap();
            git_commit(dir, branch, date);
        }
        git(dir, &["checkout", "--quiet", "main"]);

        // Pull requests show however long they've been quiet.
        let mut old = pull_request("acme/widgets", 1, "Fix the gears");
        old.updated_at = Utc.with_ymd_and_hms(2025, 11, 1, 0, 0, 0).unwrap();
        let github = Arc::new(FakeGithub::new());
        github.set_search(SectionKind::Authored.search_query(), vec![old.clone()]);
        github.set_detail(old.repository.parse().unwrap(), detail(&old));
        let projects = vec![project(1, dir, "acme/widgets")];
        let (tab, cx) = open_docket(cx, github, projects);

        assert!(cx.is_drawn(&old.id));
        assert!(cx.is_drawn("branch-1-this-week"));
        assert!(!cx.is_drawn("branch-1-last-week"));
        assert!(
            top_of(cx, &old.id) < top_of(cx, "show-more"),
            "the button is at the bottom"
        );

        cx.click_on("show-more");
        assert!(cx.is_drawn("branch-1-last-week"), "another week back");
        assert!(!cx.is_drawn("branch-1-before"));

        cx.click_on("show-more");
        assert!(cx.is_drawn("branch-1-before"));
        assert!(!cx.is_drawn("show-more"), "there's nothing older");

        // Choosing a project starts from the last week again, and the
        // sidebar beside a pull request can show more too.
        cx.click_on("project-1");
        assert_eq!(tab.read_with(cx, |tab, _| tab.weeks), 1);
        cx.click_on(&old.id);
        assert!(!cx.is_drawn("branch-1-last-week"));
        cx.click_on("pr-sidebar-show-more");
        assert!(cx.is_drawn("branch-1-last-week"));
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

    /// The paths in the open diff, once it's loaded.
    fn diff_paths(state: &Entity<Tab>, cx: &mut VisualTestContext) -> Vec<String> {
        state.read_with(cx, |tab, _| {
            match &tab.diff_files().expect("a diff is open").diff {
                DiffLoad::Ready(diff) => diff.files.iter().map(|f| f.path().to_owned()).collect(),
                DiffLoad::Failed(err) => panic!("the diff failed: {err}"),
                _ => panic!("the diff hasn't loaded"),
            }
        })
    }

    fn commit_headlines(state: &Entity<Tab>, cx: &mut VisualTestContext) -> Vec<String> {
        state.read_with(cx, |tab, _| {
            match &tab.branch.as_ref().expect("a branch is open").commits {
                CommitsLoad::Ready(listed) => {
                    listed.commits.iter().map(|c| c.headline.clone()).collect()
                }
                CommitsLoad::Failed(err) => panic!("listing commits failed: {err}"),
                CommitsLoad::Loading => panic!("the commits haven't loaded"),
            }
        })
    }

    #[gpui::test]
    fn branches_without_pull_requests_open_their_changes(cx: &mut TestAppContext) {
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
        // The default branch moves on after the branches left it.
        git(dir, &["checkout", "--quiet", "main"]);
        commit("later.md", "Later", "2026-01-04T00:00:00Z");

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

        // A branch opens to what it changes since it left main, like a
        // pull request's files.
        cx.click_on("branch-1-sprockets");
        state.read_with(cx, |state, cx| {
            let open = state.branch.as_ref().expect("the branch is open");
            assert_eq!(open.branch.name, "sprockets");
            assert_eq!(open.base.as_ref().map(|b| b.name.as_str()), Some("main"));
            assert!(state.browser.is_none());
            assert!(
                state.title(cx).ends_with(" (sprockets)"),
                "{}",
                state.title(cx)
            );
        });
        assert_eq!(diff_paths(&state, cx), ["sprockets.md"]);
        assert!(cx.is_drawn("branch-back"));
        assert!(
            cx.is_drawn("branch-tab-files"),
            "with a pull request's tabs"
        );
        assert_eq!(width_of(cx, "sidebar-navigation"), RAIL_WIDTH);
        assert_eq!(width_of(cx, "sidebar-pull-requests"), RAIL_WIDTH);
        assert_eq!(width_of(cx, "sidebar-files"), SIDEBAR_WIDTH);
        assert!(cx.is_drawn("rail-branch-1-sprockets"));

        // Its commits are those since it left main, without a sidebar.
        cx.click_on("branch-tab-commits");
        assert_eq!(commit_headlines(&state, cx), ["Sprockets"]);
        let tip = state.read_with(cx, |state, _| {
            state.branch.as_ref().unwrap().branch.oid.clone()
        });
        assert!(cx.is_drawn(&format!("commit-{tip}")));
        assert!(
            !cx.is_drawn(&format!("open-commit-{tip}")),
            "it may not be pushed"
        );
        assert!(!cx.is_drawn("sidebar-files"));
        assert_eq!(width_of(cx, "sidebar-pull-requests"), SIDEBAR_WIDTH);

        // A refresh picks up new commits on it.
        git(dir, &["checkout", "--quiet", "sprockets"]);
        commit("cogs.md", "Cogs", "2026-01-06T00:00:00Z");
        git(dir, &["checkout", "--quiet", "main"]);
        cx.simulate_keystrokes("cmd-r");
        assert_eq!(commit_headlines(&state, cx), ["Sprockets", "Cogs"]);
        cx.click_on("branch-tab-files");
        assert_eq!(
            state.read_with(cx, |state, _| state.branch.as_ref().map(|b| b.tab)),
            Some(BranchTab::Files)
        );
        assert_eq!(diff_paths(&state, cx), ["cogs.md", "sprockets.md"]);
        assert_eq!(width_of(cx, "sidebar-files"), SIDEBAR_WIDTH);

        // The default branch is checked out, so its files are browsed on
        // disk.
        rest_mouse_on(cx, "sidebar-pull-requests");
        cx.click_on("branch-1-main");
        state.read_with(cx, |state, _| {
            assert!(state.branch.is_none());
            let browser = state.browser.as_ref().expect("the browser is open");
            assert_eq!(browser.branch.as_deref(), Some("main"));
            assert!(browser.working_copy().is_some());
        });
        assert!(cx.is_drawn("browser-tree-later.md"), "main's files");
        assert_eq!(width_of(cx, "sidebar-project-files"), SIDEBAR_WIDTH);

        // Escape closes a branch, and the docket beside it opens a pull
        // request in its place. Opening one changes the sidebars under the
        // mouse, so it leaves them before peeking again.
        let leave = |cx: &mut VisualTestContext, view: &str| {
            let at = cx.bounds_of(view).expect("it's drawn").center();
            move_mouse(cx, at);
        };
        leave(cx, "browser-back");
        rest_mouse_on(cx, "sidebar-pull-requests");
        cx.click_on("branch-1-sprockets");
        cx.simulate_keystrokes("escape");
        assert!(state.read_with(cx, |state, _| state.branch.is_none()));
        cx.click_on("branch-1-sprockets");
        leave(cx, "branch-back");
        rest_mouse_on(cx, "sidebar-pull-requests");
        cx.click_on(&older.id);
        state.read_with(cx, |state, _| {
            assert!(state.branch.is_none());
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
        mine.updated_at = Utc.with_ymd_and_hms(2025, 12, 31, 12, 0, 0).unwrap();
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
    ) -> (Entity<Tab>, &'a mut VisualTestContext) {
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

        cx.click_on("pr-tab-files");
        let tab = state.read_with(cx, |state, _| state.detail.as_ref().map(|d| d.tab));
        assert_eq!(tab, Some(PrTab::Files));
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

    #[gpui::test]
    fn commits_tab_lists_the_timelines_commits(cx: &mut TestAppContext) {
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        let mut detail = detail(&pr);
        let commit = |id: &str, headline: &str| {
            TimelineItem::Commit(Commit {
                id: id.into(),
                url: Some(format!("{}/commits/{id}", pr.url)),
                short_oid: id.into(),
                headline: headline.into(),
                author: Some("hubot".into()),
                committed_at: Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap(),
            })
        };
        detail.timeline = vec![commit("c1", "Add sprockets"), commit("c2", "Oil them")];
        detail.commit_count = 2;
        let github = Arc::new(FakeGithub::new());
        github.set_search(
            SectionKind::ReviewRequested.search_query(),
            vec![pr.clone()],
        );
        github.set_detail(pr.repository.parse().unwrap(), detail.clone());
        let (state, cx) = open_docket(cx, github.clone(), Vec::new());
        cx.click_on(&pr.id);

        cx.click_on("pr-tab-commits");
        let tab = state.read_with(cx, |state, _| state.detail.as_ref().map(|d| d.tab));
        assert_eq!(tab, Some(PrTab::Commits));
        assert!(cx.is_drawn("commit-c1"));
        assert!(cx.is_drawn("commit-c2"));
        assert!(cx.is_drawn("open-commit-c1"), "each opens on GitHub");
        assert!(!cx.is_drawn("pr-commits-github"), "none are missing");
        assert!(!cx.is_drawn("sidebar-files"), "only the Files tab has one");
        assert_eq!(width_of(cx, "sidebar-pull-requests"), SIDEBAR_WIDTH);

        // A timeline too long to load in full may be missing some.
        detail.commit_count = 300;
        github.set_detail(pr.repository.parse().unwrap(), detail);
        cx.simulate_keystrokes("cmd-r");
        assert!(cx.is_drawn("pr-commits-github"));
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

    #[gpui::test]
    fn tabs_start_like_their_window_and_are_independent(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        git_checkout(checkout.path(), &[("README.md", "# Widgets\n")]);
        let widgets = project(1, checkout.path(), "acme/widgets");
        let github = Arc::new(FakeGithub::new());
        let mine = pull_request("acme/widgets", 8, "Fix the gears");
        github.set_search(SectionKind::Authored.search_query(), vec![mine.clone()]);
        github.set_detail(mine.repository.parse().unwrap(), detail(&mine));
        let (first, cx) = open_docket(cx, github, vec![widgets.clone()]);

        cx.click_on("project-1");
        cx.simulate_keystrokes("cmd-t");
        let windows = docket_windows(cx);
        assert_eq!(windows.len(), 2, "the tab is a window of its own");
        let (second, mut second_cx) = window_context(cx, windows[1]);
        let selected = second.read_with(&second_cx, |tab, _| tab.selected_project);
        assert_eq!(
            selected,
            Some(ProjectId(1)),
            "it starts on the same project"
        );
        assert!(second_cx.is_drawn(&mine.id));

        second_cx.click_on(&mine.id);
        assert!(second.read_with(&second_cx, |tab, _| tab.detail.is_some()));
        assert!(first.read_with(cx, |tab, _| tab.detail.is_none()));
        assert!(cx.is_drawn(&mine.id), "the first tab still shows the list");

        // Each tab's title names what it shows, for the tab bar.
        assert_eq!(first.read_with(cx, |tab, cx| tab.title(cx)), widgets.name);
        assert_eq!(
            second.read_with(&second_cx, |tab, cx| tab.title(cx)),
            "Fix the gears #8"
        );

        // Escape backs out in its own tab only.
        cx.click_on("project-all");
        cx.simulate_keystrokes("escape");
        assert!(second.read_with(&second_cx, |tab, _| tab.detail.is_some()));
        second_cx.simulate_keystrokes("escape");
        assert!(second.read_with(&second_cx, |tab, _| tab.detail.is_none()));
        let selected = first.read_with(cx, |tab, _| tab.selected_project);
        assert_eq!(selected, None);
    }

    #[gpui::test]
    fn closing_a_tab_leaves_the_others(cx: &mut TestAppContext) {
        let (first, cx) = open_docket(cx, Arc::new(FakeGithub::new()), Vec::new());
        cx.simulate_keystrokes("cmd-t");
        let windows = docket_windows(cx);
        assert_eq!(windows.len(), 2);

        let (_, mut second_cx) = window_context(cx, windows[1]);
        second_cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        assert_eq!(docket_windows(cx), vec![windows[0]]);
        assert!(first.read_with(cx, |tab, _| tab.detail.is_none()));
        assert!(cx.is_drawn("project-all"));
    }

    #[gpui::test]
    fn opens_definitions_in_a_new_tab(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        git_checkout(
            checkout.path(),
            &[
                ("README.md", "Read the guide.\n"),
                ("docs/guide.md", "# Guide\n\nTurn it on.\n"),
            ],
        );
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (first, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("project-1");
        cx.click_on("branch-1-main");
        // `docs/`, then `README.md`.
        cx.simulate_keystrokes("down down");
        let open_file = |tab: &Entity<Tab>, cx: &gpui::App| {
            let browser = tab.read(cx).browser.as_ref()?;
            Some((browser.project.0, browser.file.as_ref()?.path.clone()))
        };
        assert_eq!(
            cx.read(|cx| open_file(&first, cx)),
            Some((1, "README.md".into()))
        );

        // As if the server answered a command-option-click on "guide".
        let at = |line, character| Position { line, character };
        let definition = Definition {
            location: Location {
                path: checkout.path().join("docs/guide.md"),
                range: (at(2, 0), at(2, 4)),
                extent: (at(2, 0), at(2, 12)),
            },
            relative: Some("docs/guide.md".into()),
            encoding: PositionEncoding::Utf16,
        };
        let windows = docket_windows(cx);
        let browser = cx.read(|cx| {
            let workspace = windows[0].read(cx).unwrap();
            workspace.file_browser.as_ref().unwrap().1.clone()
        });
        browser.update(cx, |browser, cx| {
            browser.show_definitions(0, 5..10, Ok(vec![definition]), None, true, cx)
        });
        cx.run_until_parked();

        let windows = docket_windows(cx);
        assert_eq!(windows.len(), 2, "the definition opened in a new tab");
        let (second, second_cx) = window_context(cx, windows[1]);
        assert_eq!(
            second_cx.read(|cx| open_file(&second, cx)),
            Some((1, "docs/guide.md".into()))
        );
        let expanded = second.read_with(&second_cx, |tab, _| {
            tab.browser.as_ref().unwrap().expanded.contains("docs")
        });
        assert!(expanded, "the tree shows where it is");
        assert_eq!(
            cx.read(|cx| open_file(&first, cx)),
            Some((1, "README.md".into())),
            "the first tab stays where it was"
        );
    }
}
