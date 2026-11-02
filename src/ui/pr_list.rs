//! List of pull requests for the selected sidebar section, scoped to the
//! selected project.

use gpui::{App, Entity, Hsla, PromptLevel, Window, div, prelude::*};

use super::components::{Button, ButtonStyle, display_path};
use super::theme::Theme;
use crate::github::{PullRequest, PullRequestState, ReviewDecision};
use crate::projects::Project;
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
        let project = state.selected_project();
        let pull_requests: Vec<_> = state.visible_pull_requests(state.selected).collect();

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
                !section.loading && section.error.is_none() && pull_requests.is_empty(),
                |el| {
                    el.child(
                        div()
                            .p_4()
                            .text_color(theme.text_muted)
                            .child(match project {
                                Some(project) => {
                                    format!("Nothing here for {}.", project.repository)
                                }
                                None => "Nothing here.".to_owned(),
                            }),
                    )
                },
            )
            .children(
                pull_requests
                    .into_iter()
                    .map(|pr| pr_row(pr, project.is_none(), theme)),
            );

        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .children(project.map(|project| project_header(project, &self.state, theme)))
            .child(header)
            .child(body)
    }
}

fn project_header(project: &Project, state: &Entity<AppState>, theme: &Theme) -> impl IntoElement {
    let id = project.id;
    let path = project.path.clone();
    let edit = state.clone();
    let remove = state.clone();
    let remove_message = format!("Remove {} from Docket?", project.name);

    let location = if project.path_missing {
        div()
            .text_color(theme.danger)
            .child(format!("Folder not found: {}", display_path(&project.path)))
    } else {
        div()
            .text_color(theme.text_muted)
            .child(display_path(&project.path))
    };

    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_3()
        .bg(theme.surface)
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .gap_1()
                .child(div().text_lg().child(project.name.clone()))
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .text_xs()
                        .child(project.repository.to_string())
                        .child(location.truncate()),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .gap_2()
                .when(!project.path_missing, |el| {
                    el.child(
                        Button::new("reveal-project", "Show in Finder")
                            .style(ButtonStyle::Secondary)
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    )
                })
                .child(
                    Button::new(
                        "edit-project",
                        if project.path_missing {
                            "Locate…"
                        } else {
                            "Connection…"
                        },
                    )
                    .style(ButtonStyle::Secondary)
                    .on_click(move |_, _, cx| {
                        edit.update(cx, |state, cx| state.edit_project(id, cx));
                    }),
                )
                .child(
                    Button::new("remove-project", "Remove")
                        .style(ButtonStyle::Danger)
                        .on_click(move |_, window, cx| {
                            let answer = window.prompt(
                                PromptLevel::Warning,
                                &remove_message,
                                Some("Your local checkout isn't affected."),
                                &["Remove", "Cancel"],
                                cx,
                            );
                            let remove = remove.clone();
                            cx.spawn(async move |cx| {
                                if answer.await == Ok(0) {
                                    remove
                                        .update(cx, |state, cx| state.remove_project(id, cx))
                                        .ok();
                                }
                            })
                            .detach();
                        }),
                ),
        )
}

fn pr_row(pr: &PullRequest, show_repository: bool, theme: &Theme) -> impl IntoElement {
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
                .child(if show_repository {
                    format!("{}#{}", pr.repository, pr.number)
                } else {
                    format!("#{}", pr.number)
                })
                .children(pr.author.clone())
                .child(div().text_color(status_color).child(status))
                .child(format!("+{} −{}", pr.additions, pr.deletions))
                .child(pr.updated_at.format("%b %-d").to_string()),
        )
        // TODO: open an in-app detail view instead of the browser.
        .on_click(move |_, _, cx| cx.open_url(&url))
}
