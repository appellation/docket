//! The open pull request: its detail, fetched in full, and which collapsible
//! parts of its timeline the user has toggled.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{Context, SharedString, Task};

use super::{PrFiles, Tab};
use crate::github::{PullRequest, PullRequestDetail, RepoRef, TimelineItem};
use crate::markdown::Document;

pub struct PrDetail {
    pub repo: RepoRef,
    pub number: u64,
    /// The list row it was opened from, shown until the detail loads.
    pub summary: PullRequest,
    pub loaded: Option<LoadedDetail>,
    pub loading: bool,
    pub error: Option<SharedString>,
    pub tab: PrTab,
    /// The diff, once the Files tab has been opened.
    pub files: Option<PrFiles>,
    /// Keys of collapsible entries flipped from their default.
    toggled: HashSet<SharedString>,
    _task: Option<Task<()>>,
}

/// The open pull request's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrTab {
    #[default]
    Conversation,
    Files,
}

pub struct LoadedDetail {
    pub pr: PullRequestDetail,
    /// Parsed bodies of the description, comments, and reviews, by node ID.
    docs: HashMap<String, Arc<Document>>,
}

impl LoadedDetail {
    fn new(pr: PullRequestDetail) -> Self {
        let mut docs = HashMap::new();
        let mut add = |id: &str, body: &str| {
            docs.insert(id.to_owned(), Arc::new(Document::parse(body)));
        };
        add(&pr.id, &pr.body);
        for item in &pr.timeline {
            match item {
                TimelineItem::Comment(comment) => add(&comment.id, &comment.body),
                TimelineItem::Review(review) => {
                    add(&review.id, &review.body);
                    for comment in review.threads.iter().flat_map(|t| &t.comments) {
                        add(&comment.id, &comment.body);
                    }
                }
                TimelineItem::Commit(_) | TimelineItem::Event(_) => {}
            }
        }
        Self { pr, docs }
    }

    pub fn doc(&self, id: &str) -> Arc<Document> {
        self.docs.get(id).cloned().unwrap_or_default()
    }
}

impl PrDetail {
    /// Whether a collapsible entry is open, given whether it starts open.
    pub fn is_expanded(&self, key: &str, by_default: bool) -> bool {
        by_default != self.toggled.contains(key)
    }
}

impl Tab {
    pub fn open_pull_request(&mut self, pr: &PullRequest, cx: &mut Context<Self>) {
        let Ok(repo) = pr.repository.parse::<RepoRef>() else {
            return;
        };
        self.detail = Some(PrDetail {
            repo,
            number: pr.number,
            summary: pr.clone(),
            loaded: None,
            loading: false,
            error: None,
            tab: PrTab::default(),
            files: None,
            toggled: HashSet::new(),
            _task: None,
        });
        self.refresh_pull_request(cx);
    }

    pub fn close_pull_request(&mut self, cx: &mut Context<Self>) {
        if self.detail.take().is_some() {
            cx.notify();
        }
    }

    /// Re-fetches the open pull request, keeping what's shown until the new
    /// data arrives.
    pub(super) fn refresh_pull_request(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.app.read(cx).client() else {
            return;
        };
        let Some(detail) = &mut self.detail else {
            return;
        };
        let (repo, number) = (detail.repo.clone(), detail.number);
        detail.loading = true;
        detail._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let repo = repo.clone();
                    async move { client.pull_request(&repo, number).map(LoadedDetail::new) }
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(detail) = &mut this.detail else {
                    return;
                };
                if detail.repo != repo || detail.number != number {
                    return;
                }
                detail.loading = false;
                match result {
                    Ok(loaded) => {
                        detail.loaded = Some(loaded);
                        detail.error = None;
                    }
                    Err(err) => detail.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Expands or collapses a timeline entry, review thread, or the checks
    /// list.
    pub fn toggle_detail_entry(&mut self, key: SharedString, cx: &mut Context<Self>) {
        if let Some(detail) = &mut self.detail {
            if !detail.toggled.remove(&key) {
                detail.toggled.insert(key);
            }
            cx.notify();
        }
    }
}
