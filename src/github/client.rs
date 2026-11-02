//! Blocking GitHub GraphQL client. Run calls on GPUI's background executor.

use anyhow::{Context as _, Result};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::models::{PullRequest, RepoAccessError, RepoRef, Repository, User};
use super::queries::{self, RepositoryData, SearchData, SearchNode, ViewerData};
use crate::auth::AccessToken;
use crate::config::GITHUB_API_URL;

const USER_AGENT: &str = concat!("docket/", env!("CARGO_PKG_VERSION"));
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

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
    /// e.g. `NOT_FOUND`, `FORBIDDEN`
    #[serde(rename = "type")]
    kind: Option<String>,
}

/// Errors GraphQL reported in the response body.
#[derive(Debug)]
struct GraphqlErrors(Vec<GraphqlError>);

impl GraphqlErrors {
    fn has_kind(&self, kinds: &[&str]) -> bool {
        self.0
            .iter()
            .any(|e| e.kind.as_deref().is_some_and(|k| kinds.contains(&k)))
    }
}

impl std::fmt::Display for GraphqlErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let messages: Vec<_> = self.0.iter().map(|e| e.message.as_str()).collect();
        write!(f, "GraphQL error: {}", messages.join("; "))
    }
}

impl std::error::Error for GraphqlErrors {}

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

    /// Looks up a repository the viewer can access. If GitHub won't show it,
    /// the error is a [`RepoAccessError`] explaining why where possible.
    pub fn repository(&self, repo: &RepoRef) -> Result<Repository> {
        let result: Result<RepositoryData> = self.graphql(
            queries::REPOSITORY,
            json!({ "owner": repo.owner, "name": repo.name }),
        );
        match result {
            Ok(RepositoryData {
                repository: Some(node),
            }) => node.try_into(),
            Ok(RepositoryData { repository: None }) => Err(self.diagnose_access(repo)?.into()),
            Err(err)
                if err
                    .downcast_ref::<GraphqlErrors>()
                    .is_some_and(|e| e.has_kind(&["NOT_FOUND", "FORBIDDEN"])) =>
            {
                Err(self.diagnose_access(repo)?.into())
            }
            Err(err) => Err(err),
        }
    }

    /// GraphQL hides repositories the token can't reach, whatever the reason.
    /// REST says why: SAML SSO not authorized for this token, or the org
    /// restricting OAuth apps it hasn't approved.
    fn diagnose_access(&self, repo: &RepoRef) -> Result<RepoAccessError> {
        let mut response = ureq::get(format!(
            "{GITHUB_API_URL}/repos/{}/{}",
            repo.owner, repo.name
        ))
        .config()
        .http_status_as_error(false)
        .build()
        .header("Authorization", &format!("Bearer {}", self.token.secret()))
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()?;

        // `X-GitHub-SSO: required; url=https://github.com/orgs/<org>/sso?...`
        let sso_url = response
            .headers()
            .get("x-github-sso")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once("url="))
            .map(|(_, url)| url.trim().to_owned());
        if let Some(url) = sso_url {
            return Ok(RepoAccessError::SsoRequired {
                org: repo.owner.clone(),
                url,
            });
        }

        let status = response.status().as_u16();
        let message = response
            .body_mut()
            .read_json::<Value>()
            .ok()
            .and_then(|body| body["message"].as_str().map(str::to_owned))
            .unwrap_or_default();
        Ok(match status {
            403 if message.contains("OAuth App access restrictions") => {
                RepoAccessError::OauthAppRestricted {
                    org: repo.owner.clone(),
                }
            }
            404 => RepoAccessError::NotFound(repo.clone()),
            _ => RepoAccessError::Other(format!("GitHub returned {status}: {message}")),
        })
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
            return Err(GraphqlErrors(response.errors).into());
        }
        response.data.context("GraphQL response had no data")
    }
}
