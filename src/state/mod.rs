//! Application state.
//!
//! `AppState` is what every window shares: the sign-in, the PR searches,
//! and the projects and their branches. It's a GPUI entity registered as a global so actions
//! (menus, keybindings) can reach it without a window. Each window (a
//! native tab, on macOS) has its own [`Tab`] for what it shows: the
//! selected project, and the open pull request or branch.
//! Tabs observe `AppState` and follow its [`AppEvent`]s.

mod branch;
mod browse;
mod code_intel;
mod docket;
mod live;
mod pr_detail;
mod projects;
mod review;
mod tab;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Error, Result, bail};
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, Global, SharedString, Task,
    WeakEntity,
};

use crate::auth::{
    AccessToken, Credential, CredentialSource, DeviceCode, DeviceFlow, PollOutcome, TokenStore,
    github_cli,
};
use crate::config::Config;
use crate::github::{GithubClient, PullRequest, User};
use crate::projects::{Branches, Project, ProjectId};

pub use branch::{BranchDiff, BranchTab, CommitsLoad};
pub use browse::{BrowserKey, BrowserScope, DirLoad, FileLoad, OpenFile, ProjectBrowser};
pub use code_intel::{CodeIntel, IntelOwner, LspTarget, ServerState};
pub use docket::DocketItem;
#[cfg(test)]
pub use live::{DOCKET_INTERVAL, PULL_REQUEST_INTERVAL};
pub use pr_detail::{LoadedDetail, PrDetail, PrTab};
pub use projects::{AccessFix, CandidateSource, ProjectAccessIssue, ProjectSetup, RepoCheck};
pub use review::{DiffFiles, DiffLoad};
pub use tab::Tab;

pub enum AuthStatus {
    /// Checking the keychain for a saved sign-in.
    Restoring,
    SignedOut,
    RequestingCode,
    /// Waiting for the user to enter `code.user_code` on github.com.
    AwaitingUser(DeviceCode),
    /// Showing the personal access token form; `error` is from the last try.
    EnteringToken {
        error: Option<SharedString>,
    },
    /// Checking a token from the user or the GitHub CLI against GitHub.
    Verifying,
    SignedIn {
        user: User,
        client: GithubClient,
        source: CredentialSource,
    },
    Failed(SharedString),
}

/// The searches the docket's pull requests come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionKind {
    ReviewRequested,
    Authored,
    Involved,
}

impl SectionKind {
    pub const ALL: [SectionKind; 3] = [Self::ReviewRequested, Self::Authored, Self::Involved];

    pub(crate) fn search_query(self) -> &'static str {
        match self {
            // `user-review-requested` matches only direct requests, not ones
            // made through a team the user belongs to.
            Self::ReviewRequested => "is:open is:pr archived:false user-review-requested:@me",
            Self::Authored => "is:open is:pr archived:false author:@me",
            Self::Involved => "is:open is:pr archived:false involves:@me -author:@me",
        }
    }
}

#[derive(Default)]
pub struct Section {
    pub pull_requests: Vec<PullRequest>,
    pub loading: bool,
    pub error: Option<SharedString>,
}

pub struct AppState {
    config: Config,
    pub auth: AuthStatus,
    sections: [Section; SectionKind::ALL.len()],
    pub projects: Vec<Project>,
    /// The project a tab last scoped its docket to, saved so the next
    /// launch starts there; `None` is everything.
    pub last_project: Option<ProjectId>,
    /// Each project's default branch, and those with your work on them.
    branches: HashMap<ProjectId, Branches>,
    /// Default branches being pulled from their remote, by project.
    pulls: HashMap<ProjectId, Task<()>>,
    /// Why a project's default branch last failed to pull.
    pull_errors: HashMap<ProjectId, SharedString>,
    /// Projects whose pull requests GitHub hides from this sign-in.
    project_access: HashMap<ProjectId, ProjectAccessIssue>,
    /// When the docket last started loading, so polling (`live.rs`) knows
    /// when it's due again.
    docket_loaded_at: Option<Instant>,
    _auth_task: Option<Task<()>>,
    _refresh_tasks: Vec<Task<()>>,
    _watch_task: Option<Task<()>>,
    _branch_tasks: Vec<Task<()>>,
    _load_projects_task: Option<Task<()>>,
    _access_tasks: Vec<Task<()>>,
    _save_projects_task: Option<Task<()>>,
}

