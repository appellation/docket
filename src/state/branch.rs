//! A branch opened from the docket, other than the default: what it
//! changes since it left the default branch, as a pull request from it
//! would show.
//!
//! The diff is read from Git at the branch's tip, so uncommitted changes
//! where it's checked out don't show. It's compared to the default branch
//! as the remote has it (see `projects::branches`). Branches are re-read on
//! every refresh, and the diff reloads if the tip or the base moved.

use anyhow::bail;
use gpui::Context;

use super::{DiffFiles, Tab};
use crate::diff::git as diff_git;
use crate::projects::{BaseBranch, Branch, ProjectId};

pub struct BranchDiff {
    pub project: ProjectId,
    /// The branch as the docket last listed it.
    pub branch: Branch,
    /// What it's compared to, as last read; `None` when the project has no
    /// default branch.
    pub base: Option<BaseBranch>,
    pub files: DiffFiles,
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
            files: DiffFiles::new(),
        });
        self.load_branch_diff(cx);
        cx.notify();
    }

    pub fn close_branch(&mut self, cx: &mut Context<Self>) {
        if self.branch.take().is_some() {
            cx.notify();
        }
    }

    /// Called when a project's branches are read again: reloads the open
    /// branch's diff if its tip or base moved, or if the last load failed.
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
        if moved {
            self.load_branch_diff(cx);
        } else {
            self.retry_diff(cx);
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
}
