//! UI tests: Docket in a headless GPUI window, signed in to a fake GitHub,
//! with fixtures to fill it.
//!
//! GPUI's test platform has no screen. Windows lay out and paint into
//! nothing, and input goes straight to them, so tests never show a window,
//! move the mouse, or take focus. Text is measured with placeholder metrics,
//! so tests check behavior and structure (what's drawn, what's selected,
//! what state changed), not how things look.
//!
//! Nothing reaches the user's sign-in, projects, or GitHub: `TokenStore`,
//! `ProjectStore`, the GitHub CLI, and `GithubClient` (unless faked) all do
//! nothing or fail under `cfg(test)`.
//!
//! ```ignore
//! #[gpui::test]
//! fn opens_a_pull_request(cx: &mut TestAppContext) {
//!     let github = Arc::new(FakeGithub::new());
//!     let pr = pull_request("acme/widgets", 7, "Add sprockets");
//!     github.set_search(SectionKind::ReviewRequested.search_query(), vec![pr.clone()]);
//!     let (tab, cx) = open_docket(cx, github, Vec::new());
//!     cx.click_on(&pr.id);
//!     assert!(tab.read_with(cx, |tab, _| tab.detail.is_some()));
//! }
//! ```
//!
//! Elements are found by their test selector. Components use their id as
//! one (`ListItem`, `Button`, `IconButton`), and so do PR rows (the PR's
//! `id`); add `.test_selector(|| …)` (`components::TestSelector`) to
//! anything else a test needs to find. Selectors cost nothing outside tests.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use chrono::{TimeZone as _, Utc};
use gpui::{
    Bounds, Entity, Modifiers, Pixels, TestAppContext, VisualContext as _, VisualTestContext,
    WindowHandle, WindowId, px, size,
};

use crate::github::{
    Actor, FakeGithub, GithubClient, MergeStatus, PullRequest, PullRequestDetail, PullRequestState,
};
use crate::projects::{Project, ProjectId};
use crate::state::{AppState, Tab};
use crate::ui::{Workspace, windows};

/// Opens a Docket window the size of the real one, signed in to `github`
/// with `projects`, and waits for the first refresh. Returns the window's
/// tab.
pub fn open_docket(
    cx: &mut TestAppContext,
    github: Arc<FakeGithub>,
    projects: Vec<Project>,
) -> (Entity<Tab>, &mut VisualTestContext) {
    cx.update(|cx| {
        crate::init(cx);
        AppState::init_for_test(GithubClient::fake(github), projects, cx);
    });
    let window = cx
        .update(|cx| windows::open(None, false, cx, |_, _| {}))
        .expect("the window opens");
    let (tab, cx) = window_context(cx, window);
    (tab, cx.into_mut())
}

/// Docket's windows (tabs, on macOS), in the order they opened.
pub fn docket_windows(cx: &TestAppContext) -> Vec<WindowHandle<Workspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<Workspace>())
        .collect()
}

/// A window's tab, and a context for driving the window, sized like the
/// real one.
pub fn window_context(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> (Entity<Tab>, VisualTestContext) {
    let tab = cx
        .update(|cx| window.read(cx).map(|workspace| workspace.tab().clone()))
        .expect("the window is open");
    let cx = VisualTestContext::from_window(window.into(), cx);
    cx.simulate_resize(size(px(1100.), px(720.)));
    cx.run_until_parked();
    (tab, cx)
}

thread_local! {
    /// The test selectors of the elements built for each window's last
    /// frame. GPUI's own map of them (`debug_bounds`) is never cleared, so
    /// it still has elements that are gone. Each GPUI test runs on one
    /// thread.
    static DRAWN: RefCell<HashMap<WindowId, HashSet<String>>> = RefCell::default();
    /// The window being drawn. Windows draw one at a time.
    static DRAWING: Cell<Option<WindowId>> = const { Cell::new(None) };
}

/// Called as a window's root view renders, before any element is built.
pub fn start_frame(window: WindowId) {
    DRAWING.set(Some(window));
    DRAWN.with_borrow_mut(|drawn| drawn.entry(window).or_default().clear());
}

pub fn note_drawn(selector: &str) {
    let Some(window) = DRAWING.get() else {
        return;
    };
    DRAWN.with_borrow_mut(|drawn| drawn.entry(window).or_default().insert(selector.to_owned()));
}

pub trait VisualTestContextExt {
    /// Where the element with `selector` was drawn in the last frame.
    fn bounds_of(&mut self, selector: &str) -> Option<Bounds<Pixels>>;

    /// Whether an element with `selector` was drawn in the last frame.
    fn is_drawn(&mut self, selector: &str) -> bool {
        self.bounds_of(selector).is_some()
    }

    /// Clicks the middle of the element with `selector`, then runs until
    /// everything it started is done.
    fn click_on(&mut self, selector: &str);
}

impl VisualTestContextExt for VisualTestContext {
    fn bounds_of(&mut self, selector: &str) -> Option<Bounds<Pixels>> {
        let window = self.window_handle().window_id();
        let drawn =
            DRAWN.with_borrow(|drawn| drawn.get(&window).is_some_and(|d| d.contains(selector)));
        if !drawn {
            return None;
        }
        // GPUI looks selectors up by `&'static str`; leaking in tests is fine.
        self.debug_bounds(selector.to_owned().leak())
    }

    fn click_on(&mut self, selector: &str) {
        let bounds = self
            .bounds_of(selector)
            .unwrap_or_else(|| panic!("nothing with debug selector {selector:?} was drawn"));
        self.simulate_click(bounds.center(), Modifiers::none());
        self.run_until_parked();
    }
}

// --- Fixtures -----------------------------------------------------------

/// What the docket counts back from in tests: a week after the fixtures'
/// usual 2026-01-01, so they show without showing more.
pub fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 7, 0, 0, 0).unwrap()
}

