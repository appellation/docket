//! Thin GitHub API layer. Talks to GitHub directly with the user's OAuth token.

mod client;
#[cfg(test)]
mod fake;
mod links;
mod models;
mod queries;

pub use client::GithubClient;
#[cfg(test)]
pub use fake::{FakeGithub, SubmittedReview};
pub use links::{PullRequestLink, PullRequestPage};
pub use models::{
    Actor, Check, CheckStatus, Comment, Commit, DiffLineRef, DiffSide, Event, EventKind,
    FileViewedState, MergeStatus, PullRequest, PullRequestDetail, PullRequestState,
    PullRequestVersion, Release, ReleaseAsset, RepoAccessError, RepoRef, Repository, Review,
    ReviewComment, ReviewDecision, ReviewEvent, ReviewState, ReviewThread, Reviewer,
    ReviewerStatus, TimelineItem, User, ViewedFiles,
};
