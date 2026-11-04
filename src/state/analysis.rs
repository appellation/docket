//! The open diff's changes, grouped by Claude (`ai::analysis`): the
//! connection to Anthropic every window shares, and each diff's analysis.
//!
//! A diff is analyzed once it loads, while grouping is on and Anthropic is
//! connected, so like the diff itself it's usually ready by the time the
//! Files tab opens. Analyses are kept by what went into them, in memory
//! and on disk (`ai::cache`), so one is asked for again only when the diff
//! changes: a new head, base, or (for a branch) commit.

use std::sync::Arc;

use anyhow::Result;
use gpui::{Context, SharedString};

use super::{AppEvent, AppState, Tab};
use crate::ai::{
    self, AnalysisRequest, AnthropicClient, AnthropicCredential, AnthropicSource, AnthropicStore,
    ApiError, DiffAnalysis,
};
use crate::auth::AccessToken;

pub enum AnthropicStatus {
    /// Checking the keychain for a saved connection.
    Restoring,
    /// `error` is why the last try to connect failed.
    Disconnected {
        error: Option<SharedString>,
    },
    /// Checking a credential against Anthropic.
    Connecting(AnthropicSource),
    Connected(AnthropicClient),
}

pub enum AnalysisLoad {
    /// Not asked for: grouping is off, Anthropic isn't connected, or the
    /// diff hasn't loaded.
    Idle,
    Running,
    Ready(Arc<DiffAnalysis>),
    Failed(SharedString),
}

impl AppState {
    pub fn analysis_client(&self) -> Option<&AnthropicClient> {
        match &self.anthropic {
            AnthropicStatus::Connected(client) => Some(client),
            _ => None,
        }
    }

    /// Whether diffs show grouped by change: grouping is on, and there's
    /// a connection to group them with.
    pub fn groups_changes(&self) -> bool {
        self.group_changes && self.analysis_client().is_some()
    }

    pub fn set_group_changes(&mut self, group: bool, cx: &mut Context<Self>) {
        self.group_changes = group;
        cx.emit(AppEvent::AnalysisEnabled);
        cx.notify();
    }

