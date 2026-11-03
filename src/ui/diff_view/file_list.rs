//! The changed files beside the diff, the deepest of the sidebars
//! (`sidebar_stack`) on the Files tab. Clicking one scrolls the diff to it.
//! Below them, the status of the language servers behind hover, which the
//! file browser shows too.
//!
//! Files are grouped by the directory they're in, rather than nested in a
//! tree of every directory above them: each directory with changes is one
//! row naming its whole path, which collapses its files. Top-level files
//! follow the directories, as in the file browser.

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClipboardItem, Entity, FontWeight, SharedString, div, prelude::*, px,
    uniform_list,
};

use super::DiffView;
use crate::diff::{FileDiff, FileStatus, directories};
use crate::state::{CodeIntel, DiffLoad, IntelOwner, ServerState, Tab};
use crate::ui::components::{
    Button, ButtonSize, ButtonStyle, Icon, IconButton, IconName, IconSize, MONOSPACE, Spinner,
    Tooltip, TreeItem,
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

/// The diff's sidebar: the changed files and the language servers'
/// status, or collapsed, a rail of the files' status icons.
pub fn files_sidebar(view: &Entity<DiffView>, collapsed: bool, cx: &App) -> AnyElement {
    let theme = Theme::get(cx);
    let diff_view = view.read(cx);
    let state = &diff_view.state;
    let current = diff_view.current_file;
    let root = div().size_full().flex().flex_col();
    let Some(files) = state.read(cx).diff_files() else {
        return root.into_any_element();
    };
    let diff = match &files.diff {
        DiffLoad::Ready(diff) => Some(diff),
        _ => None,
    };

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

    let header = sidebar_header(cx)
        .justify_between()
        .gap_2()
        .px_4()
        .text_xs()
        .text_color(theme.text_muted)
        .child(div().font_weight(FontWeight::MEDIUM).child(
            match diff.map(|diff| diff.files.len()) {
                Some(1) => "1 file".to_owned(),
                Some(n) => format!("{n} files"),
                None => "Files".to_owned(),
            },
        ))
        // The commit shown, which can trail the PR or branch until a
        // refresh.
        .children(diff.and_then(|diff| diff.head_oid.get(..7)).map(|head| {
            div()
                .font_family(MONOSPACE)
                .text_color(theme.text_subtle)
                .child(head.to_owned())
        }));

    let rows = Rc::new(
        diff.map(|diff| tree_rows(&diff.files, &diff_view.collapsed_dirs))
            .unwrap_or_default(),
    );
    let list = {
        let view = view.clone();
        let rows = rows.clone();
        uniform_list("diff-files", rows.len(), move |range, _, cx| {
            render_tree_rows(&view, &rows, range, cx)
        })
        .flex_1()
        .py_2()
        .track_scroll(diff_view.files_scroll.clone())
    };
    root.child(header)
        .child(list)
        .children(
            files
                .code_intel
                .as_ref()
                .map(|intel| server_status(state, intel, IntelOwner::Diff, theme)),
        )
        .into_any_element()
}

/// A row of the changed files.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TreeRow {
    /// A directory with changes, and the files in it.
    Directory { path: String, files: Range<usize> },
    /// A file, indented under its directory unless it's top-level.
    File { file: usize, depth: usize },
}

/// The rows for files in `sort_files`'s order, leaving out the files of
/// collapsed directories.
fn tree_rows(files: &[FileDiff], collapsed: &HashSet<String>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    for (dir, range) in directories(files) {
        if dir.is_empty() {
            rows.extend(range.map(|file| TreeRow::File { file, depth: 0 }));
            continue;
        }
        let expanded = !collapsed.contains(dir);
        rows.push(TreeRow::Directory {
            path: dir.to_owned(),
            files: range.clone(),
        });
        if expanded {
            rows.extend(range.map(|file| TreeRow::File { file, depth: 1 }));
        }
    }
    rows
}

