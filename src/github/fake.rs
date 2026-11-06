//! A stand-in for GitHub in tests: canned answers to `GithubClient`'s calls,
//! with no network. Build one, seed what the test needs, and sign in with
//! `GithubClient::fake`.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Result, anyhow};

use serde_json::json;

use super::models::{
    FileViewedState, PullRequest, PullRequestDetail, PullRequestVersion, Release, ReleaseAsset,
    RepoRef, Repository, ReviewComment, ReviewEvent, User, ViewedFiles,
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
    /// Releases in the order they were published, with whether each is a
    /// prerelease and its assets' contents by name.
    releases: Vec<FakeRelease>,
    /// The viewer's viewed files, by pull request node ID.
    viewed: HashMap<String, ViewedFiles>,
    /// Why marking files as viewed fails, if it does.
    viewed_error: Option<String>,
    /// Pull requests with more viewed files than the version's page holds.
    many_files: bool,
    /// Reviews submitted, in order.
    reviews: Vec<SubmittedReview>,
    /// Why submitting a review fails, if it does.
    review_error: Option<String>,
    /// What's been done to pull requests, by node ID, in order.
    actions: Vec<FakeAction>,
    /// Why merging, closing, and commenting fail, if they do.
    action_error: Option<String>,
    /// Why looking up a repository fails, by repository.
    repository_errors: Vec<(RepoRef, String)>,
}

/// A review submitted to the fake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmittedReview {
    pub pull_request_id: String,
    pub commit_oid: Option<String>,
    pub event: ReviewEvent,
    pub body: String,
    pub comments: Vec<ReviewComment>,
}

/// Something done to a pull request through the fake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FakeAction {
    /// Merged, with the head it was expected to be at.
    Merged {
        id: String,
        head: String,
    },
    Closed {
        id: String,
    },
    Commented {
        id: String,
        body: String,
    },
}

struct FakeRelease {
    repo: RepoRef,
    release: Release,
    prerelease: bool,
    contents: HashMap<String, Vec<u8>>,
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
                releases: Vec::new(),
                viewed: HashMap::new(),
                viewed_error: None,
                many_files: false,
                reviews: Vec::new(),
                review_error: None,
                actions: Vec::new(),
                action_error: None,
                repository_errors: Vec::new(),
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

    /// Sets the viewer's viewed state for a file of the pull request with
    /// node ID `pull_request_id`, as if set on github.com.
    pub fn set_viewed_state(&self, pull_request_id: &str, path: &str, state: FileViewedState) {
        let mut data = self.data.lock().unwrap();
        data.viewed
            .entry(pull_request_id.to_owned())
            .or_default()
            .insert(path.to_owned(), state);
    }

    /// The viewer's viewed state for a file, as GitHub has it.
    pub fn viewed_state(&self, pull_request_id: &str, path: &str) -> FileViewedState {
        let data = self.data.lock().unwrap();
        data.viewed
            .get(pull_request_id)
            .and_then(|files| files.get(path))
            .copied()
            .unwrap_or(FileViewedState::Unviewed)
    }

    /// Makes marking files as viewed fail with `error`, or succeed again.
    pub fn fail_viewed(&self, error: Option<&str>) {
        self.data.lock().unwrap().viewed_error = error.map(str::to_owned);
    }

    /// Answers version checks as if each pull request had too many files
    /// for viewed states to come with them.
    pub fn set_many_files(&self, many: bool) {
        self.data.lock().unwrap().many_files = many;
    }

    /// The reviews submitted so far, in order.
    pub fn reviews(&self) -> Vec<SubmittedReview> {
        self.data.lock().unwrap().reviews.clone()
    }

    /// Makes submitting reviews fail with `error`, or succeed again.
    pub fn fail_reviews(&self, error: Option<&str>) {
        self.data.lock().unwrap().review_error = error.map(str::to_owned);
    }

    /// Pull requests merged, closed, and commented on, in order.
    pub fn actions(&self) -> Vec<FakeAction> {
        self.data.lock().unwrap().actions.clone()
    }

    /// Makes merging, closing, and commenting fail with `error`, or
    /// succeed again.
    pub fn fail_actions(&self, error: Option<&str>) {
        self.data.lock().unwrap().action_error = error.map(str::to_owned);
    }

    /// Makes looking up `repo` fail with `error`.
    pub fn fail_repository(&self, repo: RepoRef, error: &str) {
        let mut data = self.data.lock().unwrap();
        data.repository_errors.push((repo, error.to_owned()));
    }

    /// How many times a pull request's detail has been fetched.
    pub fn detail_fetches(&self) -> usize {
        self.data.lock().unwrap().detail_fetches
    }

    /// Publishes `tag` as `repo`'s latest release, with `assets` (name and
    /// contents) attached. Their sizes and digests are filled in.
    pub fn set_latest_release(&self, repo: RepoRef, tag: &str, assets: Vec<(&str, Vec<u8>)>) {
        self.set_release(repo, tag, "0000000", false, assets);
    }

