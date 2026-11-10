//! A pull request's or branch's commits, oldest first and grouped by the
//! day they were committed, as GitHub's Commits tab lists them.

use chrono::{Local, NaiveDate};
use gpui::{AnyElement, App, ElementId, FontWeight, SharedString, Window, div, prelude::*, px};

use super::components::{Icon, IconButton, IconName, MONOSPACE, TestSelector as _, relative_time};
use super::theme::{Color, Theme};
use crate::github::Commit;

#[derive(IntoElement)]
pub struct CommitList {
    id: ElementId,
    commits: Vec<Commit>,
    /// Shown above them, like a note that some were left out.
    notice: Option<AnyElement>,
}

impl CommitList {
    pub fn new(id: impl Into<ElementId>, commits: impl IntoIterator<Item = Commit>) -> Self {
        Self {
            id: id.into(),
            commits: commits.into_iter().collect(),
            notice: None,
        }
    }

    pub fn notice(mut self, notice: Option<impl IntoElement>) -> Self {
        self.notice = notice.map(IntoElement::into_any_element);
        self
    }
}

impl RenderOnce for CommitList {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let mut days: Vec<(NaiveDate, Vec<Commit>)> = Vec::new();
        for commit in self.commits {
            let day = commit.committed_at.with_timezone(&Local).date_naive();
            match days.last_mut() {
                Some((last, commits)) if *last == day => commits.push(commit),
                _ => days.push((day, vec![commit])),
            }
        }
        let empty = days.is_empty();

        div().id(self.id).flex_1().overflow_y_scroll().child(
            div()
                .flex()
                .flex_col()
                .gap_4()
                .max_w(px(1120.))
                .px_6()
                .py_4()
                .children(self.notice)
                .when(empty, |el| {
                    el.child(
                        div()
                            .pt(px(48.))
                            .flex()
                            .justify_center()
                            .text_color(theme.text_muted)
                            .child("No commits"),
                    )
                })
                .children(days.into_iter().map(|(day, commits)| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_xs()
                                .text_color(theme.text_muted)
                                .child(Icon::new(IconName::GitCommit).color(Color::Muted))
                                .child(format!("Commits on {}", day.format("%b %-d, %Y"))),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .rounded_md()
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.background)
                                .overflow_hidden()
                                .children(
                                    commits
                                        .iter()
                                        .enumerate()
                                        .map(|(ix, commit)| commit_row(commit, ix > 0, theme)),
                                ),
                        )
                })),
        )
    }
}

/// The commit's headline, who committed it and when, and its short object
/// name, opening it on GitHub where it has a page there.
fn commit_row(commit: &Commit, divided: bool, theme: &Theme) -> impl IntoElement {
    let author = commit.author.as_deref().unwrap_or("Someone");
    div()
        .test_selector(|| format!("commit-{}", commit.id))
        .flex()
        .items_center()
        .gap_3()
        .min_h(px(52.))
        .pl_4()
        .pr_2()
        .py_2()
        .when(divided, |el| {
            el.border_t_1().border_color(theme.border_subtle)
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(commit.headline.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(theme.text_muted)
                        .child(format!(
                            "{author} committed {}",
                            relative_time(commit.committed_at)
                        )),
                ),
        )
        .child(
            div()
                .flex_none()
                .font_family(MONOSPACE)
                .text_xs()
                .text_color(theme.text_subtle)
                .child(commit.short_oid.clone()),
        )
        .children(commit.url.clone().map(|url| {
            IconButton::new(
                SharedString::from(format!("open-commit-{}", commit.id)),
                IconName::ExternalLink,
            )
            .tooltip("Open on GitHub")
            .on_click(move |_, _, cx| crate::ui::open_in_browser(&url, cx))
        }))
}
