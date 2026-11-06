//! Merging and closing pull requests from the docket.
//!
//! A project can merge by comment instead (`MergeSettings`), for
//! repositories where a bot does the merging: Docket leaves one of the
//! project's comments, chosen at random, and the pull request stays in the
//! docket until the bot merges it.
//!
//! GitHub's search lags behind a merge or close for a few seconds, so a
//! pull request merged or closed here is left out of the docket until a
//! search shows it updated since.

use chrono::{DateTime, Utc};
use gpui::{Context, SharedString, Task};

use super::{AppEvent, AppState};
use crate::github::{PullRequest, RepoRef};
use crate::projects::{MergePlan, MergeSettings};

/// What's being done to a pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrActionKind {
    Merge,
    /// Leaving this comment, to have a bot merge it.
    Comment(String),
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrActionStatus {
    Running,
    Failed(SharedString),
    /// Merged or closed, at this time.
    Done(DateTime<Utc>),
    /// Commented on, for a bot to merge.
    Commented,
}

/// The last thing done to a pull request from the docket, by node ID.
pub struct PrAction {
    pub kind: PrActionKind,
    /// `owner/name#number`, for messages.
    pub label: String,
    pub status: PrActionStatus,
    _task: Option<Task<()>>,
}

impl PrAction {
    pub fn is_running(&self) -> bool {
        self.status == PrActionStatus::Running
    }
}

impl AppState {
    /// How `pr`'s project merges, if it's in one.
    pub fn merge_settings(&self, pr: &PullRequest) -> Option<&MergeSettings> {
        self.projects
            .iter()
            .find(|p| p.contains(pr))
            .map(|p| &p.merge)
    }

    /// What merging `pr` does now, by its project's settings, choosing a
    /// comment at random. `None` when its project merges by comment but has
    /// none to choose from. Pull requests outside every project merge on
    /// GitHub.
    pub fn merge_plan(&self, pr: &PullRequest) -> Option<MergePlan> {
        match self.merge_settings(pr) {
            Some(merge) => merge.plan(),
            None => Some(MergePlan::Merge),
        }
    }

    /// Merges `pr` as `plan` says: on GitHub, or by leaving a comment.
    pub fn merge_pull_request(
        &mut self,
        pr: &PullRequest,
        plan: MergePlan,
        cx: &mut Context<Self>,
    ) {
        let kind = match plan {
            MergePlan::Merge => PrActionKind::Merge,
            MergePlan::Comment(body) => PrActionKind::Comment(body),
        };
        self.act_on(pr, kind, cx);
    }

    /// Closes `pr` without merging it.
    pub fn close_pull_request(&mut self, pr: &PullRequest, cx: &mut Context<Self>) {
        self.act_on(pr, PrActionKind::Close, cx);
    }

    pub fn pr_action(&self, id: &str) -> Option<&PrAction> {
        self.pr_actions.get(id)
    }

    /// Why actions failed, with each one's pull request's node ID.
    pub fn pr_action_errors(&self) -> impl Iterator<Item = (&str, &PrAction, &SharedString)> {
        self.pr_actions
            .iter()
            .filter_map(|(id, action)| match &action.status {
                PrActionStatus::Failed(error) => Some((id.as_str(), action, error)),
                _ => None,
            })
    }

    pub fn dismiss_pr_action(&mut self, id: &str, cx: &mut Context<Self>) {
        if self
            .pr_actions
            .get(id)
            .is_some_and(|action| matches!(action.status, PrActionStatus::Failed(_)))
        {
            self.pr_actions.remove(id);
            cx.notify();
        }
    }

    /// Whether the docket leaves `pr` out: merged or closed here since the
    /// search that found it last saw it change.
    pub(super) fn is_done_with(&self, pr: &PullRequest) -> bool {
        self.pr_actions.get(&pr.id).is_some_and(
            |action| matches!(action.status, PrActionStatus::Done(at) if pr.updated_at <= at),
        )
    }

    fn act_on(&mut self, pr: &PullRequest, kind: PrActionKind, cx: &mut Context<Self>) {
        let Some(client) = self.client() else {
            return;
        };
        if self
            .pr_actions
            .get(&pr.id)
            .is_some_and(PrAction::is_running)
        {
            return;
        }
        let repo: Option<RepoRef> = pr.repository.parse().ok();
        let (id, head) = (pr.id.clone(), pr.head_ref_oid.clone());
        let task = cx.spawn({
            let (id, kind) = (id.clone(), kind.clone());
            async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn({
                        let id = id.clone();
                        async move {
                            match kind {
                                PrActionKind::Merge => {
                                    let repo = repo
                                        .ok_or_else(|| anyhow::anyhow!("not a repository: {id}"))?;
                                    client.merge_pull_request(&repo, &id, &head)
                                }
                                PrActionKind::Comment(body) => client.add_comment(&id, &body),
                                PrActionKind::Close => client.close_pull_request(&id),
                            }
                        }
                    })
                    .await;
                this.update(cx, |this, cx| this.finish_action(&id, result, cx))
                    .ok();
            }
        });
        self.pr_actions.insert(
            id,
            PrAction {
                kind,
                label: format!("{}#{}", pr.repository, pr.number),
                status: PrActionStatus::Running,
                _task: Some(task),
            },
        );
        cx.notify();
    }

    fn finish_action(&mut self, id: &str, result: anyhow::Result<()>, cx: &mut Context<Self>) {
        let Some(action) = self.pr_actions.get_mut(id) else {
            return;
        };
        action._task = None;
        let succeeded = result.is_ok();
        action.status = match result {
            Err(err) => PrActionStatus::Failed(format!("{err:#}").into()),
            // A bot merges it, and the docket shows it until then.
            Ok(()) if matches!(action.kind, PrActionKind::Comment(_)) => PrActionStatus::Commented,
            Ok(()) => PrActionStatus::Done(Utc::now()),
        };
        if succeeded {
            // The docket and an open pull request catch up.
            self.load_docket(true, cx);
            cx.emit(AppEvent::PullRequestChanged);
        }
        cx.notify();
    }
}
