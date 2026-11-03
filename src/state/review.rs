//! The open pull request's Files tab: loading its diff from the project's
//! checkout and highlighting it in the background.
//!
//! The diff loads the first time the tab opens, so reading the conversation
//! never waits on a fetch. It appears as plain text once loaded; each file's
//! highlighting is applied as it finishes, so a large PR doesn't wait on its
//! slowest file.

use std::sync::Arc;

use anyhow::Result;
use gpui::{Context, SharedString, Task};

use super::{CodeIntel, PrTab, Tab};
use crate::diff::git::{self as diff_git, PullRequestRefs};
use crate::diff::{FileContent, PullRequestDiff, TextSnapshot};
use crate::projects::{Checkout, ProjectId};
use crate::syntax;

pub struct PrFiles {
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
    /// it's from a fork. Until its head moves.
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

impl PrFiles {
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
            self.load_diff(cx);
        }
        cx.notify();
    }

    /// Called when an inbox refreshes: reloads the open PR's diff if its
    /// head or base moved, or if the last load failed.
    pub(super) fn sync_pr_files(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        let updated = self
            .app
            .read(cx)
            .sections
            .iter()
            .flat_map(|section| &section.pull_requests)
            .find(|pr| pr.id == detail.summary.id);
        let Some(updated) = updated else {
            return;
        };
        let moved = updated.head_ref_oid != detail.summary.head_ref_oid
            || updated.base_ref_oid != detail.summary.base_ref_oid;
        detail.summary = updated.clone();
        if detail.files.is_none() {
            return;
        }
        if moved {
            self.load_diff(cx);
        } else {
            self.retry_pr_files(cx);
        }
    }

    /// Loads the open PR's diff again if it failed or had no project, e.g.
    /// after a project was added.
    pub(super) fn retry_pr_files(&mut self, cx: &mut Context<Self>) {
        let retry = self
            .detail
            .as_ref()
            .and_then(|detail| detail.files.as_ref())
            .is_some_and(|files| matches!(files.diff, DiffLoad::Failed(_) | DiffLoad::NoProject));
        if retry {
            self.load_diff(cx);
        }
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        // Prefer the selected project when several share the repository.
        let project = self.detail.as_ref().and_then(|detail| {
            let pr = &detail.summary;
            self.selected_project(cx)
                .filter(|p| p.contains(pr))
                .or_else(|| self.app.read(cx).projects.iter().find(|p| p.contains(pr)))
                .cloned()
        });
        let Some(detail) = &mut self.detail else {
            return;
        };
        let files = detail.files.get_or_insert_with(|| PrFiles {
            project: None,
            diff: DiffLoad::Loading,
            revision: 0,
            highlighting: 0,
            code_intel: None,
            allow_project_code: false,
            _load_task: None,
            _highlight_tasks: Vec::new(),
        });
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
        let pr = &detail.summary;
        let refs = PullRequestRefs {
            number: pr.number,
            base_ref_name: pr.base_ref_name.clone(),
            base_oid: pr.base_ref_oid.clone(),
            head_oid: pr.head_ref_oid.clone(),
        };
        files._load_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let checkout = Checkout::scan(&project.path)?;
                    let remote = diff_git::remote_for(&checkout, &project.repository);
                    diff_git::load(&checkout.root, &remote, &refs)
                })
                .await;
            this.update(cx, |this, cx| this.finish_load(revision, result, cx))
                .ok();
        }));
    }

    /// The open PR's files, if `revision` is still current. Tasks are
    /// cancelled when their `PrFiles` is dropped, so this only filters out
    /// superseded loads of the same PR.
    pub(super) fn pr_files_at(&mut self, revision: u64) -> Option<&mut PrFiles> {
        self.detail
            .as_mut()?
            .files
            .as_mut()
            .filter(|files| files.revision == revision)
    }

    fn finish_load(
        &mut self,
        revision: u64,
        result: Result<PullRequestDiff>,
        cx: &mut Context<Self>,
    ) {
        let Some(files) = self.pr_files_at(revision) else {
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
                    let Some(files) = this.pr_files_at(revision) else {
                        return;
                    };
                    files.highlighting = files.highlighting.saturating_sub(1);
                    if let DiffLoad::Ready(diff) = &mut files.diff
                        && let FileContent::Text(text) = &mut diff.files[ix].content
                    {
                        text.old_syntax = old;
                        text.new_syntax = new;
                    }
                    cx.notify();
                })
                .ok();
            }));
        }
        files.highlighting = tasks.len();
        files._highlight_tasks = tasks;
        files.diff = DiffLoad::Ready(diff);
        self.start_code_intel(revision, cx);
        cx.notify();
    }
}
