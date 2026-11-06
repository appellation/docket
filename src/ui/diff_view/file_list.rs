//! The changed files beside the diff, the deepest of the sidebars
//! (`sidebar_stack`) on the Files tab. Clicking one scrolls the diff to it.
//! Below them, the status of the language servers behind hover, which the
//! file browser shows too.
//!
//! Files are in a tree of their directories, as in the file browser, with
//! chains of directories that hold nothing but one another compacted into
//! one row: `3rdparty/python`, and under it `api`. A directory's
//! subdirectories come before its files, and top-level files last, the
//! order the diff lists them in (`diff::sort_files`).
//!
//! Grouped by change, the tree has a row per group, its title (the diff has
//! its summary and impact), with a tree of that group's files under it. A
//! file with hunks in several groups is under each, and clicking it
//! scrolls to its hunks in that group. Long titles wrap, so the tree is a
//! `list`, whose rows can be any height, rather than a uniform one.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClipboardItem, Context, ElementId, Entity, FontWeight, ListAlignment,
    ListState, SharedString, div, list, prelude::*, px,
};

use super::DiffView;
use crate::ai::DiffAnalysis;
use crate::diff::{FileDiff, FileStatus, Viewed};
use crate::state::{CodeIntel, IntelOwner, ServerState, Tab};
use crate::ui::components::{
    Button, ButtonSize, ButtonStyle, Icon, IconButton, IconName, IconSize, MONOSPACE, Spinner,
    TestSelector as _, Tooltip, TreeItem,
};
use crate::ui::sidebar_stack::{rail_header, rail_items, sidebar_header};
use crate::ui::theme::{Color, Theme};

pub fn status_icon(status: FileStatus) -> (IconName, Color) {
    match status {
        FileStatus::Added => (IconName::FilePlus, Color::Success),
        FileStatus::Deleted => (IconName::FileMinus, Color::Danger),
        FileStatus::Renamed { .. } | FileStatus::Copied { .. } => {
            (IconName::FileSymlink, Color::Accent)
        }
        FileStatus::Modified | FileStatus::TypeChanged => (IconName::FilePen, Color::Warning),
    }
}

/// A directory collapsed in the tree: by group (grouped by change), and
/// path.
pub type DirKey = (Option<usize>, String);

/// The tree's rows, and the list drawing them.
pub struct FileTree {
    rows: Rc<Vec<TreeRow>>,
    list: ListState,
    /// The diff revision and analysis the rows are of. When they change,
    /// the list starts over; otherwise it keeps its place and the heights
    /// of the rows that stayed.
    source: Option<(u64, Option<usize>)>,
}

impl FileTree {
    pub fn new() -> Self {
        Self {
            rows: Rc::default(),
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            source: None,
        }
    }

    /// Takes `rows`, telling the list which changed.
    fn update(&mut self, rows: Vec<TreeRow>, source: (u64, Option<usize>)) {
        if self.source != Some(source) {
            self.source = Some(source);
            self.list.reset(rows.len());
            self.rows = Rc::new(rows);
            return;
        }
        let old = &self.rows;
        let prefix = old.iter().zip(&rows).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(rows[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        if prefix == old.len() && prefix == rows.len() {
            return;
        }
        self.list
            .splice(prefix..old.len() - suffix, rows.len() - prefix - suffix);
        self.rows = Rc::new(rows);
    }
}

/// A row of the changed files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeRow {
    /// A group of changes, by its title.
    Group { group: usize },
    /// A directory with changes, named from the directory above it, and
    /// the files under it, however deep.
    Directory {
        group: Option<usize>,
        path: String,
        label: String,
        depth: usize,
        files: Vec<usize>,
    },
    File {
        group: Option<usize>,
        file: usize,
        depth: usize,
    },
}

/// A directory in the tree, before it's compacted into rows.
#[derive(Default)]
struct Dir<'a> {
    name: &'a str,
    dirs: Vec<Dir<'a>>,
    files: Vec<usize>,
}

