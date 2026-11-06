//! Blocking GitHub GraphQL client. Run calls on GPUI's background executor.
//!
//! In tests it can answer from a `FakeGithub` instead, and never reaches
//! GitHub.

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

#[cfg(test)]
use super::FakeGithub;
use super::models::{
    PullRequest, PullRequestDetail, PullRequestVersion, Release, ReleaseAsset, RepoAccessError,
    RepoRef, Repository, User, ViewedFiles,
};
use super::queries::{
    self, MergeMethodData, PullRequestDetailData, PullRequestTimelineData, PullRequestVersionData,
    ReleaseData, RepositoryData, SearchData, SearchNode, ViewedFilesData, ViewedFilesPage,
    ViewerData,
};
use crate::auth::AccessToken;
use crate::config::{GITHUB_API_URL, USER_AGENT};

const PAGE_SIZE: u32 = 50;

/// Timeline pages to fetch past the first (100 items each). Enough for all
/// but the longest-running pull requests.
const MAX_TIMELINE_PAGES: usize = 9;

/// Pages of a pull request's files to read viewed states from (100 each):
/// GitHub lists at most 3,000.
const MAX_FILE_PAGES: usize = 30;

/// Errors that only blank out part of a pull request's detail.
const PARTIAL_ERRORS: &[&str] = &["FORBIDDEN", "INSUFFICIENT_SCOPES"];

