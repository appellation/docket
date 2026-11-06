//! The pull requests Docket opens from the browser: every one in a
//! project's repository, and the docket's. The app writes them to its data
//! directory as they change, and the native host, a process of its own,
//! reads them. Blocking disk I/O; the app runs it on the background
//! executor.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::config::APP_DIR_NAME;
use crate::github::{PullRequest, PullRequestLink, RepoRef};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenLinks {
    /// Projects' repositories, `owner/name` in lowercase, sorted.
    pub repositories: Vec<String>,
    /// The docket's pull requests, `owner/name#number` in lowercase,
    /// sorted.
    pub pull_requests: Vec<String>,
}

impl OpenLinks {
    pub fn new<'a>(
        repositories: impl IntoIterator<Item = &'a RepoRef>,
        pull_requests: impl IntoIterator<Item = &'a PullRequest>,
    ) -> Self {
        let mut repositories: Vec<String> = repositories
            .into_iter()
            .map(|repo| repo.to_string().to_lowercase())
            .collect();
        repositories.sort();
        repositories.dedup();
        let mut pull_requests: Vec<String> = pull_requests
            .into_iter()
            .map(|pr| format!("{}#{}", pr.repository, pr.number).to_lowercase())
            .collect();
        pull_requests.sort();
        pull_requests.dedup();
        Self {
            repositories,
            pull_requests,
        }
    }

    /// Whether Docket opens `link`.
    pub fn opens(&self, link: &PullRequestLink) -> bool {
        let repo = link.repo.to_string().to_lowercase();
        self.repositories.binary_search(&repo).is_ok()
            || self
                .pull_requests
                .binary_search(&format!("{repo}#{}", link.number))
                .is_ok()
    }

    fn path() -> Option<PathBuf> {
        if cfg!(test) {
            return None;
        }
        Some(
            dirs::data_dir()?
                .join(APP_DIR_NAME)
                .join("extension-links.json"),
        )
    }

    /// What the app last saved; nothing, if it can't be read.
    pub fn load() -> Self {
        Self::path()
            .and_then(|path| fs::read(path).ok())
            .and_then(|json| serde_json::from_slice(&json).ok())
            .unwrap_or_default()
    }

    /// Writes atomically (temp file + rename), so the host never reads
    /// half of it.
    pub fn save(&self) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        let dir = path.parent().expect("store path has a parent");
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::pull_request;

    #[test]
    fn opens_projects_repositories_and_the_dockets_pull_requests() {
        let repo: RepoRef = "Acme/Widgets".parse().unwrap();
        let pr = pull_request("Other/Thing", 12, "Elsewhere");
        let links = OpenLinks::new([&repo, &repo], [&pr]);
        assert_eq!(links.repositories, ["acme/widgets"]);
        assert_eq!(links.pull_requests, ["other/thing#12"]);

        let opens = |url: &str| links.opens(&PullRequestLink::parse(url).unwrap());
        assert!(opens("https://github.com/acme/widgets/pull/1"));
        assert!(opens("https://github.com/ACME/widgets/pull/99/files"));
        assert!(opens("https://github.com/other/thing/pull/12"));
        assert!(!opens("https://github.com/other/thing/pull/13"));
        assert!(!opens("https://github.com/acme/gadgets/pull/1"));
    }
}
