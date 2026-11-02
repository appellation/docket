//! GraphQL documents and their response shapes.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::models::{PullRequest, PullRequestState, ReviewDecision, User};

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
    repository: Repository,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
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
