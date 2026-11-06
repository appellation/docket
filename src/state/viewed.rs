//! Files marked as viewed in diffs (see `diff::viewed`), shared by every
//! window and saved across launches.
//!
//! A pull request's are GitHub's, the checkboxes on its Files tab, so
//! marking a file in Docket marks it on github.com and the other way
//! around. GitHub's states arrive with the pull request's version
//! (`live.rs`), so they follow github.com as the pull request is watched,
//! and a file is changed since viewed when GitHub says so (`DISMISSED`).
//! A mark made in Docket shows at once, and goes to GitHub in the
//! background, one at a time and in order; one GitHub refuses (a token
//! that can only read, say) stays in Docket, with an error to try again.
//! Until GitHub's states arrive, and for branches, which GitHub doesn't
//! know of, the marks saved on disk answer. GitHub's are saved there too
//! as they arrive.

use std::collections::HashMap;

use anyhow::Result;
use chrono::Utc;
use gpui::{Context, SharedString, Task};

use super::{AppState, Tab};
use crate::diff::{FileDiff, ViewKey, Viewed, ViewedStore};
use crate::github::{FileViewedState, ViewedFiles};
use crate::state::DiffLoad;

#[derive(Default)]
pub(super) struct ViewedMarks {
    store: ViewedStore,
    /// The saved marks have been read, so saving won't lose them.
    loaded: bool,
    /// Marked before they were, so to save once they are.
    unsaved: bool,
    /// Pull requests' viewed files as GitHub has them, by scope.
    github: HashMap<String, GithubViewed>,
    /// Numbers marks sent to GitHub, to tell which is latest.
    marks_sent: u64,
}

struct GithubViewed {
    /// The pull request's node ID, which marks are sent for.
    pull_request_id: String,
    /// GitHub's states, once they've been fetched.
    files: Option<ViewedFiles>,
    /// Marks made in Docket that GitHub doesn't have yet, by path: being
    /// sent, or refused. They show in place of GitHub's.
    unsynced: HashMap<String, Mark>,
    /// Why GitHub last refused a mark.
    error: Option<SharedString>,
    /// Sending marks, each after the one before.
    _mark_task: Option<Task<()>>,
}

#[derive(Clone, Copy)]
struct Mark {
    viewed: bool,
    /// Which send it is (`marks_sent`).
    sent: u64,
    /// Being sent, rather than refused.
    sending: bool,
}

impl GithubViewed {
    fn new(pull_request_id: String) -> Self {
        Self {
            pull_request_id,
            files: None,
            unsynced: HashMap::new(),
            error: None,
            _mark_task: None,
        }
    }
}

