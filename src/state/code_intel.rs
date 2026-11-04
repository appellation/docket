//! Language servers, for the open diff (a PR's Files tab, or a branch's
//! changes) and for browsing a project's files.
//!
//! For a diff, once it loads, Docket checks out its head in its worktree
//! for the project (see `lsp::worktree`) and starts a server for each
//! language on the diff's new side, using the project's own toolchain where
//! it has one (see `lsp::servers`). Servers stop when the PR or branch
//! closes or its diff reloads, since they live in `DiffFiles`, so each tab
//! has its own.
//!
//! Once a server is running, Docket also asks it for the outline of each
//! file it serves whose excerpts start partway into the file, so hunk
//! headers can name the scopes they're in.
//!
//! Some servers run code from the project they analyze: rust-analyzer runs
//! build scripts, proc macros, and whatever `.cargo/config.toml` and
//! `rust-analyzer.toml` tell it to; Go runs the toolchain `go.mod` names.
//! For a PR from a fork, that's code from a stranger, so those servers wait
//! for the user to start them. Branches in the repository itself need write
//! access to push, so they're trusted, as are the user's own PRs and local
//! branches.
//!
//! Browsing runs servers where the files are, starting each the first time
//! a file in its language opens. In the user's own checkout (or a branch
//! not checked out there, in Docket's worktree) every server starts
//! without asking, as their editor's would, and stops when the browser
//! closes. A pull request's or branch's Browse tab is in Docket's worktree
//! at the diff's head, so it shares the diff's servers rather than running
//! a second set in the same folder, adding any its files need; they wait on
//! trust as the diff's do.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures::channel::mpsc;
use futures::{FutureExt as _, StreamExt as _};
use gpui::{App, AsyncApp, Context, SharedString, Task};

use super::{AuthStatus, BrowserScope, DiffFiles, DiffLoad, FileLoad, Tab};
use crate::diff::{FileContent, TextSnapshot};
use crate::github::PullRequest;
use crate::lsp::{
    self, LanguageServer, Missing, Outline, Resolved, ServerActivity, ServerEvent, ServerSpec,
    server_for, worktree,
};
use crate::projects::Checkout;
use crate::projects::files::LocalFile;
use crate::syntax::Language;

/// Longest to wait for a server to answer `initialize`.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// A started server counts as loading until it reports progress, or for
/// this long if it reports none. Some (typescript-language-server) answer
/// right away from a partial view of the project, before saying they're
/// loading the rest.
const STARTUP_GRACE: Duration = Duration::from_secs(5);
/// Tries at an outline before giving up, for servers that turn requests
/// away while they load.
const OUTLINE_ATTEMPTS: u32 = 5;
const OUTLINE_RETRY_INTERVAL: Duration = Duration::from_secs(2);

pub struct CodeIntel {
    /// Where the servers run: Docket's worktree at the diff's head once
    /// it's checked out, or the browsed checkout.
    pub root: Option<PathBuf>,
    /// Why the worktree couldn't be prepared.
    pub error: Option<SharedString>,
    pub servers: Vec<ServerSlot>,
    /// Outlines of the diff's files on the new side, by file index.
    pub outlines: HashMap<usize, Arc<Outline>>,
    /// The worktree is being prepared; servers asked for meanwhile start
    /// once it's ready.
    preparing: bool,
    _prepare_task: Option<Task<()>>,
}

/// Whose servers: the open diff's (a PR's or a branch's), or the file
/// browser's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntelOwner {
    Diff,
    Browser,
}

/// Where a server's slot is, for its tasks to find it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntelKey {
    /// The open diff, at a `DiffFiles::revision`. Superseded loads of the
    /// same diff don't match.
    Diff { revision: u64 },
    /// The browsed project. Its servers go with the browser, so nothing
    /// stale can match.
    Browser,
}

pub struct ServerSlot {
    pub spec: &'static ServerSpec,
    /// Where the server comes from, e.g. "project TypeScript 6.0.2".
    pub source: Option<SharedString>,
    pub state: ServerState,
    _task: Option<Task<()>>,
    _outline_tasks: Vec<Task<()>>,
}

