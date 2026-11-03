//! Root view of a window (a native tab, on macOS): shows sign-in until
//! authenticated, then what the window's [`Tab`] shows: the sidebars for
//! what's open (`sidebar_stack`), and the view beside them.
//!
//! Actions that act on what a window shows (Refresh, Escape, the project
//! shortcuts, Close Tab) are handled here, so they reach the window they
//! came from. The workspace takes focus whenever nothing in it has it, so
//! they always have somewhere to dispatch from. The window's title is the
//! tab's, which names it in the tab bar and the Window menu.

use gpui::{Action, AnyElement, App, Context, Entity, FocusHandle, Window, div, prelude::*};

use super::code::Definition;
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
use crate::projects::ProjectId;
use crate::state::{AuthStatus, PrTab, Tab};
use crate::{AddProject, Cancel, CloseTab, Minimize, NextProject, PreviousProject, Refresh, Zoom};

pub struct Workspace {
    tab: Entity<Tab>,
    /// The sign-in screen's personal access token field. Views are rebuilt
    /// each frame, so the field's entity lives here.
    token_input: Entity<TextInput>,
    /// The open PR's Files tab, keyed by PR id so opening another PR starts
    /// fresh (scroll position, collapsed files).
    diff_view: Option<(String, Entity<DiffView>)>,
    /// The open project's files, keyed by project so browsing another
    /// starts fresh (expanded directories, selection).
    file_browser: Option<(ProjectId, Entity<FileBrowserView>)>,
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
        } else if self.file_browser.is_some() {
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

    /// Creates or drops the diff view to match the open PR.
    fn sync_diff_view(&mut self, cx: &mut Context<Self>) {
        let open_id = self
            .tab
            .read(cx)
            .detail
            .as_ref()
            .map(|detail| detail.summary.id.clone());
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

    /// Creates or drops the file browser to match the browsed project.
    fn sync_file_browser(&mut self, cx: &mut Context<Self>) {
        let open = self
            .tab
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
                let tab = self.tab.clone();
                let view = cx.new(|cx| FileBrowserView::new(tab, cx));
                self.file_browser = Some((id, view));
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
                .map(|el| match (&tab.setup, &tab.detail, &self.file_browser) {
                    (Some(_), ..) => el.child(ProjectSetupView::new(self.tab.clone())),
                    (None, Some(_), _) => el.child(PrDetailView::new(
                        self.tab.clone(),
                        self.diff_view.as_ref().map(|(_, view)| view.clone()),
                    )),
                    (None, None, Some((_, browser))) => el.child(browser.clone()),
                    (None, None, None) => el.child(PrList::new(self.tab.clone())),
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

    use gpui::{Entity, Modifiers, Pixels, Point, TestAppContext, VisualTestContext, point, px};

    use crate::github::{FakeGithub, PullRequest};
    use crate::lsp::{Location, Position, PositionEncoding};
    use crate::state::{PrTab, SectionKind, Tab};
    use crate::test_support::{
        VisualTestContextExt as _, detail, docket_windows, git_checkout, open_docket, project,
        pull_request, window_context,
    };
    use crate::ui::code::{CodeDefinition as _, Definition};
    use crate::ui::sidebar_stack::{PEEK_DELAY, RAIL_WIDTH, SIDEBAR_WIDTH};

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
        assert!(cx.is_drawn(&pr.id), "the list moves to a sidebar");
        assert!(cx.is_drawn("rail-Review requested"), "navigation collapses");
        assert!(!cx.is_drawn("Review requested"));

        cx.simulate_keystrokes("escape");
        assert!(state.read_with(cx, |state, _| state.detail.is_none()));
        assert!(cx.is_drawn(&pr.id));
        assert!(cx.is_drawn("Review requested"));
        assert!(!cx.is_drawn("rail-Review requested"));
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

    #[gpui::test]
    fn hovering_a_rail_expands_it_in_place_of_the_deepest(cx: &mut TestAppContext) {
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        let (_, cx) = open_pull_request(cx, &pr);
        let rail = format!("rail-{}", pr.id);
        let content = cx.bounds_of("pr-back").unwrap().origin;
        assert_eq!(width_of(cx, "sidebar-navigation"), RAIL_WIDTH);
        assert_eq!(width_of(cx, "sidebar-pull-requests"), SIDEBAR_WIDTH);

        rest_mouse_on(cx, "sidebar-navigation");
        assert!(cx.is_drawn("Review requested"), "navigation expands");
        assert!(cx.is_drawn(&rail), "the inbox collapses");
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
        assert!(cx.is_drawn("rail-Review requested"));
    }

    #[gpui::test]
    fn crossing_a_rail_leaves_it_collapsed(cx: &mut TestAppContext) {
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        let (_, cx) = open_pull_request(cx, &pr);

        let rail = cx.bounds_of("sidebar-navigation").unwrap().center();
        cx.simulate_mouse_move(rail, None, Modifiers::none());
        rest_mouse_on(cx, "pr-back");
        assert!(cx.is_drawn("rail-Review requested"));
        assert!(cx.is_drawn(&pr.id));
    }

    #[gpui::test]
    fn rail_icons_act_without_expanding(cx: &mut TestAppContext) {
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        let (state, cx) = open_pull_request(cx, &pr);

        cx.click_on(&format!("rail-{}", SectionKind::Authored.label()));
        state.read_with(cx, |state, _| {
            assert!(state.detail.is_none());
            assert_eq!(state.selected, SectionKind::Authored);
        });
    }

    #[gpui::test]
    fn files_tab_adds_a_sidebar_and_collapses_the_inbox(cx: &mut TestAppContext) {
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

        // Peeking at the inbox swaps it with the files.
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

        // Heading right, past the edge of navigation and onto the inbox's
        // rail, navigation stays open until the mouse rests there.
        let inbox = cx.bounds_of("sidebar-pull-requests").unwrap().center();
        move_mouse(cx, inbox);
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

        // Resting on the line between navigation's rail and the inbox
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
        let github = Arc::new(FakeGithub::new());
        let mine = pull_request("acme/widgets", 8, "Fix the gears");
        github.set_search(SectionKind::Authored.search_query(), vec![mine.clone()]);
        github.set_detail(mine.repository.parse().unwrap(), detail(&mine));
        let (first, cx) = open_docket(cx, github, Vec::new());

        cx.click_on(SectionKind::Authored.label());
        cx.simulate_keystrokes("cmd-t");
        let windows = docket_windows(cx);
        assert_eq!(windows.len(), 2, "the tab is a window of its own");
        let (second, mut second_cx) = window_context(cx, windows[1]);
        let selected = second.read_with(&second_cx, |tab, _| tab.selected);
        assert_eq!(
            selected,
            SectionKind::Authored,
            "it starts on the same inbox"
        );
        assert!(second_cx.is_drawn(&mine.id));

        second_cx.click_on(&mine.id);
        assert!(second.read_with(&second_cx, |tab, _| tab.detail.is_some()));
        assert!(first.read_with(cx, |tab, _| tab.detail.is_none()));
        assert!(cx.is_drawn(&mine.id), "the first tab still shows the list");

        // Each tab's title names what it shows, for the tab bar.
        assert_eq!(
            first.read_with(cx, |tab, cx| tab.title(cx)),
            "My pull requests"
        );
        assert_eq!(
            second.read_with(&second_cx, |tab, cx| tab.title(cx)),
            "Fix the gears #8"
        );

        // Escape backs out in its own tab only.
        cx.click_on(SectionKind::ReviewRequested.label());
        cx.simulate_keystrokes("escape");
        assert!(second.read_with(&second_cx, |tab, _| tab.detail.is_some()));
        second_cx.simulate_keystrokes("escape");
        assert!(second.read_with(&second_cx, |tab, _| tab.detail.is_none()));
        let selected = first.read_with(cx, |tab, _| tab.selected);
        assert_eq!(selected, SectionKind::ReviewRequested);
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
        assert!(cx.is_drawn(SectionKind::ReviewRequested.label()));
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
        cx.click_on("browse-project");
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