impl<'a> Dir<'a> {
    /// The tree of `files` (indices into `diff`, in its order).
    fn of(diff: &'a [FileDiff], files: impl IntoIterator<Item = usize>) -> Self {
        let mut root = Dir::default();
        for file in files {
            let dir = diff[file].directory();
            let mut node = &mut root;
            for name in dir.split('/').filter(|name| !name.is_empty()) {
                let ix = match node.dirs.iter().position(|d| d.name == name) {
                    Some(ix) => ix,
                    None => {
                        node.dirs.push(Dir {
                            name,
                            ..Default::default()
                        });
                        node.dirs.len() - 1
                    }
                };
                node = &mut node.dirs[ix];
            }
            node.files.push(file);
        }
        root
    }

    fn all_files(&self, out: &mut Vec<usize>) {
        for dir in &self.dirs {
            dir.all_files(out);
        }
        out.extend(&self.files);
    }

    /// Rows for what's in this directory, at `depth`: its directories, then
    /// its files.
    fn rows(
        &self,
        path: &str,
        group: Option<usize>,
        depth: usize,
        collapsed: &HashSet<DirKey>,
        rows: &mut Vec<TreeRow>,
    ) {
        for child in &self.dirs {
            // Directories with nothing but one directory in them are one row.
            let mut node = child;
            let mut label = child.name.to_owned();
            while node.files.is_empty() && node.dirs.len() == 1 {
                node = &node.dirs[0];
                label = format!("{label}/{}", node.name);
            }
            let path = match path {
                "" => label.clone(),
                path => format!("{path}/{label}"),
            };
            let mut files = Vec::new();
            node.all_files(&mut files);
            let expanded = !collapsed.contains(&(group, path.clone()));
            rows.push(TreeRow::Directory {
                group,
                path: path.clone(),
                label,
                depth,
                files,
            });
            if expanded {
                node.rows(&path, group, depth + 1, collapsed, rows);
            }
        }
        rows.extend(
            self.files
                .iter()
                .map(|&file| TreeRow::File { group, file, depth }),
        );
    }
}

/// The rows for `files` in `sort_files`'s order, file by file, leaving out
/// what's under collapsed directories.
fn tree_rows(files: &[FileDiff], collapsed: &HashSet<DirKey>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    Dir::of(files, 0..files.len()).rows("", None, 0, collapsed, &mut rows);
    rows
}

/// The rows grouped by change: each group, then the tree of its files.
fn grouped_tree_rows(
    files: &[FileDiff],
    analysis: &DiffAnalysis,
    collapsed_groups: &HashSet<usize>,
    collapsed: &HashSet<DirKey>,
) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    for (group, change) in analysis.groups.iter().enumerate() {
        rows.push(TreeRow::Group { group });
        if collapsed_groups.contains(&group) {
            continue;
        }
        // Hunks are in the diff's order, so their files are too.
        let mut in_group: Vec<usize> = change
            .hunks
            .iter()
            .map(|hunk| hunk.file)
            .filter(|&file| file < files.len())
            .collect();
        in_group.dedup();
        Dir::of(files, in_group).rows("", Some(group), 1, collapsed, &mut rows);
    }
    rows
}

