//! Browsing a project's files: its directories, each listed when first
//! expanded, and the open file, read and highlighted in the background.
//!
//! The docket opens the default branch here; other branches open to their
//! changes instead (`branch.rs`). Where the default branch is checked out,
//! in the project's checkout or one of its worktrees, it's browsed on disk,
//! uncommitted changes and untracked files included. Otherwise it's read
//! from Git at its tip, without checking it out. The open file appears as plain text once read, and
//! colors in when highlighting finishes.
//!
//! A pull request's or branch's Browse tab browses its files here too,
//! alongside it rather than in its place (`BrowserScope`): a pull request's
//! at its head, once its diff has fetched it, and a branch's where it's
//! checked out, or else at its tip. They follow the head or tip when it
//! moves.
//!
//! While no file is open, the main panel lists a directory (`dir`): the
//! root to start, or whichever was navigated to since.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{App, Context, SharedString, Task};

use super::{BranchTab, CodeIntel, DiffLoad, PrTab, Tab};
use crate::projects::files::{self, DirEntry, LocalFile, Tree};
use crate::projects::{Branch, ProjectId};
use crate::syntax::{self, Language, SyntaxSnapshot};

/// A project, the branch of it being browsed (`None` for the checkout as
/// it is), and what it's browsed for.
pub type BrowserKey = (ProjectId, Option<String>, BrowserScope);

/// What the files are browsed for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserScope {
    /// On their own, from the docket or a definition opened in a new tab.
    Project,
    /// A pull request's Browse tab, by its number.
    PullRequest(u64),
    /// A branch's Browse tab.
    Branch,
}

pub struct ProjectBrowser {
    pub project: ProjectId,
    /// The default branch, opened from the docket; `None` for the checkout
    /// as it is, as a definition opened in a new tab shows. In a Browse
    /// tab, the pull request's head branch, or the branch.
    pub branch: Option<String>,
    pub scope: BrowserScope,
    source: BrowseSource,
    /// Listings of the root (`""`) and each directory expanded since, by
    /// path.
    pub dirs: HashMap<String, DirLoad>,
    /// Paths of the expanded directories. One inside a collapsed directory
    /// stays expanded for when its parent opens again.
    pub expanded: HashSet<String>,
    /// The checked-out branch, or the commit when detached.
    pub head: Option<String>,
    /// Bumped whenever `dirs` or `expanded` change, so views know to
    /// rebuild their rows.
    pub revision: u64,
    pub file: Option<OpenFile>,
    /// The directory the main panel lists while no file is open, by path:
    /// the root (`""`) to start.
    pub dir: String,
    /// A refresh is re-listing the directories; the last listings stay
    /// shown.
    pub reloading: bool,
    /// Where files are read from, settled with the root's listing (the
    /// index read, or the branch's tip found) and used until the next.
    tree: Option<Tree>,
    _root_task: Option<Task<()>>,
    _dir_tasks: HashMap<String, Task<()>>,
    /// Language servers for hover, started as files in their languages
    /// open.
    pub code_intel: Option<CodeIntel>,
}

#[derive(Clone, PartialEq, Eq)]
enum BrowseSource {
    /// Files on disk: the project's checkout, or a worktree with the
    /// branch checked out.
    WorkingCopy(PathBuf),
    /// The tip of a ref (`refs/heads/…`, or a remote's default branch),
    /// read from Git in the project's checkout.
    Tip(String),
}

pub enum DirLoad {
    Loading,
    Ready(Arc<Vec<DirEntry>>),
    Failed(SharedString),
}

pub struct OpenFile {
    /// Relative to the checkout's root.
    pub path: String,
    pub content: FileLoad,
    /// Set once highlighting finishes, for text in a known language.
    pub syntax: Option<Arc<SyntaxSnapshot>>,
    _task: Option<Task<()>>,
}

pub enum FileLoad {
    Loading,
    Ready(Arc<LocalFile>),
    Failed(SharedString),
}