impl AppState {
    pub(super) fn load_viewed(&mut self, cx: &mut Context<Self>) {
        self._load_viewed_task = Some(cx.spawn(async move |this, cx| {
            let saved = cx
                .background_executor()
                .spawn(async { ViewedStore::load() })
                .await;
            this.update(cx, |this, cx| {
                this.viewed.store.merge(saved);
                this.viewed.loaded = true;
                if this.viewed.unsaved {
                    this.save_viewed(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Whether `file` is marked as viewed in the diff `scope`.
    pub fn viewed(&self, scope: &str, file: &FileDiff) -> Viewed {
        let github = self.viewed.github.get(scope);
        if let Some(mark) = github.and_then(|g| g.unsynced.get(file.path())) {
            return if mark.viewed { Viewed::Yes } else { Viewed::No };
        }
        match github.and_then(|g| g.files.as_ref()) {
            Some(files) => match files.get(file.path()) {
                Some(FileViewedState::Viewed) => Viewed::Yes,
                Some(FileViewedState::Dismissed) => Viewed::Changed,
                Some(FileViewedState::Unviewed) | None => Viewed::No,
            },
            None => self.viewed.store.viewed(scope, file),
        }
    }

    /// Why GitHub refused a file of the pull request `scope` being marked
    /// as viewed, or unmarked.
    pub fn viewed_error(&self, scope: &str) -> Option<&SharedString> {
        self.viewed.github.get(scope)?.error.as_ref()
    }

    /// Marks the file at `path` as viewed in the diff `scope` while its
    /// key is `key`, or with `None`, unmarks it. A pull request's file
    /// (`pull_request_id` its node ID) is marked on GitHub too.
    pub fn set_viewed(
        &mut self,
        scope: &str,
        path: &str,
        key: Option<ViewKey>,
        pull_request_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let viewed = key.is_some();
        self.viewed.store.set(scope, path, key, Utc::now());
        self.save_viewed(cx);
        if let Some(id) = pull_request_id {
            self.viewed
                .github
                .entry(scope.to_owned())
                .or_insert_with(|| GithubViewed::new(id.to_owned()));
            self.send_mark(scope, path, viewed, cx);
        }
        cx.notify();
    }

    /// Sends the marks GitHub refused again.
    pub fn retry_viewed(&mut self, scope: &str, cx: &mut Context<Self>) {
        let Some(github) = self.viewed.github.get_mut(scope) else {
            return;
        };
        github.error = None;
        let refused: Vec<_> = github
            .unsynced
            .iter()
            .filter(|(_, mark)| !mark.sending)
            .map(|(path, mark)| (path.clone(), mark.viewed))
            .collect();
        for (path, viewed) in refused {
            self.send_mark(scope, &path, viewed, cx);
        }
        cx.notify();
    }

    fn send_mark(&mut self, scope: &str, path: &str, viewed: bool, cx: &mut Context<Self>) {
        let Some(client) = self.client() else {
            return;
        };
        self.viewed.marks_sent += 1;
        let sent = self.viewed.marks_sent;
        let Some(github) = self.viewed.github.get_mut(scope) else {
            return;
        };
        let mark = Mark {
            viewed,
            sent,
            sending: true,
        };
        github.unsynced.insert(path.to_owned(), mark);
        let id = github.pull_request_id.clone();
        let (scope, path) = (scope.to_owned(), path.to_owned());
        // After the one before, so GitHub ends up with the last.
        let previous = github._mark_task.take();
        github._mark_task = Some(cx.spawn(async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let result = cx
                .background_executor()
                .spawn({
                    let path = path.clone();
                    async move { client.set_file_viewed(&id, &path, viewed) }
                })
                .await;
            this.update(cx, |this, cx| {
                this.finish_mark(&scope, &path, mark, result, cx);
            })
            .ok();
        }));
    }

    fn finish_mark(
        &mut self,
        scope: &str,
        path: &str,
        mark: Mark,
        result: Result<()>,
        cx: &mut Context<Self>,
    ) {
        let Some(github) = self.viewed.github.get_mut(scope) else {
            return;
        };
        let latest = github
            .unsynced
            .get(path)
            .is_some_and(|m| m.sent == mark.sent);
        match result {
            Ok(()) => {
                let state = if mark.viewed {
                    FileViewedState::Viewed
                } else {
                    FileViewedState::Unviewed
                };
                github
                    .files
                    .get_or_insert_default()
                    .insert(path.to_owned(), state);
                if latest {
                    github.unsynced.remove(path);
                }
                if github.unsynced.values().all(|m| m.sending) {
                    github.error = None;
                }
            }
            Err(err) => {
                eprintln!("failed to mark {path} in {scope} on GitHub: {err:#}");
                if latest && let Some(m) = github.unsynced.get_mut(path) {
                    m.sending = false;
                }
                github.error = Some(format!("{err:#}").into());
            }
        }
        cx.notify();
    }

    /// Takes GitHub's viewed states for the pull request `scope`, saving
    /// them for the files `keys` lists (each path with its key).
    fn set_github_viewed(
        &mut self,
        scope: &str,
        pull_request_id: &str,
        files: ViewedFiles,
        keys: Vec<(String, ViewKey)>,
        cx: &mut Context<Self>,
    ) {
        let github = self
            .viewed
            .github
            .entry(scope.to_owned())
            .or_insert_with(|| GithubViewed::new(pull_request_id.to_owned()));
        github.pull_request_id = pull_request_id.to_owned();
        // A refused mark that GitHub has now, made on github.com, say.
        github.unsynced.retain(|path, mark| {
            let state = files.get(path).copied();
            let matches = if mark.viewed {
                state == Some(FileViewedState::Viewed)
            } else {
                state != Some(FileViewedState::Viewed)
            };
            mark.sending || !matches
        });
        if github.unsynced.is_empty() {
            github.error = None;
        }

        let now = Utc::now();
        let mut changed = false;
        for (path, key) in keys {
            if github.unsynced.contains_key(&path) {
                continue;
            }
            let saved = self.viewed.store.key(scope, &path);
            match files.get(&path) {
                Some(FileViewedState::Viewed) if saved != Some(&key) => {
                    self.viewed.store.set(scope, &path, Some(key), now);
                    changed = true;
                }
                Some(FileViewedState::Unviewed) | None if saved.is_some() => {
                    self.viewed.store.set(scope, &path, None, now);
                    changed = true;
                }
                _ => {}
            }
        }
        github.files = Some(files);
        if changed {
            self.save_viewed(cx);
        }
        cx.notify();
    }

    fn save_viewed(&mut self, cx: &mut Context<Self>) {
        if !self.viewed.loaded {
            self.viewed.unsaved = true;
            return;
        }
        self.viewed.unsaved = false;
        let store = self.viewed.store.clone();
        // Chain onto the previous save so writes land in order.
        let previous = self._save_viewed_task.take();
        self._save_viewed_task = Some(cx.background_executor().spawn(async move {
            if let Some(previous) = previous {
                previous.await;
            }
            if let Err(err) = store.save(Utc::now()) {
                eprintln!("failed to save viewed files: {err:#}");
            }
        }));
    }
}

/// The scope of a pull request's viewed files.
pub(super) fn pull_request_scope(repo: &crate::github::RepoRef, number: u64) -> String {
    format!("{repo}#{number}")
}

impl Tab {
    /// The open pull request's node ID, when the open diff is its, to mark
    /// its files as viewed on GitHub.
    pub fn viewed_pull_request_id(&self) -> Option<&str> {
        Some(&self.detail.as_ref()?.summary.as_ref()?.id)
    }

    /// Takes GitHub's viewed states for the open pull request.
    pub(super) fn apply_github_viewed(&mut self, files: ViewedFiles, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        let Some(id) = detail.summary.as_ref().map(|pr| pr.id.clone()) else {
            return;
        };
        let scope = pull_request_scope(&detail.repo, detail.number);
        let keys = match detail.files.as_ref().map(|files| &files.diff) {
            Some(DiffLoad::Ready(diff)) => diff
                .files
                .iter()
                .map(|file| (file.path().to_owned(), file.view_key.clone()))
                .collect(),
            _ => Vec::new(),
        };
        self.app.update(cx, |app, cx| {
            app.set_github_viewed(&scope, &id, files, keys, cx);
        });
    }

    /// Fetches GitHub's viewed states for the open pull request, when they
    /// didn't fit in its version.
    pub(super) fn sync_github_viewed(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.app.read(cx).client() else {
            return;
        };
        let Some(detail) = &mut self.detail else {
            return;
        };
        let (repo, number) = (detail.repo.clone(), detail.number);
        detail._viewed_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let repo = repo.clone();
                    async move { client.viewed_files(&repo, number) }
                })
                .await;
            this.update(cx, |this, cx| {
                let open = this
                    .detail
                    .as_ref()
                    .is_some_and(|d| d.repo == repo && d.number == number);
                match result {
                    Ok(files) if open => this.apply_github_viewed(files, cx),
                    Ok(_) => {}
                    Err(err) => {
                        eprintln!("failed to read {repo}#{number}'s viewed files: {err:#}")
                    }
                }
            })
            .ok();
        }));
    }
}
