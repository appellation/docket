//! Domain types the UI works with, decoupled from the wire format.

use std::fmt;
use std::str::FromStr;

use anyhow::{Error, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub login: String,
    pub name: Option<String>,
    #[serde(rename = "avatarUrl")]
    pub avatar_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Clone, Debug)]
pub struct PullRequest {
    /// GraphQL node ID; used for mutations.
    pub id: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// `owner/name`
    pub repository: String,
    pub author: Option<String>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub review_decision: Option<ReviewDecision>,
    pub additions: u64,
    pub deletions: u64,
    pub updated_at: DateTime<Utc>,
}

/// A GitHub repository coordinate. Serializes as `owner/name`.
///
/// GitHub owner and repo names are case-insensitive, and so is equality.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    /// Whether `name_with_owner` (as GitHub returns it) is this repository.
    pub fn matches(&self, name_with_owner: &str) -> bool {
        name_with_owner
            .split_once('/')
            .is_some_and(|(owner, name)| {
                owner.eq_ignore_ascii_case(&self.owner) && name.eq_ignore_ascii_case(&self.name)
            })
    }
}

impl PartialEq for RepoRef {
    fn eq(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.name.eq_ignore_ascii_case(&other.name)
    }
}

impl Eq for RepoRef {}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

impl FromStr for RepoRef {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Ok(Self {
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                })
            }
            _ => bail!("expected `owner/name`, got {s:?}"),
        }
    }
}

impl TryFrom<String> for RepoRef {
    type Error = Error;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<RepoRef> for String {
    fn from(repo: RepoRef) -> Self {
        repo.to_string()
    }
}

/// What the viewer can see about a repository, used to confirm a project's
/// connection.
#[derive(Clone, Debug)]
pub struct Repository {
    pub repo: RepoRef,
    pub is_private: bool,
    /// `ADMIN`, `MAINTAIN`, `WRITE`, `TRIAGE` or `READ`.
    pub viewer_permission: Option<String>,
    /// Set when this repository is a fork; PRs usually land on the parent.
    pub parent: Option<RepoRef>,
}

/// Why GitHub won't show the viewer a repository.
#[derive(Debug)]
pub enum RepoAccessError {
    /// The org enforces SAML SSO and this token isn't authorized for it.
    /// `url` starts that authorization.
    SsoRequired {
        org: String,
        url: String,
    },
    /// The org restricts OAuth apps and hasn't approved Docket.
    OauthAppRestricted {
        org: String,
    },
    /// Doesn't exist, or the account can't see it.
    NotFound(RepoRef),
    Other(String),
}

impl fmt::Display for RepoAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SsoRequired { org, .. } => write!(
                f,
                "{org} requires single sign-on. Authorize Docket for {org}, then check again."
            ),
            Self::OauthAppRestricted { org } => write!(
                f,
                "{org} limits which third-party apps can access its repositories, and hasn't \
                 approved Docket. Request approval (an org owner has to grant it), then check again."
            ),
            Self::NotFound(repo) => write!(
                f,
                "{repo} doesn't exist on GitHub, or Docket can't see it. If it belongs to an \
                 organization, check that the organization has approved Docket."
            ),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for RepoAccessError {}