pub enum ServerState {
    /// Not found in the project or on the system.
    NotInstalled {
        install: SharedString,
    },
    /// Runs project code, and the PR is from a fork: waiting for the user
    /// to allow it.
    NeedsTrust,
    Starting,
    Running {
        server: Arc<LanguageServer>,
        activity: ServerActivity,
    },
    Failed(SharedString),
}

/// The worktree, if any server will start, and how to run each server.
type Prepared = (
    Option<PathBuf>,
    Vec<(&'static ServerSpec, Result<Resolved, Missing>)>,
);

/// What a request to a file's server (hover, definition) needs, for a file
/// on the diff's new side or in the browsed checkout.
pub struct LspTarget {
    pub server: Arc<LanguageServer>,
    /// Where the server runs: the PR's worktree, or the checkout.
    pub root: PathBuf,
    /// The file where the server runs.
    pub path: PathBuf,
    pub language_id: &'static str,
    pub text: Arc<TextSnapshot>,
    /// Whether the server is still loading, so an empty answer may just
    /// mean "not yet".
    pub busy: bool,
}

impl DiffFiles {
    /// The outline of a file's new side, once its server has given it.
    pub fn outline(&self, file: usize) -> Option<&Arc<Outline>> {
        self.code_intel.as_ref()?.outlines.get(&file)
    }

    pub fn lsp_target(&self, file: usize) -> Option<LspTarget> {
        let DiffLoad::Ready(diff) = &self.diff else {
            return None;
        };
        let file = diff.files.get(file)?;
        let FileContent::Text(content) = &file.content else {
            return None;
        };
        let text = content.new.clone()?;
        let path = file.new_path.as_deref()?;
        self.code_intel.as_ref()?.lsp_target(path, text)
    }
}

impl Tab {
    /// Whose servers the browser uses: in a pull request's or branch's
    /// Browse tab in Docket's worktree, the diff's, which run in the same
    /// checkout at the same head; otherwise its own.
    fn browser_intel_key(&self) -> Option<IntelKey> {
        let browser = self.browser.as_ref()?;
        if browser.scope != BrowserScope::Project && browser.in_docket_worktree() {
            let files = self.diff_files()?;
            return Some(IntelKey::Diff {
                revision: files.revision,
            });
        }
        Some(IntelKey::Browser)
    }

    /// Whose servers the browser's status and retries are for.
    pub fn browser_intel_owner(&self) -> IntelOwner {
        match self.browser_intel_key() {
            Some(IntelKey::Diff { .. }) => IntelOwner::Diff,
            _ => IntelOwner::Browser,
        }
    }

    /// The servers for the browsed files.
    pub fn browser_code_intel(&self) -> Option<&CodeIntel> {
        match self.browser_intel_key()? {
            IntelKey::Diff { .. } => self.diff_files()?.code_intel.as_ref(),
            IntelKey::Browser => self.browser.as_ref()?.code_intel.as_ref(),
        }
    }

    /// For the browser's open file, once its server is running.
    pub fn browser_lsp_target(&self) -> Option<LspTarget> {
        let file = self.browser.as_ref()?.file.as_ref()?;
        let FileLoad::Ready(content) = &file.content else {
            return None;
        };
        let LocalFile::Text(text) = &**content else {
            return None;
        };
        self.browser_code_intel()?
            .lsp_target(&file.path, text.clone())
    }

    /// Starts the server for the browser's open file, if it has none.
    pub(super) fn start_open_browser_server(&mut self, cx: &mut Context<Self>) {
        if let Some(language) = self.browser.as_ref().and_then(|b| b.open_language()) {
            self.start_browser_server(language, cx);
        }
    }
}

impl CodeIntel {
    /// For `text`, at `path` relative to where the servers run, if its
    /// language's server is running.
    fn lsp_target(&self, path: &str, text: Arc<TextSnapshot>) -> Option<LspTarget> {
        let language = text.language?;
        let spec = server_for(language)?;
        let slot = self
            .servers
            .iter()
            .find(|slot| std::ptr::eq(slot.spec, spec))?;
        let ServerState::Running { server, activity } = &slot.state else {
            return None;
        };
        let root = self.root.clone()?;
        Some(LspTarget {
            server: server.clone(),
            path: root.join(path),
            root,
            language_id: lsp::language_id(path, language),
            text,
            busy: activity.busy,
        })
    }
}

impl Tab {
    /// Starts the servers that wait on trust, for this PR's current head.
    pub fn trust_code_intel(&mut self, cx: &mut Context<Self>) {
        if let Some(files) = self.diff_files_mut() {
            files.allow_project_code = true;
            self.retry_code_intel(IntelOwner::Diff, cx);
        }
    }

    /// Looks for servers and starts them again, e.g. after installing one.
    pub fn retry_code_intel(&mut self, owner: IntelOwner, cx: &mut Context<Self>) {
        if owner == IntelOwner::Browser {
            let Some(browser) = &mut self.browser else {
                return;
            };
            browser.code_intel = None;
            if let Some(language) = browser.open_language() {
                self.start_browser_server(language, cx);
            }
            cx.notify();
            return;
        }
        let Some(files) = self.diff_files_mut() else {
            return;
        };
        if !matches!(files.diff, DiffLoad::Ready(_)) {
            return;
        }
        // Dropping the old servers stops them.
        files.code_intel = None;
        let revision = files.revision;
        self.start_code_intel(revision, cx);
        // Those the Browse tab added, once the worktree is ready.
        self.start_open_browser_server(cx);
        cx.notify();
    }

    /// Starts language servers for the diff loaded at `revision`.
    pub(super) fn start_code_intel(&mut self, revision: u64, cx: &mut Context<Self>) {
        self.start_code_intel_with(revision, &[], cx);
    }

    /// Starts the diff's language servers, and `extra` ones the Browse tab
    /// asked for.
    fn start_code_intel_with(
        &mut self,
        revision: u64,
        extra: &[&'static ServerSpec],
        cx: &mut Context<Self>,
    ) {
        let Some(files) = self.diff_files_at(revision) else {
            return;
        };
        let DiffLoad::Ready(diff) = &files.diff else {
            return;
        };
        let mut specs = diff_specs(files);
        for spec in extra {
            if !specs.iter().any(|s| std::ptr::eq(*s, *spec)) {
                specs.push(spec);
            }
        }
        let head = diff.head_oid.clone();
        let allowed = files.allow_project_code;
        let project = files
            .project
            .and_then(|id| self.app.read(cx).project(id))
            .cloned();
        let (Some(project), false) = (project, specs.is_empty()) else {
            return;
        };
        let trusted = allowed || self.diff_is_trusted(cx);

        let prepare = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let checkout = Checkout::scan(&project.path)?;
                    let servers: Vec<_> = specs
                        .into_iter()
                        .map(|spec| (spec, spec.resolve(&checkout.root)))
                        .collect();
                    let starting: Vec<_> = servers
                        .iter()
                        .filter(|(spec, resolved)| {
                            resolved.is_ok() && (trusted || !spec.runs_project_code)
                        })
                        .map(|(spec, _)| *spec)
                        .collect();
                    // Don't touch the worktree if no server will start.
                    if starting.is_empty() {
                        return Ok((None, servers));
                    }
                    let dir = worktree::prepare(&checkout.root, project.id, &head)?;
                    if starting.iter().any(|spec| spec.links_dependencies) {
                        // Hover still works without them, just with fewer types.
                        if let Err(err) = worktree::link_dependencies(&checkout.root, &dir) {
                            eprintln!("couldn't link dependencies into the worktree: {err:#}");
                        }
                    }
                    Ok((Some(dir), servers))
                })
                .await;
            this.update(cx, |this, cx| {
                this.worktree_ready(revision, trusted, result, cx)
            })
            .ok();
        });
        let Some(files) = self.diff_files_at(revision) else {
            return;
        };
        files.code_intel = Some(CodeIntel {
            root: None,
            error: None,
            servers: Vec::new(),
            outlines: HashMap::new(),
            preparing: true,
            _prepare_task: Some(prepare),
        });
    }

    /// Starts the diff's servers that aren't running for it yet, as when
    /// the Browse tab started its servers before the Files tab showed.
    pub(super) fn add_diff_servers(&mut self, revision: u64, cx: &mut Context<Self>) {
        let specs = match self.diff_files_at(revision) {
            Some(files) if files.code_intel.is_some() => diff_specs(files),
            _ => return,
        };
        for spec in specs {
            self.ensure_diff_server(revision, spec, cx);
        }
    }

    /// Adds `spec`'s server to the diff's, starting them if they haven't
    /// started, for a file the Browse tab opened.
    fn ensure_diff_server(
        &mut self,
        revision: u64,
        spec: &'static ServerSpec,
        cx: &mut Context<Self>,
    ) {
        let trusted = self.diff_is_trusted(cx);
        let browser_root = self.browser_root(cx);
        let checkout = self
            .diff_files()
            .and_then(|files| files.project)
            .and_then(|id| self.app.read(cx).project(id))
            .map(|project| project.path.clone());
        let Some(files) = self.diff_files_at(revision) else {
            return;
        };
        if !matches!(files.diff, DiffLoad::Ready(_)) {
            return;
        }
        let trusted = trusted || files.allow_project_code;
        let Some(intel) = &mut files.code_intel else {
            self.start_code_intel_with(revision, &[spec], cx);
            return;
        };
        let added = intel
            .servers
            .iter()
            .any(|slot| std::ptr::eq(slot.spec, spec));
        // Once it's prepared, `worktree_ready` asks again.
        if added || intel.preparing || intel.error.is_some() {
            return;
        }
        // No server of the diff's started, so it didn't prepare the
        // worktree; the Browse tab's listing did.
        let Some(root) = intel.root.clone().or(browser_root) else {
            return;
        };
        intel.root = Some(root.clone());
        self.add_server(
            IntelKey::Diff { revision },
            spec,
            root,
            trusted,
            checkout,
            cx,
        );
    }

    /// Starts the server for `language` for the browsed files, unless
    /// it's already there (running, starting, or not installed).
    pub(super) fn start_browser_server(
        &mut self,
        language: &'static Language,
        cx: &mut Context<Self>,
    ) {
        let Some(spec) = server_for(language) else {
            return;
        };
        let key = match self.browser_intel_key() {
            Some(IntelKey::Diff { revision }) => {
                self.ensure_diff_server(revision, spec, cx);
                return;
            }
            Some(key) => key,
            None => return,
        };
        let Some(root) = self.browser_root(cx) else {
            return;
        };
        let checkout = self.browser_checkout(cx);
        let Some(browser) = &mut self.browser else {
            return;
        };
        // In Docket's worktree, dependencies are linked in from the
        // checkout.
        let link_from = checkout.filter(|_| browser.in_docket_worktree());
        let intel = browser.code_intel.get_or_insert_with(|| CodeIntel {
            root: Some(root.clone()),
            error: None,
            servers: Vec::new(),
            outlines: HashMap::new(),
            preparing: false,
            _prepare_task: None,
        });
        if intel
            .servers
            .iter()
            .any(|slot| std::ptr::eq(slot.spec, spec))
        {
            return;
        }
        // The user's own checkout, or a branch of the repository itself.
        self.add_server(key, spec, root, true, link_from, cx);
    }

    /// Adds a slot for `spec` to `key`'s servers, which must be there, and
    /// starts it in `root` once it's found, unless it runs project code
    /// that isn't `trusted`. `link_from` is the checkout to link
    /// dependencies in from, when `root` is Docket's worktree.
    fn add_server(
        &mut self,
        key: IntelKey,
        spec: &'static ServerSpec,
        root: PathBuf,
        trusted: bool,
        link_from: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let task = cx.spawn(async move |this, cx| {
            let resolved = cx
                .background_executor()
                .spawn({
                    let root = root.clone();
                    async move {
                        if let Some(checkout) = link_from.filter(|_| spec.links_dependencies)
                            && let Err(err) = worktree::link_dependencies(&checkout, &root)
                        {
                            eprintln!("couldn't link dependencies into the worktree: {err:#}");
                        }
                        spec.resolve(&root)
                    }
                })
                .await;
            let resolved = this.update(cx, |this, cx| {
                let slot = this.server_slot(key, spec)?;
                let started = match resolved {
                    Ok(resolved) => {
                        slot.source = Some(resolved.source.clone().into());
                        if spec.runs_project_code && !trusted {
                            slot.state = ServerState::NeedsTrust;
                            None
                        } else {
                            Some(resolved)
                        }
                    }
                    Err(missing) => {
                        slot.state = ServerState::NotInstalled {
                            install: missing.install.into(),
                        };
                        None
                    }
                };
                cx.notify();
                started
            });
            if let Ok(Some(resolved)) = resolved {
                run_server(this, key, spec, resolved, root, cx).await;
            }
        });
        let Some(intel) = self.code_intel_mut(key) else {
            return;
        };
        intel.servers.push(ServerSlot {
            spec,
            source: None,
            state: ServerState::Starting,
            _task: Some(task),
            _outline_tasks: Vec::new(),
        });
        cx.notify();
    }

    /// Whether to run project code for the open diff: a PR on a branch of
    /// the repository itself, or the user's own, or a local branch.
    fn diff_is_trusted(&self, cx: &App) -> bool {
        let Some(detail) = &self.detail else {
            return self.branch.is_some();
        };
        let viewer = match &self.app.read(cx).auth {
            AuthStatus::SignedIn { user, .. } => Some(user.login.as_str()),
            _ => None,
        };
        detail
            .summary
            .as_ref()
            .is_some_and(|pr| is_trusted(pr, viewer))
    }

    fn worktree_ready(
        &mut self,
        revision: u64,
        trusted: bool,
        result: Result<Prepared>,
        cx: &mut Context<Self>,
    ) {
        let key = IntelKey::Diff { revision };
        let Some(intel) = self.code_intel_mut(key) else {
            return;
        };
        intel.preparing = false;
        let (dir, servers) = match result {
            Ok(ready) => ready,
            Err(err) => {
                intel.error = Some(format!("{err:#}").into());
                cx.notify();
                return;
            }
        };
        intel.root = dir.clone();
        intel.servers = servers
            .into_iter()
            .map(|(spec, resolved)| {
                let source = resolved
                    .as_ref()
                    .ok()
                    .map(|resolved| resolved.source.clone().into());
                let (state, task) = match (resolved, &dir) {
                    (Err(missing), _) => (
                        ServerState::NotInstalled {
                            install: missing.install.into(),
                        },
                        None,
                    ),
                    (Ok(_), _) if spec.runs_project_code && !trusted => {
                        (ServerState::NeedsTrust, None)
                    }
                    (Ok(resolved), Some(dir)) => (
                        ServerState::Starting,
                        Some(cx.spawn({
                            let dir = dir.clone();
                            async move |this, cx| {
                                run_server(this, key, spec, resolved, dir, cx).await
                            }
                        })),
                    ),
                    (Ok(_), None) => unreachable!("the worktree is prepared when a server starts"),
                };
                ServerSlot {
                    spec,
                    source,
                    state,
                    _task: task,
                    _outline_tasks: Vec::new(),
                }
            })
            .collect();
        // The Browse tab's, asked for while the worktree was prepared.
        self.start_open_browser_server(cx);
        cx.notify();
    }

    /// Asks a newly running server for the outlines of the files it
    /// serves, one task per file so each shows as soon as it's ready.
    fn load_outlines(&mut self, revision: u64, spec: &'static ServerSpec, cx: &mut Context<Self>) {
        let Some(files) = self.diff_files_at(revision) else {
            return;
        };
        let DiffLoad::Ready(diff) = &files.diff else {
            return;
        };
        let wanted: Vec<usize> = diff
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| {
                let FileContent::Text(text) = &file.content else {
                    return false;
                };
                // Only files with an excerpt that could hide a scope.
                text.excerpts
                    .iter()
                    .any(|e| text.scope_anchor(e).is_some_and(|(_, from)| from > 0))
            })
            .map(|(ix, _)| ix)
            .collect();
        let targets: Vec<_> = wanted
            .into_iter()
            .filter_map(|ix| Some((ix, files.lsp_target(ix)?)))
            .filter(|(_, target)| std::ptr::eq(target.server.spec, spec))
            .collect();
        let tasks = targets
            .into_iter()
            .map(|(ix, target)| {
                cx.spawn(async move |this, cx| {
                    let background = cx.background_executor().clone();
                    let outline = background
                        .clone()
                        .spawn(async move {
                            target.server.open(
                                &target.path,
                                target.language_id,
                                target.text.text(),
                            )?;
                            let mut attempt = 1;
                            loop {
                                match target.server.outline(&target.path).await {
                                    Ok(outline) => return Ok(outline),
                                    Err(_) if attempt < OUTLINE_ATTEMPTS => {
                                        attempt += 1;
                                        background.timer(OUTLINE_RETRY_INTERVAL).await;
                                    }
                                    Err(err) => return Err(err),
                                }
                            }
                        })
                        .await;
                    this.update(cx, |this, cx| match outline {
                        Ok(outline) => {
                            let key = IntelKey::Diff { revision };
                            if let Some(intel) = this.code_intel_mut(key) {
                                intel.outlines.insert(ix, Arc::new(outline));
                                cx.notify();
                            }
                        }
                        Err(err) => eprintln!("no outline from {}: {err:#}", spec.name),
                    })
                    .ok();
                })
            })
            .collect();
        if let Some(slot) = self.server_slot(IntelKey::Diff { revision }, spec) {
            slot._outline_tasks = tasks;
        }
    }

    fn code_intel_mut(&mut self, key: IntelKey) -> Option<&mut CodeIntel> {
        match key {
            IntelKey::Diff { revision } => self.diff_files_at(revision)?.code_intel.as_mut(),
            IntelKey::Browser => self.browser.as_mut()?.code_intel.as_mut(),
        }
    }

    fn server_slot(&mut self, key: IntelKey, spec: &'static ServerSpec) -> Option<&mut ServerSlot> {
        self.code_intel_mut(key)?
            .servers
            .iter_mut()
            .find(|slot| std::ptr::eq(slot.spec, spec))
    }

    fn update_server(
        &mut self,
        key: IntelKey,
        spec: &'static ServerSpec,
        update: impl FnOnce(&mut ServerState),
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.server_slot(key, spec) else {
            return;
        };
        let readiness = |state: &ServerState| match state {
            ServerState::Running { activity, .. } => Some(activity.busy),
            _ => None,
        };
        let before = readiness(&slot.state);
        update(&mut slot.state);
        let changed = readiness(&slot.state) != before;
        cx.notify();
        // A browsed checkout's symbols are searched again as its servers
        // start and finish loading.
        let settled = !matches!(
            slot.state,
            ServerState::Running { .. } | ServerState::Starting
        );
        if self.browser_intel_key() == Some(key) && (changed || settled) {
            self.search_servers_changed(cx);
        }
    }
}

