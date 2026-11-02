//! Thin GitHub API layer. Talks to GitHub directly with the user's OAuth token.

mod client;
mod models;
mod queries;

pub use client::GithubClient;
pub use models::{
    Actor, Check, CheckStatus, Comment, Commit, Event, EventKind, MergeStatus, PullRequest,
    PullRequestDetail, PullRequestState, RepoAccessError, RepoRef, Repository, Review,
    ReviewDecision, ReviewState, ReviewThread, Reviewer, ReviewerStatus, TimelineItem, User,
};
