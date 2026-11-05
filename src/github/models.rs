//! Domain types the UI works with, decoupled from the wire format.

use std::fmt;
use std::str::FromStr;

use anyhow::{Error, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub login: String,
    pub name: Option<String>,
    #[serde(rename = "avatarUrl")]
    pub avatar_url: String,
}

impl User {
    pub fn display_name(&self) -> &str {
        display_name(&self.login, self.name.as_deref())
    }
}

/// What to call someone: the name on their profile, or their login when
/// they haven't set one.
pub fn display_name<'a>(login: &'a str, name: Option<&'a str>) -> &'a str {
    match name.map(str::trim) {
        Some(name) if !name.is_empty() => name,
        _ => login,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Clone, Debug)]
pub struct PullRequest {
    /// GraphQL node ID; used for mutations.
    pub id: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// `owner/name`
    pub repository: String,
    pub author: Option<Actor>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub review_decision: Option<ReviewDecision>,
    pub additions: u64,
    pub deletions: u64,
    pub updated_at: DateTime<Utc>,
    /// The branch it merges into, and that branch's current commit.
    pub base_ref_name: String,
    pub base_ref_oid: String,
    /// The branch it's from, and that branch's current commit.
    pub head_ref_name: String,
    pub head_ref_oid: String,
    /// Whether the head branch is in a fork rather than the repository.
    pub is_cross_repository: bool,
}

/// A GitHub repository coordinate. Serializes as `owner/name`.
///
/// GitHub owner and repo names are case-insensitive, and so is equality.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    /// Whether `name_with_owner` (as GitHub returns it) is this repository.
    pub fn matches(&self, name_with_owner: &str) -> bool {
        name_with_owner
            .split_once('/')
            .is_some_and(|(owner, name)| {
                owner.eq_ignore_ascii_case(&self.owner) && name.eq_ignore_ascii_case(&self.name)
            })
    }
}

impl PartialEq for RepoRef {
    fn eq(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.name.eq_ignore_ascii_case(&other.name)
    }
}

impl Eq for RepoRef {}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

impl FromStr for RepoRef {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Ok(Self {
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                })
            }
            _ => bail!("expected `owner/name`, got {s:?}"),
        }
    }
}

