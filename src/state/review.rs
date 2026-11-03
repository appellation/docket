//! The open diff: a pull request's Files tab, or a branch's changes.
//! Loading it from the project's checkout and highlighting it in the
//! background.
//!
//! A PR's diff starts loading in the background as soon as the PR opens
//! (once its commits are known), so fetching it overlaps reading the
//! conversation, and the Files tab is usually ready when it opens. A
//! branch's loads when it opens (see `branch.rs`). It appears as plain text
//! once loaded; each file's highlighting is applied as it finishes, so a
//! large diff doesn't wait on its slowest file.
//!
//! Language servers start last: once the diff is on screen and
//! highlighted. Preparing their worktree and starting them is heavy (a
//! checkout, then a server indexing the project), and would otherwise
//! compete with highlighting, or run for PRs whose files are never opened.

use std::sync::Arc;

use anyhow::Result;
use gpui::{Context, SharedString, Task};

use super::{CodeIntel, PrTab, Tab};
use crate::diff::git::{self as diff_git, PullRequestRefs};
use crate::diff::{FileContent, PullRequestDiff, TextSnapshot};
use crate::projects::{Checkout, Project, ProjectId};
use crate::syntax;

pub struct DiffFiles {
    /// The project whose checkout the diff comes from.
    pub project: Option<ProjectId>,
    pub diff: DiffLoad,
    /// Bumped whenever `diff` changes state, so views know to rebuild what
    /// they derive from it. Highlighting doesn't bump it.
    pub revision: u64,
    /// Files whose highlighting hasn't finished.
    pub highlighting: usize,
    /// Language servers for hover, once the diff has loaded.
    pub code_intel: Option<CodeIntel>,
    /// The user allowed servers that run project code for this PR, though
    /// it's from a fork. Until its head moves. Branches don't need it.
    pub allow_project_code: bool,
    _load_task: Option<Task<()>>,
    _highlight_tasks: Vec<Task<()>>,
}

pub enum DiffLoad {
    Loading,
    Ready(PullRequestDiff),
    Failed(SharedString),
    /// No project is connected to the PR's repository.
    NoProject,
}

impl DiffFiles {
    pub(super) fn new() -> Self {
        Self {
            project: None,
            diff: DiffLoad::Loading,
            revision: 0,
            highlighting: 0,
            code_intel: None,
            allow_project_code: false,
            _load_task: None,
            _highlight_tasks: Vec::new(),
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.diff, DiffLoad::Loading) || self.highlighting > 0
    }
}