impl ProjectBrowser {
    pub fn root(&self) -> Option<&DirLoad> {
        self.dirs.get("")
    }

    /// Which project and branch this is, and what for. A result for
    /// another is stale.
    pub fn key(&self) -> BrowserKey {
        (self.project, self.branch.clone(), self.scope.clone())
    }

    /// The directories to list: those expanded in the tree, and the one the
    /// main panel lists.
    fn listed_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.expanded.iter().cloned().collect();
        if self.file.is_none() && !self.dir.is_empty() && !self.expanded.contains(&self.dir) {
            paths.push(self.dir.clone());
        }
        paths
    }

    /// The folder on disk being browsed, unless it's a branch read from
    /// Git.
    pub fn working_copy(&self) -> Option<&Path> {
        match &self.source {
            BrowseSource::WorkingCopy(path) => Some(path),
            BrowseSource::Tip(_) => None,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.reloading || matches!(self.root(), None | Some(DirLoad::Loading))
    }

    /// The open file's language, once it's read.
    pub(super) fn open_language(&self) -> Option<&'static Language> {
        let file = self.file.as_ref()?;
        match &file.content {
            FileLoad::Ready(content) => match &**content {
                LocalFile::Text(text) => text.language,
                _ => None,
            },
            _ => None,
        }
    }
}

/// The root's listing, and those of the expanded directories.
struct Listings {
    tree: Tree,
    head: Option<String>,
    root: Vec<DirEntry>,
    dirs: Vec<(String, anyhow::Result<Vec<DirEntry>>)>,
}

impl Tab {
    /// Opens the project's checkout as it is on disk.
    pub fn browse_project(&mut self, id: ProjectId, cx: &mut Context<Self>) {
        let Some(project) = self.app.read(cx).project(id) else {
            return;
        };
        let source = BrowseSource::WorkingCopy(project.path.clone());
        self.open_browser(id, None, BrowserScope::Project, source, cx);
    }

    /// Opens the project's default branch from the docket: where it's
    /// checked out, or else its tip.
    pub fn browse_branch(&mut self, id: ProjectId, name: &str, cx: &mut Context<Self>) {
        let app = self.app.read(cx);
        if app.project(id).is_none() {
            return;
        }
        let source = match app.branch(id, name) {
            Some(Branch {
                worktree: Some(worktree),
                ..
            }) => BrowseSource::WorkingCopy(worktree.clone()),
            Some(branch) => BrowseSource::Tip(branch.reference.clone()),
            None => BrowseSource::Tip(format!("refs/heads/{name}")),
        };
        self.open_browser(id, Some(name.to_owned()), BrowserScope::Project, source, cx);
    }

    /// The Browse tab's files, for the open pull request or branch: which,
    /// where they're read from, and whether the tab is showing. `None` for
    /// a pull request until its diff has fetched its head.
    fn tab_browser(&self) -> Option<(BrowserKey, BrowseSource, bool)> {
        if let Some(detail) = &self.detail {
            let files = detail.files.as_ref()?;
            if !matches!(files.diff, DiffLoad::Ready(_)) {
                return None;
            }
            let pr = detail.summary.as_ref()?;
            let key = (
                files.project?,
                Some(pr.head_ref_name.clone()),
                BrowserScope::PullRequest(detail.number),
            );
            let source = BrowseSource::Tip(pr.head_ref_oid.clone());
            return Some((key, source, detail.tab == PrTab::Browse));
        }
        let open = self.branch.as_ref()?;
        let key = (
            open.project,
            Some(open.branch.name.clone()),
            BrowserScope::Branch,
        );
        // By its tip rather than its ref, so it moves with the tip.
        let source = match &open.branch.worktree {
            Some(worktree) => BrowseSource::WorkingCopy(worktree.clone()),
            None => BrowseSource::Tip(open.branch.oid.clone()),
        };
        Some((key, source, open.tab == BranchTab::Browse))
    }