impl DiffView {
    /// The diff's sidebar: the changed files and the language servers'
    /// status, or collapsed, a rail of the files' status icons.
    pub fn files_sidebar(&mut self, collapsed: bool, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let theme = Theme::get(cx);
        let tab = self.state.read(cx);
        let root = div().size_full().flex().flex_col();
        let Some(files) = tab.diff_files() else {
            return root.into_any_element();
        };
        let diff = match &files.diff {
            crate::state::DiffLoad::Ready(diff) => Some(diff),
            _ => None,
        };
        let current = self.current_file;

        if collapsed {
            let rows = diff.into_iter().flat_map(|diff| {
                diff.files.iter().enumerate().map(|(ix, file)| {
                    let (icon, color) = status_icon(file.status);
                    let view = view.clone();
                    IconButton::new(("rail-diff-file", ix), icon)
                        .icon_color(color)
                        .tooltip(file.path().to_owned())
                        .selected(current == Some(ix))
                        .on_click(move |_, _, cx| {
                            view.update(cx, |view, cx| view.reveal_file(ix, cx));
                        })
                })
            });
            return root
                .child(rail_header(
                    Icon::new(IconName::FileDiff).size(IconSize::Medium),
                    cx,
                ))
                .child(rail_items("diff-files-rail").children(rows))
                .into_any_element();
        }

        let analysis = super::shown_analysis(tab.app.read(cx), files);
        let rows = match (diff, analysis) {
            (Some(diff), Some(analysis)) => grouped_tree_rows(
                &diff.files,
                analysis,
                &self.collapsed_groups,
                &self.collapsed_dirs,
            ),
            (Some(diff), None) => tree_rows(&diff.files, &self.collapsed_dirs),
            (None, _) => Vec::new(),
        };
        let source = (files.revision, analysis.map(|a| Arc::as_ptr(a) as usize));
        let app = tab.app.read(cx);
        let viewed = diff.map_or(0, |diff| {
            diff.files
                .iter()
                .filter(|file| app.viewed(&files.viewed_scope, file) == Viewed::Yes)
                .count()
        });
        let header = sidebar_header(cx)
            .justify_between()
            .gap_2()
            .px_4()
            .text_xs()
            .text_color(theme.text_muted)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).child(
                        match (analysis, diff.map(|diff| diff.files.len())) {
                            (Some(analysis), _) if analysis.groups.len() == 1 => {
                                "1 change".to_owned()
                            }
                            (Some(analysis), _) => format!("{} changes", analysis.groups.len()),
                            (None, Some(1)) => "1 file".to_owned(),
                            (None, Some(n)) => format!("{n} files"),
                            (None, None) => "Files".to_owned(),
                        },
                    ))
                    // How far through them the review is, as GitHub shows it.
                    .when(viewed > 0, |el| {
                        el.child(
                            div()
                                .test_selector(|| "diff-files-viewed".to_owned())
                                .text_color(theme.text_subtle)
                                .child(format!(
                                    "{viewed}/{} viewed",
                                    diff.map_or(0, |diff| diff.files.len())
                                )),
                        )
                    }),
            )
            // The commit shown, which can trail the PR or branch until a
            // refresh.
            .children(diff.and_then(|diff| diff.head_oid.get(..7)).map(|head| {
                div()
                    .font_family(MONOSPACE)
                    .text_color(theme.text_subtle)
                    .child(head.to_owned())
            }));
        let status = files
            .code_intel
            .as_ref()
            .map(|intel| server_status(&self.state, intel, IntelOwner::Diff, theme));

        self.tree.update(rows, source);
        let tree = {
            let rows = self.tree.rows.clone();
            list(self.tree.list.clone(), move |ix, _, cx| {
                render_tree_row(&view, &rows[ix], cx)
            })
            .flex_1()
            .py_2()
        };
        root.child(header)
            .child(tree)
            .children(status)
            .into_any_element()
    }
}

