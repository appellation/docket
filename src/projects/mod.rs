//! Projects: a local checkout connected to the GitHub repository its PRs
//! live in.
//!
//! The local path is what editor integration and LSP build on; the
//! repository is what scopes the docket. Projects are local state only
//! (see `store.rs`), independent of the signed-in account.

pub mod branches;
pub mod files;
pub(crate) mod git;
pub mod search;
mod store;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::github::{PullRequest, RepoRef};

pub use branches::{BaseBranch, Branch, BranchCommits, Branches};
pub use git::Checkout;
pub use store::ProjectStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId(pub u64);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    /// Display name; defaults to the checkout's folder name.
    pub name: String,
    /// Root of the local Git checkout.
    pub path: PathBuf,
    pub repository: RepoRef,
    /// Logins of accounts whose comments are automated though GitHub doesn't
    /// mark them as bots (a CI user, a deploy account): the timeline
    /// collapses them with bot comments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automated_authors: Vec<String>,
    /// Set at load time if `path` no longer exists (moved or deleted).
    #[serde(skip)]
    pub path_missing: bool,
}

impl Project {
    pub fn contains(&self, pr: &PullRequest) -> bool {
        self.repository.matches(&pr.repository)
    }
}

/// Logins from a list separated by commas or whitespace, without a leading
/// `@`, each once.
pub fn parse_logins(text: &str) -> Vec<String> {
    let mut logins: Vec<String> = Vec::new();
    for login in text.split(|c: char| c == ',' || c.is_whitespace()) {
        let login = login.trim_start_matches('@');
        if !login.is_empty() && !logins.iter().any(|l| l.eq_ignore_ascii_case(login)) {
            logins.push(login.to_owned());
        }
    }
    logins
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_logins_separated_by_commas_or_spaces() {
        assert_eq!(
            parse_logins(" deploy-bot, @ci-runner  Deploy-Bot,,release\nnightly "),
            ["deploy-bot", "ci-runner", "release", "nightly"]
        );
        assert!(parse_logins(" , @ ").is_empty());
    }
}
