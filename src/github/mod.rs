//! Thin GitHub API layer. Talks to GitHub directly with the user's OAuth token.

mod client;
mod models;
mod queries;

pub use client::GithubClient;
pub use models::{
    PullRequest, PullRequestState, RepoAccessError, RepoRef, Repository, ReviewDecision, User,
};
