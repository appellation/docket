//! Language servers for the open PR's Files tab.
//!
//! Once the diff loads, Docket checks out the PR's head in its worktree for
//! the project (see `lsp::worktree`) and starts a server for each language
//! on the diff's new side, using the project's own toolchain where it has
//! one (see `lsp::servers`). Servers stop when the PR closes or its diff
//! reloads, since they live in `PrFiles`.
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
//! access to push, so they're trusted, as are the user's own PRs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures::channel::mpsc;
use futures::{FutureExt as _, StreamExt as _};
use gpui::{AsyncApp, Context, SharedString, Task};

use super::{AppState, AuthStatus, DiffLoad, PrFiles};
use crate::diff::{FileContent, TextSnapshot};
use crate::github::PullRequest;
use crate::lsp::{
    self, LanguageServer, Missing, Outline, Resolved, ServerActivity, ServerEvent, ServerSpec,
    server_for, worktree,
};
use crate::projects::Checkout;

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
    /// Docket's worktree at the PR's head, once it's checked out.
    pub worktree: Option<PathBuf>,
    /// Why the worktree couldn't be prepared.
    pub error: Option<SharedString>,
    pub servers: Vec<ServerSlot>,
    /// Outlines of the diff's files on the new side, by file index.
    pub outlines: HashMap<usize, Arc<Outline>>,
    _prepare_task: Task<()>,
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

/// What a hover request needs, for a file on the diff's new side.
pub struct HoverTarget {
    pub server: Arc<LanguageServer>,
    /// The file in Docket's worktree.
    pub path: PathBuf,
    pub language_id: &'static str,
    pub text: Arc<TextSnapshot>,
    /// Whether the server is still loading, so an empty answer may just
    /// mean "not yet".
    pub busy: bool,
}

impl PrFiles {
    /// The outline of a file's new side, once its server has given it.
    pub fn outline(&self, file: usize) -> Option<&Arc<Outline>> {
        self.code_intel.as_ref()?.outlines.get(&file)
    }

    pub fn hover_target(&self, file: usize) -> Option<HoverTarget> {
        let DiffLoad::Ready(diff) = &self.diff else {
            return None;
        };
        let file = diff.files.get(file)?;
        let FileContent::Text(content) = &file.content else {
            return None;
        };
        let text = content.new.clone()?;
        let path = file.new_path.as_deref()?;
        let spec = server_for(text.language?)?;
        let intel = self.code_intel.as_ref()?;
        let slot = intel
            .servers
            .iter()
            .find(|slot| std::ptr::eq(slot.spec, spec))?;
        let ServerState::Running { server, activity } = &slot.state else {
            return None;
        };
        Some(HoverTarget {
            server: server.clone(),
            path: intel.worktree.as_ref()?.join(path),
            language_id: lsp::language_id(path, text.language?),
            text,
            busy: activity.busy,
        })
    }
}

impl AppState {
    /// Starts the servers that wait on trust, for this PR's current head.
    pub fn trust_code_intel(&mut self, cx: &mut Context<Self>) {
        if let Some(files) = self.detail.as_mut().and_then(|d| d.files.as_mut()) {
            files.allow_project_code = true;
            self.retry_code_intel(cx);
        }
    }

    /// Looks for servers and starts them again, e.g. after installing one.
    pub fn retry_code_intel(&mut self, cx: &mut Context<Self>) {
        let Some(files) = self
            .detail
            .as_mut()
            .and_then(|detail| detail.files.as_mut())
        else {
            return;
        };
        if !matches!(files.diff, DiffLoad::Ready(_)) {
            return;
        }
        // Dropping the old servers stops them.
        files.code_intel = None;
        let revision = files.revision;
        self.start_code_intel(revision, cx);
        cx.notify();
    }

