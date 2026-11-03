//! What one window shows. On macOS windows are native tabs, so this is a
//! tab: the selected project, and its docket, or in its place a pull
//! request, a branch's changes, the default branch's files, or a project's
//! setup.
//!
//! Tabs are independent, like windows: opening a pull request in one leaves
//! the others where they were, and each has its own language servers. What
//! they share (the sign-in, pull requests, projects, and branches) is in
//! `AppState`. Tabs re-render when it changes and follow its events: a
//! refresh reloads an open PR's or branch's diff if its head moved, and
//! removing a project closes its branches and files.

use chrono::{DateTime, Duration, Utc};
use gpui::{App, Context, Entity, Subscription, Task};

use super::docket::{self, Docket};
use super::{AppEvent, AppState, BranchDiff, PrDetail, ProjectBrowser, ProjectSetup};
use crate::projects::{Project, ProjectId};

pub struct Tab {
    pub app: Entity<AppState>,
    /// Scopes the docket to one project; `None` shows everything.
    pub selected_project: Option<ProjectId>,
    /// How many weeks back the docket shows branches from. Pull requests
    /// show however old they are.
    pub weeks: u32,
    /// Set while adding or editing a project; replaces the docket.
    pub setup: Option<ProjectSetup>,
    /// The pull request opened from the docket; replaces it until closed.
    pub detail: Option<PrDetail>,
    /// A branch's changes, opened from the docket; replaces it too.
    pub branch: Option<BranchDiff>,
    /// The default branch's files, opened from the docket; replaces it too.
    pub browser: Option<ProjectBrowser>,
    pub(super) _folder_prompt_task: Option<Task<()>>,
    _app_subscriptions: [Subscription; 2],
}

impl Tab {
    /// A tab showing the docket, scoped to `project`.
    pub fn new(app: Entity<AppState>, project: Option<ProjectId>, cx: &mut Context<Self>) -> Self {
        let subscriptions = [
            cx.observe(&app, |_, _, cx| cx.notify()),
            cx.subscribe(&app, |this, _, event, cx| this.app_changed(*event, cx)),
        ];
        Self {
            app,
            selected_project: project,
            weeks: 1,
            setup: None,
            detail: None,
            branch: None,
            browser: None,
            _folder_prompt_task: None,
            _app_subscriptions: subscriptions,
        }
    }

    fn app_changed(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        match event {
            AppEvent::PullRequestsLoaded => self.sync_pr_files(cx),
            AppEvent::ProjectsLoaded => {
                if self.selected_project.is_none() {
                    self.selected_project = self.app.read(cx).last_project;
                }
                self.retry_diff(cx);
            }
            AppEvent::ProjectSaved(id) => {
                // The project may have moved; list its files from where it
                // is now.
                if self.browser.as_ref().is_some_and(|b| b.project == id) {
                    self.refresh_browser(cx);
                }
                self.retry_diff(cx);
            }
            AppEvent::BranchesLoaded(id) => self.sync_branch_files(id, cx),
            AppEvent::ProjectRemoved(id) => {
                if self.selected_project == Some(id) {
                    self.selected_project = None;
                }
                if self.setup.as_ref().is_some_and(|s| s.editing == Some(id)) {
                    self.setup = None;
                }
                if self.branch.as_ref().is_some_and(|b| b.project == id) {
                    self.branch = None;
                }
                if self.browser.as_ref().is_some_and(|b| b.project == id) {
                    self.browser = None;
                }
            }
            // An in-progress setup needs the client, so it closes too.
            AppEvent::SignedOut => {
                self.setup = None;
                self.detail = None;
                self.branch = None;
                self.browser = None;
            }
        }
        cx.notify();
    }

