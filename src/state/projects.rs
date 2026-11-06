//! Project state: the saved projects, and the setup flow that connects a
//! local checkout to a GitHub repository (in a tab, where it's shown).
//!
//! Setup scans the chosen folder's Git remotes, checks each one on GitHub,
//! and pre-selects the most likely home for the project's PRs: an `upstream`
//! remote or a fork's parent over `origin`.

use std::path::PathBuf;

use anyhow::Result;
use gpui::{Context, PathPromptOptions, SharedString, Task};

use super::{AppEvent, AppState, AuthStatus, SectionKind, Tab};
use crate::auth::CredentialSource;
use crate::config::GITHUB_WEB_URL;
use crate::github::{GithubClient, PullRequest, RepoAccessError, RepoRef, Repository};
use crate::projects::{Checkout, MergeSettings, Project, ProjectId, ProjectStore, parse_logins};

pub struct ProjectSetup {
    /// The project being edited; `None` when adding a new one.
    pub editing: Option<ProjectId>,
    /// The folder the user chose.
    pub path: PathBuf,
    /// Root of the Git checkout containing `path`, once scanned.
    pub root: Option<PathBuf>,
    pub scanning: bool,
    pub error: Option<SharedString>,
    pub candidates: Vec<RepoCandidate>,
    pub selected: Option<usize>,
    /// Logins whose comments are automated (`Project::automated_authors`).
    pub automated_authors: Vec<String>,
    /// How the project's pull requests merge.
    pub merge: MergeSettings,
    /// Once the user picks a candidate, stop auto-selecting.
    user_selected: bool,
    _tasks: Vec<Task<()>>,
}

pub struct RepoCandidate {
    pub repo: RepoRef,
    pub source: CandidateSource,
    pub check: RepoCheck,
}

pub enum CandidateSource {
    /// A Git remote, by name.
    Remote(String),
    /// The parent of a fork found among the remotes.
    ForkParent(RepoRef),
    /// The repository the project being edited is already connected to.
    Current,
}

pub enum RepoCheck {
    Checking,
    Ok(Repository),
    Failed {
        message: SharedString,
        /// A page on GitHub that resolves the failure, when there is one.
        fix: Option<AccessFix>,
    },
}

pub struct AccessFix {
    pub label: &'static str,
    pub url: String,
}

/// Why GitHub hides a project's pull requests from this sign-in.
pub struct ProjectAccessIssue {
    pub message: SharedString,
    pub fix: Option<AccessFix>,
}

impl RepoCandidate {
    /// Lower is a better default for where the project's PRs live.
    fn rank(&self) -> (bool, u8) {
        let is_fork = matches!(&self.check, RepoCheck::Ok(repo) if repo.parent.is_some());
        let source = match &self.source {
            CandidateSource::Current => 0,
            CandidateSource::ForkParent(_) => 1,
            CandidateSource::Remote(name) if name == "upstream" => 1,
            CandidateSource::Remote(name) if name == "origin" => 2,
            CandidateSource::Remote(_) => 3,
        };
        (is_fork, source)
    }
}

impl ProjectSetup {
    fn new(path: PathBuf, editing: Option<ProjectId>) -> Self {
        Self {
            editing,
            path,
            root: None,
            scanning: true,
            error: None,
            candidates: Vec::new(),
            selected: None,
            automated_authors: Vec::new(),
            merge: MergeSettings::default(),
            user_selected: false,
            _tasks: Vec::new(),
        }
    }

    /// Whether the merge settings are complete: commenting needs a comment.
    pub fn merge_is_valid(&self) -> bool {
        self.merge.plan().is_some()
    }

    /// The selected repository, if GitHub confirmed the viewer can access it.
    pub fn confirmed_repository(&self) -> Option<&Repository> {
        match &self.candidates.get(self.selected?)?.check {
            RepoCheck::Ok(repo) => Some(repo),
            _ => None,
        }
    }

