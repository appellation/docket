//! Browsing a project's checkout: its file tree, and the open file, read
//! from disk and highlighted in the background.
//!
//! This is the local version of the project, uncommitted changes and
//! untracked files included, not any commit on GitHub. The open file
//! appears as plain text once read, and colors in when highlighting
//! finishes.

use std::sync::Arc;

use gpui::{Context, SharedString, Task};

use super::AppState;
use crate::projects::ProjectId;
use crate::projects::files::{self, FileTree, LocalFile};
use crate::syntax::{self, SyntaxSnapshot};

pub struct ProjectBrowser {
    pub project: ProjectId,
    pub tree: TreeLoad,
    /// Bumped whenever `tree` changes, so views know to rebuild their rows.
    pub revision: u64,
    pub file: Option<OpenFile>,
    /// A refresh is re-listing the files; the last listing stays shown.
    pub reloading: bool,
    _tree_task: Option<Task<()>>,
}

pub enum TreeLoad {
    Loading,
    Ready(Arc<FileTree>),
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
    pub fn is_busy(&self) -> bool {
        matches!(self.tree, TreeLoad::Loading) || self.reloading
    }
}

impl AppState {
    /// Opens a project's files in place of the PR list.
    pub fn browse_project(&mut self, id: ProjectId, cx: &mut Context<Self>) {
        if self.project(id).is_none() {
            return;
        }
        self.setup = None;
        self.detail = None;
        self.browser = Some(ProjectBrowser {
            project: id,
            tree: TreeLoad::Loading,
            revision: 0,
            file: None,
            reloading: false,
            _tree_task: None,
        });
        self.load_tree(cx);
        cx.notify();
    }

    pub fn close_browser(&mut self, cx: &mut Context<Self>) {
        if self.browser.take().is_some() {
            cx.notify();
        }
    }

    /// Re-lists the files and re-reads the open one, keeping what's shown
    /// until they arrive.
    pub(super) fn refresh_browser(&mut self, cx: &mut Context<Self>) {
        if self.browser.is_none() {
            return;
        }
        self.load_tree(cx);
        self.load_open_file(cx);
    }

    fn load_tree(&mut self, cx: &mut Context<Self>) {
        let project = self
            .browser
            .as_ref()
            .and_then(|browser| self.project(browser.project))
            .cloned();
        let Some(browser) = &mut self.browser else {
            return;
        };
        let Some(project) = project else {
            return;
        };
        if project.path_missing {
            browser.tree = TreeLoad::Failed(
                format!(
                    "{} isn't at {} anymore. Locate it from the project's Connection settings.",
                    project.name,
                    project.path.display()
                )
                .into(),
            );
            browser.revision += 1;
            browser._tree_task = None;
            return;
        }

        browser.reloading = matches!(browser.tree, TreeLoad::Ready(_));
        let id = project.id;
        browser._tree_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { files::list(&project.path) })
                .await;
            this.update(cx, |this, cx| {
                let Some(browser) = this.browser.as_mut().filter(|b| b.project == id) else {
                    return;
                };
                browser.reloading = false;
                browser.revision += 1;
                browser.tree = match result {
                    Ok(tree) => TreeLoad::Ready(Arc::new(tree)),
                    Err(err) => TreeLoad::Failed(format!("{err:#}").into()),
                };
                cx.notify();
            })
            .ok();
        }));
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
    fn load_open_file(&mut self, cx: &mut Context<Self>) {
        let root = self
            .browser
            .as_ref()
            .and_then(|browser| self.project(browser.project))
            .filter(|project| !project.path_missing)
            .map(|project| project.path.clone());
        let Some(file) = self.browser.as_mut().and_then(|b| b.file.as_mut()) else {
            return;
        };
        let Some(root) = root else {
            return;
        };
        let path = file.path.clone();
        file._task = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let result = executor
                .spawn({
                    let path = path.clone();
                    async move { files::read(&root, &path) }
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

fn same_text(a: &LocalFile, b: &LocalFile) -> bool {
    match (a, b) {
        (LocalFile::Text(a), LocalFile::Text(b)) => a.text() == b.text(),
        _ => false,
    }
}