fn render_tree_rows(
    view: &Entity<DiffView>,
    rows: &[TreeRow],
    range: Range<usize>,
    cx: &App,
) -> Vec<AnyElement> {
    let theme = Theme::get(cx);
    let diff_view = view.read(cx);
    let current = diff_view.current_file;
    let Some(DiffLoad::Ready(diff)) = diff_view
        .state
        .read(cx)
        .diff_files()
        .map(|files| &files.diff)
    else {
        return Vec::new();
    };
    rows[range.start.min(rows.len())..range.end.min(rows.len())]
        .iter()
        .map(|row| match row {
            TreeRow::Directory { path, files } => {
                let expanded = !diff_view.collapsed_dirs.contains(path);
                // A collapsed directory stands in for the file shown.
                let selected = !expanded && current.is_some_and(|ix| files.contains(&ix));
                let view = view.clone();
                let toggle = path.clone();
                TreeItem::new(("diff-dir", files.start), 0)
                    .selector(format!("diff-dir-{path}"))
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
                    .child(path.clone())
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
                        view.update(cx, |view, cx| view.toggle_directory(&toggle, cx));
                    })
                    .into_any_element()
            }
            &TreeRow::File { file: ix, depth } => {
                let file = &diff.files[ix];
                let (icon, color) = status_icon(file.status);
                let view = view.clone();
                TreeItem::new(("diff-file", ix), depth)
                    .selected(current == Some(ix))
                    .icon(icon, color)
                    .child(file.name().to_owned())
                    .end_slot(stats(file.additions, file.deletions, theme))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |view, cx| view.reveal_file(ix, cx));
                    })
                    .into_any_element()
            }
        })
        .collect()
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
    let retry = intel.error.is_some()
        || intel.servers.iter().any(|slot| {
            matches!(
                slot.state,
                ServerState::NotInstalled { .. } | ServerState::Failed(_)
            )
        });
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
        .when(retry, |el| {
            el.child(
                Button::new("code-intel-retry", "Check Again")
                    .icon(IconName::RefreshCw)
                    .style(ButtonStyle::Secondary)
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

    #[test]
    fn collapsed_directories_hide_their_files() {
        let file = |path: &str| FileDiff {
            old_path: Some(path.to_owned()),
            new_path: Some(path.to_owned()),
            status: FileStatus::Modified,
            additions: 0,
            deletions: 0,
            content: FileContent::Binary,
        };
        let files = ["docs/a.md", "docs/guide/b.md", "README.md"].map(file);
        let dir = |path: &str, files| TreeRow::Directory {
            path: path.to_owned(),
            files,
        };
        let file = |file, depth| TreeRow::File { file, depth };
        assert_eq!(
            tree_rows(&files, &HashSet::new()),
            [
                dir("docs", 0..1),
                file(0, 1),
                dir("docs/guide", 1..2),
                file(1, 1),
                file(2, 0),
            ]
        );
        // A directory inside a collapsed one is its own row, so it stays.
        assert_eq!(
            tree_rows(&files, &HashSet::from(["docs".to_owned()])),
            [
                dir("docs", 0..1),
                dir("docs/guide", 1..2),
                file(1, 1),
                file(2, 0)
            ]
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

        // Each directory heads its own files, and top-level files follow.
        let top = |cx: &mut VisualTestContext, selector: &str| {
            cx.bounds_of(selector)
                .unwrap_or_else(|| panic!("{selector} isn't drawn"))
                .origin
                .y
        };
        let order = [
            "diff-dir-docs",
            "diff-file-0",
            "diff-dir-docs/guide",
            "diff-file-1",
            "diff-file-2",
        ]
        .map(|selector| top(cx, selector));
        assert!(order.is_sorted(), "{order:?}");

        // Clicking a directory collapses just its files, and again expands
        // them.
        cx.click_on("diff-dir-docs");
        assert!(!cx.is_drawn("diff-file-0"));
        assert!(cx.is_drawn("diff-dir-docs/guide"));
        assert!(cx.is_drawn("diff-file-1"));
        cx.click_on("diff-dir-docs");
        assert!(cx.is_drawn("diff-file-0"));
    }
}