    fn auto_select(&mut self) {
        if self.user_selected {
            return;
        }
        self.selected = self
            .candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| !matches!(c.check, RepoCheck::Failed { .. }))
            .min_by_key(|(_, c)| c.rank())
            .map(|(ix, _)| ix);
    }
}

impl AppState {
    pub(super) fn load_projects(&mut self, cx: &mut Context<Self>) {
        self._load_projects_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { ProjectStore::load() })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(store) => {
                        this.last_project = store
                            .selected
                            .filter(|id| store.projects.iter().any(|p| p.id == *id));
                        this.projects = store.projects;
                        this.check_project_access(cx);
                        this.load_branches(cx);
                    }
                    Err(err) => eprintln!("failed to load projects: {err:#}"),
                }
                cx.emit(AppEvent::ProjectsLoaded);
                cx.notify();
            })
            .ok();
        }));
    }

    fn save_projects(&mut self, cx: &mut Context<Self>) {
        let store = ProjectStore {
            projects: self.projects.clone(),
            selected: self.last_project,
        };
        // Chain onto the previous save so writes land in order.
        let previous = self._save_projects_task.take();
        self._save_projects_task = Some(cx.background_executor().spawn(async move {
            if let Some(previous) = previous {
                previous.await;
            }
            if let Err(err) = store.save() {
                eprintln!("failed to save projects: {err:#}");
            }
        }));
    }

    pub fn project(&self, id: ProjectId) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// Logins whose comments on `repo`'s pull requests are automated, from
    /// every project connected to it.
    pub fn automated_authors(&self, repo: &RepoRef) -> Vec<String> {
        let mut authors: Vec<String> = Vec::new();
        for project in self.projects.iter().filter(|p| p.repository == *repo) {
            for author in &project.automated_authors {
                if !authors.iter().any(|a| a.eq_ignore_ascii_case(author)) {
                    authors.push(author.clone());
                }
            }
        }
        authors
    }

    /// Saves which project a tab scoped its docket to, for the next
    /// launch to start there.
    pub fn remember_project(&mut self, id: Option<ProjectId>, cx: &mut Context<Self>) {
        if self.last_project != id {
            self.last_project = id;
            self.save_projects(cx);
        }
    }

    /// PRs from `kind`'s search, limited to `project` if given.
    pub fn pull_requests_in<'a>(
        &'a self,
        kind: SectionKind,
        project: Option<&'a Project>,
    ) -> impl Iterator<Item = &'a PullRequest> {
        self.section(kind)
            .pull_requests
            .iter()
            .filter(move |pr| project.is_none_or(|p| p.contains(pr)))
            .filter(|pr| !self.is_done_with(pr))
    }

    pub fn remove_project(&mut self, id: ProjectId, cx: &mut Context<Self>) {
        self.projects.retain(|p| p.id != id);
        self.project_access.remove(&id);
        self.branches.remove(&id);
        self.pulls.remove(&id);
        self.pull_errors.remove(&id);
        if self.last_project == Some(id) {
            self.last_project = None;
        }
        self.save_projects(cx);
        cx.emit(AppEvent::ProjectRemoved(id));
        cx.notify();
    }

    /// A page on GitHub that resolves `err` for the current sign-in.
    pub(super) fn access_fix(&self, err: &RepoAccessError) -> Option<AccessFix> {
        // Org restrictions on OAuth apps don't apply to a token of your own.
        let oauth = matches!(
            self.auth,
            AuthStatus::SignedIn {
                source: CredentialSource::OAuth,
                ..
            }
        );
        match err {
            RepoAccessError::SsoRequired { url, .. } => Some(AccessFix {
                label: "Authorize SSO",
                url: url.clone(),
            }),
            // Docket's authorization page lists each org's access, with a
            // "Request" button where it hasn't been granted. A plain 404
            // can also be an org restriction, so offer it there too.
            RepoAccessError::OauthAppRestricted { .. } if oauth => Some(AccessFix {
                label: "Request approval",
                url: self.app_authorization_url(),
            }),
            RepoAccessError::NotFound(_) if oauth => Some(AccessFix {
                label: "Review org access",
                url: self.app_authorization_url(),
            }),
            _ => None,
        }
    }

    pub fn project_access_issue(&self, id: ProjectId) -> Option<&ProjectAccessIssue> {
        self.project_access.get(&id)
    }

    /// GitHub search silently leaves out repositories the sign-in can't
    /// reach (an org's SSO, say), so the docket can look empty when it isn't.
    /// Checks each project's repository so the UI can say why.
    pub(super) fn check_project_access(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client() else {
            return;
        };
        self._access_tasks = self
            .projects
            .iter()
            .map(|project| {
                let (id, repo, client) = (project.id, project.repository.clone(), client.clone());
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { client.repository(&repo) })
                        .await;
                    this.update(cx, |this, cx| {
                        match result {
                            Ok(_) => {
                                this.project_access.remove(&id);
                            }
                            Err(err) => match err.downcast_ref::<RepoAccessError>() {
                                Some(access) => {
                                    let issue = ProjectAccessIssue {
                                        message: access.to_string().into(),
                                        fix: this.access_fix(access),
                                    };
                                    this.project_access.insert(id, issue);
                                }
                                // Offline or rate limited: the docket shows
                                // that error itself.
                                None => eprintln!("failed to check project access: {err:#}"),
                            },
                        }
                        cx.notify();
                    })
                    .ok();
                })
            })
            .collect();
    }

    fn app_authorization_url(&self) -> String {
        format!(
            "{GITHUB_WEB_URL}/settings/connections/applications/{}",
            self.config.client_id
        )
    }
}

