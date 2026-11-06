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
    /// How the docket merges the project's pull requests.
    #[serde(default, skip_serializing_if = "MergeSettings::is_default")]
    pub merge: MergeSettings,
    /// Set at load time if `path` no longer exists (moved or deleted).
    #[serde(skip)]
    pub path_missing: bool,
}

/// How the docket merges a project's pull requests: on GitHub, or, where a
/// bot merges them, by leaving it a comment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeSettings {
    /// Comment instead of merging.
    #[serde(default)]
    pub by_comment: bool,
    /// The comments to choose from, one at random each merge. Kept when
    /// `by_comment` is off, so turning it back on finds them.
    #[serde(default)]
    pub comments: Vec<String>,
}

/// What merging a pull request does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergePlan {
    /// Merges it on GitHub.
    Merge,
    /// Leaves this comment on it.
    Comment(String),
}

impl MergeSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// What merging does now, choosing a comment at random. `None` when
    /// it's to comment but there are no comments to choose from.
    pub fn plan(&self) -> Option<MergePlan> {
        if !self.by_comment {
            return Some(MergePlan::Merge);
        }
        if self.comments.is_empty() {
            return None;
        }
        // Each `RandomState` is seeded afresh, which is random enough to
        // choose a comment.
        use std::hash::BuildHasher as _;
        let random = std::collections::hash_map::RandomState::new().hash_one(());
        let ix = (random % self.comments.len() as u64) as usize;
        Some(MergePlan::Comment(self.comments[ix].clone()))
    }

    /// Comments as they're typed in a project's settings: separated by
    /// commas, with blanks dropped.
    pub fn parse_comments(text: &str) -> Vec<String> {
        text.split(',')
            .map(str::trim)
            .filter(|comment| !comment.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// The comments as `parse_comments` reads them.
    pub fn comments_text(&self) -> String {
        self.comments.join(", ")
    }
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

    #[test]
    fn merges_by_a_comment_from_the_list() {
        let mut merge = MergeSettings::default();
        assert_eq!(merge.plan(), Some(MergePlan::Merge));

        merge.by_comment = true;
        assert_eq!(merge.plan(), None, "no comment to leave");

        merge.comments = MergeSettings::parse_comments(" /merge ,, ship it ,");
        assert_eq!(merge.comments, ["/merge", "ship it"]);
        assert_eq!(merge.comments_text(), "/merge, ship it");
        let chosen: std::collections::HashSet<_> = (0..200)
            .filter_map(|_| match merge.plan() {
                Some(MergePlan::Comment(body)) => Some(body),
                _ => None,
            })
            .collect();
        assert_eq!(chosen.len(), 2, "each comment comes up: {chosen:?}");
    }
}
