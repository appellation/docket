//! List of pull requests for the selected sidebar section, scoped to the
//! selected project.

use gpui::{App, Entity, FontWeight, PromptLevel, Window, div, prelude::*, px};

use super::components::{
    Badge, Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconButton, IconName,
    IconSize, Spinner, display_path,
};
use super::theme::{Color, Theme};
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

        let header = {
            let refresh = self.state.clone();
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .h(px(48.))
                .px_4()
                .border_b_1()
                .border_color(theme.border_subtle)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(state.selected.label()),
                        )
                        .when(!pull_requests.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_color(theme.text_subtle)
                                    .child(pull_requests.len().to_string()),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(section.loading, |el| {
                            el.child(Spinner::new("pr-list-loading"))
                        })
                        .child(
                            IconButton::new("refresh", IconName::RefreshCw)
                                .tooltip("Refresh (⌘R)")
                                .disabled(section.loading)
                                .on_click(move |_, _, cx| {
                                    refresh.update(cx, |state, cx| state.refresh(cx));
                                }),
                        ),
                )
        };

        // GitHub search leaves out repositories this sign-in can't reach,
        // so say which projects that hides instead of looking empty.
        let access_issues: Vec<_> = state
            .projects
            .iter()
            .filter(|p| project.is_none_or(|selected| selected.id == p.id))
            .filter_map(|p| Some((p, state.project_access_issue(p.id)?)))
            .collect();
        let empty = !section.loading
            && section.error.is_none()
            && access_issues.is_empty()
            && pull_requests.is_empty();
        let body = div()
            .id("pr-list")
            .flex_1()
            .overflow_y_scroll()
            .children(access_issues.into_iter().map(|(hidden, issue)| {
                let title = if project.is_some() {
                    "GitHub is hiding this project's pull requests".to_owned()
                } else {
                    format!("GitHub is hiding pull requests in {}", hidden.repository)
                };
                let recheck = self.state.clone();
                div().px_4().pt_4().pb_2().child(
                    Callout::new(CalloutKind::Warning, issue.message.clone())
                        .title(title)
                        .when_some(issue.fix.as_ref(), |callout, fix| {
                            let url = fix.url.clone();
                            callout.action(
                                Button::new(("fix-access", hidden.id.0), fix.label)
                                    .icon(IconName::ExternalLink)
                                    .size(ButtonSize::Compact)
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                        })
                        .action(
                            Button::new(("recheck-access", hidden.id.0), "Check again")
                                .icon(IconName::RefreshCw)
                                .size(ButtonSize::Compact)
                                .style(ButtonStyle::Secondary)
                                .on_click(move |_, _, cx| {
                                    recheck.update(cx, |state, cx| state.refresh(cx));
                                }),
                        ),
                )
            }))
            .children(section.error.clone().map(|err| {
                div().p_4().child(
                    Callout::new(CalloutKind::Error, err).title("Couldn't load pull requests"),
                )
            }))
            .when(empty, |el| {
                el.child(empty_state(
                    match project {
                        Some(project) => format!("Nothing here for {}.", project.repository),
                        None => "Nothing here.".to_owned(),
                    },
                    theme,
                ))
            })
            .children(
                pull_requests
                    .into_iter()
                    .map(|pr| pr_row(pr, project.is_none(), &self.state, theme)),
            );

        div()
            .flex_1()
            .min_w_0()
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
    let browse = state.clone();
    let edit = state.clone();
    let remove = state.clone();
    let remove_message = format!("Remove {} from Docket?", project.name);

    let location = if project.path_missing {
        div()
            .flex()
            .items_center()
            .gap_1()
            .min_w_0()
            .text_color(theme.danger)
            .child(
                Icon::new(IconName::TriangleAlert)
                    .size(IconSize::XSmall)
                    .color(Color::Danger),
            )
            .child(
                div()
                    .truncate()
                    .child(format!("Folder not found: {}", display_path(&project.path))),
            )
    } else {
        div()
            .truncate()
            .text_color(theme.text_subtle)
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
        .border_color(theme.border_subtle)
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_w_0()
                .child(Icon::new(IconName::FolderGit).size(IconSize::Large))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(project.name.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .min_w_0()
                                .text_xs()
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(theme.text_muted)
                                        .child(project.repository.to_string()),
                                )
                                .child(location),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .gap_2()
                .when(!project.path_missing, |el| {
                    el.child(
                        Button::new("browse-project", "Browse Files")
                            .icon(IconName::FolderTree)
                            .style(ButtonStyle::Secondary)
                            .on_click(move |_, _, cx| {
                                browse.update(cx, |state, cx| state.browse_project(id, cx));
                            }),
                    )
                    .child(
                        Button::new("reveal-project", "Show in Finder")
                            .icon(IconName::FolderOpen)
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
                    .icon(IconName::Settings)
                    .style(ButtonStyle::Secondary)
                    .on_click(move |_, _, cx| {
                        edit.update(cx, |state, cx| state.edit_project(id, cx));
                    }),
                )
                .child(
                    Button::new("remove-project", "Remove")
                        .icon(IconName::Trash)
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

fn empty_state(message: String, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .pt(px(96.))
        .text_color(theme.text_muted)
        .child(
            Icon::new(IconName::CircleCheck)
                .size(IconSize::Large)
                .color(Color::Subtle),
        )
        .child(message)
}

fn pr_row(
    pr: &PullRequest,
    show_repository: bool,
    state: &Entity<AppState>,
    theme: &Theme,
) -> impl IntoElement {
    let (open, state) = (pr.clone(), state.clone());
    let (state_icon, state_color) = match (pr.state, pr.is_draft) {
        (PullRequestState::Merged, _) => (IconName::GitMerge, Color::Merged),
        (PullRequestState::Closed, _) => (IconName::GitPullRequestClosed, Color::Danger),
        (_, true) => (IconName::GitPullRequestDraft, Color::Muted),
        (_, false) => (IconName::GitPullRequest, Color::Success),
    };
    let status = match (pr.state, pr.is_draft, pr.review_decision) {
        (PullRequestState::Merged, ..) => Badge::new("Merged").color(Color::Merged),
        (PullRequestState::Closed, ..) => Badge::new("Closed").color(Color::Danger),
        (_, true, _) => Badge::new("Draft"),
        (_, _, Some(ReviewDecision::Approved)) => Badge::new("Approved")
            .icon(IconName::CircleCheck)
            .color(Color::Success),
        (_, _, Some(ReviewDecision::ChangesRequested)) => Badge::new("Changes requested")
            .icon(IconName::FileDiff)
            .color(Color::Danger),
        (_, _, Some(ReviewDecision::ReviewRequired)) | (_, _, None) => {
            Badge::new("Review required").icon(IconName::Eye)
        }
    };

    div()
        .id(gpui::SharedString::from(pr.id.clone()))
        .flex()
        .items_start()
        .gap_3()
        .px_4()
        .py_2p5()
        .border_b_1()
        .border_color(theme.border_subtle)
        .cursor_pointer()
        .hover(|style| style.bg(theme.element_hover))
        .child(
            div().h(px(20.)).flex().items_center().child(
                Icon::new(state_icon)
                    .size(IconSize::Medium)
                    .color(state_color),
            ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(pr.title.clone()),
                )
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
                        .child(
                            div()
                                .text_color(theme.text_subtle)
                                .child(pr.updated_at.format("%b %-d").to_string()),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_3()
                .h(px(20.))
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .text_xs()
                        .child(
                            div()
                                .text_color(theme.success)
                                .child(format!("+{}", pr.additions)),
                        )
                        .child(
                            div()
                                .text_color(theme.danger)
                                .child(format!("−{}", pr.deletions)),
                        ),
                )
                .child(status),
        )
        .on_click(move |_, _, cx| {
            state.update(cx, |state, cx| state.open_pull_request(&open, cx));
        })
}
