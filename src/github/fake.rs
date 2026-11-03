//! A stand-in for GitHub in tests: canned answers to `GithubClient`'s calls,
//! with no network. Build one, seed what the test needs, and sign in with
//! `GithubClient::fake`.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Result, anyhow};

use serde_json::json;

use super::models::{
    PullRequest, PullRequestDetail, PullRequestVersion, RepoRef, Repository, User,
};

pub struct FakeGithub {
    data: Mutex<FakeData>,
}

struct FakeData {
    viewer: User,
    /// Results by search query; any other query finds nothing.
    searches: HashMap<String, Vec<PullRequest>>,
    /// Each with its version: bumped every time it's set.
    details: Vec<(RepoRef, PullRequestDetail, u64)>,
    /// The last version given out.
    version: u64,
    /// How many times a detail was fetched.
    detail_fetches: usize,
}

impl FakeGithub {
    pub fn new() -> Self {
        Self {
            data: Mutex::new(FakeData {
                viewer: User {
                    login: "octocat".into(),
                    name: Some("The Octocat".into()),
                    avatar_url: String::new(),
                },
                searches: HashMap::new(),
                details: Vec::new(),
                version: 0,
                detail_fetches: 0,
            }),
        }
    }

    /// Sets what a search finds, from the next fetch on.
    pub fn set_search(&self, query: &str, pull_requests: Vec<PullRequest>) {
        let mut data = self.data.lock().unwrap();
        data.searches.insert(query.to_owned(), pull_requests);
    }

    /// Adds (or replaces) a pull request's detail, as a new version.
    pub fn set_detail(&self, repo: RepoRef, detail: PullRequestDetail) {
        let mut data = self.data.lock().unwrap();
        data.details
            .retain(|(r, d, _)| !(*r == repo && d.number == detail.number));
        data.version += 1;
        let version = data.version;
        data.details.push((repo, detail, version));
    }

    /// How many times a pull request's detail has been fetched.
    pub fn detail_fetches(&self) -> usize {
        self.data.lock().unwrap().detail_fetches
    }

    pub fn viewer(&self) -> Result<User> {
        Ok(self.data.lock().unwrap().viewer.clone())
    }

    pub fn search_pull_requests(&self, query: &str) -> Result<Vec<PullRequest>> {
        let data = self.data.lock().unwrap();
        Ok(data.searches.get(query).cloned().unwrap_or_default())
    }

    pub fn pull_request(&self, repo: &RepoRef, number: u64) -> Result<PullRequestDetail> {
        let mut data = self.data.lock().unwrap();
        data.detail_fetches += 1;
        data.details
            .iter()
            .find(|(r, d, _)| r == repo && d.number == number)
            .map(|(_, d, _)| d.clone())
            .ok_or_else(|| anyhow!("{repo}#{number} not found"))
    }

    pub fn pull_request_version(&self, repo: &RepoRef, number: u64) -> Result<PullRequestVersion> {
        let data = self.data.lock().unwrap();
        data.details
            .iter()
            .find(|(r, d, _)| r == repo && d.number == number)
            .map(|(_, _, version)| PullRequestVersion(json!(version)))
            .ok_or_else(|| anyhow!("{repo}#{number} not found"))
    }

    /// Every repository is public and writable, so no project is flagged
    /// as hidden.
    pub fn repository(&self, repo: &RepoRef) -> Result<Repository> {
        Ok(Repository {
            repo: repo.clone(),
            is_private: false,
            viewer_permission: Some("WRITE".into()),
            parent: None,
        })
    }
}

impl std::fmt::Debug for FakeGithub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeGithub")
    }
}
