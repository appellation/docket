//! Blocking GitHub GraphQL client. Run calls on GPUI's background executor.

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::models::{PullRequest, User};
use super::queries::{self, SearchData, SearchNode, ViewerData};
use crate::auth::AccessToken;
use crate::config::GITHUB_API_URL;

const USER_AGENT: &str = concat!("gh-reviews/", env!("CARGO_PKG_VERSION"));
const PAGE_SIZE: u32 = 50;

#[derive(Clone, Debug)]
pub struct GithubClient {
    token: AccessToken,
}

#[derive(Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
}

impl GithubClient {
    pub fn new(token: AccessToken) -> Self {
        Self { token }
    }

    pub fn viewer(&self) -> Result<User> {
        let data: ViewerData = self.graphql(queries::VIEWER, json!({}))?;
        Ok(data.viewer)
    }

    /// Runs a GitHub search (e.g. `is:open is:pr review-requested:@me`).
    pub fn search_pull_requests(&self, query: &str) -> Result<Vec<PullRequest>> {
        let data: SearchData = self.graphql(
            queries::SEARCH_PULL_REQUESTS,
            json!({ "query": query, "first": PAGE_SIZE }),
        )?;
        Ok(data
            .search
            .nodes
            .into_iter()
            .filter_map(|node| match node {
                SearchNode::PullRequest(pr) => Some(pr.into()),
                SearchNode::Other(_) => None,
            })
            .collect())
    }

    fn graphql<T: DeserializeOwned>(&self, query: &str, variables: Value) -> Result<T> {
        let response: GraphqlResponse<T> = ureq::post(format!("{GITHUB_API_URL}/graphql"))
            .header("Authorization", &format!("Bearer {}", self.token.secret()))
            .header("User-Agent", USER_AGENT)
            .send_json(json!({ "query": query, "variables": variables }))?
            .body_mut()
            .read_json()
            .context("decoding GraphQL response")?;

        if !response.errors.is_empty() {
            let messages: Vec<_> = response.errors.into_iter().map(|e| e.message).collect();
            return Err(anyhow!("GraphQL error: {}", messages.join("; ")));
        }
        response.data.context("GraphQL response had no data")
    }
}