impl Tab {
    // --- Setup ----------------------------------------------------------

    /// Asks for a folder, then starts connecting it as a new project.
    pub fn add_project(&mut self, cx: &mut Context<Self>) {
        self.prompt_for_folder(None, cx);
    }

    /// Reopens setup for an existing project. If its folder has gone
    /// missing, setup says so, to choose where it is now or remove it.
    pub fn edit_project(&mut self, id: ProjectId, cx: &mut Context<Self>) {
        let Some(project) = self.app.read(cx).project(id) else {
            return;
        };
        if project.path_missing {
            // The settings rather than a folder picker, so the project can
            // be removed instead of located.
            let mut setup = ProjectSetup::new(project.path.clone(), Some(id));
            setup.automated_authors = project.automated_authors.clone();
            setup.merge = project.merge.clone();
            setup.scanning = false;
            setup.error = Some(
                "The folder isn't there anymore. Choose where the checkout is now, or remove \
                 the project."
                    .into(),
            );
            self.setup = Some(setup);
            cx.notify();
        } else {
            let merge = project.merge.clone();
            self.start_setup(project.path.clone(), Some(id), cx);
            if let Some(setup) = &mut self.setup {
                setup.merge = merge;
            }
        }
    }

    /// Picks a different folder for the project being set up.
    pub fn change_setup_folder(&mut self, cx: &mut Context<Self>) {
        let editing = self.setup.as_ref().and_then(|s| s.editing);
        self.prompt_for_folder(editing, cx);
    }

