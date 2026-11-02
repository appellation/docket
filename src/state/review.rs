//! The open pull request: loading its diff from the project's checkout and
//! highlighting it in the background.
//!
//! The diff appears as plain text once loaded; each file's highlighting is
//! applied as it finishes, so a large PR doesn't wait on its slowest file.

use std::sync::Arc;

use anyhow::Result;
use gpui::{Context, SharedString, Task};

use super::AppState;
use crate::diff::git::{self as diff_git, PullRequestRefs};
use crate::diff::{FileContent, PullRequestDiff, TextSnapshot};
use crate::github::PullRequest;
use crate::projects::{Checkout, ProjectId};
use crate::syntax;

pub struct OpenPullRequest {
    pub pr: PullRequest,
    /// The project whose checkout the diff comes from.
    pub project: Option<ProjectId>,
    pub diff: DiffLoad,
    /// Bumped whenever `diff` changes state, so views know to rebuild what
    /// they derive from it. Highlighting doesn't bump it.
    pub revision: u64,
    /// Files whose highlighting hasn't finished.
    pub highlighting: usize,
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

impl AppState {
    /// Opens a PR in place of the inbox and starts loading its diff.
    pub fn open_pull_request(&mut self, pr: PullRequest, cx: &mut Context<Self>) {
        self.setup = None;
        self.open_pull_request = Some(OpenPullRequest {
            pr,
            project: None,
            diff: DiffLoad::Loading,
            revision: 0,
            highlighting: 0,
            _load_task: None,
            _highlight_tasks: Vec::new(),
        });
        self.load_diff(cx);
    }

    pub fn close_pull_request(&mut self, cx: &mut Context<Self>) {
        if self.open_pull_request.take().is_some() {
            cx.notify();
        }
    }

    /// Called when an inbox refreshes: reloads the open PR's diff if its
    /// head or base moved, or if the last load failed.
    pub(super) fn sync_open_pull_request(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &mut self.open_pull_request else {
            return;
        };
        let updated = self
            .sections
            .iter()
            .flat_map(|section| &section.pull_requests)
            .find(|pr| pr.id == open.pr.id);
        let moved = updated.is_some_and(|pr| {
            pr.head_ref_oid != open.pr.head_ref_oid || pr.base_ref_oid != open.pr.base_ref_oid
        });
        if let Some(pr) = updated {
            open.pr = pr.clone();
        }
        if moved {
            self.load_diff(cx);
        } else {
            self.retry_open_pull_request(cx);
        }
    }

    /// Loads the open PR's diff again if it failed or had no project, e.g.
    /// after a project was added.
    pub(super) fn retry_open_pull_request(&mut self, cx: &mut Context<Self>) {
        if self
            .open_pull_request
            .as_ref()
            .is_some_and(|open| matches!(open.diff, DiffLoad::Failed(_) | DiffLoad::NoProject))
        {
            self.load_diff(cx);
        }
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        // Prefer the selected project when several share the repository.
        let project = self.open_pull_request.as_ref().and_then(|open| {
            self.selected_project()
                .filter(|p| p.contains(&open.pr))
                .or_else(|| self.projects.iter().find(|p| p.contains(&open.pr)))
                .cloned()
        });
        let Some(open) = &mut self.open_pull_request else {
            return;
        };
        open.project = project.as_ref().map(|p| p.id);
        open.revision += 1;
        open.highlighting = 0;
        open._highlight_tasks.clear();
        cx.notify();

        let Some(project) = project else {
            open.diff = DiffLoad::NoProject;
            open._load_task = None;
            return;
        };
        if project.path_missing {
            open.diff = DiffLoad::Failed(
                format!(
                    "{} isn't at {} anymore. Locate it from the project's Connection settings.",
                    project.name,
                    project.path.display()
                )
                .into(),
            );
            open._load_task = None;
            return;
        }

        open.diff = DiffLoad::Loading;
        let revision = open.revision;
        let refs = PullRequestRefs {
            number: open.pr.number,
            base_ref_name: open.pr.base_ref_name.clone(),
            base_oid: open.pr.base_ref_oid.clone(),
            head_oid: open.pr.head_ref_oid.clone(),
        };
        open._load_task = Some(cx.spawn(async move |this, cx| {
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

    fn finish_load(
        &mut self,
        revision: u64,
        result: Result<PullRequestDiff>,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self
            .open_pull_request
            .as_mut()
            .filter(|open| open.revision == revision)
        else {
            return;
        };
        open.revision += 1;
        let revision = open.revision;
        let diff = match result {
            Ok(diff) => diff,
            Err(err) => {
                open.diff = DiffLoad::Failed(format!("{err:#}").into());
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
                    let Some(open) = this
                        .open_pull_request
                        .as_mut()
                        .filter(|open| open.revision == revision)
                    else {
                        return;
                    };
                    open.highlighting = open.highlighting.saturating_sub(1);
                    if let DiffLoad::Ready(diff) = &mut open.diff
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
        open.highlighting = tasks.len();
        open._highlight_tasks = tasks;
        open.diff = DiffLoad::Ready(diff);
        cx.notify();
    }
}
