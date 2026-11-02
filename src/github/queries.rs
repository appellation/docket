//! GraphQL documents and their response shapes.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::models::{
    Actor, Check, CheckStatus, CheckSummary, Comment, Commit, Event, EventKind, Label, MergeStatus,
    PullRequest, PullRequestDetail, PullRequestState, RepoRef, Repository, Review, ReviewDecision,
    ReviewState, ReviewThread, Reviewer, ReviewerStatus, TimelineItem, User,
};

pub const VIEWER: &str = r#"
query Viewer {
  viewer { login name avatarUrl }
}
"#;

pub const SEARCH_PULL_REQUESTS: &str = r#"
query SearchPullRequests($query: String!, $first: Int!) {
  search(query: $query, type: ISSUE, first: $first) {
    nodes {
      ... on PullRequest {
        id
        number
        title
        url
        state
        isDraft
        reviewDecision
        additions
        deletions
        updatedAt
        author { login }
        repository { nameWithOwner }
      }
    }
  }
}
"#;

pub const REPOSITORY: &str = r#"
query Repository($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    nameWithOwner
    isPrivate
    viewerPermission
    parent { nameWithOwner }
  }
}
"#;

// Shared by the detail query and its timeline pages. A macro rather than a
// `const` so `concat!` can splice it into both documents.
macro_rules! detail_fragments {
    () => {
        r#"
fragment ActorFields on Actor {
  __typename
  login
  avatarUrl(size: 64)
}

fragment ReviewerFields on RequestedReviewer {
  __typename
  ... on Actor { login avatarUrl(size: 64) }
  ... on Team { name avatarUrl(size: 64) }
}

fragment TimelineFields on PullRequestTimelineItemsConnection {
  pageInfo { hasNextPage endCursor }
  nodes {
    __typename
    ... on Node { id }
    ... on IssueComment {
      url
      body
      createdAt
      isMinimized
      minimizedReason
      author { ...ActorFields }
    }
    ... on PullRequestReview {
      url
      state
      body
      createdAt
      submittedAt
      author { ...ActorFields }
    }
    ... on PullRequestCommit {
      url
      commit {
        abbreviatedOid
        messageHeadline
        committedDate
        author { name user { login } }
      }
    }
    ... on MergedEvent { createdAt actor { login } commit { abbreviatedOid } mergeRefName }
    ... on ClosedEvent { createdAt actor { login } }
    ... on ReopenedEvent { createdAt actor { login } }
    ... on ReadyForReviewEvent { createdAt actor { login } }
    ... on ConvertToDraftEvent { createdAt actor { login } }
    ... on ReviewRequestedEvent { createdAt actor { login } requestedReviewer { ...ReviewerFields } }
    ... on ReviewRequestRemovedEvent { createdAt actor { login } requestedReviewer { ...ReviewerFields } }
    ... on ReviewDismissedEvent { createdAt actor { login } review { author { login } } }
    ... on DeployedEvent { createdAt actor { login } deployment { environment } }
    ... on DeploymentEnvironmentChangedEvent {
      createdAt
      actor { login }
      deploymentStatus { deployment { environment } }
    }
    ... on HeadRefForcePushedEvent { createdAt actor { login } }
    ... on HeadRefDeletedEvent { createdAt actor { login } }
    ... on HeadRefRestoredEvent { createdAt actor { login } }
    ... on BaseRefChangedEvent { createdAt actor { login } previousRefName currentRefName }
    ... on BaseRefForcePushedEvent { createdAt actor { login } }
    ... on LabeledEvent { createdAt actor { login } label { name } }
    ... on UnlabeledEvent { createdAt actor { login } label { name } }
    ... on AssignedEvent { createdAt actor { login } assignee { ... on Actor { login } } }
    ... on UnassignedEvent { createdAt actor { login } assignee { ... on Actor { login } } }
    ... on RenamedTitleEvent { createdAt actor { login } previousTitle currentTitle }
    ... on CrossReferencedEvent {
      createdAt
      actor { login }
      source {
        ... on Issue { number repository { nameWithOwner } }
        ... on PullRequest { number repository { nameWithOwner } }
      }
    }
    ... on MilestonedEvent { createdAt actor { login } milestoneTitle }
    ... on DemilestonedEvent { createdAt actor { login } milestoneTitle }
    ... on AutoMergeEnabledEvent { createdAt actor { login } }
    ... on AutoMergeDisabledEvent { createdAt actor { login } }
    ... on AddedToMergeQueueEvent { createdAt actor { login } }
    ... on RemovedFromMergeQueueEvent { createdAt actor { login } }
  }
}
"#
    };
}

