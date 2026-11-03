//! Browsing a project's files: its directories, each listed when first
//! expanded, and the open file, read and highlighted in the background.
//!
//! A branch that's checked out, in the project's checkout or one of its
//! worktrees, is browsed on disk, uncommitted changes and untracked files
//! included. Any other branch is read from Git at its tip, without
//! checking it out. The open file appears as plain text once read, and
//! colors in when highlighting finishes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{Context, SharedString, Task};

use super::{AppState, CodeIntel};
use crate::projects::files::{self, DirEntry, LocalFile, Tree};
use crate::projects::{Branch, ProjectId};
use crate::syntax::{self, Language, SyntaxSnapshot};

/// A project, and the branch of it being browsed.
pub type BrowserKey = (ProjectId, String);

pub struct ProjectBrowser {
    pub project: ProjectId,
    /// The branch opened from the docket.
    pub branch: String,
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

#[derive(Clone)]
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

    /// Which project and branch this is. A result for another is stale.
    pub fn key(&self) -> BrowserKey {
        (self.project, self.branch.clone())
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

impl AppState {
    /// Opens one of the project's branches from the docket: where it's
    /// checked out, or else its tip.
    pub fn browse_branch(&mut self, id: ProjectId, name: &str, cx: &mut Context<Self>) {
        if self.project(id).is_none() {
            return;
        }
        let source = match self.branch(id, name) {
            Some(Branch {
                worktree: Some(worktree),
                ..
            }) => BrowseSource::WorkingCopy(worktree.clone()),
            Some(branch) => BrowseSource::Tip(branch.reference.clone()),
            None => BrowseSource::Tip(format!("refs/heads/{name}")),
        };
        self.setup = None;
        self.detail = None;
        self.browser = Some(ProjectBrowser {
            project: id,
            branch: name.to_owned(),
            source,
            dirs: HashMap::new(),
            expanded: HashSet::new(),
            head: None,
            revision: 0,
            file: None,
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
    fn browser_checkout(&self) -> Option<PathBuf> {
        let project = self.project(self.browser.as_ref()?.project)?;
        (!project.path_missing).then(|| project.path.clone())
    }

    /// The folder on disk being browsed, where language servers run. A
    /// branch read from Git has none.
    pub(super) fn browser_root(&self) -> Option<PathBuf> {
        self.browser_checkout()?;
        self.browser.as_ref()?.working_copy().map(Path::to_owned)
    }

    /// Reads the index (or finds the branch's tip) and lists the root, then
    /// the expanded directories, then re-reads the open file. Listings of
    /// collapsed directories are dropped, to be listed afresh when they're
    /// expanded again.
    fn load_listings(&mut self, cx: &mut Context<Self>) {
        let checkout = self.browser_checkout();
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
        for path in &browser.expanded {
            browser.dirs.entry(path.clone()).or_insert(DirLoad::Loading);
        }
        let expanded: Vec<String> = browser.expanded.iter().cloned().collect();
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
                            (tree, Some(branch))
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
