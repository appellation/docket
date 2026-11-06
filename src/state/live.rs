//! Live updates: the docket and the open pull request follow GitHub as it
//! changes, without a refresh.
//!
//! GitHub has no way to push changes to Docket: webhooks need a server to
//! receive them, and its GraphQL API has no subscriptions. So Docket polls,
//! as cheaply as it can, and quietly: what's shown stays until what replaces
//! it arrives, with no spinners.
//!
//! - The docket's searches (a point of rate limit each) run again every
//!   [`DOCKET_INTERVAL`], with the projects' branches, which are local.
//! - An open pull request's detail costs dozens of points, so it's watched
//!   through its version ([`GithubClient::pull_request_version`]), about one
//!   point, every [`PULL_REQUEST_INTERVAL`], and fetched again only when that
//!   changes. Only while its window is active: there may be a pull request
//!   open in every tab. The check also brings the files marked as viewed
//!   (`viewed.rs`), which aren't part of the version.
//!
//! A window that becomes active catches up on whatever is due at once, so
//! coming back to Docket shows where things stand. Together that's about
//! 420 of GitHub's 5,000 points an hour while a pull request is open.
//!
//! [`GithubClient::pull_request_version`]: crate::github::GithubClient::pull_request_version

use std::time::{Duration, Instant};

use gpui::Context;

use super::{AppState, Tab};

/// How often the docket loads again.
pub const DOCKET_INTERVAL: Duration = Duration::from_secs(60);
/// How often an open pull request is checked for changes, while its
/// window is active.
pub const PULL_REQUEST_INTERVAL: Duration = Duration::from_secs(15);

/// How long until `interval` has passed since `last`, or `None` if it has
/// (or there was none).
fn wait_after(last: Option<Instant>, interval: Duration, now: Instant) -> Option<Duration> {
    let due = last? + interval;
    (now < due).then(|| due - now)
}

impl AppState {
    /// Polls the docket from now until signing out.
    pub(super) fn watch_docket(&mut self, cx: &mut Context<Self>) {
        self._watch_task = Some(cx.spawn(async move |this, cx| {
            while let Ok(wait) = this.update(cx, |this, cx| this.poll_docket(cx)) {
                cx.background_executor().timer(wait).await;
            }
        }));
    }

    /// Loads the docket again if it's been [`DOCKET_INTERVAL`] since it
    /// last did. Returns how long until it's due next.
    pub(super) fn poll_docket(&mut self, cx: &mut Context<Self>) -> Duration {
        let now = cx.background_executor().now();
        if let Some(wait) = wait_after(self.docket_loaded_at, DOCKET_INTERVAL, now) {
            return wait;
        }
        self.load_docket(true, cx);
        DOCKET_INTERVAL
    }
}

impl Tab {
    /// Called as the tab's window becomes active or inactive. Its pull
    /// request is only watched while it's active, and it catches up on
    /// whatever is due as it becomes active.
    pub fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.window_active = active;
        if active {
            self.app.update(cx, |app, cx| app.poll_docket(cx));
            self.poll_pull_request(cx);
        }
    }

    /// Watches the open pull request until it closes.
    pub(super) fn watch_pull_request(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            while let Ok(Some(wait)) = this.update(cx, |this, cx| this.poll_pull_request(cx)) {
                cx.background_executor().timer(wait).await;
            }
        });
        if let Some(detail) = &mut self.detail {
            detail._watch_task = Some(task);
        }
    }

    /// Checks whether the open pull request has changed, if its window is
    /// active and it's been [`PULL_REQUEST_INTERVAL`] since it was last
    /// checked, and loads it again if it has. Returns how long until it's
    /// due next, or `None` with nothing open to watch.
    fn poll_pull_request(&mut self, cx: &mut Context<Self>) -> Option<Duration> {
        let client = self.app.read(cx).client()?;
        let active = self.window_active;
        let detail = self.detail.as_mut()?;
        let now = cx.background_executor().now();
        if let Some(wait) = wait_after(detail.checked_at, PULL_REQUEST_INTERVAL, now) {
            return Some(wait);
        }
        if !active {
            return Some(PULL_REQUEST_INTERVAL);
        }
        detail.checked_at = Some(now);
        let (repo, number) = (detail.repo.clone(), detail.number);
        detail._check_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let repo = repo.clone();
                    async move { client.pull_request_version(&repo, number) }
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(detail) = &this.detail else {
                    return;
                };
                match result {
                    Ok((version, viewed)) => {
                        if detail.version.as_ref() != Some(&version) {
                            this.load_pull_request(true, cx);
                        }
                        // Marked as viewed on github.com, say.
                        match viewed {
                            Some(viewed) => this.apply_github_viewed(viewed, cx),
                            None => this.sync_github_viewed(cx),
                        }
                    }
                    // Offline, say. What's shown stays, and the next check
                    // tries again.
                    Err(err) => eprintln!("failed to check {repo}#{number} for changes: {err:#}"),
                }
            })
            .ok();
        }));
        Some(PULL_REQUEST_INTERVAL)
    }
}
