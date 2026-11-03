//! A branch opened from the docket: what it changes since it left the
//! default branch, laid out like a pull request, with its changed files in
//! the sidebar beside it. Its tabs are a pull request's `TabBar`; Files is
//! the only one so far.

use gpui::{App, Entity, FontWeight, Window, div, prelude::*, px};

use super::components::{Badge, IconButton, IconName, Spinner, TabBar, TabBarItem, relative_time};
use super::diff_view::DiffView;
use super::pr_detail::branch;
use super::theme::Theme;
use crate::projects::Project;
use crate::state::{BranchDiff, DiffLoad, Tab};

#[derive(IntoElement)]
pub struct BranchView {
    state: Entity<Tab>,
    /// The diff. It keeps scroll position and collapsed files, so it's an
    /// entity owned by the workspace rather than rebuilt each frame.
    diff_view: Option<Entity<DiffView>>,
}

impl BranchView {
    pub fn new(state: Entity<Tab>, diff_view: Option<Entity<DiffView>>) -> Self {
        Self { state, diff_view }
    }
}

impl RenderOnce for BranchView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let root = div().flex_1().min_w_0().h_full().flex().flex_col();
        let tab = self.state.read(cx);
        let Some(open) = &tab.branch else {
            return root;
        };
        let project = tab.app.read(cx).project(open.project);
        root.child(header(open, project, &self.state, theme))
            .child(title(open, theme))
            .child(tabs(open))
            .children(self.diff_view)
    }
}

fn tabs(open: &BranchDiff) -> impl IntoElement {
    let files = match &open.files.diff {
        DiffLoad::Ready(diff) => Some(diff.files.len().to_string()),
        _ => None,
    };
    TabBar::new().child(
        TabBarItem::new("branch-tab-files", IconName::FileDiff, "Files")
            .count(files)
            .selected(true),
    )
}

fn header(
    open: &BranchDiff,
    project: Option<&Project>,
    state: &Entity<Tab>,
    theme: &Theme,
) -> impl IntoElement {
    let (back, refresh) = (state.clone(), state.clone());
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_4()
        .h(px(48.))
        .pl_2()
        .pr_4()
        .border_b_1()
        .border_color(theme.border_subtle)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    IconButton::new("branch-back", IconName::ArrowLeft)
                        .tooltip("Back (Esc)")
                        .on_click(move |_, _, cx| {
                            back.update(cx, |state, cx| state.close_branch(cx));
                        }),
                )
                .children(project.map(|project| {
                    div()
                        .truncate()
                        .text_color(theme.text_muted)
                        .child(project.repository.to_string())
                })),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .when(open.files.is_busy(), |el| {
                    el.child(Spinner::new("branch-refreshing"))
                })
                .child(
                    IconButton::new("branch-refresh", IconName::RefreshCw)
                        .tooltip("Refresh (⌘R)")
                        .on_click(move |_, _, cx| {
                            refresh.update(cx, |state, cx| state.refresh(cx));
                        }),
                ),
        )
}

/// The branch's name, and what it's compared to, as a pull request's title
/// shows its branches.
fn title(open: &BranchDiff, theme: &Theme) -> impl IntoElement {
    let branch_info = &open.branch;
    let stats = match &open.files.diff {
        DiffLoad::Ready(diff) => {
            let (additions, deletions) = diff.files.iter().fold((0, 0), |(a, d), file| {
                (a + file.additions, d + file.deletions)
            });
            let files = match diff.files.len() {
                1 => "1 file".to_owned(),
                n => format!("{n} files"),
            };
            Some((additions, deletions, files))
        }
        _ => None,
    };
    let meta = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_1p5()
        .gap_y_1()
        .text_color(theme.text_muted)
        .child(if branch_info.worktree.is_some() {
            Badge::new("Checked out").icon(IconName::FolderOpen)
        } else {
            Badge::new("No pull request").icon(IconName::GitBranch)
        })
        .child(branch(&branch_info.name, theme))
        .children(open.base.as_ref().map(|base| {
            div()
                .flex()
                .items_center()
                .gap_x_1p5()
                .child("since it left")
                .child(branch(&base.name, theme))
        }))
        .children(stats.map(|(additions, deletions, files)| {
            div()
                .flex()
                .gap_1()
                .pl_2()
                .text_xs()
                .child(
                    div()
                        .text_color(theme.success)
                        .child(format!("+{additions}")),
                )
                .child(
                    div()
                        .text_color(theme.danger)
                        .child(format!("−{deletions}")),
                )
                .child(
                    div()
                        .text_color(theme.text_subtle)
                        .child(format!("· {files}")),
                )
        }));
    div()
        .flex()
        .flex_none()
        .flex_col()
        .gap_2()
        .px_6()
        .pt_5()
        // The tabs below draw the line under it.
        .pb_2()
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .child(branch_info.subject.clone()),
        )
        .child(meta)
        .child(div().text_xs().text_color(theme.text_subtle).child(format!(
            "Committed {}",
            relative_time(branch_info.updated_at)
        )))
}