/// Changes to the shared state that tabs act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppEvent {
    /// A search loaded, so an open pull request's head or base may have
    /// moved.
    PullRequestsLoaded,
    /// The saved projects were read at launch.
    ProjectsLoaded,
    /// A project was added, or edited (its folder or repository).
    ProjectSaved(ProjectId),
    /// A project's branches were read again, so an open branch's tip or
    /// base may have moved.
    BranchesLoaded(ProjectId),
    ProjectRemoved(ProjectId),
    SignedIn,
    SignedOut,
}

impl EventEmitter<AppEvent> for AppState {}

struct GlobalAppState(Entity<AppState>);

impl Global for GlobalAppState {}

impl AppState {
    pub fn init(config: Config, cx: &mut App) -> Entity<Self> {
        let state = cx.new(|cx| {
            let mut this = Self::new(config);
            this.restore_session(cx);
            this.load_projects(cx);
            this
        });
        cx.set_global(GlobalAppState(state.clone()));
        state
    }

    /// Signed in with `client` and showing `projects`, rather than restoring
    /// the saved sign-in and projects.
    #[cfg(test)]
    pub fn init_for_test(
        client: GithubClient,
        projects: Vec<Project>,
        cx: &mut App,
    ) -> Entity<Self> {
        let state = cx.new(|cx| {
            let mut this = Self::new(Config::load());
            this.auth = AuthStatus::SignedIn {
                user: client.viewer().expect("a test client knows its viewer"),
                client,
                source: CredentialSource::OAuth,
            };
            this.projects = projects;
            this.refresh(cx);
            this.watch_docket(cx);
            this
        });
        cx.set_global(GlobalAppState(state.clone()));
        state
    }

