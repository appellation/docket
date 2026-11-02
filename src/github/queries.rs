//! GraphQL documents and their response shapes.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::models::{PullRequest, PullRequestState, RepoRef, Repository, ReviewDecision, User};

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
        baseRefName
        baseRefOid
        headRefName
        headRefOid
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
    PullRequest(Box<PullRequestNode>),
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
    base_ref_name: String,
    base_ref_oid: String,
    head_ref_name: String,
    head_ref_oid: String,
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
            base_ref_name: node.base_ref_name,
            base_ref_oid: node.base_ref_oid,
            head_ref_name: node.head_ref_name,
            head_ref_oid: node.head_ref_oid,
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