    /// Starts language servers for the diff loaded at `revision`.
    pub(super) fn start_code_intel(&mut self, revision: u64, cx: &mut Context<Self>) {
        let Some(files) = self.pr_files_at(revision) else {
            return;
        };
        let DiffLoad::Ready(diff) = &files.diff else {
            return;
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
        let head = diff.head_oid.clone();
        let allowed = files.allow_project_code;
        let project = files
            .project
            .and_then(|id| self.projects.iter().find(|p| p.id == id))
            .cloned();
        let (Some(project), false) = (project, specs.is_empty()) else {
            return;
        };
        let trusted = allowed || self.pr_is_trusted();

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
        let Some(files) = self.pr_files_at(revision) else {
            return;
        };
        files.code_intel = Some(CodeIntel {
            worktree: None,
            error: None,
            servers: Vec::new(),
            outlines: HashMap::new(),
            _prepare_task: prepare,
        });
    }

    /// Whether to run project code for the open PR: it's on a branch of
    /// the repository itself, or it's the user's own.
    fn pr_is_trusted(&self) -> bool {
        let Some(detail) = &self.detail else {
            return false;
        };
        let viewer = match &self.auth {
            AuthStatus::SignedIn { user, .. } => Some(user.login.as_str()),
            _ => None,
        };
        is_trusted(&detail.summary, viewer)
    }

    fn worktree_ready(
        &mut self,
        revision: u64,
        trusted: bool,
        result: Result<Prepared>,
        cx: &mut Context<Self>,
    ) {
        let Some(intel) = self.code_intel_at(revision) else {
            return;
        };
        let (dir, servers) = match result {
            Ok(ready) => ready,
            Err(err) => {
                intel.error = Some(format!("{err:#}").into());
                cx.notify();
                return;
            }
        };
        intel.worktree = dir.clone();
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
                                run_server(this, revision, spec, resolved, dir, cx).await
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
        cx.notify();
    }

    /// Asks a newly running server for the outlines of the files it
    /// serves, one task per file so each shows as soon as it's ready.
    fn load_outlines(&mut self, revision: u64, spec: &'static ServerSpec, cx: &mut Context<Self>) {
        let Some(files) = self.pr_files_at(revision) else {
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
            .filter_map(|ix| Some((ix, files.hover_target(ix)?)))
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
                            if let Some(intel) = this.code_intel_at(revision) {
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
        let slot = self.code_intel_at(revision).and_then(|intel| {
            intel
                .servers
                .iter_mut()
                .find(|slot| std::ptr::eq(slot.spec, spec))
        });
        if let Some(slot) = slot {
            slot._outline_tasks = tasks;
        }
    }

    fn code_intel_at(&mut self, revision: u64) -> Option<&mut CodeIntel> {
        self.pr_files_at(revision)?.code_intel.as_mut()
    }

    fn update_server(
        &mut self,
        revision: u64,
        spec: &'static ServerSpec,
        update: impl FnOnce(&mut ServerState),
        cx: &mut Context<Self>,
    ) {
        let slot = self.code_intel_at(revision).and_then(|intel| {
            intel
                .servers
                .iter_mut()
                .find(|slot| std::ptr::eq(slot.spec, spec))
        });
        if let Some(slot) = slot {
            update(&mut slot.state);
            cx.notify();
        }
    }
}

fn is_trusted(pr: &PullRequest, viewer: Option<&str>) -> bool {
    !pr.is_cross_repository || (viewer.is_some() && pr.author.as_deref() == viewer)
}

/// Starts a server, then applies its events until it exits or its slot is
/// dropped (which drops this task).
async fn run_server(
    this: gpui::WeakEntity<AppState>,
    revision: u64,
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
            revision,
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
        if running {
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
                    apply_event(&this, revision, spec, settled, failed, cx);
                }
                continue;
            }
        };
        let Some(event) = event else {
            break;
        };
        reported |= matches!(event, ServerEvent::Activity(_));
        if !apply_event(&this, revision, spec, event, failed, cx) {
            break;
        }
    }
}

/// Applies a server's event to its slot; returns whether the slot is still
/// there to apply more to.
fn apply_event(
    this: &gpui::WeakEntity<AppState>,
    revision: u64,
    spec: &'static ServerSpec,
    event: ServerEvent,
    failed: bool,
    cx: &mut AsyncApp,
) -> bool {
    let applied = this.update(cx, |this, cx| {
        this.update_server(
            revision,
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
            author: Some(author.into()),
            state: PullRequestState::Open,
            is_draft: false,
            review_decision: None,
            additions: 0,
            deletions: 0,
            updated_at: Default::default(),
            base_ref_name: "main".into(),
            base_ref_oid: String::new(),
            head_ref_oid: String::new(),
            is_cross_repository,
        };
        assert!(is_trusted(&pr(false, "alice"), Some("me")));
        assert!(is_trusted(&pr(true, "me"), Some("me")));
        assert!(!is_trusted(&pr(true, "alice"), Some("me")));
        assert!(!is_trusted(&pr(true, "alice"), None));
    }
}
