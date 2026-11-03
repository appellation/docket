//! A stand-in for GitHub in tests: canned answers to `GithubClient`'s calls,
//! with no network. Build one, seed what the test needs, and sign in with
//! `GithubClient::fake`.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Result, anyhow};

use super::models::{PullRequest, PullRequestDetail, RepoRef, Repository, User};

pub struct FakeGithub {
    data: Mutex<FakeData>,
}

struct FakeData {
    viewer: User,
    /// Results by search query; any other query finds nothing.
    searches: HashMap<String, Vec<PullRequest>>,
    details: Vec<(RepoRef, PullRequestDetail)>,
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
            }),
        }
    }

    /// Sets what a search finds, from the next fetch on.
    pub fn set_search(&self, query: &str, pull_requests: Vec<PullRequest>) {
        let mut data = self.data.lock().unwrap();
        data.searches.insert(query.to_owned(), pull_requests);
    }

    /// Adds (or replaces) a pull request's detail.
    pub fn set_detail(&self, repo: RepoRef, detail: PullRequestDetail) {
        let mut data = self.data.lock().unwrap();
        data.details
            .retain(|(r, d)| !(*r == repo && d.number == detail.number));
        data.details.push((repo, detail));
    }

    pub fn viewer(&self) -> Result<User> {
        Ok(self.data.lock().unwrap().viewer.clone())
    }

    pub fn search_pull_requests(&self, query: &str) -> Result<Vec<PullRequest>> {
        let data = self.data.lock().unwrap();
        Ok(data.searches.get(query).cloned().unwrap_or_default())
    }

    pub fn pull_request(&self, repo: &RepoRef, number: u64) -> Result<PullRequestDetail> {
        let data = self.data.lock().unwrap();
        data.details
            .iter()
            .find(|(r, d)| r == repo && d.number == number)
            .map(|(_, d)| d.clone())
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
