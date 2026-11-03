//! Projects: a local checkout connected to the GitHub repository its PRs
//! live in.
//!
//! The local path is what editor integration and LSP build on; the
//! repository is what scopes the docket. Projects are local state only
//! (see `store.rs`), independent of the signed-in account.

pub mod branches;
pub mod files;
pub(crate) mod git;
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
    /// Set at load time if `path` no longer exists (moved or deleted).
    #[serde(skip)]
    pub path_missing: bool,
}

impl Project {
    pub fn contains(&self, pr: &PullRequest) -> bool {
        self.repository.matches(&pr.repository)
    }
}
