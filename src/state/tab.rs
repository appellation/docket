//! What one window shows. On macOS windows are native tabs, so this is a
//! tab: the selected inbox and project, and the PR list, or in its place a
//! pull request, a project's files, or a project's setup.
//!
//! Tabs are independent, like windows: opening a pull request in one leaves
//! the others where they were, and each has its own language servers. What
//! they share (the sign-in, inboxes, and projects) is in `AppState`. Tabs
//! re-render when it changes and follow its events: an inbox refresh
//! reloads an open PR's diff if its head moved, and removing a project
//! closes its files.

use gpui::{App, Context, Entity, Subscription, Task};

use super::{AppEvent, AppState, PrDetail, ProjectBrowser, ProjectSetup, SectionKind};
use crate::github::PullRequest;
use crate::projects::{Project, ProjectId};

pub struct Tab {
    pub app: Entity<AppState>,
    pub selected: SectionKind,
    /// Scopes the inboxes to one project; `None` shows everything.
    pub selected_project: Option<ProjectId>,
    /// Set while adding or editing a project; replaces the PR list.
    pub setup: Option<ProjectSetup>,
    /// The pull request opened from the list; replaces it until closed.
    pub detail: Option<PrDetail>,
    /// A project's files, opened from its header; replaces the list too.
    pub browser: Option<ProjectBrowser>,
    pub(super) _folder_prompt_task: Option<Task<()>>,
    _app_subscriptions: [Subscription; 2],
}

impl Tab {
    /// A tab showing `selected`'s inbox, scoped to `project`.
    pub fn new(
        app: Entity<AppState>,
        selected: SectionKind,
        project: Option<ProjectId>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = [
            cx.observe(&app, |_, _, cx| cx.notify()),
            cx.subscribe(&app, |this, _, event, cx| this.app_changed(*event, cx)),
        ];
        Self {
            app,
            selected,
            selected_project: project,
            setup: None,
            detail: None,
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
                self.retry_pr_files(cx);
            }
            AppEvent::ProjectSaved(id) => {
                // The project may have moved; list its files from where it
                // is now.
                if self.browser.as_ref().is_some_and(|b| b.project == id) {
                    self.refresh_browser(cx);
                }
                self.retry_pr_files(cx);
            }
            AppEvent::ProjectRemoved(id) => {
                if self.selected_project == Some(id) {
                    self.selected_project = None;
                }
                if self.setup.as_ref().is_some_and(|s| s.editing == Some(id)) {
                    self.setup = None;
                }
                if self.browser.as_ref().is_some_and(|b| b.project == id) {
                    self.browser = None;
                }
            }
            // An in-progress setup needs the client, so it closes too.
            AppEvent::SignedOut => {
                self.setup = None;
                self.detail = None;
                self.browser = None;
            }
        }
        cx.notify();
    }

    pub fn selected_project<'a>(&self, cx: &'a App) -> Option<&'a Project> {
        self.app.read(cx).project(self.selected_project?)
    }

    /// PRs in `kind`'s inbox for the selected project.
    pub fn visible_pull_requests<'a>(
        &self,
        kind: SectionKind,
        cx: &'a App,
    ) -> impl Iterator<Item = &'a PullRequest> {
        self.app
            .read(cx)
            .pull_requests_in(kind, self.selected_project(cx))
    }

    pub fn select(&mut self, kind: SectionKind, cx: &mut Context<Self>) {
        self.selected = kind;
        self.setup = None;
        self.detail = None;
        self.browser = None;
        cx.notify();
    }

    /// Scopes the inboxes to a project, or to everything with `None`.
    pub fn select_project(&mut self, id: Option<ProjectId>, cx: &mut Context<Self>) {
        self.setup = None;
        self.detail = None;
        self.browser = None;
        self.selected_project = id;
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
    /// pull request or project files.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.app.read(cx).is_signed_in() {
            self.app.update(cx, |app, cx| app.cancel_sign_in(cx));
        } else if self.setup.is_some() {
            self.cancel_setup(cx);
        } else if self.detail.is_none() {
            self.close_browser(cx);
        } else {
            self.close_pull_request(cx);
        }
    }

    /// Re-fetches the inboxes, and what this tab shows: the open pull
    /// request, or the project's files.
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
        if let Some(browser) = &self.browser {
            let project = app.project(browser.project).map(|p| p.name.as_str());
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
            Some(project) => format!("{} — {}", self.selected.label(), project.name),
            None => self.selected.label().to_owned(),
        }
    }
}
