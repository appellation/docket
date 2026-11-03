//! The open pull request: its detail, fetched in full, and which collapsible
//! parts of its timeline the user has toggled.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{Context, SharedString, Task};

use super::{DiffFiles, DiffLoad, Tab};
use crate::config::GITHUB_WEB_URL;
use crate::github::{PullRequest, PullRequestDetail, PullRequestLink, RepoRef, TimelineItem};
use crate::markdown::Document;

pub struct PrDetail {
    pub repo: RepoRef,
    pub number: u64,
    /// The docket's row for it, shown until the detail loads. `None` when
    /// it was opened from a link to one the docket doesn't list, until then.
    pub summary: Option<PullRequest>,
    pub loaded: Option<LoadedDetail>,
    pub loading: bool,
    pub error: Option<SharedString>,
    pub tab: PrTab,
    /// The diff, once the Files tab has been opened.
    pub files: Option<DiffFiles>,
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

    /// The pull request on GitHub.
    pub fn url(&self) -> String {
        match &self.summary {
            Some(pr) => pr.url.clone(),
            None => format!("{GITHUB_WEB_URL}/{}/pull/{}", self.repo, self.number),
        }
    }
}

impl Tab {
    pub fn open_pull_request(&mut self, pr: &PullRequest, cx: &mut Context<Self>) {
        let Ok(repo) = pr.repository.parse::<RepoRef>() else {
            return;
        };
        self.show_pull_request(repo, pr.number, Some(pr.clone()), cx);
    }

    /// Opens a link to a pull request: from the docket's row if it has one,
    /// or else from GitHub. A link to its files opens the Files tab.
    pub fn open_pull_request_link(&mut self, link: PullRequestLink, cx: &mut Context<Self>) {
        let is_open = self
            .detail
            .as_ref()
            .is_some_and(|d| d.repo == link.repo && d.number == link.number);
        if !is_open {
            let row = self
                .app
                .read(cx)
                .sections
                .iter()
                .flat_map(|section| &section.pull_requests)
                .find(|pr| link.repo.matches(&pr.repository) && pr.number == link.number)
                .cloned();
            self.show_pull_request(link.repo, link.number, row, cx);
        }
        if link.files {
            self.select_pr_tab(PrTab::Files, cx);
        }
    }

    fn show_pull_request(
        &mut self,
        repo: RepoRef,
        number: u64,
        summary: Option<PullRequest>,
        cx: &mut Context<Self>,
    ) {
        self.setup = None;
        self.browser = None;
        self.branch = None;
        self.detail = Some(PrDetail {
            repo,
            number,
            summary,
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
                let mut load_diff = false;
                match result {
                    Ok(loaded) => {
                        // Opened from a link: now its diff can load.
                        if detail.summary.is_none() {
                            detail.summary = Some(loaded.pr.summary.clone());
                            load_diff = detail.files.is_some();
                        }
                        detail.loaded = Some(loaded);
                        detail.error = None;
                    }
                    Err(err) => {
                        let err: SharedString = format!("{err:#}").into();
                        // Without its commits, its diff can't load either.
                        if let (None, Some(files)) = (&detail.summary, &mut detail.files) {
                            files.diff = DiffLoad::Failed(err.clone());
                        }
                        detail.error = Some(err);
                    }
                }
                if load_diff {
                    this.load_pr_diff(cx);
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
