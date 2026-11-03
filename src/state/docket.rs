//! The docket: what's pending in a tab's selected project, or in all of
//! them.
//! Pull requests waiting on your review come first, then each project's
//! default branch, pinned. Below them are the rest of the pull requests
//! you're involved in and your branches without one, together, most
//! recently updated first. Pull requests always show; branches only if
//! they've been committed to since a cutoff (a week back, to start), so
//! old experiments don't crowd the docket.
//!
//! Branches are read from each project's checkout (`projects::branches`)
//! on every refresh.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use gpui::{Context, SharedString};

use super::{AppState, AuthStatus, SectionKind};
use crate::github::PullRequest;
use crate::projects::{Branch, Project, ProjectId, branches};

#[derive(Clone, Copy)]
pub enum DocketItem<'a> {
    PullRequest(&'a PullRequest),
    Branch {
        project: &'a Project,
        branch: &'a Branch,
        /// The project's default branch, pinned.
        pinned: bool,
    },
}

impl DocketItem<'_> {
    fn updated_at(&self) -> DateTime<Utc> {
        match self {
            Self::PullRequest(pr) => pr.updated_at,
            Self::Branch { branch, .. } => branch.updated_at,
        }
    }
}

pub struct Docket<'a> {
    /// Waiting on your review, most recently updated first.
    pub review_requested: Vec<&'a PullRequest>,
    /// Each project's default branch, in the order of the projects.
    pub pinned: Vec<DocketItem<'a>>,
    /// Every other pull request, and the branches updated since the
    /// cutoff, most recently updated first.
    pub recent: Vec<DocketItem<'a>>,
    /// How many more branches were last updated before the cutoff.
    pub older: usize,
}

impl<'a> Docket<'a> {
    /// Whether nothing's pending. The default branches are always there,
    /// so they don't count.
    pub fn is_empty(&self) -> bool {
        self.review_requested.is_empty() && self.recent.is_empty()
    }

    /// Whether there's both review requests and everything else, so
    /// they're worth telling apart.
    pub fn is_grouped(&self) -> bool {
        !self.review_requested.is_empty() && !self.recent.is_empty()
    }

    /// Every item, in order.
    pub fn items(&self) -> impl Iterator<Item = DocketItem<'a>> + '_ {
        self.review_requested
            .iter()
            .map(|pr| DocketItem::PullRequest(pr))
            .chain(self.pinned.iter().copied())
            .chain(self.recent.iter().copied())
    }
}

impl AppState {
    /// The docket for `project`, or with `None`, for every project, with
    /// the branches updated since `since`.
    pub fn docket<'a>(&'a self, project: Option<&'a Project>, since: DateTime<Utc>) -> Docket<'a> {
        let mut review_requested: Vec<_> = self
            .pull_requests_in(SectionKind::ReviewRequested, project)
            .collect();
        review_requested.sort_by_key(|pr| std::cmp::Reverse(pr.updated_at));

        let mut seen: HashSet<&str> = review_requested.iter().map(|pr| pr.id.as_str()).collect();
        let pull_requests = [SectionKind::Authored, SectionKind::Involved]
            .into_iter()
            .flat_map(|kind| self.pull_requests_in(kind, project))
            .filter(|pr| seen.insert(&pr.id))
            .map(DocketItem::PullRequest);
        let in_scope = || {
            self.projects
                .iter()
                .filter(|p| project.is_none_or(|selected| selected.id == p.id))
                .filter_map(|project| Some((project, self.branches.get(&project.id)?)))
        };
        let pinned = in_scope()
            .filter_map(|(project, branches)| {
                Some(DocketItem::Branch {
                    project,
                    branch: branches.default.as_ref()?,
                    pinned: true,
                })
            })
            .collect();
        let branches = in_scope().flat_map(|(project, branches)| {
            branches
                .yours
                .iter()
                .filter(|branch| !self.has_pull_request(project, branch))
                .map(move |branch| DocketItem::Branch {
                    project,
                    branch,
                    pinned: false,
                })
        });
        let (branches, older): (Vec<_>, Vec<_>) =
            branches.partition(|item| item.updated_at() >= since);
        let mut recent: Vec<_> = pull_requests.chain(branches).collect();
        recent.sort_by_key(|item| std::cmp::Reverse(item.updated_at()));

        Docket {
            review_requested,
            pinned,
            recent,
            older: older.len(),
        }
    }

    /// Whether any pull request is still loading.
    pub fn is_loading_docket(&self) -> bool {
        self.sections.iter().any(|section| section.loading)
    }

    /// Why pull requests are missing, once each: going offline fails every
    /// search the same way.
    pub fn docket_errors(&self) -> Vec<SharedString> {
        let mut errors: Vec<SharedString> = Vec::new();
        for error in self.sections.iter().filter_map(|s| s.error.clone()) {
            if !errors.contains(&error) {
                errors.push(error);
            }
        }
        errors
    }

    /// Whether `branch` is the head of an open pull request in `project`:
    /// by name, as it is here or on its remote, or by its tip. Another
    /// person's fork can have a branch of the same name, so for theirs
    /// only the tip counts.
    fn has_pull_request(&self, project: &Project, branch: &Branch) -> bool {
        let viewer = match &self.auth {
            AuthStatus::SignedIn { user, .. } => Some(user.login.as_str()),
            _ => None,
        };
        self.sections
            .iter()
            .flat_map(|section| &section.pull_requests)
            .filter(|pr| project.contains(pr))
            .any(|pr| {
                let yours = !pr.is_cross_repository || pr.author.as_deref() == viewer;
                let same_name = pr.head_ref_name == branch.name
                    || branch.upstream.as_deref() == Some(pr.head_ref_name.as_str());
                pr.head_ref_oid == branch.oid || (yours && same_name)
            })
    }

    /// One of the project's branches in the docket, the default included.
    pub fn branch(&self, project: ProjectId, name: &str) -> Option<&Branch> {
        let branches = self.branches.get(&project)?;
        branches
            .default
            .iter()
            .chain(&branches.yours)
            .find(|branch| branch.name == name)
    }

    /// Re-reads every project's branches. What's shown stays until each
    /// project's arrive.
    pub(super) fn load_branches(&mut self, cx: &mut Context<Self>) {
        let projects: HashMap<ProjectId, &Project> =
            self.projects.iter().map(|p| (p.id, p)).collect();
        self.branches.retain(|id, _| projects.contains_key(id));
        self._branch_tasks = projects
            .into_values()
            .filter(|project| !project.path_missing)
            .map(|project| {
                let (id, root, repository) =
                    (project.id, project.path.clone(), project.repository.clone());
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { branches::read(&root, &repository) })
                        .await;
                    this.update(cx, |this, cx| {
                        match result {
                            Ok(branches) => {
                                this.branches.insert(id, branches);
                            }
                            // Branches are extra, and the pull requests
                            // still show, so this is only logged.
                            Err(err) => eprintln!("failed to list branches: {err:#}"),
                        }
                        cx.notify();
                    })
                    .ok();
                })
            })
            .collect();
    }
}

/// The time the docket's cutoff counts back from. Tests have a fixed one,
/// so their fixtures' dates stay recent.
pub(super) fn now() -> DateTime<Utc> {
    #[cfg(test)]
    return crate::test_support::now();
    #[cfg(not(test))]
    Utc::now()
}