fn render_tree_row(view: &Entity<DiffView>, row: &TreeRow, cx: &mut App) -> AnyElement {
    let theme = Theme::get(cx);
    let diff_view = view.read(cx);
    let current = diff_view.current_file;
    let current_group = diff_view.current_group;
    let tab = diff_view.state.read(cx);
    let Some(files) = tab.diff_files() else {
        return div().into_any_element();
    };
    let crate::state::DiffLoad::Ready(diff) = &files.diff else {
        return div().into_any_element();
    };
    match row {
        &TreeRow::Group { group } => {
            let Some(analysis) = super::shown_analysis(tab.app.read(cx), files) else {
                return div().into_any_element();
            };
            let expanded = !diff_view.collapsed_groups.contains(&group);
            super::analysis::group_row(
                view,
                group,
                analysis,
                expanded,
                current_group == Some(group),
                theme,
            )
        }
        TreeRow::Directory {
            group,
            path,
            label,
            depth,
            files,
        } => {
            let key = (*group, path.clone());
            let expanded = !diff_view.collapsed_dirs.contains(&key);
            // A collapsed directory stands in for the file shown.
            let selected = !expanded
                && current_group == *group
                && current.is_some_and(|ix| files.contains(&ix));
            let view = view.clone();
            let selector = match group {
                Some(group) => format!("diff-group-{group}-dir-{path}"),
                None => format!("diff-dir-{path}"),
            };
            // By path: a directory and the one nested in it can start with
            // the same file.
            TreeItem::new(ElementId::Name(selector.clone().into()), *depth)
                .selector(selector)
                .selected(selected)
                .disclosure(expanded)
                .icon(
                    if expanded {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    },
                    Color::Muted,
                )
                .tooltip(path.clone())
                .child(label.clone())
                .when(!expanded, |item| {
                    item.end_slot(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.text_subtle)
                            .child(files.len().to_string()),
                    )
                })
                .on_click(move |_, _, cx| {
                    view.update(cx, |view, cx| view.toggle_directory(key.clone(), cx));
                })
                .into_any_element()
        }
        &TreeRow::File { group, file, depth } => {
            let diff_file = &diff.files[file];
            let viewed = tab.app.read(cx).viewed(&files.viewed_scope, diff_file) == Viewed::Yes;
            let (icon, color) = status_icon(diff_file.status);
            let view = view.clone();
            let selector = match group {
                Some(group) => format!("diff-group-{group}-file-{file}"),
                None => format!("diff-file-{file}"),
            };
            let viewed_selector = format!("{selector}-viewed");
            TreeItem::new(row_id("diff-file", group, file), depth)
                .selector(selector)
                .selected(current == Some(file) && current_group == group)
                .icon(icon, color)
                .child(diff_file.name().to_owned())
                .end_slot(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .when(viewed, |el| {
                            el.child(
                                div().test_selector(|| viewed_selector).child(
                                    Icon::new(IconName::Check)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                ),
                            )
                        })
                        .child(stats(diff_file.additions, diff_file.deletions, theme)),
                )
                .on_click(move |_, _, cx| {
                    view.update(cx, |view, cx| match group {
                        Some(group) => view.reveal_group_file(group, file, cx),
                        None => view.reveal_file(file, cx),
                    });
                })
                .into_any_element()
        }
    }
}

/// A row's element ID, unique across groups.
fn row_id(kind: &'static str, group: Option<usize>, ix: usize) -> ElementId {
    match group {
        Some(group) => ElementId::NamedInteger(format!("{kind}-{group}").into(), ix as u64),
        None => (kind, ix).into(),
    }
}