impl Tab {
    /// Switches the open PR between its conversation and its files.
    pub fn select_pr_tab(&mut self, tab: PrTab, cx: &mut Context<Self>) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        detail.tab = tab;
        if tab == PrTab::Files && detail.files.is_none() {
            self.load_pr_diff(cx);
        }
        self.start_code_intel_when_shown(cx);
        cx.notify();
    }

    /// Called when a search refreshes: reloads the open PR's diff if its
    /// head or base moved, or if the last load failed.
    pub(super) fn sync_pr_files(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        let Some(summary) = &detail.summary else {
            return;
        };
        let updated = self
            .app
            .read(cx)
            .sections
            .iter()
            .flat_map(|section| &section.pull_requests)
            .find(|pr| pr.id == summary.id);
        // Search lags behind: a row older than what the detail fetched
        // would move the diff back.
        let Some(updated) = updated.filter(|pr| pr.updated_at >= summary.updated_at) else {
            return;
        };
        let moved = updated.head_ref_oid != summary.head_ref_oid
            || updated.base_ref_oid != summary.base_ref_oid;
        detail.summary = Some(updated.clone());
        if detail.files.is_none() {
            return;
        }
        if moved {
            self.load_pr_diff(cx);
        } else {
            self.retry_diff(cx);
        }
    }

    /// Loads the open diff again if it failed or had no project, e.g.
    /// after a project was added or found again.
    pub(super) fn retry_diff(&mut self, cx: &mut Context<Self>) {
        let retry = self
            .diff_files()
            .is_some_and(|files| matches!(files.diff, DiffLoad::Failed(_) | DiffLoad::NoProject));
        if !retry {
            return;
        }
        if self.detail.is_some() {
            self.load_pr_diff(cx);
        } else {
            self.load_branch_diff(cx);
        }
    }

    /// The open diff: the PR's, once its Files tab has opened, or the
    /// branch's.
    pub fn diff_files(&self) -> Option<&DiffFiles> {
        match (&self.detail, &self.branch) {
            (Some(detail), _) => detail.files.as_ref(),
            (None, Some(branch)) => Some(&branch.files),
            (None, None) => None,
        }
    }

    pub(super) fn diff_files_mut(&mut self) -> Option<&mut DiffFiles> {
        match (&mut self.detail, &mut self.branch) {
            (Some(detail), _) => detail.files.as_mut(),
            (None, Some(branch)) => Some(&mut branch.files),
            (None, None) => None,
        }
    }

    /// Loads the open pull request's diff, or, until its commits are known,
    /// shows it loading.
    pub(super) fn load_pr_diff(&mut self, cx: &mut Context<Self>) {
        // Prefer the selected project when several share the repository.
        let project = self.detail.as_ref().and_then(|detail| {
            let pr = detail.summary.as_ref()?;
            self.selected_project(cx)
                .filter(|p| p.contains(pr))
                .or_else(|| self.app.read(cx).projects.iter().find(|p| p.contains(pr)))
                .cloned()
        });
        let Some(detail) = &mut self.detail else {
            return;
        };
        detail.files.get_or_insert_with(DiffFiles::new);
        let Some(pr) = &detail.summary else {
            return;
        };
        let refs = PullRequestRefs {
            number: pr.number,
            base_ref_name: pr.base_ref_name.clone(),
            base_oid: pr.base_ref_oid.clone(),
            head_oid: pr.head_ref_oid.clone(),
        };
        self.load_diff(
            project,
            move |project, checkout| {
                let remote = diff_git::remote_for(checkout, &project.repository);
                diff_git::load(&checkout.root, &remote, &refs)
            },
            cx,
        );
    }

    /// Loads the open diff from `project`'s checkout with `load`, on the
    /// background executor, then highlights it and starts its language
    /// servers.
    pub(super) fn load_diff(
        &mut self,
        project: Option<Project>,
        load: impl FnOnce(&Project, &Checkout) -> Result<PullRequestDiff> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(files) = self.diff_files_mut() else {
            return;
        };
        files.project = project.as_ref().map(|p| p.id);
        files.revision += 1;
        files.highlighting = 0;
        files._highlight_tasks.clear();
        files.code_intel = None;
        files.allow_project_code = false;
        cx.notify();

        let Some(project) = project else {
            files.diff = DiffLoad::NoProject;
            files._load_task = None;
            return;
        };
        if project.path_missing {
            files.diff = DiffLoad::Failed(
                format!(
                    "{} isn't at {} anymore. Locate it from the project's Connection settings.",
                    project.name,
                    project.path.display()
                )
                .into(),
            );
            files._load_task = None;
            return;
        }

        files.diff = DiffLoad::Loading;
        let revision = files.revision;
        files._load_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let checkout = Checkout::scan(&project.path)?;
                    load(&project, &checkout)
                })
                .await;
            this.update(cx, |this, cx| this.finish_load(revision, result, cx))
                .ok();
        }));
    }

    /// Starts the open diff's language servers if it's on screen (a PR's
    /// Files tab, or a branch), loaded, and highlighted, and they haven't
    /// started yet.
    fn start_code_intel_when_shown(&mut self, cx: &mut Context<Self>) {
        let shown = match &self.detail {
            Some(detail) => detail.tab == PrTab::Files,
            None => self.branch.is_some(),
        };
        let Some(files) = self.diff_files() else {
            return;
        };
        let ready = matches!(files.diff, DiffLoad::Ready(_)) && files.highlighting == 0;
        if shown && ready && files.code_intel.is_none() {
            let revision = files.revision;
            self.start_code_intel(revision, cx);
        }
    }

    /// The open diff, if `revision` is still current. Tasks are cancelled
    /// when their `DiffFiles` is dropped, so this only filters out
    /// superseded loads of the same diff.
    pub(super) fn diff_files_at(&mut self, revision: u64) -> Option<&mut DiffFiles> {
        self.diff_files_mut()
            .filter(|files| files.revision == revision)
    }

    fn finish_load(
        &mut self,
        revision: u64,
        result: Result<PullRequestDiff>,
        cx: &mut Context<Self>,
    ) {
        let Some(files) = self.diff_files_at(revision) else {
            return;
        };
        files.revision += 1;
        let revision = files.revision;
        let diff = match result {
            Ok(diff) => diff,
            Err(err) => {
                files.diff = DiffLoad::Failed(format!("{err:#}").into());
                cx.notify();
                return;
            }
        };

        // Highlight in file order, so the top of the diff colors first.
        let mut tasks = Vec::new();
        for (ix, file) in diff.files.iter().enumerate() {
            let FileContent::Text(text) = &file.content else {
                continue;
            };
            let (old, new) = (text.old.clone(), text.new.clone());
            if old
                .as_ref()
                .or(new.as_ref())
                .is_none_or(|t| t.language.is_none())
            {
                continue;
            }
            tasks.push(cx.spawn(async move |this, cx| {
                let (old, new) = cx
                    .background_executor()
                    .spawn(async move {
                        let highlight = |side: Option<&Arc<TextSnapshot>>| {
                            let side = side?;
                            syntax::highlight(side.text(), side.language?).map(Arc::new)
                        };
                        (highlight(old.as_ref()), highlight(new.as_ref()))
                    })
                    .await;
                this.update(cx, |this, cx| {
                    let Some(files) = this.diff_files_at(revision) else {
                        return;
                    };
                    files.highlighting = files.highlighting.saturating_sub(1);
                    if let DiffLoad::Ready(diff) = &mut files.diff
                        && let FileContent::Text(text) = &mut diff.files[ix].content
                    {
                        text.old_syntax = old;
                        text.new_syntax = new;
                    }
                    this.start_code_intel_when_shown(cx);
                    cx.notify();
                })
                .ok();
            }));
        }
        files.highlighting = tasks.len();
        files._highlight_tasks = tasks;
        files.diff = DiffLoad::Ready(diff);
        self.start_code_intel_when_shown(cx);
        cx.notify();
    }
}
