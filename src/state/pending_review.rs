//! Reviews being written: the comments left on a pull request's diff,
//! kept until the review is submitted with Approve, Request Changes, or
//! Comment.
//!
//! They're Docket's own until then, not GitHub's pending reviews, and
//! submitting sends the review and its comments at once
//! (`addPullRequestReview`). They're on `AppState`, by pull request, so
//! every window shows the same ones, and closing the pull request keeps
//! them. They don't outlast quitting.

use std::collections::HashMap;

use gpui::{Context, SharedString, Task};

use super::{AppEvent, AppState, Tab};
use crate::github::{DiffLineRef, ReviewComment, ReviewEvent};

#[derive(Default)]
pub struct PendingReviews {
    /// By pull request node ID.
    reviews: HashMap<String, PendingReview>,
    next_id: u64,
}

#[derive(Default)]
pub struct PendingReview {
    pub comments: Vec<DraftComment>,
    /// The commit the comments were left on, whose lines they number.
    pub commit_oid: Option<String>,
    /// Submitting it, and how.
    pub submitting: Option<ReviewEvent>,
    /// Why it last failed to submit.
    pub error: Option<SharedString>,
    /// How it was last submitted, until another comment is left.
    pub submitted: Option<ReviewEvent>,
    _task: Option<Task<()>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftComment {
    /// Unique among every pull request's comments this session.
    pub id: u64,
    pub comment: ReviewComment,
}

impl PendingReview {
    /// Whether submitting `event` needs a summary: GitHub refuses a
    /// review that requests changes or comments with nothing to say.
    pub fn needs_summary(&self, event: ReviewEvent) -> bool {
        event != ReviewEvent::Approve && self.comments.is_empty()
    }
}

impl AppState {
    /// The review being written of the pull request with node ID
    /// `pull_request`, if one is.
    pub fn pending_review(&self, pull_request: &str) -> Option<&PendingReview> {
        self.pending_reviews.reviews.get(pull_request)
    }

    /// Leaves a comment on the pull request's diff at `commit_oid`, or
    /// changes the one `editing`.
    pub fn save_review_comment(
        &mut self,
        pull_request: &str,
        commit_oid: &str,
        editing: Option<u64>,
        comment: ReviewComment,
        cx: &mut Context<Self>,
    ) {
        let next_id = &mut self.pending_reviews.next_id;
        let review = self
            .pending_reviews
            .reviews
            .entry(pull_request.to_owned())
            .or_default();
        review.submitted = None;
        review
            .commit_oid
            .get_or_insert_with(|| commit_oid.to_owned());
        match editing.and_then(|id| review.comments.iter_mut().find(|c| c.id == id)) {
            Some(draft) => draft.comment = comment,
            None => {
                *next_id += 1;
                review.comments.push(DraftComment {
                    id: *next_id,
                    comment,
                });
            }
        }
        cx.notify();
    }

    pub fn remove_review_comment(&mut self, pull_request: &str, id: u64, cx: &mut Context<Self>) {
        let Some(review) = self.pending_reviews.reviews.get_mut(pull_request) else {
            return;
        };
        review.comments.retain(|c| c.id != id);
        if review.comments.is_empty() {
            review.commit_oid = None;
        }
        cx.notify();
    }

    /// Submits the review of the pull request with node ID `pull_request`,
    /// with its comments and `body`. `head_oid` is the commit to review if
    /// no comment says which.
    pub fn submit_review(
        &mut self,
        pull_request: &str,
        head_oid: Option<String>,
        event: ReviewEvent,
        body: String,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else {
            return;
        };
        let review = self
            .pending_reviews
            .reviews
            .entry(pull_request.to_owned())
            .or_default();
        if review.submitting.is_some() {
            return;
        }
        review.submitting = Some(event);
        review.error = None;
        let commit_oid = review.commit_oid.clone().or(head_oid);
        let comments: Vec<ReviewComment> =
            review.comments.iter().map(|c| c.comment.clone()).collect();
        let sent: Vec<u64> = review.comments.iter().map(|c| c.id).collect();
        let id = pull_request.to_owned();
        review._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let id = id.clone();
                    async move {
                        client.submit_review(&id, commit_oid.as_deref(), event, &body, &comments)
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(review) = this.pending_reviews.reviews.get_mut(&id) else {
                    return;
                };
                review.submitting = None;
                match result {
                    Ok(()) => {
                        // Comments left while it was sent wait for the next.
                        review.comments.retain(|c| !sent.contains(&c.id));
                        if review.comments.is_empty() {
                            review.commit_oid = None;
                        }
                        review.submitted = Some(event);
                        cx.emit(AppEvent::ReviewSubmitted);
                        this.refresh(cx);
                    }
                    Err(err) => {
                        eprintln!("failed to submit a review of {id}: {err:#}");
                        review.error = Some(format!("{err:#}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Forgets why the last review failed to submit.
    pub fn dismiss_review_error(&mut self, pull_request: &str, cx: &mut Context<Self>) {
        if let Some(review) = self.pending_reviews.reviews.get_mut(pull_request) {
            review.error = None;
            cx.notify();
        }
    }
}

impl Tab {
    /// The open pull request's node ID, its head, and whether the user
    /// wrote it, when the open diff is its: what a review of it needs.
    pub fn reviewable(&self, cx: &gpui::App) -> Option<Reviewable> {
        let detail = self.detail.as_ref()?;
        detail.files.as_ref()?;
        let summary = detail.summary.as_ref()?;
        let viewer = match &self.app.read(cx).auth {
            super::AuthStatus::SignedIn { user, .. } => Some(user.login.as_str()),
            _ => None,
        };
        let author = summary.author.as_ref().map(|a| a.login.as_str());
        Some(Reviewable {
            pull_request: summary.id.clone(),
            head_oid: summary.head_ref_oid.clone(),
            is_own: viewer.is_some() && author == viewer,
        })
    }
}

/// What reviewing the open pull request needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reviewable {
    /// Its node ID.
    pub pull_request: String,
    pub head_oid: String,
    /// Whether the user wrote it, so can only comment: GitHub refuses
    /// approving, or requesting changes on, your own pull request.
    pub is_own: bool,
}

/// The lines a comment is on, as GitHub shows them: `L12`, or `R10–R14`.
pub fn line_label(line: DiffLineRef, start: Option<DiffLineRef>) -> String {
    let one = |l: DiffLineRef| {
        let side = match l.side {
            crate::github::DiffSide::Left => "L",
            crate::github::DiffSide::Right => "R",
        };
        format!("{side}{}", l.line)
    };
    match start.filter(|s| *s != line) {
        Some(start) => format!("Lines {}–{}", one(start), one(line)),
        None => format!("Line {}", one(line)),
    }
}