    /// Opens the Browse tab's files once it's showing, and moves them to
    /// the pull request's head or branch's tip when it moves, keeping what's
    /// expanded and open.
    pub(super) fn sync_tab_browser(&mut self, cx: &mut Context<Self>) {
        let Some(((project, branch, scope), source, showing)) = self.tab_browser() else {
            return;
        };
        if let Some(browser) = &mut self.browser
            && browser.project == project
            && browser.branch == branch
            && browser.scope == scope
        {
            if browser.source != source {
                browser.source = source;
                self.load_listings(cx);
            }
            return;
        }
        if showing {
            self.open_browser(project, branch, scope, source, cx);
        }
    }

    fn open_browser(
        &mut self,
        id: ProjectId,
        branch: Option<String>,
        scope: BrowserScope,
        source: BrowseSource,
        cx: &mut Context<Self>,
    ) {
        // A Browse tab's files open beside its pull request or branch.
        if scope == BrowserScope::Project {
            self.setup = None;
            self.detail = None;
            self.branch = None;
        }
        self.browser = Some(ProjectBrowser {
            project: id,
            branch,
            scope,
            source,
            dirs: HashMap::new(),
            expanded: HashSet::new(),
            head: None,
            revision: 0,
            file: None,
            dir: String::new(),
            reloading: false,
            tree: None,
            _root_task: None,
            _dir_tasks: HashMap::new(),
            code_intel: None,
        });
        self.load_listings(cx);
        cx.notify();
    }

    pub fn close_browser(&mut self, cx: &mut Context<Self>) {
        if self.browser.take().is_some() {
            cx.notify();
        }
    }

    /// Re-lists the expanded directories and re-reads the open file,
    /// keeping what's shown until they arrive.
    pub(super) fn refresh_browser(&mut self, cx: &mut Context<Self>) {
        self.load_listings(cx);
    }

    /// The browsed project's checkout, if it's where Docket last saw it.
    fn browser_checkout(&self, cx: &App) -> Option<PathBuf> {
        let project = self.app.read(cx).project(self.browser.as_ref()?.project)?;
        (!project.path_missing).then(|| project.path.clone())
    }

    /// The folder on disk being browsed, where language servers run. A
    /// branch read from Git has none.
    pub(super) fn browser_root(&self, cx: &App) -> Option<PathBuf> {
        self.browser_checkout(cx)?;
        self.browser.as_ref()?.working_copy().map(Path::to_owned)
    }

