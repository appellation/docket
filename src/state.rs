//! Application state shared across views.
//!
//! `AppState` is a GPUI entity registered as a global so actions (menus,
//! keybindings) can reach it without a focused view. Views observe it and
//! re-render on `cx.notify()`.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, Global, SharedString, Task, WeakEntity,
};

use crate::auth::{AccessToken, DeviceCode, DeviceFlow, PollOutcome, TokenStore};
use crate::config::Config;
use crate::github::{GithubClient, PullRequest, User};

pub enum AuthStatus {
    /// Checking the keychain for a saved token.
    Restoring,
    SignedOut,
    RequestingCode,
    /// Waiting for the user to enter `code.user_code` on github.com.
    AwaitingUser(DeviceCode),
    SignedIn {
        user: User,
        client: GithubClient,
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
            Self::ReviewRequested => "is:open is:pr archived:false review-requested:@me",
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
    _auth_task: Option<Task<()>>,
    _refresh_tasks: Vec<Task<()>>,
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
                _auth_task: None,
                _refresh_tasks: Vec::new(),
            };
            this.restore_session(cx);
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
                let token = cx
                    .background_executor()
                    .spawn(async { TokenStore::load() })
                    .await?;
                match token {
                    Some(token) => Self::complete_sign_in(token, &this, cx).await,
                    None => Ok(()),
                }
            }
            .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) if matches!(this.auth, AuthStatus::Restoring) => {
                        this.auth = AuthStatus::SignedOut;
                    }
                    Ok(()) => {}
                    // A stale or revoked token just means the user signs in again.
                    Err(err) => {
                        eprintln!("failed to restore session: {err:#}");
                        this.auth = AuthStatus::SignedOut;
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
                let token = token.clone();
                async move { TokenStore::save(&token) }
            })
            .await?;
        Self::complete_sign_in(token, this, cx).await
    }

    async fn complete_sign_in(
        token: AccessToken,
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
            this.auth = AuthStatus::SignedIn { user, client };
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
        self._auth_task = None;
        self._refresh_tasks.clear();
        cx.notify();
    }

    // --- Data -----------------------------------------------------------

    /// Re-fetches every section in parallel.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client() else {
            return;
        };
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