#[derive(Clone, Debug)]
pub struct GithubClient {
    token: AccessToken,
    #[cfg(test)]
    fake: Option<std::sync::Arc<FakeGithub>>,
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
        Self {
            token,
            #[cfg(test)]
            fake: None,
        }
    }

    /// A client that answers from `fake`.
    #[cfg(test)]
    pub fn fake(fake: std::sync::Arc<FakeGithub>) -> Self {
        Self {
            token: AccessToken::new("fake"),
            fake: Some(fake),
        }
    }

    pub fn viewer(&self) -> Result<User> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.viewer();
        }
        let data: ViewerData = self.graphql(queries::VIEWER, json!({}))?;
        Ok(data.viewer)
    }

    /// Runs a GitHub search (e.g. `is:open is:pr review-requested:@me`).
    pub fn search_pull_requests(&self, query: &str) -> Result<Vec<PullRequest>> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.search_pull_requests(query);
        }
        let data: SearchData = self.graphql(
            queries::SEARCH_PULL_REQUESTS,
            json!({ "query": query, "first": PAGE_SIZE }),
        )?;
        Ok(data
            .search
            .nodes
            .into_iter()
            .filter_map(|node| match node {
                SearchNode::PullRequest(pr) => Some((*pr).into()),
                SearchNode::Other(_) => None,
            })
            .collect())
    }

    /// Fetches everything the detail view shows, including the whole
    /// timeline (up to a limit).
    pub fn pull_request(&self, repo: &RepoRef, number: u64) -> Result<PullRequestDetail> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.pull_request(repo, number);
        }
        let variables = json!({
            "owner": repo.owner,
            "name": repo.name,
            "number": number,
            "itemTypes": queries::TIMELINE_ITEM_TYPES,
        });
        // Parts of a pull request can be out of the token's reach: team
        // reviewers without `read:org`, or references from an org whose SSO
        // the token isn't authorized for. GitHub nulls those and answers the
        // rest.
        let data: PullRequestDetailData = self.graphql_tolerating(
            queries::PULL_REQUEST_DETAIL,
            variables.clone(),
            PARTIAL_ERRORS,
        )?;
        let node = data
            .repository
            .and_then(|r| r.pull_request)
            .with_context(|| format!("{repo}#{number} not found"))?;

        let mut rest = Vec::new();
        let mut page_info = node.timeline_items.page_info.clone();
        for _ in 0..MAX_TIMELINE_PAGES {
            let (true, Some(after)) = (page_info.has_next_page, page_info.end_cursor) else {
                break;
            };
            let mut variables = variables.clone();
            variables["after"] = after.into();
            let data: PullRequestTimelineData =
                self.graphql_tolerating(queries::PULL_REQUEST_TIMELINE, variables, PARTIAL_ERRORS)?;
            let Some(page) = data
                .repository
                .and_then(|r| r.pull_request)
                .map(|pr| pr.timeline_items)
            else {
                break;
            };
            rest.extend(page.nodes.into_iter().flatten());
            page_info = page.page_info;
        }
        Ok(node.into_detail(rest))
    }

    /// The pull request's version, which changes whenever anything
    /// [`Self::pull_request`] returns does. Far cheaper than fetching it.
    /// With it, the files the viewer has marked as viewed, when they fit
    /// on one page; otherwise [`Self::viewed_files`] has them all.
    pub fn pull_request_version(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<(PullRequestVersion, Option<ViewedFiles>)> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.pull_request_version(repo, number);
        }
        let data: PullRequestVersionData = self.graphql_tolerating(
            queries::PULL_REQUEST_VERSION,
            json!({ "owner": repo.owner, "name": repo.name, "number": number }),
            PARTIAL_ERRORS,
        )?;
        let mut version = data
            .repository
            .and_then(|r| r.pull_request)
            .with_context(|| format!("{repo}#{number} not found"))?;
        let viewed = version
            .as_object_mut()
            .and_then(|pr| pr.remove("viewerViewedFiles"))
            .and_then(|page| serde_json::from_value::<ViewedFilesPage>(page).ok())
            .filter(|page| !page.page_info.has_next_page)
            .map(|page| viewed_in(page).collect());
        Ok((PullRequestVersion(version), viewed))
    }

    /// The viewer's viewed state for each of the pull request's files.
    pub fn viewed_files(&self, repo: &RepoRef, number: u64) -> Result<ViewedFiles> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.viewed_files(repo, number);
        }
        let mut variables = json!({ "owner": repo.owner, "name": repo.name, "number": number });
        let mut files = ViewedFiles::new();
        for _ in 0..MAX_FILE_PAGES {
            let data: ViewedFilesData =
                self.graphql(queries::PULL_REQUEST_VIEWED_FILES, variables.clone())?;
            let page = data
                .repository
                .and_then(|r| r.pull_request)
                .and_then(|pr| pr.files)
                .with_context(|| format!("{repo}#{number} not found"))?;
            let next = page.page_info.clone();
            files.extend(viewed_in(page));
            let (true, Some(after)) = (next.has_next_page, next.end_cursor) else {
                break;
            };
            variables["after"] = after.into();
        }
        Ok(files)
    }

    /// Marks a pull request's file (by its node ID, and the file's path) as
    /// viewed, or unmarks it.
    pub fn set_file_viewed(&self, pull_request_id: &str, path: &str, viewed: bool) -> Result<()> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.set_file_viewed(pull_request_id, path, viewed);
        }
        let mutation = if viewed {
            queries::MARK_FILE_AS_VIEWED
        } else {
            queries::UNMARK_FILE_AS_VIEWED
        };
        let _: Value = self.graphql(
            mutation,
            json!({ "pullRequestId": pull_request_id, "path": path }),
        )?;
        Ok(())
    }

    /// Merges a pull request (by its node ID) in `repo` with the viewer's
    /// default merge method there, as GitHub's merge button would. Fails
    /// if its head has moved from `head_oid`, so it never merges commits
    /// the viewer hasn't seen.
    pub fn merge_pull_request(
        &self,
        repo: &RepoRef,
        pull_request_id: &str,
        head_oid: &str,
    ) -> Result<()> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.merge_pull_request(repo, pull_request_id, head_oid);
        }
        let data: MergeMethodData = self.graphql(
            queries::VIEWER_DEFAULT_MERGE_METHOD,
            json!({ "owner": repo.owner, "name": repo.name }),
        )?;
        let method = data
            .repository
            .with_context(|| format!("{repo} not found"))?
            .viewer_default_merge_method;
        let _: Value = self.graphql(
            queries::MERGE_PULL_REQUEST,
            json!({
                "pullRequestId": pull_request_id,
                "mergeMethod": method,
                "expectedHeadOid": head_oid,
            }),
        )?;
        Ok(())
    }

    /// Closes a pull request (by its node ID) without merging it.
    pub fn close_pull_request(&self, pull_request_id: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.close_pull_request(pull_request_id);
        }
        let _: Value = self.graphql(
            queries::CLOSE_PULL_REQUEST,
            json!({ "pullRequestId": pull_request_id }),
        )?;
        Ok(())
    }

    /// Comments `body` on a pull request or issue, by its node ID.
    pub fn add_comment(&self, subject_id: &str, body: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.add_comment(subject_id, body);
        }
        let _: Value = self.graphql(
            queries::ADD_COMMENT,
            json!({ "subjectId": subject_id, "body": body }),
        )?;
        Ok(())
    }

    /// Looks up a repository the viewer can access. If GitHub won't show it,
    /// the error is a [`RepoAccessError`] explaining why where possible.
    pub fn repository(&self, repo: &RepoRef) -> Result<Repository> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.repository(repo);
        }
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
        if cfg!(test) {
            bail!("tests can't reach GitHub; sign in with `GithubClient::fake`");
        }
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

    /// The repository's latest release, if it has one: the newest that
    /// isn't a draft or prerelease.
    pub fn latest_release(&self, repo: &RepoRef) -> Result<Option<Release>> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.latest_release(repo);
        }
        let data: ReleaseData = self.graphql(
            &queries::latest_release(),
            json!({ "owner": repo.owner, "name": repo.name }),
        )?;
        let repository = data
            .repository
            .with_context(|| format!("{repo} not found"))?;
        Ok(repository.release.map(Release::from))
    }

    /// The release tagged `tag`, if there is one, prerelease or not.
    pub fn release(&self, repo: &RepoRef, tag: &str) -> Result<Option<Release>> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.release(repo, tag);
        }
        let data: ReleaseData = self.graphql(
            &queries::release_by_tag(),
            json!({ "owner": repo.owner, "name": repo.name, "tag": tag }),
        )?;
        let repository = data
            .repository
            .with_context(|| format!("{repo} not found"))?;
        Ok(repository.release.map(Release::from))
    }

    /// Downloads a release asset into `out`. Its link is already signed,
    /// so the token isn't sent (it would be refused anyway).
    pub fn download_release_asset(
        &self,
        asset: &ReleaseAsset,
        out: &mut dyn std::io::Write,
    ) -> Result<()> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.download_release_asset(asset, out);
        }
        if cfg!(test) {
            bail!("tests can't reach GitHub; sign in with `GithubClient::fake`");
        }
        let mut response = ureq::get(&asset.url)
            .header("User-Agent", USER_AGENT)
            .call()
            .with_context(|| format!("downloading {}", asset.name))?;
        std::io::copy(&mut response.body_mut().as_reader(), out)
            .with_context(|| format!("downloading {}", asset.name))?;
        Ok(())
    }

    fn graphql<T: DeserializeOwned>(&self, query: &str, variables: Value) -> Result<T> {
        self.graphql_tolerating(query, variables, &[])
    }

    /// Like `graphql`, but returns the (partial) data when every error is
    /// one of the `tolerated` kinds.
    fn graphql_tolerating<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        tolerated: &[&str],
    ) -> Result<T> {
        if cfg!(test) {
            bail!("tests can't reach GitHub; sign in with `GithubClient::fake`");
        }
        let response: GraphqlResponse<T> = ureq::post(format!("{GITHUB_API_URL}/graphql"))
            .header("Authorization", &format!("Bearer {}", self.token.secret()))
            .header("User-Agent", USER_AGENT)
            .send_json(json!({ "query": query, "variables": variables }))?
            .body_mut()
            .read_json()
            .context("decoding GraphQL response")?;

        let errors = GraphqlErrors(response.errors);
        let all_tolerated = errors
            .0
            .iter()
            .all(|e| e.kind.as_deref().is_some_and(|k| tolerated.contains(&k)));
        match response.data {
            Some(data) if all_tolerated => {
                if !errors.0.is_empty() {
                    eprintln!("ignoring {errors}");
                }
                Ok(data)
            }
            _ if !errors.0.is_empty() => Err(errors.into()),
            _ => bail!("GraphQL response had no data"),
        }
    }
}

/// A page's files and their viewed states.
fn viewed_in(page: ViewedFilesPage) -> impl Iterator<Item = (String, super::FileViewedState)> {
    page.nodes
        .into_iter()
        .flatten()
        .map(|file| (file.path, file.viewer_viewed_state))
}