/// The servers for the languages on a loaded diff's new side.
fn diff_specs(files: &DiffFiles) -> Vec<&'static ServerSpec> {
    let DiffLoad::Ready(diff) = &files.diff else {
        return Vec::new();
    };
    let mut specs: Vec<&'static ServerSpec> = Vec::new();
    for file in &diff.files {
        if let FileContent::Text(text) = &file.content
            && let Some(spec) = text.new.as_ref().and_then(|t| server_for(t.language?))
            && !specs.iter().any(|s| std::ptr::eq(*s, spec))
        {
            specs.push(spec);
        }
    }
    specs
}

fn is_trusted(pr: &PullRequest, viewer: Option<&str>) -> bool {
    !pr.is_cross_repository
        || (viewer.is_some() && pr.author.as_ref().map(|a| a.login.as_str()) == viewer)
}

/// Starts a server, then applies its events until it exits or its slot is
/// dropped (which drops this task).
async fn run_server(
    this: gpui::WeakEntity<Tab>,
    key: IntelKey,
    spec: &'static ServerSpec,
    resolved: Resolved,
    dir: PathBuf,
    cx: &mut AsyncApp,
) {
    let (events_tx, mut events) = mpsc::unbounded();
    let background = cx.background_executor().clone();
    let started = background
        .spawn({
            let background = background.clone();
            async move {
                let start = LanguageServer::start(spec, resolved, dir, events_tx);
                let timeout = background.timer(START_TIMEOUT);
                match futures::future::select(Box::pin(start), timeout).await {
                    futures::future::Either::Left((started, _)) => started,
                    futures::future::Either::Right(_) => {
                        Err(anyhow!("it didn't start within a minute"))
                    }
                }
            }
        })
        .await;
    let failed = started.is_err();
    this.update(cx, |this, cx| {
        let running = started.is_ok();
        this.update_server(
            key,
            spec,
            |state| {
                *state = match started {
                    Ok(server) => ServerState::Running {
                        server: Arc::new(server),
                        activity: ServerActivity {
                            busy: true,
                            message: None,
                        },
                    },
                    Err(err) => ServerState::Failed(format!("{err:#}").into()),
                }
            },
            cx,
        );
        // Outlines are for hunk headers.
        if let (true, IntelKey::Diff { revision }) = (running, key) {
            this.load_outlines(revision, spec, cx);
        }
    })
    .ok();

    let mut reported = false;
    let mut grace = cx.background_executor().timer(STARTUP_GRACE).fuse();
    loop {
        let event = futures::select_biased! {
            event = events.next() => event,
            _ = grace => {
                if !reported {
                    let settled = ServerEvent::Activity(ServerActivity::default());
                    apply_event(&this, key, spec, settled, failed, cx);
                }
                continue;
            }
        };
        let Some(event) = event else {
            break;
        };
        reported |= matches!(event, ServerEvent::Activity(_));
        if !apply_event(&this, key, spec, event, failed, cx) {
            break;
        }
    }
}