/// A user with no profile name, shown by login.
pub fn actor(login: &str) -> Actor {
    Actor {
        login: login.into(),
        name: None,
        avatar_url: None,
        is_bot: false,
    }
}

/// An open pull request whose `id` (and so debug selector) is
/// `repository#number`.
pub fn pull_request(repository: &str, number: u64, title: &str) -> PullRequest {
    PullRequest {
        id: format!("{repository}#{number}"),
        number,
        title: title.into(),
        url: format!("https://github.com/{repository}/pull/{number}"),
        repository: repository.into(),
        author: Some(actor("hubot")),
        state: PullRequestState::Open,
        is_draft: false,
        review_decision: None,
        additions: 12,
        deletions: 3,
        updated_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        base_ref_name: "main".into(),
        base_ref_oid: "0".repeat(40),
        head_ref_name: format!("feature-{number}"),
        head_ref_oid: "1".repeat(40),
        is_cross_repository: false,
        author_is_member: false,
    }
}

/// `pr`'s detail, with an empty timeline and nothing pending.
pub fn detail(pr: &PullRequest) -> PullRequestDetail {
    PullRequestDetail {
        id: pr.id.clone(),
        number: pr.number,
        title: pr.title.clone(),
        url: pr.url.clone(),
        author: None,
        body: String::new(),
        state: pr.state,
        is_draft: pr.is_draft,
        created_at: pr.updated_at,
        base_ref: pr.base_ref_name.clone(),
        head_ref: "feature".into(),
        additions: pr.additions,
        deletions: pr.deletions,
        changed_files: 1,
        commit_count: 1,
        review_decision: pr.review_decision,
        merge_status: MergeStatus::Clean,
        checks: Vec::new(),
        check_summary: Default::default(),
        reviewers: Vec::new(),
        assignees: Vec::new(),
        timeline: Vec::new(),
        summary: pr.clone(),
    }
}

/// A project for the checkout at `path`, connected to `repository`
/// (`owner/name`).
pub fn project(id: u64, path: &Path, repository: &str) -> Project {
    Project {
        id: ProjectId(id),
        name: path
            .file_name()
            .map_or_else(|| repository.into(), |name| name.to_string_lossy().into()),
        path: path.to_owned(),
        repository: repository.parse().expect("repository is owner/name"),
        automated_authors: Vec::new(),
        merge: Default::default(),
        path_missing: !path.is_dir(),
    }
}

/// Makes `dir` a Git checkout of `files` (path and contents), committed on
/// `main`, which is checked out. It's pinned in the docket as
/// `branch-<project id>-main`, and clicking it browses the checkout. A
/// branch of yours off it, `feature`, is in the docket too, with an empty
/// commit.
pub fn git_checkout(dir: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    git(dir, &["init", "--quiet", "-b", "main"]);
    git_commit(dir, "Add files", "2025-12-01T00:00:00Z");
    git(dir, &["checkout", "--quiet", "-b", "feature"]);
    git_at(
        dir,
        Some("2026-01-01T00:00:00Z"),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Start the feature",
        ],
    );
    git(dir, &["checkout", "--quiet", "main"]);
}

/// Runs `git` in `dir`. Commits are by `you@example.com`, which a checkout
/// that's just been initialized is configured as, so its branches are
/// yours.
pub fn git(dir: &Path, args: &[&str]) {
    git_at(dir, None, args);
}

/// Commits everything in `dir`, as `you@example.com` at `date` (RFC 3339).
pub fn git_commit(dir: &Path, message: &str, date: &str) {
    git(dir, &["add", "--all"]);
    git_at(dir, Some(date), &["commit", "--quiet", "-m", message]);
}

fn git_at(dir: &Path, date: Option<&str>, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    if let Some(date) = date {
        command
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date);
    }
    let status = command
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "You")
        .env("GIT_AUTHOR_EMAIL", "you@example.com")
        .env("GIT_COMMITTER_NAME", "You")
        .env("GIT_COMMITTER_EMAIL", "you@example.com")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
    if args.first() == Some(&"init") {
        git(dir, &["config", "user.email", "you@example.com"]);
    }
}
