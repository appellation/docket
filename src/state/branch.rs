//! A branch opened from the docket, other than the default: what it
//! changes since it left the default branch, as a pull request from it
//! would show.
//!
//! The diff is read from Git at the branch's tip, so uncommitted changes
//! where it's checked out don't show. It's compared to the default branch
//! as the remote has it (see `projects::branches`). Branches are re-read on
//! every refresh, and the diff and commits reload if the tip or the base
//! moved.

use anyhow::bail;
use gpui::{Context, SharedString, Task};

use super::{DiffFiles, Tab};
use crate::diff::git as diff_git;
use crate::projects::{BaseBranch, Branch, BranchCommits, ProjectId, branches};

pub struct BranchDiff {
    pub project: ProjectId,
    /// The branch as the docket last listed it.
    pub branch: Branch,
    /// What it's compared to, as last read; `None` when the project has no
    /// default branch.
    pub base: Option<BaseBranch>,
    pub tab: BranchTab,
    pub files: DiffFiles,
    pub commits: CommitsLoad,
    _commits_task: Option<Task<()>>,
}

/// The open branch's tabs, a pull request's but for its conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BranchTab {
    #[default]
    Files,
    Commits,
}

pub enum CommitsLoad {
    Loading,
    Ready(BranchCommits),
    Failed(SharedString),
}

impl Tab {
    /// Opens one of the project's branches from the docket, showing what
    /// it changes.
    pub fn open_branch(&mut self, id: ProjectId, name: &str, cx: &mut Context<Self>) {
        let app = self.app.read(cx);
        let Some(branch) = app.branch(id, name).cloned() else {
            return;
        };
        let base = app.base_branch(id).cloned();
        self.setup = None;
        self.detail = None;
        self.browser = None;
        self.branch = Some(BranchDiff {
            project: id,
            branch,
            base,
            tab: BranchTab::default(),
            files: DiffFiles::new(),
            commits: CommitsLoad::Loading,
            _commits_task: None,
        });
        self.load_branch_diff(cx);
        self.load_branch_commits(cx);
        cx.notify();
    }

    pub fn select_branch_tab(&mut self, tab: BranchTab, cx: &mut Context<Self>) {
        if let Some(open) = &mut self.branch {
            open.tab = tab;
            cx.notify();
        }
    }

    pub fn close_branch(&mut self, cx: &mut Context<Self>) {
        if self.branch.take().is_some() {
            cx.notify();
        }
    }

    /// Called when a project's branches are read again: reloads the open
    /// branch's diff and commits if its tip or base moved, or if the last
    /// load failed.
    /// A branch that's no longer listed (merged, say) stays as it was.
    pub(super) fn sync_branch_files(&mut self, project: ProjectId, cx: &mut Context<Self>) {
        let Some(open) = self.branch.as_mut().filter(|open| open.project == project) else {
            return;
        };
        let app = self.app.read(cx);
        let Some(updated) = app.branch(project, &open.branch.name) else {
            return;
        };
        let base = app.base_branch(project);
        let moved = updated.oid != open.branch.oid
            || base.map(|b| &b.oid) != open.base.as_ref().map(|b| &b.oid);
        open.branch = updated.clone();
        open.base = base.cloned();
        let failed = matches!(open.commits, CommitsLoad::Failed(_));
        if moved {
            self.load_branch_diff(cx);
        } else {
            self.retry_diff(cx);
        }
        if moved || failed {
            self.load_branch_commits(cx);
        }
    }

    pub(super) fn load_branch_diff(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.branch else {
            return;
        };
        let project = self.app.read(cx).project(open.project).cloned();
        let head = open.branch.oid.clone();
        let base = open.base.as_ref().map(|base| base.oid.clone());
        self.load_diff(
            project,
            move |_, checkout| {
                let Some(base) = base else {
                    bail!("the project has no default branch to compare it with");
                };
                diff_git::load_branch(&checkout.root, &base, &head)
            },
            cx,
        );
    }

    /// Lists the open branch's commits since it left the default branch,
    /// on the background executor. What's listed stays until they arrive.
    fn load_branch_commits(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.branch else {
            return;
        };
        let root = self
            .app
            .read(cx)
            .project(open.project)
            .map(|project| project.path.clone());
        let head = open.branch.oid.clone();
        let base = open.base.as_ref().map(|base| base.oid.clone());
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let head = head.clone();
                    async move {
                        let (Some(root), Some(base)) = (root, base) else {
                            bail!("the project has no default branch to compare it with");
                        };
                        branches::commits(&root, &base, &head)
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(open) = this.branch.as_mut().filter(|open| open.branch.oid == head) else {
                    return;
                };
                open.commits = match result {
                    Ok(commits) => CommitsLoad::Ready(commits),
                    Err(err) => CommitsLoad::Failed(format!("{err:#}").into()),
                };
                cx.notify();
            })
            .ok();
        });
        if let Some(open) = &mut self.branch {
            open._commits_task = Some(task);
        }
    }
}
