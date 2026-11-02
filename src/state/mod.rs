//! Application state shared across views.
//!
//! `AppState` is a GPUI entity registered as a global so actions (menus,
//! keybindings) can reach it without a focused view. Views observe it and
//! re-render on `cx.notify()`.

mod pr_detail;
mod projects;

use std::time::{Duration, Instant};

use anyhow::{Error, Result, bail};
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, Global, SharedString, Task, WeakEntity,
};

use crate::auth::{
    AccessToken, Credential, CredentialSource, DeviceCode, DeviceFlow, PollOutcome, TokenStore,
    github_cli,
};
use crate::config::Config;
use crate::github::{GithubClient, PullRequest, User};
use crate::projects::{Project, ProjectId};

pub use pr_detail::{LoadedDetail, PrDetail};
pub use projects::{AccessFix, CandidateSource, ProjectSetup, RepoCheck};

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

/// The PR inboxes shown in the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionKind {
    ReviewRequested,
    Authored,
    Involved,
}

impl SectionKind {
    pub const ALL: [SectionKind; 3] = [Self::ReviewRequested, Self::Authored, Self::Involved];

    pub fn label(self) -> &'static str {
        match self {
            Self::ReviewRequested => "Review requested",
            Self::Authored => "My pull requests",
            Self::Involved => "Involved",
        }
    }

    fn search_query(self) -> &'static str {
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
    pub selected: SectionKind,
    sections: [Section; SectionKind::ALL.len()],
    pub projects: Vec<Project>,
    /// Scopes the inboxes to one project; `None` shows everything.
    pub selected_project: Option<ProjectId>,
    /// Set while adding or editing a project; replaces the PR list.
    pub setup: Option<ProjectSetup>,
    /// The pull request opened from the list; replaces it until closed.
    pub detail: Option<PrDetail>,
    _auth_task: Option<Task<()>>,
    _refresh_tasks: Vec<Task<()>>,
    _load_projects_task: Option<Task<()>>,
    _save_projects_task: Option<Task<()>>,
    _folder_prompt_task: Option<Task<()>>,
}

struct GlobalAppState(Entity<AppState>);

impl Global for GlobalAppState {}

impl AppState {
    pub fn init(config: Config, cx: &mut App) -> Entity<Self> {
        let state = cx.new(|cx| {
            let mut this = Self {
                config,
                auth: AuthStatus::Restoring,
                selected: SectionKind::ReviewRequested,
                sections: Default::default(),
                projects: Vec::new(),
                selected_project: None,
                setup: None,
                detail: None,
                _auth_task: None,
                _refresh_tasks: Vec::new(),
                _load_projects_task: None,
                _save_projects_task: None,
                _folder_prompt_task: None,
            };
            this.restore_session(cx);
            this.load_projects(cx);
            this
        });
        cx.set_global(GlobalAppState(state.clone()));
        state
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

    pub fn select(&mut self, kind: SectionKind, cx: &mut Context<Self>) {
        self.selected = kind;
        self.setup = None;
        self.detail = None;
        cx.notify();
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

    /// Escape: backs out of whatever is in progress, or out of the open
    /// pull request.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        match self.auth {
            AuthStatus::SignedIn { .. } if self.setup.is_some() => self.cancel_setup(cx),
            AuthStatus::SignedIn { .. } => self.close_pull_request(cx),
            AuthStatus::RequestingCode
            | AuthStatus::AwaitingUser(_)
            | AuthStatus::EnteringToken { .. } => self.show_sign_in_options(cx),
            _ => {}
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
            cx.notify();
        })
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = TokenStore::clear() {
            eprintln!("failed to clear saved token: {err:#}");
        }
        self.auth = AuthStatus::SignedOut;
        self.sections = Default::default();
        // Projects are local and outlive the session; an in-progress setup
        // needs the client, so it doesn't.
        self.setup = None;
        self.detail = None;
        self._auth_task = None;
        self._refresh_tasks.clear();
        cx.notify();
    }

    // --- Data -----------------------------------------------------------

    /// Re-fetches every section in parallel, and the open pull request.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client() else {
            return;
        };
        self.refresh_pull_request(cx);
        self._refresh_tasks = SectionKind::ALL
            .into_iter()
            .map(|kind| {
                self.section_mut(kind).loading = true;
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