/// The timeline item types the detail view shows. Everything else
/// (subscriptions, mentions, pins, …) is noise.
pub const TIMELINE_ITEM_TYPES: &[&str] = &[
    "ISSUE_COMMENT",
    "PULL_REQUEST_REVIEW",
    "PULL_REQUEST_COMMIT",
    "MERGED_EVENT",
    "CLOSED_EVENT",
    "REOPENED_EVENT",
    "READY_FOR_REVIEW_EVENT",
    "CONVERT_TO_DRAFT_EVENT",
    "REVIEW_REQUESTED_EVENT",
    "REVIEW_REQUEST_REMOVED_EVENT",
    "REVIEW_DISMISSED_EVENT",
    "DEPLOYED_EVENT",
    "DEPLOYMENT_ENVIRONMENT_CHANGED_EVENT",
    "HEAD_REF_FORCE_PUSHED_EVENT",
    "HEAD_REF_DELETED_EVENT",
    "HEAD_REF_RESTORED_EVENT",
    "BASE_REF_CHANGED_EVENT",
    "BASE_REF_FORCE_PUSHED_EVENT",
    "LABELED_EVENT",
    "UNLABELED_EVENT",
    "ASSIGNED_EVENT",
    "UNASSIGNED_EVENT",
    "RENAMED_TITLE_EVENT",
    "CROSS_REFERENCED_EVENT",
    "MILESTONED_EVENT",
    "DEMILESTONED_EVENT",
    "AUTO_MERGE_ENABLED_EVENT",
    "AUTO_MERGE_DISABLED_EVENT",
    "ADDED_TO_MERGE_QUEUE_EVENT",
    "REMOVED_FROM_MERGE_QUEUE_EVENT",
];

pub const PULL_REQUEST_DETAIL: &str = concat!(
    r#"
query PullRequestDetail(
  $owner: String!
  $name: String!
  $number: Int!
  $itemTypes: [PullRequestTimelineItemsItemType!]
) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      id
      number
      title
      url
      state
      isDraft
      body
      createdAt
      additions
      deletions
      changedFiles
      reviewDecision
      mergeable
      mergeStateStatus
      baseRefName
      headRefName
      isCrossRepository
      headRepositoryOwner { login }
      author { ...ActorFields }
      commits { totalCount }
      lastCommit: commits(last: 1) {
        nodes {
          commit {
            statusCheckRollup {
              state
              contexts(first: 100) {
                checkRunCountsByState { state count }
                statusContextCountsByState { state count }
                nodes {
                  __typename
                  ... on CheckRun {
                    name
                    status
                    conclusion
                    detailsUrl
                    checkSuite { workflowRun { workflow { name } } }
                  }
                  ... on StatusContext { context state targetUrl }
                }
              }
            }
          }
        }
      }
      reviewRequests(first: 25) {
        nodes { requestedReviewer { ...ReviewerFields } }
      }
      latestReviews(first: 50) {
        nodes { state author { ...ActorFields } }
      }
      latestOpinionatedReviews(first: 50) {
        nodes { state author { ...ActorFields } }
      }
      assignees(first: 25) { nodes { login avatarUrl(size: 64) } }
      labels(first: 25) { nodes { name color } }
      reviewThreads(first: 100) {
        nodes {
          id
          path
          line
          originalLine
          isResolved
          isOutdated
          comments(first: 50) {
            nodes {
              id
              url
              body
              createdAt
              diffHunk
              author { ...ActorFields }
              pullRequestReview { id }
            }
          }
        }
      }
      timelineItems(first: 100, itemTypes: $itemTypes) { ...TimelineFields }
    }
  }
}
"#,
    detail_fragments!()
);

/// Later pages of a pull request's timeline.
pub const PULL_REQUEST_TIMELINE: &str = concat!(
    r#"
query PullRequestTimeline(
  $owner: String!
  $name: String!
  $number: Int!
  $itemTypes: [PullRequestTimelineItemsItemType!]
  $after: String!
) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      timelineItems(first: 100, after: $after, itemTypes: $itemTypes) { ...TimelineFields }
    }
  }
}
"#,
    detail_fragments!()
);

#[derive(Deserialize)]
pub struct ViewerData {
    pub viewer: User,
}

#[derive(Deserialize)]
pub struct SearchData {
    pub search: SearchConnection,
}

#[derive(Deserialize)]
pub struct SearchConnection {
    pub nodes: Vec<SearchNode>,
}