    /// Publishes (or replaces) the release tagged `tag`, at `commit`. The
    /// latest release is the last published that isn't a `prerelease`.
    pub fn set_release(
        &self,
        repo: RepoRef,
        tag: &str,
        commit: &str,
        prerelease: bool,
        assets: Vec<(&str, Vec<u8>)>,
    ) {
        use sha2::Digest as _;
        let release = Release {
            tag: tag.to_owned(),
            url: format!("https://github.com/{repo}/releases/tag/{tag}"),
            commit: Some(commit.to_owned()),
            assets: assets
                .iter()
                .map(|(name, contents)| ReleaseAsset {
                    name: (*name).to_owned(),
                    url: format!("https://example.com/{tag}/{name}"),
                    size: contents.len() as u64,
                    sha256: Some(crate::updater::hex(&sha2::Sha256::digest(contents))),
                })
                .collect(),
        };
        let contents = assets
            .into_iter()
            .map(|(name, contents)| (name.to_owned(), contents))
            .collect();
        let mut data = self.data.lock().unwrap();
        data.releases
            .retain(|r| !(r.repo == repo && r.release.tag == tag));
        data.releases.push(FakeRelease {
            repo,
            release,
            prerelease,
            contents,
        });
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

    pub fn pull_request_version(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<(PullRequestVersion, Option<ViewedFiles>)> {
        let data = self.data.lock().unwrap();
        let (_, detail, version) = data
            .details
            .iter()
            .find(|(r, d, _)| r == repo && d.number == number)
            .ok_or_else(|| anyhow!("{repo}#{number} not found"))?;
        let viewed =
            (!data.many_files).then(|| data.viewed.get(&detail.id).cloned().unwrap_or_default());
        Ok((PullRequestVersion(json!(version)), viewed))
    }

    pub fn viewed_files(&self, repo: &RepoRef, number: u64) -> Result<ViewedFiles> {
        let data = self.data.lock().unwrap();
        let (_, detail, _) = data
            .details
            .iter()
            .find(|(r, d, _)| r == repo && d.number == number)
            .ok_or_else(|| anyhow!("{repo}#{number} not found"))?;
        Ok(data.viewed.get(&detail.id).cloned().unwrap_or_default())
    }

    pub fn set_file_viewed(&self, pull_request_id: &str, path: &str, viewed: bool) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        if let Some(error) = &data.viewed_error {
            return Err(anyhow!("{error}"));
        }
        let state = if viewed {
            FileViewedState::Viewed
        } else {
            FileViewedState::Unviewed
        };
        data.viewed
            .entry(pull_request_id.to_owned())
            .or_default()
            .insert(path.to_owned(), state);
        Ok(())
    }

    pub fn submit_review(
        &self,
        pull_request_id: &str,
        commit_oid: Option<&str>,
        event: ReviewEvent,
        body: &str,
        comments: &[ReviewComment],
    ) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        if let Some(error) = &data.review_error {
            return Err(anyhow!("{error}"));
        }
        data.reviews.push(SubmittedReview {
            pull_request_id: pull_request_id.to_owned(),
            commit_oid: commit_oid.map(str::to_owned),
            event,
            body: body.to_owned(),
            comments: comments.to_vec(),
        });
        Ok(())
    }

    pub fn merge_pull_request(
        &self,
        _repo: &RepoRef,
        pull_request_id: &str,
        head_oid: &str,
    ) -> Result<()> {
        self.act(FakeAction::Merged {
            id: pull_request_id.to_owned(),
            head: head_oid.to_owned(),
        })
    }

    pub fn close_pull_request(&self, pull_request_id: &str) -> Result<()> {
        self.act(FakeAction::Closed {
            id: pull_request_id.to_owned(),
        })
    }

    pub fn add_comment(&self, subject_id: &str, body: &str) -> Result<()> {
        self.act(FakeAction::Commented {
            id: subject_id.to_owned(),
            body: body.to_owned(),
        })
    }

    fn act(&self, action: FakeAction) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        if let Some(error) = &data.action_error {
            return Err(anyhow!("{error}"));
        }
        data.actions.push(action);
        Ok(())
    }

    /// Every repository is public and writable, so no project is flagged
    /// as hidden, except those set to fail.
    pub fn repository(&self, repo: &RepoRef) -> Result<Repository> {
        let data = self.data.lock().unwrap();
        if let Some((_, error)) = data.repository_errors.iter().find(|(r, _)| r == repo) {
            return Err(anyhow!("{error}"));
        }
        Ok(Repository {
            repo: repo.clone(),
            is_private: false,
            viewer_permission: Some("WRITE".into()),
            parent: None,
        })
    }
}

impl FakeGithub {
    pub fn latest_release(&self, repo: &RepoRef) -> Result<Option<Release>> {
        let data = self.data.lock().unwrap();
        Ok(data
            .releases
            .iter()
            .rfind(|r| r.repo == *repo && !r.prerelease)
            .map(|r| r.release.clone()))
    }

    pub fn release(&self, repo: &RepoRef, tag: &str) -> Result<Option<Release>> {
        let data = self.data.lock().unwrap();
        Ok(data
            .releases
            .iter()
            .find(|r| r.repo == *repo && r.release.tag == tag)
            .map(|r| r.release.clone()))
    }

    pub fn download_release_asset(
        &self,
        asset: &ReleaseAsset,
        out: &mut dyn std::io::Write,
    ) -> Result<()> {
        let contents = {
            let data = self.data.lock().unwrap();
            data.releases
                .iter()
                .filter(|r| r.release.assets.iter().any(|a| a.url == asset.url))
                .find_map(|r| r.contents.get(&asset.name).cloned())
                .ok_or_else(|| anyhow!("{} not found", asset.name))?
        };
        out.write_all(&contents)?;
        Ok(())
    }
}

impl std::fmt::Debug for FakeGithub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeGithub")
    }
}
