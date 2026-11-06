//! The open pull request: its detail, fetched in full, and which collapsible
//! parts of its timeline the user has toggled.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use gpui::{Context, SharedString, Task};

use super::{DiffFiles, DiffLoad, Tab};
use crate::config::GITHUB_WEB_URL;
use crate::github::{
    PullRequest, PullRequestDetail, PullRequestLink, PullRequestPage, PullRequestVersion, RepoRef,
    TimelineItem,
};
use crate::markdown::Document;

pub struct PrDetail {
    pub repo: RepoRef,
    pub number: u64,
    /// The docket's row for it, shown until the detail loads. `None` when
    /// it was opened from a link to one the docket doesn't list, until then.
    pub summary: Option<PullRequest>,
    pub loaded: Option<LoadedDetail>,
    /// Loading at the user's request; loads from watching it (`live.rs`)
    /// don't show.
    pub loading: bool,
    pub error: Option<SharedString>,
    pub tab: PrTab,
    /// The diff. It starts loading when the pull request opens, if its
    /// commits are known, so it's usually ready by the time the Files tab
    /// opens; its language servers wait for the tab (see `review.rs`).
    pub files: Option<DiffFiles>,
    /// A file to open in its Browse tab once its files are browsed, which
    /// waits for the diff to fetch its head.
    pub(super) browse_file: Option<String>,
    /// Keys of collapsible entries flipped from their default.
    toggled: HashSet<SharedString>,
    /// Its version as of `loaded`, to tell when it changes.
    pub(super) version: Option<PullRequestVersion>,
    /// When its version was last checked, or fetched with its detail.
    pub(super) checked_at: Option<Instant>,
    _task: Option<Task<()>>,
    pub(super) _check_task: Option<Task<()>>,
    pub(super) _watch_task: Option<Task<()>>,
    /// Fetching GitHub's viewed files, when they don't fit in its version.
    pub(super) _viewed_task: Option<Task<()>>,
}

/// The open pull request's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrTab {
    #[default]
    Conversation,
    Commits,
    Files,
    /// All its files, as of its head.
    Browse,
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
    /// or else from GitHub. A link to its commits or files opens that tab.
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
        match link.page {
            PullRequestPage::Conversation => {}
            PullRequestPage::Commits => self.select_pr_tab(PrTab::Commits, cx),
            PullRequestPage::Files => self.select_pr_tab(PrTab::Files, cx),
        }
    }

    pub(super) fn show_pull_request(
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
            browse_file: None,
            toggled: HashSet::new(),
            version: None,
            checked_at: None,
            _task: None,
            _check_task: None,
            _watch_task: None,
            _viewed_task: None,
        });
        self.refresh_pull_request(cx);
        self.watch_pull_request(cx);
        if self.detail.as_ref().is_some_and(|d| d.summary.is_some()) {
            self.load_pr_diff(cx);
        }
    }

    pub fn close_pull_request(&mut self, cx: &mut Context<Self>) {
        if self.detail.take().is_some() {
            // Its Browse tab's.
            self.browser = None;
            cx.notify();
        }
    }

    /// Re-fetches the open pull request, keeping what's shown until the new
    /// data arrives.
    pub(super) fn refresh_pull_request(&mut self, cx: &mut Context<Self>) {
        self.load_pull_request(false, cx);
    }

    /// Fetches the open pull request, and its version to watch it by. A
    /// `quiet` load, from watching it, doesn't show it's loading.
    pub(super) fn load_pull_request(&mut self, quiet: bool, cx: &mut Context<Self>) {
        let Some(client) = self.app.read(cx).client() else {
            return;
        };
        let now = cx.background_executor().now();
        let Some(detail) = &mut self.detail else {
            return;
        };
        let (repo, number) = (detail.repo.clone(), detail.number);
        if !quiet {
            detail.loading = true;
        }
        detail.checked_at = Some(now);
        detail._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let repo = repo.clone();
                    async move {
                        // Before the detail, so a change between the two
                        // shows as a new version next check, rather than
                        // being missed.
                        let version = client
                            .pull_request_version(&repo, number)
                            .inspect_err(|err| {
                                eprintln!("failed to check {repo}#{number}: {err:#}")
                            })
                            .ok();
                        let loaded = client.pull_request(&repo, number).map(LoadedDetail::new)?;
                        anyhow::Ok((loaded, version))
                    }
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
                let mut viewed = None;
                match result {
                    Ok((loaded, version)) => {
                        let version = version.map(|(version, files)| {
                            viewed = Some(files);
                            version
                        });
                        // Opened from a link, or its head or base moved:
                        // its diff (re)loads.
                        let summary = &loaded.pr.summary;
                        load_diff = detail.summary.as_ref().is_none_or(|known| {
                            known.head_ref_oid != summary.head_ref_oid
                                || known.base_ref_oid != summary.base_ref_oid
                        });
                        detail.summary = Some(summary.clone());
                        detail.loaded = Some(loaded);
                        detail.version = version;
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
                match viewed {
                    Some(Some(files)) => this.apply_github_viewed(files, cx),
                    // Too many files to come with the version.
                    Some(None) => this.sync_github_viewed(cx),
                    None => {}
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
