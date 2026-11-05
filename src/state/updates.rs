//! Keeping Docket up to date with its latest release (`updater.rs`).
//!
//! While signed in, Docket checks for a release every [`UPDATE_INTERVAL`],
//! with a GraphQL query of about one point. A newer one with a build for
//! this Mac downloads and installs on its own, and then waits for the user
//! to restart into it; quitting has the same effect. Checks on their own
//! are quiet. Only Check for Updates… shows that it's checking, and what it
//! found, even when that's nothing or an error.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{Context, SharedString, Task};

use super::AppState;
use crate::github::Release;
use crate::updater::{self, Version};

/// How often Docket checks for a new release.
pub const UPDATE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// How long "up to date" shows after checking by hand.
const UP_TO_DATE_SHOWN: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateStatus {
    Idle,
    /// Checking because the user asked to.
    Checking,
    /// The user asked, and this is the latest.
    UpToDate,
    Downloading(Version),
    /// Installed: restarting runs it.
    Ready(Version),
    /// A newer release Docket can't install itself, from its page: it
    /// isn't running from an app bundle, or there's no build for this Mac.
    Available {
        version: Version,
        url: SharedString,
    },
    Failed {
        message: SharedString,
        /// The release's page, to download it by hand.
        url: Option<SharedString>,
    },
}

pub struct Updates {
    pub status: UpdateStatus,
    /// The app bundle updates install into; without one, they're only
    /// announced.
    bundle: Option<PathBuf>,
    checked_at: Option<Instant>,
    _watch_task: Option<Task<()>>,
    _check_task: Option<Task<()>>,
    _install_task: Option<Task<()>>,
    _clear_task: Option<Task<()>>,
}

impl Updates {
    pub fn new() -> Self {
        Self {
            status: UpdateStatus::Idle,
            bundle: updater::running_bundle(),
            checked_at: None,
            _watch_task: None,
            _check_task: None,
            _install_task: None,
            _clear_task: None,
        }
    }
}

impl AppState {
    pub fn update_status(&self) -> &UpdateStatus {
        &self.updates.status
    }

    /// Installs updates into `bundle`, and checks for them from now on, as
    /// if Docket were running from it.
    #[cfg(test)]
    pub fn set_update_bundle(&mut self, bundle: PathBuf, cx: &mut Context<Self>) {
        self.updates.bundle = Some(bundle);
        self.watch_updates(cx);
    }

    /// Checks for updates from now until signing out. Only from an app
    /// bundle: `cargo run` has nothing to update, so it checks only when
    /// asked.
    pub(super) fn watch_updates(&mut self, cx: &mut Context<Self>) {
        if self.updates.bundle.is_none() {
            return;
        }
        self.updates._watch_task = Some(cx.spawn(async move |this, cx| {
            while let Ok(wait) = this.update(cx, |this, cx| this.poll_updates(cx)) {
                cx.background_executor().timer(wait).await;
            }
        }));
    }

    pub(super) fn stop_watching_updates(&mut self) {
        self.updates._watch_task = None;
        self.updates._check_task = None;
        self.updates.checked_at = None;
    }

    /// Checks for an update if it's been [`UPDATE_INTERVAL`] since the last
    /// check. Returns how long until the next is due.
    fn poll_updates(&mut self, cx: &mut Context<Self>) -> Duration {
        let now = cx.background_executor().now();
        if let Some(due) = self.updates.checked_at.map(|at| at + UPDATE_INTERVAL)
            && now < due
        {
            return due - now;
        }
        self.check_for_updates(false, cx);
        UPDATE_INTERVAL
    }