    pub fn selected_project<'a>(&self, cx: &'a App) -> Option<&'a Project> {
        self.app.read(cx).project(self.selected_project?)
    }

    /// The docket for the selected project.
    pub fn docket<'a>(&self, cx: &'a App) -> Docket<'a> {
        self.app
            .read(cx)
            .docket(self.selected_project(cx), self.since())
    }

    /// How far back the docket shows branches.
    pub fn since(&self) -> DateTime<Utc> {
        docket::now() - Duration::weeks(self.weeks.into())
    }

    /// Shows the docket's branches from another week back.
    pub fn show_more(&mut self, cx: &mut Context<Self>) {
        self.weeks += 1;
        cx.notify();
    }

    /// Scopes the docket to a project, or to everything with `None`.
    pub fn select_project(&mut self, id: Option<ProjectId>, cx: &mut Context<Self>) {
        self.setup = None;
        self.detail = None;
        self.branch = None;
        self.browser = None;
        self.selected_project = id;
        // Another project's docket starts from the last week again.
        self.weeks = 1;
        self.app.update(cx, |app, cx| app.remember_project(id, cx));
        cx.notify();
    }

    /// Cycles through "All projects" followed by each project.
    pub fn select_adjacent_project(&mut self, delta: isize, cx: &mut Context<Self>) {
        let scopes: Vec<Option<ProjectId>> = std::iter::once(None)
            .chain(self.app.read(cx).projects.iter().map(|p| Some(p.id)))
            .collect();
        let current = scopes
            .iter()
            .position(|id| *id == self.selected_project)
            .unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(scopes.len() as isize) as usize;
        self.select_project(scopes[next], cx);
    }

    /// Escape: backs out of whatever is in progress, or out of the open
    /// pull request, branch, or files.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.app.read(cx).is_signed_in() {
            self.app.update(cx, |app, cx| app.cancel_sign_in(cx));
        } else if self.setup.is_some() {
            self.cancel_setup(cx);
        } else if self.detail.is_some() {
            self.close_pull_request(cx);
        } else if self.branch.is_some() {
            self.close_branch(cx);
        } else {
            self.close_browser(cx);
        }
    }

    /// Re-fetches the docket, and what this tab shows: the open pull
    /// request, or the default branch's files. An open branch's diff
    /// follows its branch, which the docket re-reads.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.app.update(cx, |app, cx| app.refresh(cx));
        self.refresh_pull_request(cx);
        self.refresh_browser(cx);
    }

    /// What the tab shows, for its title in the tab bar.
    pub fn title(&self, cx: &App) -> String {
        let app = self.app.read(cx);
        if !app.is_signed_in() {
            return "Docket".to_owned();
        }
        if let Some(setup) = &self.setup {
            return match setup.editing.and_then(|id| app.project(id)) {
                Some(project) => format!("Edit {}", project.name),
                None => "Add Project".to_owned(),
            };
        }
        if let Some(detail) = &self.detail {
            let title = detail
                .loaded
                .as_ref()
                .map_or(&detail.summary.title, |loaded| &loaded.pr.title);
            return format!("{title} #{}", detail.number);
        }
        if let Some(open) = &self.branch {
            return match app.project(open.project) {
                Some(project) => format!("{} ({})", project.name, open.branch.name),
                None => open.branch.name.clone(),
            };
        }
        if let Some(browser) = &self.browser {
            let project = app.project(browser.project).map(|p| match &browser.branch {
                Some(branch) => format!("{} ({branch})", p.name),
                None => p.name.clone(),
            });
            let project = project.as_deref();
            let file = browser.file.as_ref().map(|file| {
                file.path
                    .rsplit_once('/')
                    .map_or(file.path.as_str(), |(_, name)| name)
            });
            return match (file, project) {
                (Some(file), Some(project)) => format!("{file} — {project}"),
                (None, Some(project)) => project.to_owned(),
                (Some(file), None) => file.to_owned(),
                (None, None) => "Docket".to_owned(),
            };
        }
        match self.selected_project(cx) {
            Some(project) => project.name.clone(),
            None => "Docket".to_owned(),
        }
    }
}
