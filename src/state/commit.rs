//! A commit opened from a pull request's or branch's Commits tab: what it
//! changes on its own, against its first parent, as GitHub shows a commit.
//! It's shown on the Commits tab in place of the list, until closed, and
//! analyzed like the whole diff is.
//!
//! Its diff is a `DiffFiles` of its own, beside the pull request's or
//! branch's (`DiffSlot`), so opening one leaves the Files tab as it was.
//! `Tab::diff_files` is whichever is on screen, so the diff view, its file
//! tree, and the analysis follow the commit while it shows. It isn't
//! reviewed, nor are its files marked as viewed on GitHub: GitHub's marks
//! and pending comments are on the pull request's diff. Nor does it start
//! language servers, which run in Docket's worktree at the pull request's
//! head or branch's tip, not the commit.

use gpui::Context;

use super::{BranchTab, DiffFiles, PrTab, Tab};
use crate::diff::git as diff_git;
use crate::github::{Commit, TimelineItem};
use crate::state::CommitsLoad;

/// Which of the open pull request's or branch's diffs: its own, or its
/// open commit's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffSlot {
    Main,
    Commit,
}

pub struct OpenCommit {
    pub commit: Commit,
    pub files: DiffFiles,
}

impl Tab {
    /// The open pull request's or branch's commits, as its Commits tab
    /// lists them.
    pub fn listed_commits(&self) -> Vec<&Commit> {
        if let Some(detail) = &self.detail {
            let Some(loaded) = &detail.loaded else {
                return Vec::new();
            };
            return loaded
                .pr
                .timeline
                .iter()
                .filter_map(|item| match item {
                    TimelineItem::Commit(commit) => Some(commit),
                    _ => None,
                })
                .collect();
        }
        match self.branch.as_ref().map(|open| &open.commits) {
            Some(CommitsLoad::Ready(listed)) => listed.commits.iter().collect(),
            _ => Vec::new(),
        }
    }

    /// The commit open on the pull request's or branch's Commits tab,
    /// whichever of its tabs shows.
    pub fn opened_commit(&self) -> Option<&OpenCommit> {
        match (&self.detail, &self.branch) {
            (Some(detail), _) => detail.commit.as_ref(),
            (None, Some(open)) => open.commit.as_ref(),
            (None, None) => None,
        }
    }

    pub(super) fn opened_commit_mut(&mut self) -> Option<&mut OpenCommit> {
        match (&mut self.detail, &mut self.branch) {
            (Some(detail), _) => detail.commit.as_mut(),
            (None, Some(open)) => open.commit.as_mut(),
            (None, None) => None,
        }
    }

    /// The open commit, while the Commits tab shows it.
    pub fn shown_commit(&self) -> Option<&OpenCommit> {
        let commits_tab = match (&self.detail, &self.branch) {
            (Some(detail), _) => detail.tab == PrTab::Commits,
            (None, Some(open)) => open.tab == BranchTab::Commits,
            (None, None) => false,
        };
        self.opened_commit().filter(|_| commits_tab)
    }

    /// Which diff is on screen, or behind the tab that is.
    pub(super) fn shown_slot(&self) -> DiffSlot {
        match self.shown_commit() {
            Some(_) => DiffSlot::Commit,
            None => DiffSlot::Main,
        }
    }

    /// Opens one of the listed commits (by `Commit::id`) on the Commits
    /// tab, showing what it changes.
    pub fn open_commit(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(commit) = self
            .listed_commits()
            .into_iter()
            .find(|commit| commit.id == id)
            .cloned()
        else {
            return;
        };
        if self
            .opened_commit()
            .is_some_and(|open| open.commit.id == id)
        {
            return;
        }
        let repository = match (&self.detail, &self.branch) {
            (Some(detail), _) => detail.repo.to_string(),
            (None, Some(open)) => match self.app.read(cx).project(open.project) {
                Some(project) => project.repository.to_string(),
                None => return,
            },
            (None, None) => return,
        };
        // Marks on a commit are kept apart from the pull request's or
        // branch's, as what's viewed of one isn't of the other.
        let files = DiffFiles::new(format!("{repository}@{}", commit.oid));
        let open = Some(OpenCommit { commit, files });
        match (&mut self.detail, &mut self.branch) {
            (Some(detail), _) => {
                detail.commit = open;
                detail.tab = PrTab::Commits;
            }
            (None, Some(branch)) => {
                branch.commit = open;
                branch.tab = BranchTab::Commits;
            }
            (None, None) => return,
        }
        self.load_commit_diff(cx);
        cx.notify();
    }

    /// Opens the commit `delta` places after the open one in the list.
    pub fn open_adjacent_commit(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(open) = self.opened_commit() else {
            return;
        };
        let commits = self.listed_commits();
        let Some(ix) = commits.iter().position(|c| c.id == open.commit.id) else {
            return;
        };
        let Some(next) = ix
            .checked_add_signed(delta)
            .and_then(|ix| commits.get(ix))
            .map(|commit| commit.id.clone())
        else {
            return;
        };
        self.open_commit(&next, cx);
    }

    /// Back to the list of commits.
    pub fn close_commit(&mut self, cx: &mut Context<Self>) {
        let closed = match (&mut self.detail, &mut self.branch) {
            (Some(detail), _) => detail.commit.take(),
            (None, Some(open)) => open.commit.take(),
            (None, None) => None,
        };
        if closed.is_some() {
            cx.notify();
        }
    }

    /// Loads the open commit's diff from the project's checkout: a pull
    /// request's from the project its diff is, fetching its head if the
    /// commit isn't local; a branch's, which is, from its project.
    pub(super) fn load_commit_diff(&mut self, cx: &mut Context<Self>) {
        let Some(oid) = self.opened_commit().map(|open| open.commit.oid.clone()) else {
            return;
        };
        if self.detail.is_some() {
            let Some((project, trusted, refs)) = self.pr_diff_source(cx) else {
                return;
            };
            self.load_diff(
                DiffSlot::Commit,
                project,
                trusted,
                move |project, checkout| {
                    let remote = diff_git::remote_for(checkout, &project.repository);
                    diff_git::load_commit(&checkout.root, &oid, Some((&remote, &refs)))
                },
                cx,
            );
        } else if let Some(open) = &self.branch {
            let project = self.app.read(cx).project(open.project).cloned();
            // Local branches are the user's own work.
            self.load_diff(
                DiffSlot::Commit,
                project,
                true,
                move |_, checkout| diff_git::load_commit(&checkout.root, &oid, None),
                cx,
            );
        }
    }
}
