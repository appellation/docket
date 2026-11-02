//! The changed files beside the diff. Clicking one scrolls the diff to it.

use gpui::{App, Entity, div, prelude::*, px};

use super::DiffView;
use crate::diff::{FileStatus, PullRequestDiff};
use crate::ui::components::{IconName, ListItem};
use crate::ui::theme::{Color, Theme};

pub const FILE_LIST_WIDTH: f32 = 260.;

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

pub fn file_list(
    diff: &PullRequestDiff,
    current: Option<usize>,
    view: Entity<DiffView>,
    cx: &App,
) -> impl IntoElement {
    let theme = Theme::get(cx);
    div()
        .id("diff-files")
        .flex_none()
        .w(px(FILE_LIST_WIDTH))
        .h_full()
        .overflow_y_scroll()
        .p_2()
        .bg(theme.surface)
        .border_r_1()
        .border_color(theme.border_subtle)
        .children(diff.files.iter().enumerate().map(|(ix, file)| {
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
        }))
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