    fn prompt_for_folder(&mut self, editing: Option<ProjectId>, cx: &mut Context<Self>) {
        // Connecting checks the repository on GitHub, so it needs a client.
        if self.app.read(cx).client().is_none() {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        self._folder_prompt_task = Some(cx.spawn(async move |this, cx| {
            let path = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(err)) => {
                    eprintln!("failed to open folder picker: {err:#}");
                    None
                }
                // Dismissed.
                Ok(Ok(None)) | Err(_) => None,
            };
            if let Some(path) = path {
                this.update(cx, |this, cx| this.start_setup(path, editing, cx))
                    .ok();
            }
        }));
    }

    fn start_setup(&mut self, path: PathBuf, editing: Option<ProjectId>, cx: &mut Context<Self>) {
        let mut setup = ProjectSetup::new(path.clone(), editing);
        // Choosing another folder keeps what was typed; otherwise it's the
        // project's.
        setup.automated_authors = match &self.setup {
            Some(previous) if previous.editing == editing => previous.automated_authors.clone(),
            _ => editing
                .and_then(|id| self.app.read(cx).project(id))
                .map(|p| p.automated_authors.clone())
                .unwrap_or_default(),
        };
        setup._tasks.push(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { Checkout::scan(&path) })
                .await;
            this.update(cx, |this, cx| this.finish_scan(result, cx))
                .ok();
        }));
        self.setup = Some(setup);
        cx.notify();
    }

    fn finish_scan(&mut self, result: Result<Checkout>, cx: &mut Context<Self>) {
        let app = self.app.read(cx);
        let client = app.client();
        let Some(setup) = &mut self.setup else {
            return;
        };
        setup.scanning = false;
        let checkout = match result {
            Ok(checkout) => checkout,
            Err(err) => {
                setup.error = Some(format!("{err:#}").into());
                cx.notify();
                return;
            }
        };

        // Choosing a folder that's already a project edits that project.
        if setup.editing.is_none()
            && let Some(project) = app.projects.iter().find(|p| p.path == checkout.root)
        {
            setup.editing = Some(project.id);
            if setup.automated_authors.is_empty() {
                setup.automated_authors = project.automated_authors.clone();
            }
            if setup.merge == MergeSettings::default() {
                setup.merge = project.merge.clone();
            }
        }
        let current = setup
            .editing
            .and_then(|id| app.project(id))
            .map(|p| p.repository.clone());

        for remote in checkout.remotes {
            if !setup.candidates.iter().any(|c| c.repo == remote.repository) {
                setup.candidates.push(RepoCandidate {
                    repo: remote.repository,
                    source: CandidateSource::Remote(remote.name),
                    check: RepoCheck::Checking,
                });
            }
        }
        if let Some(current) = current {
            match setup.candidates.iter().position(|c| c.repo == current) {
                Some(ix) => setup.selected = Some(ix),
                None => {
                    setup.selected = Some(setup.candidates.len());
                    setup.candidates.push(RepoCandidate {
                        repo: current,
                        source: CandidateSource::Current,
                        check: RepoCheck::Checking,
                    });
                }
            }
            setup.user_selected = true;
        }
        setup.root = Some(checkout.root);
        setup.auto_select();

        if let Some(client) = client {
            for candidate in &setup.candidates {
                let task = Self::check_repository(client.clone(), candidate.repo.clone(), cx);
                setup._tasks.push(task);
            }
        }
        cx.notify();
    }

    fn check_repository(client: GithubClient, repo: RepoRef, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let repo = repo.clone();
                    async move { client.repository(&repo) }
                })
                .await;
            this.update(cx, |this, cx| {
                this.finish_repository_check(repo, result, cx)
            })
            .ok();
        })
    }

    fn finish_repository_check(
        &mut self,
        repo: RepoRef,
        result: Result<Repository>,
        cx: &mut Context<Self>,
    ) {
        let app = self.app.read(cx);
        let client = app.client();
        let fix = result
            .as_ref()
            .err()
            .and_then(|err| err.downcast_ref::<RepoAccessError>())
            .and_then(|err| app.access_fix(err));
        let Some(setup) = &mut self.setup else {
            return;
        };
        let Some(candidate) = setup.candidates.iter_mut().find(|c| c.repo == repo) else {
            return;
        };
        match result {
            Ok(info) => {
                let parent = info.parent.clone();
                candidate.check = RepoCheck::Ok(info);
                // PRs against a fork usually target its parent; offer it.
                if let Some(parent) = parent
                    && !setup.candidates.iter().any(|c| c.repo == parent)
                {
                    setup.candidates.push(RepoCandidate {
                        repo: parent.clone(),
                        source: CandidateSource::ForkParent(repo),
                        check: RepoCheck::Checking,
                    });
                    if let Some(client) = client {
                        let task = Self::check_repository(client, parent, cx);
                        setup._tasks.push(task);
                    }
                }
            }
            Err(err) => {
                candidate.check = RepoCheck::Failed {
                    message: format!("{err:#}").into(),
                    fix,
                }
            }
        }
        setup.auto_select();
        cx.notify();
    }

    /// Re-runs failed checks, e.g. after the user fixed access on GitHub.
    pub fn recheck_repositories(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.app.read(cx).client() else {
            return;
        };
        let Some(setup) = &mut self.setup else {
            return;
        };
        for candidate in &mut setup.candidates {
            if matches!(candidate.check, RepoCheck::Failed { .. }) {
                candidate.check = RepoCheck::Checking;
                let task = Self::check_repository(client.clone(), candidate.repo.clone(), cx);
                setup._tasks.push(task);
            }
        }
        setup.auto_select();
        cx.notify();
    }

    pub fn select_candidate(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else {
            return;
        };
        if setup
            .candidates
            .get(ix)
            .is_some_and(|c| !matches!(c.check, RepoCheck::Failed { .. }))
        {
            setup.selected = Some(ix);
            setup.user_selected = true;
            cx.notify();
        }
    }

    /// Chooses whether the project's pull requests merge by comment.
    pub fn set_merge_by_comment(&mut self, by_comment: bool, cx: &mut Context<Self>) {
        if let Some(setup) = &mut self.setup
            && setup.merge.by_comment != by_comment
        {
            setup.merge.by_comment = by_comment;
            cx.notify();
        }
    }

    /// Sets the comments to merge with from what's typed: separated by
    /// commas.
    pub fn set_merge_comments(&mut self, text: &str, cx: &mut Context<Self>) {
        let comments = MergeSettings::parse_comments(text);
        if let Some(setup) = &mut self.setup
            && setup.merge.comments != comments
        {
            setup.merge.comments = comments;
            cx.notify();
        }
    }

    /// Sets the automated accounts from what's typed: logins separated by
    /// commas.
    pub fn set_automated_authors(&mut self, text: &str, cx: &mut Context<Self>) {
        let authors = parse_logins(text);
        if let Some(setup) = &mut self.setup
            && setup.automated_authors != authors
        {
            setup.automated_authors = authors;
            cx.notify();
        }
    }

    pub fn cancel_setup(&mut self, cx: &mut Context<Self>) {
        if self.setup.take().is_some() {
            cx.notify();
        }
    }

    /// Saves the setup as a project (or updates the one being edited) and
    /// selects it.
    pub fn confirm_setup(&mut self, cx: &mut Context<Self>) {
        let Some(setup) = &self.setup else {
            return;
        };
        let (Some(root), Some(repository)) = (&setup.root, setup.confirmed_repository()) else {
            return;
        };
        if !setup.merge_is_valid() {
            return;
        }
        // GitHub's canonical name, which follows renames and transfers.
        let (editing, root, repository) = (setup.editing, root.clone(), repository.repo.clone());
        let automated_authors = setup.automated_authors.clone();
        let merge = setup.merge.clone();
        self.setup = None;
        // Saving tells every tab, which reloads what it shows of the project.
        let id = self.app.update(cx, |app, cx| {
            let id = app.save_project(editing, root, repository, automated_authors, merge, cx);
            app.remember_project(Some(id), cx);
            id
        });
        self.selected_project = Some(id);
        cx.notify();
    }
}

