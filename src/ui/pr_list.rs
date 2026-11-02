//! List of pull requests for the selected sidebar section.

use gpui::{App, Entity, Hsla, Window, div, prelude::*};

use super::theme::Theme;
use crate::github::{PullRequest, PullRequestState, ReviewDecision};
use crate::state::AppState;

#[derive(IntoElement)]
pub struct PrList {
    state: Entity<AppState>,
}

impl PrList {
    pub fn new(state: Entity<AppState>) -> Self {
        Self { state }
    }
}

impl RenderOnce for PrList {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let state = self.state.read(cx);
        let section = state.section(state.selected);

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().text_lg().child(state.selected.label()))
            .when(section.loading, |el| {
                el.child(div().text_color(theme.text_muted).child("Refreshing…"))
            });

        let body = div()
            .id("pr-list")
            .flex_1()
            .overflow_y_scroll()
            .children(
                section
                    .error
                    .clone()
                    .map(|err| div().p_4().text_color(theme.danger).child(err)),
            )
            .when(
                !section.loading && section.error.is_none() && section.pull_requests.is_empty(),
                |el| {
                    el.child(
                        div()
                            .p_4()
                            .text_color(theme.text_muted)
                            .child("Nothing here."),
                    )
                },
            )
            .children(section.pull_requests.iter().map(|pr| pr_row(pr, theme)));

        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .child(body)
    }
}

fn pr_row(pr: &PullRequest, theme: &Theme) -> impl IntoElement {
    let url = pr.url.clone();
    let (status, status_color): (&str, Hsla) = match (pr.state, pr.is_draft, pr.review_decision) {
        (PullRequestState::Merged, ..) => ("Merged", theme.accent),
        (PullRequestState::Closed, ..) => ("Closed", theme.danger),
        (_, true, _) => ("Draft", theme.text_muted),
        (_, _, Some(ReviewDecision::Approved)) => ("Approved", theme.success),
        (_, _, Some(ReviewDecision::ChangesRequested)) => ("Changes requested", theme.danger),
        (_, _, Some(ReviewDecision::ReviewRequired)) | (_, _, None) => {
            ("Review required", theme.text_muted)
        }
    };

    div()
        .id(gpui::SharedString::from(pr.id.clone()))
        .flex()
        .flex_col()
        .gap_1()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|style| style.bg(theme.surface_hover))
        .child(pr.title.clone())
        .child(
            div()
                .flex()
                .gap_3()
                .text_xs()
                .text_color(theme.text_muted)
                .child(format!("{}#{}", pr.repository, pr.number))
                .children(pr.author.clone())
                .child(div().text_color(status_color).child(status))
                .child(format!("+{} −{}", pr.additions, pr.deletions))
                .child(pr.updated_at.format("%b %-d").to_string()),
        )
        // TODO: open an in-app detail view instead of the browser.
        .on_click(move |_, _, cx| cx.open_url(&url))
}