    /// Checks for a newer release, and installs it if Docket can. `manual`
    /// (Check for Updates…) shows the check and its outcome.
    pub fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        // Already found.
        if matches!(
            self.updates.status,
            UpdateStatus::Downloading(_) | UpdateStatus::Ready(_)
        ) {
            return;
        }
        let Some(client) = self.client() else {
            if manual {
                self.set_update_status(
                    UpdateStatus::Failed {
                        message: "Sign in to check for updates.".into(),
                        url: None,
                    },
                    cx,
                );
            }
            return;
        };
        self.updates.checked_at = Some(cx.background_executor().now());
        if manual {
            self.set_update_status(UpdateStatus::Checking, cx);
        }
        self.updates._check_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let client = client.clone();
                    async move { client.latest_release(&updater::releases_repo()) }
                })
                .await;
            this.update(cx, |this, cx| match result {
                Ok(release) => this.found_release(release, manual, cx),
                Err(err) => {
                    eprintln!("failed to check for updates: {err:#}");
                    if manual {
                        this.set_update_status(
                            UpdateStatus::Failed {
                                message: format!("Couldn't check for updates: {err:#}").into(),
                                url: None,
                            },
                            cx,
                        );
                    }
                }
            })
            .ok();
        }));
    }

    fn found_release(&mut self, release: Option<Release>, manual: bool, cx: &mut Context<Self>) {
        let newer = release.and_then(|release| {
            let version = Version::parse(&release.tag)?;
            (version > Version::current()).then_some((release, version))
        });
        let Some((release, version)) = newer else {
            if manual {
                self.set_update_status(UpdateStatus::UpToDate, cx);
                self.updates._clear_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(UP_TO_DATE_SHOWN).await;
                    this.update(cx, |this, cx| {
                        if this.updates.status == UpdateStatus::UpToDate {
                            this.set_update_status(UpdateStatus::Idle, cx);
                        }
                    })
                    .ok();
                }));
            } else if !matches!(self.updates.status, UpdateStatus::UpToDate) {
                // What was found before has gone, say a release was pulled.
                self.set_update_status(UpdateStatus::Idle, cx);
            }
            return;
        };
        let url: SharedString = release.url.into();
        let asset_name = updater::asset_name();
        let asset = release.assets.into_iter().find(|a| a.name == asset_name);
        let (Some(bundle), Some(asset), Some(client)) =
            (self.updates.bundle.clone(), asset, self.client())
        else {
            self.set_update_status(UpdateStatus::Available { version, url }, cx);
            return;
        };

        self.set_update_status(UpdateStatus::Downloading(version), cx);
        self.updates._install_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { updater::install(&client, &asset, version, &bundle) })
                .await;
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(()) => UpdateStatus::Ready(version),
                    // Say a bundle the user can't write to. The next check
                    // tries again.
                    Err(err) => {
                        eprintln!("failed to install Docket {version}: {err:#}");
                        UpdateStatus::Failed {
                            message: format!("Couldn't install Docket {version}: {err:#}").into(),
                            url: Some(url),
                        }
                    }
                };
                this.set_update_status(status, cx);
            })
            .ok();
        }));
    }

    /// Quits, and opens the installed update.
    pub fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        let (UpdateStatus::Ready(_), Some(bundle)) = (&self.updates.status, &self.updates.bundle)
        else {
            return;
        };
        match updater::relaunch(bundle) {
            Ok(()) => cx.quit(),
            Err(err) => {
                eprintln!("failed to restart: {err:#}");
                self.set_update_status(
                    UpdateStatus::Failed {
                        message: format!(
                            "Couldn't restart Docket: {err:#}. Quit and open it again to update."
                        )
                        .into(),
                        url: None,
                    },
                    cx,
                );
            }
        }
    }

    /// Hides an update that's available, or a failure.
    pub fn dismiss_update(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.updates.status,
            UpdateStatus::Available { .. } | UpdateStatus::Failed { .. } | UpdateStatus::UpToDate
        ) {
            self.set_update_status(UpdateStatus::Idle, cx);
        }
    }

    fn set_update_status(&mut self, status: UpdateStatus, cx: &mut Context<Self>) {
        self.updates.status = status;
        cx.notify();
    }
}
