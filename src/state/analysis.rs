//! The open diff's changes, grouped by Claude (`ai::analysis`): whether
//! Claude Code is connected, which every window shares, and each diff's
//! analysis.
//!
//! A diff is analyzed once it loads, while grouping is on and Claude Code
//! is connected, so like the diff itself it's usually ready by the time the
//! Files tab opens. Analyses are kept by what went into them, in memory
//! and on disk (`ai::cache`), so one is asked for again only when the diff
//! changes: a new head, base, or (for a branch) commit.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use gpui::{Context, SharedString};

use super::{AppEvent, AppState, Tab};
use crate::ai::{self, AnalysisRequest, ClaudeCode, ClaudeStore, DiffAnalysis, SavedConnection};

pub enum ClaudeStatus {
    /// Checking whether grouping with Claude Code was turned on.
    Restoring,
    /// `error` is why the last try to connect failed.
    Disconnected {
        error: Option<SharedString>,
    },
    /// Checking that Claude Code is installed and signed in.
    Connecting,
    Connected(ClaudeCode),
}

pub enum AnalysisLoad {
    /// Not asked for: grouping is off, Claude Code isn't connected, or the
    /// diff hasn't loaded.
    Idle,
    Running,
    Ready(Arc<DiffAnalysis>),
    Failed(SharedString),
}

/// Stops an analysis's Claude Code when the task waiting on it is dropped:
/// the diff changed, or its tab closed.
struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl AppState {
    pub fn claude_code(&self) -> Option<&ClaudeCode> {
        match &self.claude {
            ClaudeStatus::Connected(claude) => Some(claude),
            _ => None,
        }
    }

    /// Whether diffs show grouped by change: grouping is on, and Claude
    /// Code is connected to group them with.
    pub fn groups_changes(&self) -> bool {
        self.group_changes && self.claude_code().is_some()
    }

    pub fn set_group_changes(&mut self, group: bool, cx: &mut Context<Self>) {
        self.group_changes = group;
        cx.emit(AppEvent::AnalysisEnabled);
        cx.notify();
    }

    /// Connected if the user connected Claude Code before. A connection
    /// from before analysis ran in Claude Code (an Anthropic API key, or
    /// the Anthropic CLI's login) carries over if Claude Code works, and
    /// is forgotten either way, so no key is left in the keychain.
    pub(super) fn restore_claude(&mut self, cx: &mut Context<Self>) {
        self._claude_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async {
                    Ok(match ClaudeStore::load()? {
                        Some(SavedConnection::ClaudeCode) => true,
                        Some(SavedConnection::Legacy) => {
                            let works = ClaudeCode::new().verify().is_ok();
                            match works {
                                true => ClaudeStore::save()?,
                                false => ClaudeStore::clear()?,
                            }
                            works
                        }
                        None => false,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.claude = match result {
                    Ok(true) => ClaudeStatus::Connected(ClaudeCode::new()),
                    Ok(false) => ClaudeStatus::Disconnected { error: None },
                    Err(err) => {
                        let err: anyhow::Error = err;
                        eprintln!("failed to restore the Claude Code connection: {err:#}");
                        ClaudeStatus::Disconnected { error: None }
                    }
                };
                cx.emit(AppEvent::AnalysisEnabled);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Connects Claude Code, once it's found and signed in.
    pub fn connect_claude(&mut self, cx: &mut Context<Self>) {
        self.connect_claude_with(ClaudeCode::new(), true, cx);
    }

    /// Connected with `claude`, as if the user had connected.
    #[cfg(test)]
    pub fn connect_claude_for_test(&mut self, claude: ClaudeCode, cx: &mut Context<Self>) {
        self.connect_claude_with(claude, false, cx);
    }

    /// Checks `claude` works, then connects, saved for next time if `save`.
    fn connect_claude_with(&mut self, claude: ClaudeCode, save: bool, cx: &mut Context<Self>) {
        self.claude = ClaudeStatus::Connecting;
        cx.notify();
        self._claude_task = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let result = executor
                .spawn({
                    let claude = claude.clone();
                    async move {
                        claude.verify()?;
                        if save && let Err(err) = ClaudeStore::save() {
                            // It works for now; only the next launch is
                            // affected.
                            eprintln!("failed to save the Claude Code connection: {err:#}");
                        }
                        anyhow::Ok(())
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.claude = ClaudeStatus::Connected(claude);
                        cx.emit(AppEvent::AnalysisEnabled);
                    }
                    Err(err) => {
                        this.claude = ClaudeStatus::Disconnected {
                            error: Some(format!("{err:#}").into()),
                        };
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn disconnect_claude(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = ClaudeStore::clear() {
            eprintln!("failed to clear the Claude Code connection: {err:#}");
        }
        self._claude_task = None;
        self.claude = ClaudeStatus::Disconnected { error: None };
        cx.notify();
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
        let Some(claude) = app.claude_code().cloned() else {
            return;
        };
        if let Some(analysis) = cached {
            self.set_analysis(revision, AnalysisLoad::Ready(analysis), cx);
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            let cancel = Cancel(Arc::new(AtomicBool::new(false)));
            let result = cx
                .background_executor()
                .spawn({
                    let request = request.clone();
                    let cancel = cancel.0.clone();
                    async move { analyze(&request, &claude, &cancel) }
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
fn analyze(
    request: &AnalysisRequest,
    claude: &ClaudeCode,
    cancel: &AtomicBool,
) -> Result<DiffAnalysis> {
    if let Some(analysis) = ai::cache::load(&request.key) {
        return Ok(analysis);
    }
    let analysis = request.run(claude, cancel)?;
    if let Err(err) = ai::cache::save(&request.key, &analysis) {
        eprintln!("failed to cache the analysis: {err:#}");
    }
    Ok(analysis)
}