/// Applies a server's event to its slot; returns whether the slot is still
/// there to apply more to.
fn apply_event(
    this: &gpui::WeakEntity<Tab>,
    key: IntelKey,
    spec: &'static ServerSpec,
    event: ServerEvent,
    failed: bool,
    cx: &mut AsyncApp,
) -> bool {
    let applied = this.update(cx, |this, cx| {
        this.update_server(
            key,
            spec,
            |state| match event {
                ServerEvent::Activity(next) => {
                    if let ServerState::Running { activity, .. } = state {
                        *activity = next;
                    }
                }
                ServerEvent::Exited(stderr) => {
                    let reason = exit_reason(&stderr);
                    // A failed start already says why, unless stderr
                    // says more.
                    *state = match (reason, failed) {
                        (Some(reason), _) => ServerState::Failed(reason.trim().to_owned().into()),
                        (None, true) => return,
                        (None, false) => ServerState::Failed("it exited".into()),
                    };
                }
            },
            cx,
        )
    });
    applied.is_ok()
}

/// The line of a server's stderr that best says why it exited: the last
/// error, else the last line that isn't a runtime's version banner.
fn exit_reason(stderr: &str) -> Option<&str> {
    let lines = || {
        stderr
            .lines()
            .rev()
            .map(str::trim)
            .filter(|l| !l.is_empty())
    };
    lines()
        .find(|line| line.contains("Error") || line.contains("error:") || line.contains("FATAL"))
        .or_else(|| lines().find(|line| !line.starts_with("Node.js v")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::PullRequestState;

    #[test]
    fn explains_exits_with_the_error() {
        let stderr = "    at cli.mjs:25383:17\nError: tsserver process has exited (signal: SIGABRT)\n\
                      \n    at process.processTicks\n\nNode.js v26.13.0";
        assert_eq!(
            exit_reason(stderr),
            Some("Error: tsserver process has exited (signal: SIGABRT)")
        );
        assert_eq!(exit_reason("starting\nbye\n"), Some("bye"));
        assert_eq!(exit_reason(""), None);
    }

    #[test]
    fn trusts_branches_in_the_repository_and_your_own_forks() {
        let pr = |is_cross_repository, author: &str| PullRequest {
            id: String::new(),
            number: 1,
            title: String::new(),
            url: String::new(),
            repository: "o/r".into(),
            author: Some(crate::test_support::actor(author)),
            state: PullRequestState::Open,
            is_draft: false,
            review_decision: None,
            additions: 0,
            deletions: 0,
            updated_at: Default::default(),
            base_ref_name: "main".into(),
            base_ref_oid: String::new(),
            head_ref_name: "feature".into(),
            head_ref_oid: String::new(),
            is_cross_repository,
        };
        assert!(is_trusted(&pr(false, "alice"), Some("me")));
        assert!(is_trusted(&pr(true, "me"), Some("me")));
        assert!(!is_trusted(&pr(true, "alice"), Some("me")));
        assert!(!is_trusted(&pr(true, "alice"), None));
    }
}
