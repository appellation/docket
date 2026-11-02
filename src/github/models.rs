//! Domain types the UI works with, decoupled from the wire format.

use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub login: String,
    pub name: Option<String>,
    #[serde(rename = "avatarUrl")]
    pub avatar_url: String,
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
    pub author: Option<String>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub review_decision: Option<ReviewDecision>,
    pub additions: u64,
    pub deletions: u64,
    pub updated_at: DateTime<Utc>,
}