    /// Reads the index (or finds the branch's tip) and lists the root, then
    /// the expanded directories, then re-reads the open file. Listings of
    /// collapsed directories are dropped, to be listed afresh when they're
    /// expanded again.
    fn load_listings(&mut self, cx: &mut Context<Self>) {
        let checkout = self.browser_checkout(cx);
        let Some(browser) = &mut self.browser else {
            return;
        };
        let Some(checkout) = checkout else {
            browser.dirs = HashMap::from([(
                String::new(),
                DirLoad::Failed(
                    "The project's folder isn't where it was. Locate it from the project's \
                     Connection settings."
                        .into(),
                ),
            )]);
            browser.revision += 1;
            browser._root_task = None;
            browser._dir_tasks.clear();
            return;
        };

        browser.reloading = matches!(browser.root(), Some(DirLoad::Ready(_)));
        // This lists them all again, so loads in flight are superseded.
        browser._dir_tasks.clear();
        let expanded = browser.listed_paths();
        for path in &expanded {
            browser.dirs.entry(path.clone()).or_insert(DirLoad::Loading);
        }
        let source = browser.source.clone();
        let branch = browser.branch.clone();
        let key = browser.key();
        browser._root_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let (tree, head) = match source {
                        BrowseSource::WorkingCopy(root) => {
                            let tracked = Arc::new(files::tracked(&root)?);
                            let head = files::head(&root);
                            (Tree::Disk { root, tracked }, head)
                        }
                        BrowseSource::Tip(reference) => {
                            let oid = files::tip(&checkout, &reference)?;
                            let tree = Tree::Commit {
                                repo: checkout,
                                oid,
                            };
                            (tree, branch)
                        }
                    };
                    let root_entries = tree.list_dir("")?;
                    let dirs = expanded
                        .into_iter()
                        .map(|path| {
                            let listed = tree.list_dir(&path);
                            (path, listed)
                        })
                        .collect();
                    anyhow::Ok(Listings {
                        head,
                        tree,
                        root: root_entries,
                        dirs,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(browser) = this.browser.as_mut().filter(|b| b.key() == key) else {
                    return;
                };
                browser.reloading = false;
                browser.revision += 1;
                match result {
                    Ok(listings) => {
                        browser.tree = Some(listings.tree);
                        browser.head = listings.head;
                        browser.dirs = listings
                            .dirs
                            .into_iter()
                            .map(|(path, listed)| (path, dir_load(listed)))
                            .collect();
                        browser
                            .dirs
                            .insert(String::new(), DirLoad::Ready(Arc::new(listings.root)));
                        // Those expanded or navigated to while it listed.
                        let unlisted: Vec<String> = browser
                            .listed_paths()
                            .into_iter()
                            .filter(|path| !browser.dirs.contains_key(path))
                            .collect();
                        for path in unlisted {
                            this.load_dir(path, cx);
                        }
                        this.load_open_file(cx);
                    }
                    Err(err) => {
                        browser.tree = None;
                        browser.dirs = HashMap::from([(
                            String::new(),
                            DirLoad::Failed(format!("{err:#}").into()),
                        )]);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Expands or collapses a directory in the tree, listing it the first
    /// time it's expanded (or again, if that failed).
    pub fn set_browser_expanded(&mut self, path: &str, expanded: bool, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        let changed = if expanded {
            browser.expanded.insert(path.to_owned())
        } else {
            browser.expanded.remove(path)
        };
        if !changed {
            return;
        }
        browser.revision += 1;
        let listed = matches!(
            browser.dirs.get(path),
            Some(DirLoad::Ready(_) | DirLoad::Loading)
        );
        if expanded && !listed {
            self.load_dir(path.to_owned(), cx);
        }
        cx.notify();
    }

    fn load_dir(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        // Without the tree, the root is still loading, and lists the
        // expanded directories once it's done.
        let Some(tree) = browser.tree.clone() else {
            return;
        };
        browser.dirs.insert(path.clone(), DirLoad::Loading);
        let key = browser.key();
        let task = cx.spawn({
            let path = path.clone();
            async move |this, cx| {
                let listed = cx
                    .background_executor()
                    .spawn({
                        let path = path.clone();
                        async move { tree.list_dir(&path) }
                    })
                    .await;
                this.update(cx, |this, cx| {
                    let Some(browser) = this.browser.as_mut().filter(|b| b.key() == key) else {
                        return;
                    };
                    browser.dirs.insert(path, dir_load(listed));
                    browser.revision += 1;
                    cx.notify();
                })
                .ok();
            }
        });
        browser._dir_tasks.insert(path, task);
    }

    /// Lists a directory in the main panel, by its path relative to the
    /// checkout (`""` for the root), closing the open file.
    pub fn show_browser_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        if browser.file.is_none() && browser.dir == path {
            return;
        }
        browser.file = None;
        browser.dir = path.to_owned();
        let listed = matches!(
            browser.dirs.get(path),
            Some(DirLoad::Ready(_) | DirLoad::Loading)
        );
        if !listed {
            self.load_dir(path.to_owned(), cx);
        }
        cx.notify();
    }

    /// Opens a file of the open pull request or branch in its Browse tab,
    /// as a click on it in the diff does.
    pub fn browse_diff_file(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.detail.is_some() {
            self.select_pr_tab(PrTab::Browse, cx);
        } else if self.branch.is_some() {
            self.select_branch_tab(BranchTab::Browse, cx);
        } else {
            return;
        }
        if self
            .browser
            .as_ref()
            .is_some_and(|browser| browser.scope != BrowserScope::Project)
        {
            self.reveal_browser_file(path, cx);
        }
    }

    /// Opens a file, with the directories it's in expanded in the tree.
    pub fn reveal_browser_file(&mut self, path: &str, cx: &mut Context<Self>) {
        for (ix, _) in path.match_indices('/') {
            self.set_browser_expanded(&path[..ix], true, cx);
        }
        self.open_browser_file(path.to_owned(), cx);
    }

    /// Shows a file from the tree, by its path relative to the checkout.
    pub fn open_browser_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        if browser.file.as_ref().is_some_and(|file| file.path == path) {
            return;
        }
        browser.file = Some(OpenFile {
            path,
            content: FileLoad::Loading,
            syntax: None,
            _task: None,
        });
        self.load_open_file(cx);
        cx.notify();
    }

    /// Reads the open file, then highlights it. What was shown stays until
    /// the read finishes, and stays highlighted if the text didn't change.
    /// Until the root is listed there's nothing to read from; the listing
    /// reads the file once it's done.
    fn load_open_file(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        let (Some(tree), Some(file)) = (browser.tree.clone(), browser.file.as_mut()) else {
            return;
        };
        let path = file.path.clone();
        file._task = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let result = executor
                .spawn({
                    let path = path.clone();
                    async move { tree.read(&path) }
                })
                .await;
            let content = match result {
                Ok(content) => content,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        if let Some(file) = this.open_file_at(&path) {
                            file.content = FileLoad::Failed(format!("{err:#}").into());
                            file.syntax = None;
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };

            let content = Arc::new(content);
            let unchanged = this
                .update(cx, |this, cx| {
                    let Some(file) = this.open_file_at(&path) else {
                        return true;
                    };
                    if let FileLoad::Ready(shown) = &file.content
                        && same_text(shown, &content)
                    {
                        return true;
                    }
                    file.content = FileLoad::Ready(content.clone());
                    file.syntax = None;
                    if let LocalFile::Text(text) = &*content
                        && let Some(language) = text.language
                    {
                        this.start_browser_server(language, cx);
                    }
                    cx.notify();
                    false
                })
                .unwrap_or(true);
            if unchanged {
                return;
            }

            let LocalFile::Text(text) = &*content else {
                return;
            };
            let Some(language) = text.language else {
                return;
            };
            let syntax = executor
                .spawn({
                    let content = content.clone();
                    async move {
                        let LocalFile::Text(text) = &*content else {
                            return None;
                        };
                        syntax::highlight(text.text(), language)
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(file) = this.open_file_at(&path)
                    && matches!(&file.content, FileLoad::Ready(shown) if Arc::ptr_eq(shown, &content))
                {
                    file.syntax = syntax.map(Arc::new);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// The open file, if it's still `path`. Its task is cancelled when it's
    /// replaced, so this only guards against the browser changing under a
    /// result that's already arrived.
    fn open_file_at(&mut self, path: &str) -> Option<&mut OpenFile> {
        self.browser
            .as_mut()?
            .file
            .as_mut()
            .filter(|file| file.path == path)
    }
}

fn dir_load(listed: anyhow::Result<Vec<DirEntry>>) -> DirLoad {
    match listed {
        Ok(entries) => DirLoad::Ready(Arc::new(entries)),
        Err(err) => DirLoad::Failed(format!("{err:#}").into()),
    }
}

fn same_text(a: &LocalFile, b: &LocalFile) -> bool {
    match (a, b) {
        (LocalFile::Text(a), LocalFile::Text(b)) => a.text() == b.text(),
        _ => false,
    }
}