/// One line per language server: whether hover works yet, and if not,
/// what to do about it.
pub fn server_status(
    state: &Entity<Tab>,
    intel: &CodeIntel,
    owner: IntelOwner,
    theme: &Theme,
) -> impl IntoElement {
    let status =
        |id: SharedString, icon: AnyElement, label: String, detail: Option<SharedString>| {
            div()
                .id(id)
                .flex()
                .items_start()
                .gap_2()
                .min_w_0()
                .child(div().flex_none().pt(px(1.)).child(icon))
                .child(div().flex_1().min_w_0().child(label))
                .when_some(detail, |el, detail| {
                    el.tooltip(move |_, cx| Tooltip::view(detail.clone(), cx))
                })
                .into_any_element()
        };
    let icon = |name: IconName, color: Color| {
        Icon::new(name)
            .size(IconSize::XSmall)
            .color(color)
            .into_any_element()
    };
    let spinner = |id: String| Spinner::new(id).into_any_element();

    let rows: Vec<AnyElement> = if let Some(error) = &intel.error {
        vec![status(
            "code-intel-error".into(),
            icon(IconName::TriangleAlert, Color::Warning),
            "Hover unavailable".into(),
            Some(error.clone()),
        )]
    } else if intel.servers.is_empty() {
        vec![status(
            "code-intel-worktree".into(),
            spinner("code-intel-worktree-spinner".into()),
            "Checking out for hover…".into(),
            Some(
                "Language servers run in Docket's own worktree of this project, at the \
                 commit shown."
                    .into(),
            ),
        )]
    } else {
        intel
            .servers
            .iter()
            .enumerate()
            .map(|(ix, slot)| {
                let id = SharedString::from(format!("code-intel-{ix}"));
                let name = slot.spec.name;
                let named = match &slot.source {
                    Some(source) => format!("{name} · {source}"),
                    None => name.to_owned(),
                };
                match &slot.state {
                    ServerState::NotInstalled { install } => div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(status(
                            id,
                            icon(IconName::Info, Color::Subtle),
                            format!("No hover: {name} isn't installed. To install it:"),
                            None,
                        ))
                        .child(install_command(ix, install.clone(), theme))
                        .into_any_element(),
                    ServerState::NeedsTrust => {
                        let state = state.clone();
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(status(
                                id,
                                icon(IconName::TriangleAlert, Color::Warning),
                                format!(
                                    "No hover: this pull request is from a fork, and {name} \
                                     can run code from it."
                                ),
                                Some(
                                    "Build scripts, macros, and toolchain or config files in \
                                     the pull request can run commands on this Mac."
                                        .into(),
                                ),
                            ))
                            .child(
                                Button::new(("code-intel-trust", ix), format!("Run {name}"))
                                    .style(ButtonStyle::Secondary)
                                    .size(ButtonSize::Compact)
                                    .on_click(move |_, _, cx| {
                                        state.update(cx, |state, cx| state.trust_code_intel(cx));
                                    }),
                            )
                            .into_any_element()
                    }
                    ServerState::Starting => status(
                        id,
                        spinner(format!("code-intel-spinner-{ix}")),
                        format!("Starting {named}…"),
                        None,
                    ),
                    ServerState::Restarting(reason) => status(
                        id,
                        spinner(format!("code-intel-spinner-{ix}")),
                        format!("{name} stopped; restarting…"),
                        Some(reason.clone()),
                    ),
                    ServerState::Running { activity, .. } if activity.busy => status(
                        id,
                        spinner(format!("code-intel-spinner-{ix}")),
                        format!(
                            "{name}: {}",
                            activity.message.as_deref().unwrap_or("loading")
                        ),
                        None,
                    ),
                    ServerState::Running { .. } => status(
                        id,
                        icon(IconName::CircleCheck, Color::Success),
                        named,
                        Some("Rest the mouse on code to see its hover.".into()),
                    ),
                    ServerState::Failed(error) => status(
                        id,
                        icon(IconName::TriangleAlert, Color::Warning),
                        format!("{name} stopped"),
                        Some(error.clone()),
                    ),
                }
            })
            .collect()
    };
    // Any server that ran (or tried to) can be restarted, as when it's
    // stuck; otherwise there's something to check for again.
    let restart = intel.servers.iter().any(|slot| {
        matches!(
            slot.state,
            ServerState::Starting
                | ServerState::Restarting(_)
                | ServerState::Running { .. }
                | ServerState::Failed(_)
        )
    });
    let failed = intel.servers.iter().any(|slot| {
        matches!(
            slot.state,
            ServerState::NotInstalled { .. } | ServerState::Failed(_)
        )
    });
    let retry = intel.error.is_some() || failed;
    let state = state.clone();
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap_2()
        .px_4()
        .py_2()
        .border_t_1()
        .border_color(theme.border_subtle)
        .text_xs()
        .text_color(theme.text_muted)
        .children(rows)
        .when(restart || retry, |el| {
            let (id, label) = if restart {
                ("code-intel-restart", "Restart Language Servers")
            } else {
                ("code-intel-retry", "Check Again")
            };
            el.child(
                Button::new(id, label)
                    .icon(IconName::RefreshCw)
                    // Quiet while everything's working.
                    .style(if retry {
                        ButtonStyle::Secondary
                    } else {
                        ButtonStyle::Ghost
                    })
                    .size(ButtonSize::Compact)
                    .on_click(move |_, _, cx| {
                        state.update(cx, |state, cx| state.retry_code_intel(owner, cx));
                    }),
            )
        })
}

