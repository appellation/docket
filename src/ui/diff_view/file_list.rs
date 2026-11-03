//! The changed files beside the diff, the deepest of the sidebars
//! (`sidebar_stack`) on the Files tab. Clicking one scrolls the diff to it.
//! Below them, the status of the language servers behind hover, which the
//! file browser shows too.

use gpui::{AnyElement, App, ClipboardItem, Entity, FontWeight, SharedString, div, prelude::*, px};

use super::DiffView;
use crate::diff::{FileDiff, FileStatus};
use crate::state::{CodeIntel, DiffLoad, IntelOwner, ServerState, Tab};
use crate::ui::components::{
    Button, ButtonSize, ButtonStyle, Icon, IconButton, IconName, IconSize, ListItem, MONOSPACE,
    Spinner, Tooltip,
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

/// The Files tab's sidebar: the changed files and the language servers'
/// status, or collapsed, a rail of the files' status icons.
pub fn files_sidebar(view: &Entity<DiffView>, collapsed: bool, cx: &App) -> AnyElement {
    let theme = Theme::get(cx);
    let diff_view = view.read(cx);
    let state = &diff_view.state;
    let current = diff_view.current_file;
    let root = div().size_full().flex().flex_col();
    let Some(files) = state
        .read(cx)
        .detail
        .as_ref()
        .and_then(|d| d.files.as_ref())
    else {
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
        // The commit shown, which can trail the PR until a refresh.
        .children(diff.and_then(|diff| diff.head_oid.get(..7)).map(|head| {
            div()
                .font_family(MONOSPACE)
                .text_color(theme.text_subtle)
                .child(head.to_owned())
        }));

    root.child(header)
        .child(
            div()
                .id("diff-files")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_px()
                .p_2()
                .children(diff.into_iter().flat_map(|diff| {
                    diff.files
                        .iter()
                        .enumerate()
                        .map(|(ix, file)| file_row(ix, file, current, view, theme))
                })),
        )
        .children(
            files
                .code_intel
                .as_ref()
                .map(|intel| server_status(state, intel, IntelOwner::PullRequest, theme)),
        )
        .into_any_element()
}

fn file_row(
    ix: usize,
    file: &FileDiff,
    current: Option<usize>,
    view: &Entity<DiffView>,
    theme: &Theme,
) -> impl IntoElement {
    let path = file.path();
    let (dir, name) = match path.rsplit_once('/') {
        Some((dir, name)) => (Some(dir.to_owned()), name.to_owned()),
        None => (None, path.to_owned()),
    };
    let (icon, color) = status_icon(file.status);
    let view = view.clone();
    ListItem::new(("diff-file", ix))
        .icon_color(icon, color)
        .selected(current == Some(ix))
        .child(div().truncate().child(name))
        .children(dir.map(|dir| {
            div()
                .truncate()
                .text_xs()
                .text_color(theme.text_subtle)
                .child(dir)
        }))
        .end_slot(stats(file.additions, file.deletions, theme))
        .on_click(move |_, _, cx| {
            view.update(cx, |view, cx| view.reveal_file(ix, cx));
        })
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
                "Language servers run in Docket's own worktree of this project, at the pull \
                 request's head."
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