impl AppState {
    /// Saves a project connected to `repository` at `root`, updating
    /// `editing` if given. Returns its ID.
    fn save_project(
        &mut self,
        editing: Option<ProjectId>,
        root: PathBuf,
        repository: RepoRef,
        automated_authors: Vec<String>,
        merge: MergeSettings,
        cx: &mut Context<Self>,
    ) -> ProjectId {
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let editing = editing.and_then(|id| self.projects.iter_mut().find(|p| p.id == id));
        let id = match editing {
            Some(project) => {
                if project.path != root {
                    project.name = name;
                    project.path = root;
                }
                project.repository = repository;
                project.automated_authors = automated_authors;
                project.merge = merge;
                project.path_missing = false;
                project.id
            }
            None => {
                let id = ProjectId(self.projects.iter().map(|p| p.id.0 + 1).max().unwrap_or(1));
                self.projects.push(Project {
                    id,
                    name,
                    path: root,
                    repository,
                    automated_authors,
                    merge,
                    path_missing: false,
                });
                id
            }
        };
        // Setup only confirms repositories GitHub shows us.
        self.project_access.remove(&id);
        self.save_projects(cx);
        self.load_branches(cx);
        cx.emit(AppEvent::ProjectSaved(id));
        cx.notify();
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(s: &str) -> RepoRef {
        s.parse().unwrap()
    }

    fn checked(name: &str, parent: Option<&str>) -> RepoCheck {
        RepoCheck::Ok(Repository {
            repo: repo(name),
            is_private: false,
            viewer_permission: None,
            parent: parent.map(repo),
        })
    }

    fn candidate(name: &str, source: CandidateSource, check: RepoCheck) -> RepoCandidate {
        RepoCandidate {
            repo: repo(name),
            source,
            check,
        }
    }

    fn selected(setup: &ProjectSetup) -> Option<String> {
        Some(setup.candidates[setup.selected?].repo.to_string())
    }

    #[test]
    fn prefers_upstream_and_fork_parents_over_origin() {
        let mut setup = ProjectSetup::new(PathBuf::from("/repo"), None);
        setup.candidates = vec![
            candidate(
                "me/zed",
                CandidateSource::Remote("origin".into()),
                RepoCheck::Checking,
            ),
            candidate(
                "zed/zed",
                CandidateSource::Remote("upstream".into()),
                RepoCheck::Checking,
            ),
        ];
        setup.auto_select();
        assert_eq!(selected(&setup).as_deref(), Some("zed/zed"));

        // Only a fork remote: once its parent is found, prefer the parent.
        setup.candidates = vec![candidate(
            "me/zed",
            CandidateSource::Remote("origin".into()),
            checked("me/zed", Some("zed/zed")),
        )];
        setup.auto_select();
        assert_eq!(selected(&setup).as_deref(), Some("me/zed"));
        setup.candidates.push(candidate(
            "zed/zed",
            CandidateSource::ForkParent(repo("me/zed")),
            RepoCheck::Checking,
        ));
        setup.auto_select();
        assert_eq!(selected(&setup).as_deref(), Some("zed/zed"));
    }

    #[test]
    fn skips_failed_candidates_and_respects_user_choice() {
        let mut setup = ProjectSetup::new(PathBuf::from("/repo"), None);
        setup.candidates = vec![
            candidate(
                "gone/zed",
                CandidateSource::Remote("upstream".into()),
                RepoCheck::Failed {
                    message: "not found".into(),
                    fix: None,
                },
            ),
            candidate(
                "me/zed",
                CandidateSource::Remote("origin".into()),
                checked("me/zed", None),
            ),
            candidate(
                "other/zed",
                CandidateSource::Remote("other".into()),
                checked("other/zed", None),
            ),
        ];
        setup.auto_select();
        assert_eq!(selected(&setup).as_deref(), Some("me/zed"));

        setup.selected = Some(2);
        setup.user_selected = true;
        setup.auto_select();
        assert_eq!(selected(&setup).as_deref(), Some("other/zed"));
        assert!(setup.confirmed_repository().is_some());
    }
}