/// A shell command to install a server, with a button to copy it.
fn install_command(ix: usize, command: SharedString, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .border_1()
                .border_color(theme.border_subtle)
                .bg(theme.elevated)
                .font_family(MONOSPACE)
                .text_color(theme.text)
                .child(command.clone()),
        )
        .child(
            IconButton::new(("code-intel-copy", ix), IconName::Copy)
                .size(ButtonSize::Compact)
                .tooltip("Copy command")
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(command.to_string()));
                }),
        )
}

/// `+N −M`, omitting zero sides.
pub fn stats(additions: u32, deletions: u32, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .gap_1()
        .text_xs()
        .when(additions > 0, |el| {
            el.child(
                div()
                    .text_color(theme.success)
                    .child(format!("+{additions}")),
            )
        })
        .when(deletions > 0, |el| {
            el.child(
                div()
                    .text_color(theme.danger)
                    .child(format!("−{deletions}")),
            )
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::diff::FileContent;
    use crate::github::FakeGithub;
    use crate::test_support::{
        self, VisualTestContextExt as _, git_checkout, git_commit, open_docket, project,
    };

    fn files(paths: &[&str]) -> Vec<FileDiff> {
        let mut files: Vec<FileDiff> = paths
            .iter()
            .map(|path| FileDiff {
                old_path: Some((*path).to_owned()),
                new_path: Some((*path).to_owned()),
                status: FileStatus::Modified,
                additions: 0,
                deletions: 0,
                content: FileContent::Binary,
                view_key: crate::diff::ViewKey::default(),
            })
            .collect();
        crate::diff::sort_files(&mut files);
        files
    }

    fn dir(path: &str, label: &str, depth: usize, files: &[usize]) -> TreeRow {
        TreeRow::Directory {
            group: None,
            path: path.to_owned(),
            label: label.to_owned(),
            depth,
            files: files.to_vec(),
        }
    }

    fn file(file: usize, depth: usize) -> TreeRow {
        TreeRow::File {
            group: None,
            file,
            depth,
        }
    }

    #[test]
    fn directories_nest_and_compact() {
        let files = files(&[
            "3rdparty/python/py_universes.bzl",
            "3rdparty/python/api/uv.lock",
            "README.md",
        ]);
        let names: Vec<_> = files.iter().map(FileDiff::path).collect();
        assert_eq!(
            names,
            [
                "3rdparty/python/api/uv.lock",
                "3rdparty/python/py_universes.bzl",
                "README.md"
            ]
        );
        // `3rdparty` holds only `python`, so they're one row, and `api`
        // nests under it.
        assert_eq!(
            tree_rows(&files, &HashSet::new()),
            [
                dir("3rdparty/python", "3rdparty/python", 0, &[0, 1]),
                dir("3rdparty/python/api", "api", 1, &[0]),
                file(0, 2),
                file(1, 1),
                file(2, 0),
            ]
        );
        // Collapsing a directory hides everything under it.
        let collapsed = HashSet::from([(None, "3rdparty/python".to_owned())]);
        assert_eq!(
            tree_rows(&files, &collapsed),
            [
                dir("3rdparty/python", "3rdparty/python", 0, &[0, 1]),
                file(2, 0)
            ]
        );
    }

    #[test]
    fn siblings_share_their_parent() {
        let files = files(&["src/a/x.rs", "src/b/y.rs"]);
        assert_eq!(
            tree_rows(&files, &HashSet::new()),
            [
                dir("src", "src", 0, &[0, 1]),
                dir("src/a", "a", 1, &[0]),
                file(0, 2),
                dir("src/b", "b", 1, &[1]),
                file(1, 2),
            ]
        );
    }

    #[test]
    fn groups_head_trees_of_their_files() {
        use crate::ai::{ChangeGroup, HunkRef};
        let files = files(&["src/a.rs", "src/b.rs", "README.md"]);
        let hunk = |file| HunkRef {
            file,
            excerpt: None,
        };
        let group = |hunks| ChangeGroup {
            title: String::new(),
            summary: String::new(),
            impact: String::new(),
            hunks,
        };
        let analysis = DiffAnalysis {
            groups: vec![group(vec![hunk(0), hunk(2)]), group(vec![hunk(1)])],
        };
        let in_group = |group, row: TreeRow| match row {
            TreeRow::Directory {
                path,
                label,
                depth,
                files,
                ..
            } => TreeRow::Directory {
                group: Some(group),
                path,
                label,
                depth,
                files,
            },
            TreeRow::File { file, depth, .. } => TreeRow::File {
                group: Some(group),
                file,
                depth,
            },
            row => row,
        };
        let rows = grouped_tree_rows(&files, &analysis, &HashSet::new(), &HashSet::new());
        assert_eq!(
            rows,
            [
                TreeRow::Group { group: 0 },
                in_group(0, dir("src", "src", 1, &[0])),
                in_group(0, file(0, 2)),
                in_group(0, file(2, 1)),
                TreeRow::Group { group: 1 },
                in_group(1, dir("src", "src", 1, &[1])),
                in_group(1, file(1, 2)),
            ]
        );
        let collapsed = HashSet::from([0]);
        let rows = grouped_tree_rows(&files, &analysis, &collapsed, &HashSet::new());
        assert_eq!(
            rows[..2],
            [TreeRow::Group { group: 0 }, TreeRow::Group { group: 1 }]
        );
    }

    #[gpui::test]
    fn files_are_grouped_by_directory(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        let root = checkout.path();
        git_checkout(
            root,
            &[
                ("README.md", "# Widgets\n"),
                ("docs/index.md", "Widgets.\n"),
                ("docs/guide/intro.md", "Turn it on.\n"),
            ],
        );
        test_support::git(root, &["checkout", "--quiet", "feature"]);
        for path in ["README.md", "docs/index.md", "docs/guide/intro.md"] {
            std::fs::write(root.join(path), "Changed.\n").unwrap();
        }
        git_commit(root, "Change everything", "2026-01-02T00:00:00Z");
        test_support::git(root, &["checkout", "--quiet", "main"]);
        let projects = vec![project(1, root, "acme/widgets")];
        let (_, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("branch-1-feature");

        // Directories nest, each heading its subdirectories and then its
        // files, and top-level files follow.
        let top = |cx: &mut VisualTestContext, selector: &str| {
            cx.bounds_of(selector)
                .unwrap_or_else(|| panic!("{selector} isn't drawn"))
                .origin
                .y
        };
        let order = [
            "diff-dir-docs",
            "diff-dir-docs/guide",
            "diff-file-0",
            "diff-file-1",
            "diff-file-2",
        ]
        .map(|selector| top(cx, selector));
        assert!(order.is_sorted(), "{order:?}");

        // Clicking a directory collapses everything under it, and again
        // expands it.
        cx.click_on("diff-dir-docs");
        assert!(!cx.is_drawn("diff-file-0"));
        assert!(!cx.is_drawn("diff-dir-docs/guide"));
        assert!(cx.is_drawn("diff-file-2"));
        cx.click_on("diff-dir-docs");
        assert!(cx.is_drawn("diff-file-0"));
        assert!(cx.is_drawn("diff-dir-docs/guide"));
    }
}