impl TryFrom<String> for RepoRef {
    type Error = Error;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<RepoRef> for String {
    fn from(repo: RepoRef) -> Self {
        repo.to_string()
    }
}

/// What the viewer can see about a repository, used to confirm a project's
/// connection.
#[derive(Clone, Debug)]
pub struct Repository {
    pub repo: RepoRef,
    pub is_private: bool,
    /// `ADMIN`, `MAINTAIN`, `WRITE`, `TRIAGE` or `READ`.
    pub viewer_permission: Option<String>,
    /// Set when this repository is a fork; PRs usually land on the parent.
    pub parent: Option<RepoRef>,
}

/// Why GitHub won't show the viewer a repository.
#[derive(Debug)]
pub enum RepoAccessError {
    /// The org enforces SAML SSO and this token isn't authorized for it.
    /// `url` starts that authorization.
    SsoRequired {
        org: String,
        url: String,
    },
    /// The org restricts OAuth apps and hasn't approved Docket.
    OauthAppRestricted {
        org: String,
    },
    /// Doesn't exist, or the account can't see it.
    NotFound(RepoRef),
    Other(String),
}

impl fmt::Display for RepoAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SsoRequired { org, .. } => write!(
                f,
                "{org} requires single sign-on, and this sign-in isn't authorized for it. \
                 Authorize it for {org}, then check again."
            ),
            Self::OauthAppRestricted { org } => write!(
                f,
                "{org} limits which third-party apps can access its repositories, and hasn't \
                 approved Docket. Request approval (an org owner has to grant it), then check again."
            ),
            Self::NotFound(repo) => write!(
                f,
                "{repo} doesn't exist on GitHub, or Docket can't see it. If it belongs to an \
                 organization, check that the organization has approved Docket."
            ),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for RepoAccessError {}

/// Who did something: a user, a bot (GitHub App), or an org.
#[derive(Clone, Debug)]
pub struct Actor {
    pub login: String,
    /// The name on a user's profile, if they've set one.
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub is_bot: bool,
}

impl Actor {
    pub fn display_name(&self) -> &str {
        display_name(&self.login, self.name.as_deref())
    }
}

/// Everything the pull request detail view shows.
#[derive(Clone, Debug)]
pub struct PullRequestDetail {
    pub id: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<Actor>,
    /// Markdown, as written.
    pub body: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub created_at: DateTime<Utc>,
    pub base_ref: String,
    /// `owner:branch` when the head is on a fork, as GitHub shows it.
    pub head_ref: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub commit_count: u64,
    pub review_decision: Option<ReviewDecision>,
    pub merge_status: MergeStatus,
    /// The first page of checks on the head commit, failures first.
    pub checks: Vec<Check>,
    /// Counts across every check, including those past the first page.
    pub check_summary: CheckSummary,
    pub reviewers: Vec<Reviewer>,
    pub assignees: Vec<Actor>,
    pub timeline: Vec<TimelineItem>,
    /// The pull request as the docket lists it, as of this fetch.
    pub summary: PullRequest,
}

/// A pull request's version: what its `PULL_REQUEST_VERSION` query
/// returned, which differs whenever anything its detail shows has changed.
/// It's only compared, so it's kept as GitHub sent it.
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestVersion(pub(super) serde_json::Value);

/// Whether an open pull request can be merged, from `mergeStateStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeStatus {
    Clean,
    /// Mergeable, but some checks aren't passing.
    Unstable,
    /// Out of date with the base branch.
    Behind,
    /// Blocked by branch protection (reviews, required checks).
    Blocked,
    Conflicting,
    Draft,
    /// GitHub is still computing it.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckStatus {
    // Ordered by urgency, so sorting puts failures first.
    Failure,
    Pending,
    Success,
    /// Skipped or neutral.
    Neutral,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckSummary {
    pub failing: u64,
    pub pending: u64,
    pub passing: u64,
    pub skipped: u64,
}

impl CheckSummary {
    pub fn add(&mut self, status: CheckStatus, count: u64) {
        match status {
            CheckStatus::Failure => self.failing += count,
            CheckStatus::Pending => self.pending += count,
            CheckStatus::Success => self.passing += count,
            CheckStatus::Neutral => self.skipped += count,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewerStatus {
    Requested,
    Approved,
    ChangesRequested,
    Commented,
}

#[derive(Clone, Debug)]
pub struct Reviewer {
    /// A user's login; `None` for a team.
    pub login: Option<String>,
    /// What to call them: a user's display name, or a team's name.
    pub name: String,
    pub avatar_url: Option<String>,
    pub status: ReviewerStatus,
}

#[derive(Clone, Debug)]
pub enum TimelineItem {
    Comment(Comment),
    Review(Review),
    Commit(Commit),
    Event(Event),
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub id: String,
    pub url: String,
    pub author: Option<Actor>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    /// Why a moderator hid it (`spam`, `outdated`, …), if they did.
    pub minimized: Option<String>,
}

impl Comment {
    pub fn is_by_bot(&self) -> bool {
        self.author.as_ref().is_some_and(|a| a.is_bot)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewState {
    Pending,
    Commented,
    Approved,
    ChangesRequested,
    Dismissed,
}

#[derive(Clone, Debug)]
pub struct Review {
    pub id: String,
    pub url: String,
    pub author: Option<Actor>,
    pub state: ReviewState,
    pub body: String,
    pub submitted_at: DateTime<Utc>,
    /// Threads this review started. Replies to other reviews' threads show
    /// in those threads.
    pub threads: Vec<ReviewThread>,
}

#[derive(Clone, Debug)]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    pub line: Option<u64>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    /// The diff the first comment was left on, ending at its line.
    pub diff_hunk: String,
    pub comments: Vec<Comment>,
}

#[derive(Clone, Debug)]
pub struct Commit {
    /// Unique among commits: a pull request commit's node ID, or a local
    /// commit's object name.
    pub id: String,
    /// Where it's shown on GitHub; `None` for a local commit, which may
    /// not have been pushed.
    pub url: Option<String>,
    pub short_oid: String,
    pub headline: String,
    /// The author's display name when the commit is linked to a GitHub
    /// account, otherwise the name it was committed with.
    pub author: Option<String>,
    pub committed_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub id: String,
    /// The actor's display name.
    pub actor: Option<String>,
    pub created_at: DateTime<Utc>,
    pub kind: EventKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    Merged {
        commit: Option<String>,
        base: String,
    },
    Closed,
    Reopened,
    ReadyForReview,
    ConvertedToDraft,
    ReviewRequested {
        reviewer: Option<String>,
    },
    ReviewRequestRemoved {
        reviewer: Option<String>,
    },
    ReviewDismissed {
        reviewer: Option<String>,
    },
    Deployed {
        environment: Option<String>,
    },
    HeadRefForcePushed,
    HeadRefDeleted,
    HeadRefRestored,
    BaseRefChanged {
        from: String,
        to: String,
    },
    BaseRefForcePushed,
    Labeled {
        label: String,
    },
    Unlabeled {
        label: String,
    },
    Assigned {
        assignee: Option<String>,
    },
    Unassigned {
        assignee: Option<String>,
    },
    Renamed {
        from: String,
        to: String,
    },
    CrossReferenced {
        source: Option<String>,
    },
    Milestoned {
        milestone: String,
    },
    Demilestoned {
        milestone: String,
    },
    AutoMergeEnabled,
    AutoMergeDisabled,
    AddedToMergeQueue,
    RemovedFromMergeQueue,
}

impl EventKind {
    /// Events that change where the pull request stands. The rest are
    /// bookkeeping the timeline collapses.
    pub fn is_notable(&self) -> bool {
        matches!(
            self,
            Self::Merged { .. }
                | Self::Closed
                | Self::Reopened
                | Self::ReadyForReview
                | Self::ConvertedToDraft
        )
    }

    /// A short plural noun for summarizing collapsed events.
    pub fn category(&self) -> &'static str {
        match self {
            Self::Merged { .. } | Self::Closed | Self::Reopened => "state changes",
            Self::ReadyForReview | Self::ConvertedToDraft => "draft changes",
            Self::ReviewRequested { .. }
            | Self::ReviewRequestRemoved { .. }
            | Self::ReviewDismissed { .. } => "review requests",
            Self::Deployed { .. } => "deployments",
            Self::HeadRefForcePushed
            | Self::HeadRefDeleted
            | Self::HeadRefRestored
            | Self::BaseRefChanged { .. }
            | Self::BaseRefForcePushed => "branch changes",
            Self::Labeled { .. } | Self::Unlabeled { .. } => "label changes",
            Self::Assigned { .. } | Self::Unassigned { .. } => "assignments",
            Self::Renamed { .. } => "title changes",
            Self::CrossReferenced { .. } => "references",
            Self::Milestoned { .. } | Self::Demilestoned { .. } => "milestone changes",
            Self::AutoMergeEnabled
            | Self::AutoMergeDisabled
            | Self::AddedToMergeQueue
            | Self::RemovedFromMergeQueue => "merge queue changes",
        }
    }

    /// What happened, phrased to follow the actor's name.
    pub fn description(&self) -> String {
        let or_someone = |name: &Option<String>| name.clone().unwrap_or_else(|| "someone".into());
        match self {
            Self::Merged {
                commit: Some(commit),
                base,
            } => format!("merged commit {commit} into {base}"),
            Self::Merged { commit: None, base } => format!("merged into {base}"),
            Self::Closed => "closed this".into(),
            Self::Reopened => "reopened this".into(),
            Self::ReadyForReview => "marked this ready for review".into(),
            Self::ConvertedToDraft => "converted this to a draft".into(),
            Self::ReviewRequested { reviewer } => {
                format!("requested a review from {}", or_someone(reviewer))
            }
            Self::ReviewRequestRemoved { reviewer } => {
                format!("removed the review request for {}", or_someone(reviewer))
            }
            Self::ReviewDismissed { reviewer } => {
                format!("dismissed {}'s review", or_someone(reviewer))
            }
            Self::Deployed {
                environment: Some(environment),
            } => format!("deployed to {environment}"),
            Self::Deployed { environment: None } => "deployed this".into(),
            Self::HeadRefForcePushed => "force-pushed the branch".into(),
            Self::HeadRefDeleted => "deleted the branch".into(),
            Self::HeadRefRestored => "restored the branch".into(),
            Self::BaseRefChanged { from, to } => format!("changed the base from {from} to {to}"),
            Self::BaseRefForcePushed => "force-pushed the base branch".into(),
            Self::Labeled { label } => format!("added the {label} label"),
            Self::Unlabeled { label } => format!("removed the {label} label"),
            Self::Assigned { assignee } => format!("assigned {}", or_someone(assignee)),
            Self::Unassigned { assignee } => format!("unassigned {}", or_someone(assignee)),
            Self::Renamed { from, to } => format!("changed the title from “{from}” to “{to}”"),
            Self::CrossReferenced {
                source: Some(source),
            } => format!("referenced this from {source}"),
            Self::CrossReferenced { source: None } => "referenced this".into(),
            Self::Milestoned { milestone } => format!("added this to the {milestone} milestone"),
            Self::Demilestoned { milestone } => {
                format!("removed this from the {milestone} milestone")
            }
            Self::AutoMergeEnabled => "enabled auto-merge".into(),
            Self::AutoMergeDisabled => "disabled auto-merge".into(),
            Self::AddedToMergeQueue => "added this to the merge queue".into(),
            Self::RemovedFromMergeQueue => "removed this from the merge queue".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_fall_back_to_logins() {
        assert_eq!(display_name("octocat", Some("Mona Lisa")), "Mona Lisa");
        assert_eq!(display_name("octocat", Some(" Mona ")), "Mona");
        assert_eq!(display_name("octocat", Some("  ")), "octocat");
        assert_eq!(display_name("octocat", None), "octocat");
    }
}

/// A published release of a repository: Docket's own, for updating.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// e.g. `v0.2.0`
    pub tag: String,
    /// The release's page on github.com.
    pub url: String,
    /// The commit its tag is at.
    pub commit: Option<String>,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    /// A signed download link, good for about an hour.
    pub url: String,
    pub size: u64,
    /// Hex, where GitHub has computed one.
    pub sha256: Option<String>,
}