/// Search results can include non-PR nodes; those deserialize as `Other`.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum SearchNode {
    PullRequest(PullRequestNode),
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestNode {
    id: String,
    number: u64,
    title: String,
    url: String,
    state: PullRequestState,
    is_draft: bool,
    review_decision: Option<ReviewDecision>,
    additions: u64,
    deletions: u64,
    updated_at: DateTime<Utc>,
    author: Option<Login>,
    repository: RepositoryName,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryName {
    name_with_owner: String,
}

impl From<PullRequestNode> for PullRequest {
    fn from(node: PullRequestNode) -> Self {
        Self {
            id: node.id,
            number: node.number,
            title: node.title,
            url: node.url,
            repository: node.repository.name_with_owner,
            author: node.author.map(|a| a.login),
            state: node.state,
            is_draft: node.is_draft,
            review_decision: node.review_decision,
            additions: node.additions,
            deletions: node.deletions,
            updated_at: node.updated_at,
        }
    }
}

#[derive(Deserialize)]
pub struct RepositoryData {
    pub repository: Option<RepositoryNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryNode {
    name_with_owner: String,
    is_private: bool,
    viewer_permission: Option<String>,
    parent: Option<RepositoryName>,
}

impl TryFrom<RepositoryNode> for Repository {
    type Error = anyhow::Error;

    fn try_from(node: RepositoryNode) -> Result<Self, Self::Error> {
        Ok(Self {
            repo: node.name_with_owner.parse()?,
            is_private: node.is_private,
            viewer_permission: node.viewer_permission,
            parent: node
                .parent
                .map(|p| p.name_with_owner.parse::<RepoRef>())
                .transpose()?,
        })
    }
}

// --- Pull request detail ---------------------------------------------------

/// A connection's `nodes`. GitHub returns `null` for nodes the viewer can't
/// see, so those are skipped.
#[derive(Deserialize)]
pub struct Nodes<T> {
    nodes: Vec<Option<T>>,
}

impl<T> IntoIterator for Nodes<T> {
    type Item = T;
    type IntoIter = std::iter::Flatten<std::vec::IntoIter<Option<T>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.nodes.into_iter().flatten()
    }
}

#[derive(Deserialize)]
pub struct PullRequestDetailData {
    pub repository: Option<DetailRepository<PullRequestDetailNode>>,
}

#[derive(Deserialize)]
pub struct PullRequestTimelineData {
    pub repository: Option<DetailRepository<TimelineOnly>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailRepository<T> {
    pub pull_request: Option<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineOnly {
    pub timeline_items: TimelinePage,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePage {
    pub page_info: PageInfo,
    pub nodes: Vec<Option<TimelineNode>>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActorNode {
    #[serde(rename = "__typename")]
    typename: String,
    login: String,
    avatar_url: Option<String>,
}

impl From<ActorNode> for Actor {
    fn from(node: ActorNode) -> Self {
        Self {
            is_bot: node.typename == "Bot",
            login: node.login,
            avatar_url: node.avatar_url,
        }
    }
}

/// A user, bot, or team asked to review.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewerNode {
    login: Option<String>,
    name: Option<String>,
    avatar_url: Option<String>,
}

impl ReviewerNode {
    fn name(&self) -> Option<String> {
        self.login.clone().or_else(|| self.name.clone())
    }
}

#[derive(Deserialize)]
struct OptionalLogin {
    login: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDetailNode {
    id: String,
    number: u64,
    title: String,
    url: String,
    state: PullRequestState,
    is_draft: bool,
    body: String,
    created_at: DateTime<Utc>,
    additions: u64,
    deletions: u64,
    changed_files: u64,
    review_decision: Option<ReviewDecision>,
    mergeable: String,
    merge_state_status: String,
    base_ref_name: String,
    head_ref_name: String,
    is_cross_repository: bool,
    head_repository_owner: Option<Login>,
    author: Option<ActorNode>,
    commits: TotalCount,
    last_commit: Nodes<LastCommitNode>,
    review_requests: Nodes<ReviewRequestNode>,
    latest_reviews: Nodes<LatestReviewNode>,
    latest_opinionated_reviews: Nodes<LatestReviewNode>,
    assignees: Nodes<AssigneeNode>,
    labels: Nodes<LabelNode>,
    review_threads: Nodes<ReviewThreadNode>,
    pub timeline_items: TimelinePage,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TotalCount {
    total_count: u64,
}

#[derive(Deserialize)]
struct LastCommitNode {
    commit: LastCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LastCommit {
    status_check_rollup: Option<StatusCheckRollup>,
}

#[derive(Deserialize)]
struct StatusCheckRollup {
    contexts: CheckContexts,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckContexts {
    check_run_counts_by_state: Option<Vec<StateCount>>,
    status_context_counts_by_state: Option<Vec<StateCount>>,
    nodes: Vec<Option<CheckContextNode>>,
}

#[derive(Deserialize)]
struct StateCount {
    state: String,
    count: u64,
}

fn check_run_status(status: &str, conclusion: Option<&str>) -> CheckStatus {
    match (status, conclusion) {
        ("COMPLETED", Some("SUCCESS")) => CheckStatus::Success,
        ("COMPLETED", Some("NEUTRAL" | "SKIPPED" | "STALE") | None) => CheckStatus::Neutral,
        ("COMPLETED", Some(_)) => CheckStatus::Failure,
        _ => CheckStatus::Pending,
    }
}

fn status_context_status(state: &str) -> CheckStatus {
    match state {
        "SUCCESS" => CheckStatus::Success,
        "PENDING" | "EXPECTED" => CheckStatus::Pending,
        _ => CheckStatus::Failure,
    }
}

impl CheckContexts {
    fn summary(&self) -> CheckSummary {
        let mut summary = CheckSummary::default();
        // Check runs are counted by one state that merges status and
        // conclusion.
        for count in self.check_run_counts_by_state.iter().flatten() {
            let status = match count.state.as_str() {
                "SUCCESS" => CheckStatus::Success,
                "NEUTRAL" | "SKIPPED" | "STALE" | "COMPLETED" => CheckStatus::Neutral,
                "QUEUED" | "IN_PROGRESS" | "PENDING" | "WAITING" | "REQUESTED" => {
                    CheckStatus::Pending
                }
                _ => CheckStatus::Failure,
            };
            summary.add(status, count.count);
        }
        for count in self.status_context_counts_by_state.iter().flatten() {
            summary.add(status_context_status(&count.state), count.count);
        }
        summary
    }
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum CheckContextNode {
    CheckRun(CheckRunNode),
    StatusContext(StatusContextNode),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckRunNode {
    name: String,
    status: String,
    conclusion: Option<String>,
    details_url: Option<String>,
    check_suite: Option<CheckSuite>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckSuite {
    workflow_run: Option<WorkflowRun>,
}

#[derive(Deserialize)]
struct WorkflowRun {
    workflow: NameOnly,
}

#[derive(Deserialize)]
struct NameOnly {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusContextNode {
    context: String,
    state: String,
    target_url: Option<String>,
}

impl CheckContextNode {
    fn into_check(self) -> Option<Check> {
        match self {
            Self::CheckRun(run) => {
                let status = check_run_status(&run.status, run.conclusion.as_deref());
                // GitHub prefixes Actions checks with their workflow.
                let name = match run.check_suite.and_then(|s| s.workflow_run) {
                    Some(run_) => format!("{} / {}", run_.workflow.name, run.name),
                    None => run.name,
                };
                Some(Check {
                    name,
                    status,
                    url: run.details_url,
                })
            }
            Self::StatusContext(context) => Some(Check {
                name: context.context,
                status: status_context_status(&context.state),
                url: context.target_url,
            }),
            Self::Other => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewRequestNode {
    requested_reviewer: Option<ReviewerNode>,
}

#[derive(Deserialize)]
struct LatestReviewNode {
    state: ReviewState,
    author: Option<ActorNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssigneeNode {
    login: String,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct LabelNode {
    name: String,
    color: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewThreadNode {
    id: String,
    path: String,
    line: Option<u64>,
    original_line: Option<u64>,
    is_resolved: bool,
    is_outdated: bool,
    comments: Nodes<ReviewCommentNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewCommentNode {
    id: String,
    url: String,
    body: String,
    created_at: DateTime<Utc>,
    diff_hunk: String,
    author: Option<ActorNode>,
    pull_request_review: Option<IdOnly>,
}

#[derive(Deserialize)]
struct IdOnly {
    id: String,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
pub enum TimelineNode {
    IssueComment(IssueCommentNode),
    PullRequestReview(ReviewNode),
    PullRequestCommit(CommitNode),
    MergedEvent(EventNode),
    ClosedEvent(EventNode),
    ReopenedEvent(EventNode),
    ReadyForReviewEvent(EventNode),
    ConvertToDraftEvent(EventNode),
    ReviewRequestedEvent(EventNode),
    ReviewRequestRemovedEvent(EventNode),
    ReviewDismissedEvent(EventNode),
    DeployedEvent(EventNode),
    DeploymentEnvironmentChangedEvent(EventNode),
    HeadRefForcePushedEvent(EventNode),
    HeadRefDeletedEvent(EventNode),
    HeadRefRestoredEvent(EventNode),
    BaseRefChangedEvent(EventNode),
    BaseRefForcePushedEvent(EventNode),
    LabeledEvent(EventNode),
    UnlabeledEvent(EventNode),
    AssignedEvent(EventNode),
    UnassignedEvent(EventNode),
    RenamedTitleEvent(EventNode),
    CrossReferencedEvent(EventNode),
    MilestonedEvent(EventNode),
    DemilestonedEvent(EventNode),
    AutoMergeEnabledEvent(EventNode),
    AutoMergeDisabledEvent(EventNode),
    AddedToMergeQueueEvent(EventNode),
    RemovedFromMergeQueueEvent(EventNode),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCommentNode {
    id: String,
    url: String,
    body: String,
    created_at: DateTime<Utc>,
    is_minimized: bool,
    minimized_reason: Option<String>,
    author: Option<ActorNode>,
}

impl From<IssueCommentNode> for Comment {
    fn from(node: IssueCommentNode) -> Self {
        Self {
            id: node.id,
            url: node.url,
            author: node.author.map(Into::into),
            body: node.body,
            created_at: node.created_at,
            minimized: node
                .is_minimized
                .then(|| node.minimized_reason.unwrap_or_default().to_lowercase()),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewNode {
    id: String,
    url: String,
    state: ReviewState,
    body: String,
    created_at: DateTime<Utc>,
    submitted_at: Option<DateTime<Utc>>,
    author: Option<ActorNode>,
}

#[derive(Deserialize)]
pub struct CommitNode {
    id: String,
    url: String,
    commit: CommitFields,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitFields {
    abbreviated_oid: String,
    message_headline: String,
    committed_date: DateTime<Utc>,
    author: Option<GitActor>,
}

#[derive(Deserialize)]
struct GitActor {
    name: Option<String>,
    user: Option<Login>,
}

/// Every event type shares this shape; each fills in its own fields.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventNode {
    id: String,
    created_at: DateTime<Utc>,
    actor: Option<Login>,
    commit: Option<AbbreviatedOid>,
    merge_ref_name: Option<String>,
    requested_reviewer: Option<ReviewerNode>,
    review: Option<DismissedReview>,
    deployment: Option<Deployment>,
    deployment_status: Option<DeploymentStatus>,
    previous_ref_name: Option<String>,
    current_ref_name: Option<String>,
    label: Option<NameOnly>,
    assignee: Option<OptionalLogin>,
    previous_title: Option<String>,
    current_title: Option<String>,
    source: Option<ReferenceSource>,
    milestone_title: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AbbreviatedOid {
    abbreviated_oid: String,
}

#[derive(Deserialize)]
struct DismissedReview {
    author: Option<Login>,
}

#[derive(Deserialize)]
struct Deployment {
    environment: Option<String>,
}

#[derive(Deserialize)]
struct DeploymentStatus {
    deployment: Deployment,
}

#[derive(Deserialize)]
struct ReferenceSource {
    number: Option<u64>,
    repository: Option<RepositoryName>,
}

impl TimelineNode {
    fn into_item(self) -> Option<TimelineItem> {
        use EventKind as K;
        let (node, kind): (EventNode, fn(&mut EventNode) -> EventKind) = match self {
            Self::IssueComment(node) => return Some(TimelineItem::Comment(node.into())),
            Self::PullRequestReview(node) => {
                // The viewer's unsubmitted review isn't part of the conversation yet.
                if node.state == ReviewState::Pending {
                    return None;
                }
                return Some(TimelineItem::Review(Review {
                    id: node.id,
                    url: node.url,
                    author: node.author.map(Into::into),
                    state: node.state,
                    body: node.body,
                    submitted_at: node.submitted_at.unwrap_or(node.created_at),
                    threads: Vec::new(),
                }));
            }
            Self::PullRequestCommit(node) => {
                let author = node.commit.author;
                return Some(TimelineItem::Commit(Commit {
                    id: node.id,
                    url: node.url,
                    short_oid: node.commit.abbreviated_oid,
                    headline: node.commit.message_headline,
                    author: author
                        .as_ref()
                        .and_then(|a| a.user.as_ref().map(|u| u.login.clone()))
                        .or_else(|| author.and_then(|a| a.name)),
                    committed_at: node.commit.committed_date,
                }));
            }
            Self::MergedEvent(node) => (node, |n| K::Merged {
                commit: n.commit.take().map(|c| c.abbreviated_oid),
                base: n.merge_ref_name.take().unwrap_or_default(),
            }),
            Self::ClosedEvent(node) => (node, |_| K::Closed),
            Self::ReopenedEvent(node) => (node, |_| K::Reopened),
            Self::ReadyForReviewEvent(node) => (node, |_| K::ReadyForReview),
            Self::ConvertToDraftEvent(node) => (node, |_| K::ConvertedToDraft),
            Self::ReviewRequestedEvent(node) => (node, |n| K::ReviewRequested {
                reviewer: n.requested_reviewer.as_ref().and_then(ReviewerNode::name),
            }),
            Self::ReviewRequestRemovedEvent(node) => (node, |n| K::ReviewRequestRemoved {
                reviewer: n.requested_reviewer.as_ref().and_then(ReviewerNode::name),
            }),
            Self::ReviewDismissedEvent(node) => (node, |n| K::ReviewDismissed {
                reviewer: n.review.take().and_then(|r| r.author).map(|a| a.login),
            }),
            Self::DeployedEvent(node) => (node, |n| K::Deployed {
                environment: n.deployment.take().and_then(|d| d.environment),
            }),
            Self::DeploymentEnvironmentChangedEvent(node) => (node, |n| K::Deployed {
                environment: n
                    .deployment_status
                    .take()
                    .and_then(|s| s.deployment.environment),
            }),
            Self::HeadRefForcePushedEvent(node) => (node, |_| K::HeadRefForcePushed),
            Self::HeadRefDeletedEvent(node) => (node, |_| K::HeadRefDeleted),
            Self::HeadRefRestoredEvent(node) => (node, |_| K::HeadRefRestored),
            Self::BaseRefChangedEvent(node) => (node, |n| K::BaseRefChanged {
                from: n.previous_ref_name.take().unwrap_or_default(),
                to: n.current_ref_name.take().unwrap_or_default(),
            }),
            Self::BaseRefForcePushedEvent(node) => (node, |_| K::BaseRefForcePushed),
            Self::LabeledEvent(node) => (node, |n| K::Labeled {
                label: n.label.take().map(|l| l.name).unwrap_or_default(),
            }),
            Self::UnlabeledEvent(node) => (node, |n| K::Unlabeled {
                label: n.label.take().map(|l| l.name).unwrap_or_default(),
            }),
            Self::AssignedEvent(node) => (node, |n| K::Assigned {
                assignee: n.assignee.take().and_then(|a| a.login),
            }),
            Self::UnassignedEvent(node) => (node, |n| K::Unassigned {
                assignee: n.assignee.take().and_then(|a| a.login),
            }),
            Self::RenamedTitleEvent(node) => (node, |n| K::Renamed {
                from: n.previous_title.take().unwrap_or_default(),
                to: n.current_title.take().unwrap_or_default(),
            }),
            Self::CrossReferencedEvent(node) => (node, |n| K::CrossReferenced {
                source: n
                    .source
                    .take()
                    .and_then(|s| Some(format!("{}#{}", s.repository?.name_with_owner, s.number?))),
            }),
            Self::MilestonedEvent(node) => (node, |n| K::Milestoned {
                milestone: n.milestone_title.take().unwrap_or_default(),
            }),
            Self::DemilestonedEvent(node) => (node, |n| K::Demilestoned {
                milestone: n.milestone_title.take().unwrap_or_default(),
            }),
            Self::AutoMergeEnabledEvent(node) => (node, |_| K::AutoMergeEnabled),
            Self::AutoMergeDisabledEvent(node) => (node, |_| K::AutoMergeDisabled),
            Self::AddedToMergeQueueEvent(node) => (node, |_| K::AddedToMergeQueue),
            Self::RemovedFromMergeQueueEvent(node) => (node, |_| K::RemovedFromMergeQueue),
            Self::Other => return None,
        };
        let mut node = node;
        let kind = kind(&mut node);
        Some(TimelineItem::Event(Event {
            id: node.id,
            actor: node.actor.map(|a| a.login),
            created_at: node.created_at,
            kind,
        }))
    }
}

impl PullRequestDetailNode {
    /// Builds the detail from this response and the rest of the timeline,
    /// fetched separately.
    pub fn into_detail(self, timeline: Vec<TimelineNode>) -> PullRequestDetail {
        let author_login = self.author.as_ref().map(|a| a.login.clone());

        let mut reviewers: Vec<Reviewer> = Vec::new();
        let mut add_reviewer = |reviewer: Reviewer| {
            let is_author = author_login.as_deref() == Some(reviewer.name.as_str());
            if !is_author && !reviewers.iter().any(|r| r.name == reviewer.name) {
                reviewers.push(reviewer);
            }
        };
        // A pending request wins over an earlier review: it's waiting again.
        for request in self.review_requests {
            if let Some(reviewer) = request.requested_reviewer
                && let Some(name) = reviewer.name()
            {
                add_reviewer(Reviewer {
                    name,
                    avatar_url: reviewer.avatar_url,
                    status: ReviewerStatus::Requested,
                });
            }
        }
        let reviews = self
            .latest_opinionated_reviews
            .into_iter()
            .chain(self.latest_reviews);
        for review in reviews {
            let status = match review.state {
                ReviewState::Approved => ReviewerStatus::Approved,
                ReviewState::ChangesRequested => ReviewerStatus::ChangesRequested,
                ReviewState::Commented => ReviewerStatus::Commented,
                ReviewState::Pending | ReviewState::Dismissed => continue,
            };
            if let Some(author) = review.author {
                add_reviewer(Reviewer {
                    name: author.login,
                    avatar_url: author.avatar_url,
                    status,
                });
            }
        }

        let contexts = self
            .last_commit
            .into_iter()
            .find_map(|c| c.commit.status_check_rollup)
            .map(|rollup| rollup.contexts);
        let check_summary = contexts
            .as_ref()
            .map(CheckContexts::summary)
            .unwrap_or_default();
        let mut checks: Vec<Check> = contexts
            .into_iter()
            .flat_map(|contexts| contexts.nodes)
            .flatten()
            .filter_map(CheckContextNode::into_check)
            .collect();
        checks.sort_by_key(|c| c.status);

        let mut timeline: Vec<TimelineItem> = self
            .timeline_items
            .nodes
            .into_iter()
            .flatten()
            .chain(timeline)
            .filter_map(TimelineNode::into_item)
            .collect();
        attach_threads(&mut timeline, self.review_threads);

        let merge_status = match (self.mergeable.as_str(), self.merge_state_status.as_str()) {
            ("CONFLICTING", _) | (_, "DIRTY") => MergeStatus::Conflicting,
            (_, "CLEAN" | "HAS_HOOKS") => MergeStatus::Clean,
            (_, "UNSTABLE") => MergeStatus::Unstable,
            (_, "BEHIND") => MergeStatus::Behind,
            (_, "BLOCKED") => MergeStatus::Blocked,
            (_, "DRAFT") => MergeStatus::Draft,
            _ => MergeStatus::Unknown,
        };

        PullRequestDetail {
            id: self.id,
            number: self.number,
            title: self.title,
            url: self.url,
            author: self.author.map(Into::into),
            body: self.body,
            state: self.state,
            is_draft: self.is_draft,
            created_at: self.created_at,
            base_ref: self.base_ref_name,
            head_ref: match self.head_repository_owner {
                Some(owner) if self.is_cross_repository => {
                    format!("{}:{}", owner.login, self.head_ref_name)
                }
                _ => self.head_ref_name,
            },
            additions: self.additions,
            deletions: self.deletions,
            changed_files: self.changed_files,
            commit_count: self.commits.total_count,
            review_decision: self.review_decision,
            merge_status,
            checks,
            check_summary,
            reviewers,
            assignees: self
                .assignees
                .into_iter()
                .map(|a| Actor {
                    login: a.login,
                    avatar_url: a.avatar_url,
                    is_bot: false,
                })
                .collect(),
            labels: self
                .labels
                .into_iter()
                .map(|l| Label {
                    color: u32::from_str_radix(&l.color, 16).unwrap_or(0x808080),
                    name: l.name,
                })
                .collect(),
            timeline,
        }
    }
}

/// Hangs each review thread off the review that started it.
fn attach_threads(timeline: &mut [TimelineItem], threads: Nodes<ReviewThreadNode>) {
    for thread in threads {
        let comments: Vec<ReviewCommentNode> = thread.comments.into_iter().collect();
        let Some(review_id) = comments
            .first()
            .and_then(|c| c.pull_request_review.as_ref())
            .map(|r| r.id.clone())
        else {
            continue;
        };
        let Some(review) = timeline.iter_mut().find_map(|item| match item {
            TimelineItem::Review(review) if review.id == review_id => Some(review),
            _ => None,
        }) else {
            continue;
        };
        review.threads.push(ReviewThread {
            id: thread.id,
            path: thread.path,
            line: thread.line.or(thread.original_line),
            is_resolved: thread.is_resolved,
            is_outdated: thread.is_outdated,
            diff_hunk: comments
                .first()
                .map(|c| c.diff_hunk.clone())
                .unwrap_or_default(),
            comments: comments
                .into_iter()
                .map(|c| Comment {
                    id: c.id,
                    url: c.url,
                    author: c.author.map(Into::into),
                    body: c.body,
                    created_at: c.created_at,
                    minimized: None,
                })
                .collect(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bob re-requested a review after approving; the other request is a team
    // the token can't see, which GitHub nulls.
    const RESPONSE: &str = r#"{
      "id": "PR_1", "number": 7, "title": "Fix it", "url": "https://github.com/o/r/pull/7",
      "state": "OPEN", "isDraft": false, "body": "Body", "createdAt": "2026-01-01T00:00:00Z",
      "additions": 1, "deletions": 2, "changedFiles": 3, "reviewDecision": "REVIEW_REQUIRED",
      "mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED",
      "baseRefName": "main", "headRefName": "fix", "isCrossRepository": true,
      "headRepositoryOwner": { "login": "fork" },
      "author": { "__typename": "User", "login": "alice", "avatarUrl": null },
      "commits": { "totalCount": 1 },
      "lastCommit": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": {
        "checkRunCountsByState": [
          { "state": "SUCCESS", "count": 150 },
          { "state": "FAILURE", "count": 1 }
        ],
        "statusContextCountsByState": [],
        "nodes": [
          { "__typename": "CheckRun", "name": "build", "status": "COMPLETED",
            "conclusion": "SUCCESS", "detailsUrl": null,
            "checkSuite": { "workflowRun": { "workflow": { "name": "CI" } } } },
          { "__typename": "CheckRun", "name": "lint", "status": "COMPLETED",
            "conclusion": "FAILURE", "detailsUrl": null, "checkSuite": null }
        ]
      } } } }] },
      "reviewRequests": { "nodes": [
        { "requestedReviewer": { "__typename": "User", "login": "bob", "avatarUrl": null } },
        { "requestedReviewer": null }
      ] },
      "latestReviews": { "nodes": [
        { "state": "COMMENTED", "author": { "__typename": "User", "login": "dave", "avatarUrl": null } },
        { "state": "COMMENTED", "author": { "__typename": "User", "login": "alice", "avatarUrl": null } }
      ] },
      "latestOpinionatedReviews": { "nodes": [
        { "state": "APPROVED", "author": { "__typename": "User", "login": "bob", "avatarUrl": null } }
      ] },
      "assignees": { "nodes": [] },
      "labels": { "nodes": [{ "name": "bug", "color": "d73a4a" }] },
      "reviewThreads": { "nodes": [{
        "id": "T_1", "path": "src/lib.rs", "line": null, "originalLine": 4,
        "isResolved": false, "isOutdated": true,
        "comments": { "nodes": [{
          "id": "C_1", "url": "", "body": "Nit", "createdAt": "2026-01-01T00:00:00Z",
          "diffHunk": "@@ -1 +1 @@\n-a\n+b",
          "author": { "__typename": "User", "login": "dave", "avatarUrl": null },
          "pullRequestReview": { "id": "R_1" }
        }] }
      }] },
      "timelineItems": {
        "pageInfo": { "hasNextPage": false, "endCursor": null },
        "nodes": [
          { "__typename": "IssueComment", "id": "IC_1", "url": "", "body": "Deployed",
            "createdAt": "2026-01-01T00:00:00Z", "isMinimized": false, "minimizedReason": null,
            "author": { "__typename": "Bot", "login": "vercel", "avatarUrl": null } },
          { "__typename": "PullRequestReview", "id": "R_1", "url": "", "state": "COMMENTED",
            "body": "", "createdAt": "2026-01-01T00:00:00Z", "submittedAt": null,
            "author": { "__typename": "User", "login": "dave", "avatarUrl": null } },
          { "__typename": "LabeledEvent", "id": "E_1", "createdAt": "2026-01-01T00:00:00Z",
            "actor": { "login": "alice" }, "label": { "name": "bug" } },
          { "__typename": "SubscribedEvent", "id": "E_2" },
          null
        ]
      }
    }"#;

    #[test]
    fn builds_detail_from_response() {
        let node: PullRequestDetailNode = serde_json::from_str(RESPONSE).unwrap();
        let pr = node.into_detail(Vec::new());

        assert_eq!(pr.head_ref, "fork:fix");
        assert_eq!(pr.merge_status, MergeStatus::Blocked);
        assert_eq!(pr.labels[0].color, 0xd73a4a);

        let reviewers: Vec<_> = pr
            .reviewers
            .iter()
            .map(|r| (r.name.as_str(), r.status))
            .collect();
        assert_eq!(
            reviewers,
            [
                ("bob", ReviewerStatus::Requested),
                ("dave", ReviewerStatus::Commented)
            ]
        );

        assert_eq!(pr.checks[0].name, "lint");
        assert_eq!(pr.checks[1].name, "CI / build");
        assert_eq!(
            (pr.check_summary.passing, pr.check_summary.failing),
            (150, 1)
        );

        assert_eq!(pr.timeline.len(), 3);
        let TimelineItem::Comment(comment) = &pr.timeline[0] else {
            panic!("expected a comment");
        };
        assert!(comment.is_by_bot());
        let TimelineItem::Review(review) = &pr.timeline[1] else {
            panic!("expected a review");
        };
        assert_eq!(review.threads.len(), 1);
        assert_eq!(review.threads[0].line, Some(4));
        assert!(matches!(
            &pr.timeline[2],
            TimelineItem::Event(Event { kind: EventKind::Labeled { label }, .. }) if label == "bug"
        ));
    }
}
