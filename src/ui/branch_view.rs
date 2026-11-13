//! A branch opened from the docket: what it changes since it left the
//! default branch, laid out like a pull request, with its changed files in
//! the sidebar beside it, its commits, and all its files to browse. Its
//! tabs are a pull request's `TabBar`.

use gpui::{AnyElement, App, Entity, FontWeight, SharedString, Window, div, prelude::*, px};

use super::commit_list::CommitList;
use super::components::{
    Badge, Callout, CalloutKind, IconButton, IconName, Spinner, TabBar, TabBarItem, relative_time,
};
use super::diff_view::DiffView;
use super::file_browser::FileBrowserView;
use super::pr_detail::branch;
use super::theme::Theme;
use crate::projects::Project;
use crate::projects::branches::MAX_COMMITS;
use crate::state::{BranchDiff, BranchTab, CommitsLoad, DiffLoad, Tab};

#[derive(IntoElement)]
pub struct BranchView {
    state: Entity<Tab>,
    /// The diff. It keeps scroll position and collapsed files, so it's an
    /// entity owned by the workspace rather than rebuilt each frame.
    diff_view: Option<Entity<DiffView>>,
    /// The Browse tab, likewise.
    browser: Option<Entity<FileBrowserView>>,
}

impl BranchView {
    pub fn new(
        state: Entity<Tab>,
        diff_view: Option<Entity<DiffView>>,
        browser: Option<Entity<FileBrowserView>>,
    ) -> Self {
        Self {
            state,
            diff_view,
            browser,
        }
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
        let root = root
            .child(header(open, project, &self.state, theme, cx))
            .child(title(open, theme))
            .child(tabs(open, &self.state));
        match open.tab {
            BranchTab::Files => root.children(self.diff_view),
            BranchTab::Commits => root.child(commits(open)),
            BranchTab::Browse => root.children(self.browser),
        }
    }
}

fn tabs(open: &BranchDiff, state: &Entity<Tab>) -> impl IntoElement {
    let commits = match &open.commits {
        CommitsLoad::Ready(listed) => Some(match listed.truncated {
            true => format!("{MAX_COMMITS}+"),
            false => listed.commits.len().to_string(),
        }),
        _ => None,
    };
    let files = match &open.files.diff {
        DiffLoad::Ready(diff) => Some(diff.files.len().to_string()),
        _ => None,
    };
    let tab = |tab: BranchTab, id: &'static str, icon: IconName, label: &'static str| {
        let state = state.clone();
        TabBarItem::new(id, icon, label)
            .selected(open.tab == tab)
            .on_click(move |_, _, cx| {
                state.update(cx, |state, cx| state.select_branch_tab(tab, cx));
            })
    };
    TabBar::new()
        .child(
            tab(
                BranchTab::Commits,
                "branch-tab-commits",
                IconName::GitCommit,
                "Commits",
            )
            .count(commits),
        )
        .child(
            tab(
                BranchTab::Files,
                "branch-tab-files",
                IconName::FileDiff,
                "Files",
            )
            .count(files),
        )
        .child(tab(
            BranchTab::Browse,
            "branch-tab-browse",
            IconName::FolderTree,
            "Browse",
        ))
}

/// The Commits tab: those on the branch since it left the default branch.
fn commits(open: &BranchDiff) -> AnyElement {
    let id = SharedString::from(format!("branch-commits-{}", open.branch.name));
    match &open.commits {
        CommitsLoad::Loading => div()
            .flex_1()
            .flex()
            .justify_center()
            .pt(px(64.))
            .child(Spinner::new("branch-commits-loading"))
            .into_any_element(),
        CommitsLoad::Failed(err) => div()
            .p_6()
            .child(Callout::new(CalloutKind::Error, err.clone()).title("Couldn't list its commits"))
            .into_any_element(),
        CommitsLoad::Ready(listed) => {
            let notice = listed.truncated.then(|| {
                Callout::new(
                    CalloutKind::Warning,
                    format!("Showing the latest {MAX_COMMITS} commits."),
                )
            });
            CommitList::new(id, listed.commits.iter().cloned())
                .notice(notice)
                .into_any_element()
        }
    }
}

fn header(
    open: &BranchDiff,
    project: Option<&Project>,
    state: &Entity<Tab>,
    theme: &Theme,
    cx: &App,
) -> impl IntoElement {
    let (back, refresh) = (state.clone(), state.clone());
    // The Browse tab re-listing its files.
    let browsing = open.tab == BranchTab::Browse
        && state
            .read(cx)
            .browser
            .as_ref()
            .is_some_and(|browser| browser.reloading);
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
                .when(open.files.is_busy() || browsing, |el| {
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
        .child(branch("branch-head", &branch_info.name, theme))
        .children(open.base.as_ref().map(|base| {
            div()
                .flex()
                .items_center()
                .gap_x_1p5()
                .child("since it left")
                .child(branch("branch-base", &base.name, theme))
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
