//! Docket's Chrome extension (`chrome`): installing it, and keeping the
//! pull requests it opens in Docket (`OpenLinks`) up to date for its host.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use gpui::{Context, Task};

use super::{AppEvent, AppState, AuthStatus};
use crate::chrome::OpenLinks;
use crate::github::GithubClient;

impl AppState {
    /// Writes the extension's files again where they've changed, if it's
    /// installed, and from then on keeps its links current.
    pub(super) fn watch_extension(&mut self, cx: &mut Context<Self>) {
        if let Some(install) = self.extension_install.clone() {
            cx.background_executor()
                .spawn(async move {
                    if let Err(err) = install.refresh() {
                        eprintln!("failed to update the Chrome extension: {err:#}");
                    }
                })
                .detach();
        }
        cx.subscribe_self(|this, event: &AppEvent, cx| match event {
            AppEvent::PullRequestsLoaded
            | AppEvent::ProjectsLoaded
            | AppEvent::ProjectSaved(_)
            | AppEvent::ProjectRemoved(_)
            | AppEvent::SignedOut => this.save_open_links(cx),
            _ => {}
        })
        .detach();
    }

    /// The pull requests the extension opens in Docket: every one in a
    /// project's repository, and the docket's.
    pub fn open_links(&self) -> OpenLinks {
        OpenLinks::new(
            self.projects.iter().map(|project| &project.repository),
            self.sections
                .iter()
                .flat_map(|section| &section.pull_requests),
        )
    }

    /// Saves [`Self::open_links`] for the extension's host, when they've
    /// changed since last saved.
    fn save_open_links(&mut self, cx: &mut Context<Self>) {
        let links = self.open_links();
        if self.saved_open_links.as_ref() == Some(&links) {
            return;
        }
        self.saved_open_links = Some(links.clone());
        // Chain onto the previous save so writes land in order.
        let previous = self._save_open_links_task.take();
        self._save_open_links_task = Some(cx.background_executor().spawn(async move {
            if let Some(previous) = previous {
                previous.await;
            }
            if let Err(err) = links.save() {
                eprintln!("failed to save the Chrome extension's links: {err:#}");
            }
        }));
    }

    /// The client to check a pull request the extension asks for with, or
    /// why there's none; `None` while a sign-in is still being restored or
    /// checked, as when the extension launched Docket.
    pub fn extension_client(&self) -> Option<Result<GithubClient, String>> {
        match &self.auth {
            AuthStatus::Restoring | AuthStatus::Verifying => None,
            _ => Some(
                self.client()
                    .ok_or_else(|| "Docket isn't signed in to GitHub".to_owned()),
            ),
        }
    }

    /// Installs the extension: writes its files and registers Docket as
    /// its host. Resolves to the folder the browser loads it from.
    pub fn install_chrome_extension(&self, cx: &mut Context<Self>) -> Task<Result<PathBuf>> {
        let install = self.extension_install.clone();
        cx.background_executor().spawn(async move {
            let install = install.context("Docket can't install its extension here")?;
            install.install()?;
            Ok(install.extension_dir)
        })
    }

    /// Installs the extension under `install` rather than the user's
    /// Application Support.
    #[cfg(test)]
    pub fn set_extension_install(&mut self, install: crate::chrome::Install) {
        self.extension_install = Some(install);
    }
}