    pub(super) fn restore_anthropic(&mut self, cx: &mut Context<Self>) {
        self._anthropic_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { AnthropicStore::load() })
                .await;
            this.update(cx, |this, cx| {
                this.anthropic = match result {
                    Ok(Some(credential)) => {
                        AnthropicStatus::Connected(AnthropicClient::new(credential))
                    }
                    Ok(None) => AnthropicStatus::Disconnected { error: None },
                    Err(err) => {
                        eprintln!("failed to restore the Anthropic connection: {err:#}");
                        AnthropicStatus::Disconnected { error: None }
                    }
                };
                cx.emit(AppEvent::AnalysisEnabled);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Connects with an API key, saved once Anthropic accepts it.
    pub fn connect_anthropic_key(&mut self, key: String, cx: &mut Context<Self>) {
        let key = key.trim();
        if key.is_empty() {
            self.anthropic = AnthropicStatus::Disconnected {
                error: Some("Paste an API key first.".into()),
            };
            cx.notify();
            return;
        }
        let credential = AnthropicCredential::ApiKey(AccessToken::new(key));
        self.connect_anthropic(
            AnthropicClient::new(credential.clone()),
            Some(credential),
            cx,
        );
    }

    /// Connects with the Anthropic CLI's login.
    pub fn connect_anthropic_cli(&mut self, cx: &mut Context<Self>) {
        let credential = AnthropicCredential::Cli;
        self.connect_anthropic(
            AnthropicClient::new(credential.clone()),
            Some(credential),
            cx,
        );
    }

    /// Connected with `client`, as if the user had connected.
    #[cfg(test)]
    pub fn connect_anthropic_for_test(&mut self, client: AnthropicClient, cx: &mut Context<Self>) {
        self.connect_anthropic(client, None, cx);
    }

    /// Checks `client` works, then saves `credential` and connects.
    fn connect_anthropic(
        &mut self,
        client: AnthropicClient,
        credential: Option<AnthropicCredential>,
        cx: &mut Context<Self>,
    ) {
        self.anthropic = AnthropicStatus::Connecting(client.source());
        cx.notify();
        self._anthropic_task = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let result = executor
                .spawn({
                    let client = client.clone();
                    async move {
                        client.verify()?;
                        if let Some(credential) = credential
                            && let Err(err) = AnthropicStore::save(&credential)
                        {
                            // It works for now; only the next launch is
                            // affected.
                            eprintln!("failed to save the Anthropic connection: {err:#}");
                        }
                        anyhow::Ok(())
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.anthropic = AnthropicStatus::Connected(client);
                        cx.emit(AppEvent::AnalysisEnabled);
                    }
                    Err(err) => {
                        this.anthropic = AnthropicStatus::Disconnected {
                            error: Some(describe_connect_error(&err)),
                        };
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn disconnect_anthropic(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = AnthropicStore::clear() {
            eprintln!("failed to clear the Anthropic connection: {err:#}");
        }
        self._anthropic_task = None;
        self.anthropic = AnthropicStatus::Disconnected { error: None };
        cx.notify();
    }
}

fn describe_connect_error(err: &anyhow::Error) -> SharedString {
    match err.downcast_ref::<ApiError>() {
        Some(ApiError { status: 401, .. }) => "Anthropic didn't accept that key. Check that it \
            was copied in full and hasn't been revoked."
            .into(),
        Some(ApiError { status: 404, .. }) => {
            "This account can't use Claude Haiku 5.5, which Docket groups changes with.".into()
        }
        _ => format!("{err:#}").into(),
    }
}

impl Tab {
    /// Analyzes the open diff, if it's loaded and hasn't been, and grouping
    /// is on. From the cache when it's been analyzed before.
    pub(super) fn analyze_diff(&mut self, cx: &mut Context<Self>) {
        let (revision, request) = {
            let app = self.app.read(cx);
            let Some(files) = self.diff_files() else {
                return;
            };
            if !app.groups_changes() || !matches!(files.analysis, AnalysisLoad::Idle) {
                return;
            }
            let Some(request) = files.analysis_request.clone() else {
                return;
            };
            (files.revision, request)
        };
        let request = match request {
            Ok(request) => request,
            Err(err) => {
                self.set_analysis(revision, AnalysisLoad::Failed(err), cx);
                return;
            }
        };
        let app = self.app.read(cx);
        let cached = app.analyses.get(&request.key).cloned();
        let Some(client) = app.analysis_client().cloned() else {
            return;
        };
        if let Some(analysis) = cached {
            self.set_analysis(revision, AnalysisLoad::Ready(analysis), cx);
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let request = request.clone();
                    async move { analyze(&request, &client) }
                })
                .await;
            this.update(cx, |this, cx| {
                let analysis = match result {
                    Ok(analysis) => {
                        let analysis = Arc::new(analysis);
                        this.app.update(cx, |app, _| {
                            app.analyses.insert(request.key.clone(), analysis.clone());
                        });
                        AnalysisLoad::Ready(analysis)
                    }
                    Err(err) => AnalysisLoad::Failed(format!("{err:#}").into()),
                };
                this.set_analysis(revision, analysis, cx);
            })
            .ok();
        });
        if let Some(files) = self.diff_files_at(revision) {
            files.analysis = AnalysisLoad::Running;
            files._analysis_task = Some(task);
        }
        cx.notify();
    }

    /// Asks again after an analysis failed.
    pub fn retry_analysis(&mut self, cx: &mut Context<Self>) {
        if let Some(files) = self.diff_files_mut()
            && matches!(files.analysis, AnalysisLoad::Failed(_))
        {
            files.analysis = AnalysisLoad::Idle;
        }
        self.analyze_diff(cx);
    }

    fn set_analysis(&mut self, revision: u64, analysis: AnalysisLoad, cx: &mut Context<Self>) {
        if let Some(files) = self.diff_files_at(revision) {
            files.analysis = analysis;
            files._analysis_task = None;
            cx.notify();
        }
    }
}

/// The saved analysis, or Claude's, saved for next time. Blocking.
fn analyze(request: &AnalysisRequest, client: &AnthropicClient) -> Result<DiffAnalysis> {
    if let Some(analysis) = ai::cache::load(&request.key) {
        return Ok(analysis);
    }
    let analysis = request.run(client)?;
    if let Err(err) = ai::cache::save(&request.key, &analysis) {
        eprintln!("failed to cache the analysis: {err:#}");
    }
    Ok(analysis)
}