    fn new(config: Config) -> Self {
        Self {
            config,
            auth: AuthStatus::Restoring,
            sections: Default::default(),
            projects: Vec::new(),
            last_project: None,
            branches: HashMap::new(),
            pulls: HashMap::new(),
            pull_errors: HashMap::new(),
            project_access: HashMap::new(),
            docket_loaded_at: None,
            _auth_task: None,
            _refresh_tasks: Vec::new(),
            _watch_task: None,
            _branch_tasks: Vec::new(),
            _load_projects_task: None,
            _access_tasks: Vec::new(),
            _save_projects_task: None,
        }
    }

    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAppState>().0.clone()
    }

    pub fn section(&self, kind: SectionKind) -> &Section {
        &self.sections[kind as usize]
    }

    fn section_mut(&mut self, kind: SectionKind) -> &mut Section {
        &mut self.sections[kind as usize]
    }

    fn client(&self) -> Option<GithubClient> {
        match &self.auth {
            AuthStatus::SignedIn { client, .. } => Some(client.clone()),
            _ => None,
        }
    }

    // --- Authentication -------------------------------------------------

    fn restore_session(&mut self, cx: &mut Context<Self>) {
        self._auth_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let executor = cx.background_executor().clone();
                let Some(credential) = executor.spawn(async { TokenStore::load() }).await? else {
                    return Ok(());
                };
                let source = credential.source();
                let token = match credential {
                    Credential::OAuth(token) | Credential::PersonalAccessToken(token) => token,
                    Credential::GithubCli => executor.spawn(async { github_cli::token() }).await?,
                };
                Self::complete_sign_in(token, source, &this, cx).await
            }
            .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) if matches!(this.auth, AuthStatus::Restoring) => {
                        this.auth = AuthStatus::SignedOut;
                    }
                    Ok(()) => {}
                    // A stale or revoked token, `gh` logged out, or offline:
                    // say why, and offer every way to sign in again.
                    Err(err) => {
                        eprintln!("failed to restore session: {err:#}");
                        this.auth = AuthStatus::Failed(
                            format!("Couldn't restore your session: {err:#}").into(),
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn start_sign_in(&mut self, cx: &mut Context<Self>) {
        let flow = DeviceFlow::new(&self.config);
        self.auth = AuthStatus::RequestingCode;
        cx.notify();

        self._auth_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = Self::run_device_flow(flow, &this, cx).await {
                this.update(cx, |this, cx| {
                    this.auth = AuthStatus::Failed(format!("{err:#}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    async fn run_device_flow(
        flow: DeviceFlow,
        this: &WeakEntity<Self>,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        let executor = cx.background_executor().clone();
        let code = executor
            .spawn({
                let flow = flow.clone();
                async move { flow.request_code() }
            })
            .await?;

        this.update(cx, |this, cx| {
            this.auth = AuthStatus::AwaitingUser(code.clone());
            cx.notify();
        })?;

        let deadline = Instant::now() + Duration::from_secs(code.expires_in);
        let mut interval = Duration::from_secs(code.interval);
        let token = loop {
            executor.timer(interval).await;
            if Instant::now() >= deadline {
                bail!("the sign-in code expired; please try again");
            }
            let outcome = executor
                .spawn({
                    let (flow, code) = (flow.clone(), code.clone());
                    async move { flow.poll(&code) }
                })
                .await?;
            match outcome {
                PollOutcome::Authorized(token) => break token,
                PollOutcome::Pending { interval: next } => interval = next,
            }
        };

        executor
            .spawn({
                let credential = Credential::OAuth(token.clone());
                async move { TokenStore::save(&credential) }
            })
            .await?;
        Self::complete_sign_in(token, CredentialSource::OAuth, this, cx).await
    }

    /// Signs in as the user with the GitHub CLI's login. Works in orgs that
    /// have approved the GitHub CLI but not Docket.
    pub fn sign_in_with_github_cli(&mut self, cx: &mut Context<Self>) {
        self.auth = AuthStatus::Verifying;
        cx.notify();

        self._auth_task = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let result = async {
                let token = executor.spawn(async { github_cli::token() }).await?;
                Self::complete_sign_in(token, CredentialSource::GithubCli, &this, cx).await
            }
            .await;
            match result {
                Ok(()) => Self::save_credential(Credential::GithubCli, cx).await,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.auth = AuthStatus::Failed(format!("{err:#}").into());
                        cx.notify();
                    })
                    .ok();
                }
            }
        }));
    }

    pub fn show_token_form(&mut self, cx: &mut Context<Self>) {
        self._auth_task = None;
        self.auth = AuthStatus::EnteringToken { error: None };
        cx.notify();
    }

    /// Signs in with a personal access token. It's only saved once GitHub
    /// accepts it.
    pub fn sign_in_with_token(&mut self, token: String, cx: &mut Context<Self>) {
        let token = token.trim();
        if token.is_empty() {
            self.auth = AuthStatus::EnteringToken {
                error: Some("Paste a token first.".into()),
            };
            cx.notify();
            return;
        }
        let token = AccessToken::new(token);
        self.auth = AuthStatus::Verifying;
        cx.notify();

        self._auth_task = Some(cx.spawn(async move |this, cx| {
            let result = Self::complete_sign_in(
                token.clone(),
                CredentialSource::PersonalAccessToken,
                &this,
                cx,
            )
            .await;
            match result {
                Ok(()) => Self::save_credential(Credential::PersonalAccessToken(token), cx).await,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.auth = AuthStatus::EnteringToken {
                            error: Some(describe_token_error(&err)),
                        };
                        cx.notify();
                    })
                    .ok();
                }
            }
        }));
    }

    /// Returns to the choice of sign-in methods, abandoning one in progress.
    pub fn show_sign_in_options(&mut self, cx: &mut Context<Self>) {
        if matches!(self.auth, AuthStatus::SignedIn { .. }) {
            return;
        }
        self._auth_task = None;
        self.auth = AuthStatus::SignedOut;
        cx.notify();
    }

    pub fn is_signed_in(&self) -> bool {
        matches!(self.auth, AuthStatus::SignedIn { .. })
    }

    /// Escape while signing in: backs out of the method in progress.
    pub fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.auth,
            AuthStatus::RequestingCode
                | AuthStatus::AwaitingUser(_)
                | AuthStatus::EnteringToken { .. }
        ) {
            self.show_sign_in_options(cx);
        }
    }

    /// Persists a sign-in that already succeeded. A failure only costs
    /// signing in again next launch, so it's logged rather than shown.
    async fn save_credential(credential: Credential, cx: &mut AsyncApp) {
        let result = cx
            .background_executor()
            .spawn(async move { TokenStore::save(&credential) })
            .await;
        if let Err(err) = result {
            eprintln!("failed to save sign-in: {err:#}");
        }
    }

    async fn complete_sign_in(
        token: AccessToken,
        source: CredentialSource,
        this: &WeakEntity<Self>,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        let client = GithubClient::new(token);
        let user = cx
            .background_executor()
            .spawn({
                let client = client.clone();
                async move { client.viewer() }
            })
            .await?;
        this.update(cx, |this, cx| {
            this.auth = AuthStatus::SignedIn {
                user,
                client,
                source,
            };
            this.refresh(cx);
            this.watch_docket(cx);
            cx.emit(AppEvent::SignedIn);
            cx.notify();
        })
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = TokenStore::clear() {
            eprintln!("failed to clear saved token: {err:#}");
        }
        self.auth = AuthStatus::SignedOut;
        self.sections = Default::default();
        // Projects are local and outlive the session; tabs close what
        // needs the client.
        self.project_access.clear();
        self._access_tasks.clear();
        self._auth_task = None;
        self._refresh_tasks.clear();
        self._watch_task = None;
        self.docket_loaded_at = None;
        cx.emit(AppEvent::SignedOut);
        cx.notify();
    }

    // --- Data -----------------------------------------------------------

    /// Loads the docket again and checks each project's access again. Tabs
    /// refresh what they show themselves.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.client().is_none() {
            return;
        }
        self.check_project_access(cx);
        self.load_docket(false, cx);
    }

    /// Re-fetches every section in parallel, and re-reads the projects'
    /// branches. A `quiet` load, from polling, doesn't show it's loading.
    fn load_docket(&mut self, quiet: bool, cx: &mut Context<Self>) {
        let Some(client) = self.client() else {
            return;
        };
        self.docket_loaded_at = Some(cx.background_executor().now());
        self.load_branches(cx);
        self._refresh_tasks = SectionKind::ALL
            .into_iter()
            .map(|kind| {
                if !quiet {
                    self.section_mut(kind).loading = true;
                }
                let client = client.clone();
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { client.search_pull_requests(kind.search_query()) })
                        .await;
                    this.update(cx, |this, cx| {
                        let section = this.section_mut(kind);
                        section.loading = false;
                        match result {
                            Ok(prs) => {
                                section.pull_requests = prs;
                                section.error = None;
                            }
                            Err(err) => section.error = Some(format!("{err:#}").into()),
                        }
                        cx.emit(AppEvent::PullRequestsLoaded);
                        cx.notify();
                    })
                    .ok();
                })
            })
            .collect();
        cx.notify();
    }
}

fn describe_token_error(err: &Error) -> SharedString {
    match err.downcast_ref::<ureq::Error>() {
        Some(ureq::Error::StatusCode(401)) => "GitHub didn't accept that token. Check that it was \
            copied in full and hasn't expired or been revoked."
            .into(),
        _ => format!("{err:#}").into(),
    }
}
