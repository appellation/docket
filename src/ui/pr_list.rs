//! The docket for the selected project (`AppState::docket`): pull requests
//! waiting on your review, then your other pull requests and branches,
//! most recently updated first. With a pull request or branch open, the
//! list moves into a sidebar beside it (`PrSidebar`).

use gpui::{App, Entity, FontWeight, SharedString, Window, div, prelude::*, px};

use super::components::{
    Badge, Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconButton, IconName,
    IconSize, ListItem, Spinner, TestSelector as _, Tooltip, display_path, section_heading,
};
use super::sidebar_stack::{rail_header, rail_items};
use super::theme::{Color, Theme};
use crate::github::{PullRequest, PullRequestState, ReviewDecision};
use crate::projects::{Branch, Project, ProjectId};
use crate::state::{AppState, DocketItem};

/// The heading over pull requests waiting on your review.
const REVIEW_REQUESTED: &str = "Review requested";
/// The heading over everything else.
const RECENT: &str = "Recently updated";

/// A branch row's test selector and element id.
pub fn branch_id(project: ProjectId, branch: &str) -> String {
    format!("branch-{}-{branch}", project.0)
}

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
        let project = state.selected_project();
        let docket = state.docket();
        let errors = state.docket_errors();

        // GitHub search leaves out repositories this sign-in can't reach,
        // so say which projects that hides instead of looking empty.
        let access_issues: Vec<_> = state
            .projects
            .iter()
            .filter(|p| project.is_none_or(|selected| selected.id == p.id))
            .filter_map(|p| Some((p, state.project_access_issue(p.id)?)))
            .collect();
        let loading = state.is_loading_docket();
        let empty = errors.is_empty() && access_issues.is_empty() && docket.is_empty();
        let show_repository = project.is_none();
        // With one group, a heading would only repeat what the rows say.
        let grouped = docket.is_grouped();
        let group = |label: &'static str, count: usize| {
            div()
                .test_selector(|| label.into())
                .flex()
                .gap_2()
                .px_4()
                .pt_4()
                .pb_1p5()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text_subtle)
                .border_b_1()
                .border_color(theme.border_subtle)
                .child(label)
                .child(div().text_color(theme.text_muted).child(count.to_string()))
        };
        let item_row = |item: DocketItem| match item {
            DocketItem::PullRequest(pr) => {
                pr_row(pr, show_repository, &self.state, theme).into_any_element()
            }
            DocketItem::Branch {
                project,
                branch,
                pinned,
            } => branch_row(project, branch, pinned, show_repository, &self.state, theme)
                .into_any_element(),
        };
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
            .children(errors.into_iter().map(|err| {
                div().p_4().child(
                    Callout::new(CalloutKind::Error, err).title("Couldn't load pull requests"),
                )
            }))
            .when(!docket.review_requested.is_empty(), |el| {
                el.when(grouped, |el| {
                    el.child(group(REVIEW_REQUESTED, docket.review_requested.len()))
                })
                .children(
                    docket
                        .review_requested
                        .iter()
                        .map(|pr| pr_row(pr, show_repository, &self.state, theme)),
                )
            })
            .children(docket.pinned.iter().map(|item| item_row(*item)))
            .when(empty && loading, |el| {
                el.child(
                    div()
                        .flex()
                        .justify_center()
                        .items_center()
                        .gap_2()
                        .pt(px(96.))
                        .text_color(theme.text_muted)
                        .child(Spinner::new("pr-list-loading"))
                        .child("Loading pull requests…"),
                )
            })
            .when(empty && !loading, |el| {
                el.child(empty_state(
                    match project {
                        Some(project) => format!("Nothing pending in {}.", project.repository),
                        None => "Nothing pending.".to_owned(),
                    },
                    theme,
                ))
            })
            .when(!docket.recent.is_empty(), |el| {
                el.when(grouped, |el| el.child(group(RECENT, docket.recent.len())))
                    .children(docket.recent.iter().map(|item| item_row(*item)))
            });

        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .children(project.map(|project| project_header(project, &self.state, theme)))
            .child(body)
    }
}

/// The selected project: its name, repository, and folder (which shows in
/// Finder when clicked), and its connection settings.
fn project_header(project: &Project, state: &Entity<AppState>, theme: &Theme) -> impl IntoElement {
    let id = project.id;
    let edit = state.clone();

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
            .into_any_element()
    } else {
        let path = project.path.clone();
        let hover = theme.text_muted;
        div()
            .test_selector(|| "reveal-project".into())
            .id("reveal-project")
            .truncate()
            .cursor_pointer()
            .text_color(theme.text_subtle)
            .hover(move |style| style.text_color(hover).underline())
            .tooltip(|_, cx| Tooltip::view("Show in Finder".into(), cx))
            .on_click(move |_, _, cx| cx.reveal_path(&path))
            .child(display_path(&project.path))
            .into_any_element()
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
            IconButton::new("edit-project", IconName::Settings)
                .when(project.path_missing, |button| {
                    button.icon_color(Color::Danger)
                })
                .tooltip(if project.path_missing {
                    "Locate folder…"
                } else {
                    "Connection…"
                })
                .on_click(move |_, _, cx| {
                    edit.update(cx, |state, cx| state.edit_project(id, cx));
                }),
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

/// The docket beside the open pull request or branch: a compact row for
/// each, or collapsed, a rail of their icons.
#[derive(IntoElement)]
pub struct PrSidebar {
    state: Entity<AppState>,
    collapsed: bool,
}

impl PrSidebar {
    pub fn new(state: Entity<AppState>, collapsed: bool) -> Self {
        Self { state, collapsed }
    }
}

/// What clicking an item opens.
enum Open {
    PullRequest(Box<PullRequest>),
    Branch(ProjectId, String),
}

/// How an item shows in the sidebar: its id, icon, title, and detail line.
struct SidebarRow {
    id: String,
    icon: (IconName, Color),
    title: String,
    detail: String,
    open: bool,
}

impl RenderOnce for PrSidebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let state = self.state.read(cx);
        let project = state.selected_project();
        let open_pr = state.detail.as_ref().map(|detail| &detail.summary.id);
        let open_branch = state.browser.as_ref().map(|browser| browser.key());
        let docket = state.docket();
        let root = div().size_full().flex().flex_col();

        let row = |item: DocketItem| match item {
            DocketItem::PullRequest(pr) => SidebarRow {
                id: pr.id.clone(),
                icon: state_icon(pr),
                title: pr.title.clone(),
                detail: match (project, &pr.author) {
                    (Some(_), Some(author)) => format!("#{} · {author}", pr.number),
                    (Some(_), None) => format!("#{}", pr.number),
                    (None, _) => format!("{}#{}", pr.repository, pr.number),
                },
                open: open_pr == Some(&pr.id),
            },
            DocketItem::Branch {
                project: in_project,
                branch,
                pinned,
            } => SidebarRow {
                id: branch_id(in_project.id, &branch.name),
                icon: (IconName::GitBranch, Color::Muted),
                title: branch.name.clone(),
                detail: match (project, pinned) {
                    (Some(_), true) => "Default branch".to_owned(),
                    (Some(_), false) => branch.subject.clone(),
                    (None, _) => format!("{} · {}", in_project.repository, branch.subject),
                },
                open: open_branch
                    .as_ref()
                    .is_some_and(|(id, name)| *id == in_project.id && *name == branch.name),
            },
        };
        let on_click = |item: DocketItem| {
            let entity = self.state.clone();
            let target = match item {
                DocketItem::PullRequest(pr) => Open::PullRequest(Box::new(pr.clone())),
                DocketItem::Branch {
                    project, branch, ..
                } => Open::Branch(project.id, branch.name.clone()),
            };
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |state, cx| match &target {
                    Open::PullRequest(pr) => state.open_pull_request(pr, cx),
                    Open::Branch(id, name) => state.browse_branch(*id, name, cx),
                });
            }
        };

        if self.collapsed {
            let icons = docket.items().map(|item| {
                let SidebarRow {
                    id,
                    icon,
                    title,
                    open,
                    ..
                } = row(item);
                IconButton::new(SharedString::from(format!("rail-{id}")), icon.0)
                    .icon_color(icon.1)
                    .tooltip(title)
                    .selected(open)
                    .on_click(on_click(item))
            });
            return root
                .child(rail_header(
                    Icon::new(IconName::Inbox).size(IconSize::Medium),
                    cx,
                ))
                .child(rail_items("pr-rail").children(icons));
        }

        let grouped = docket.is_grouped();
        let list_row = |item: DocketItem| {
            let SidebarRow {
                id,
                icon,
                title,
                detail,
                open,
            } = row(item);
            ListItem::new(SharedString::from(id))
                .icon_color(icon.0, icon.1)
                .selected(open)
                .child(div().truncate().child(title))
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(theme.text_muted)
                        .child(detail),
                )
                .on_click(on_click(item))
        };

        root.child(
            div()
                .id("pr-sidebar-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_px()
                .p_2()
                // Headings bring their own space above.
                .when(grouped, |el| el.pt_0())
                .children(state.docket_errors().into_iter().map(|err| {
                    Callout::new(CalloutKind::Error, err).title("Couldn't load pull requests")
                }))
                .when(!docket.review_requested.is_empty(), |el| {
                    el.when(grouped, |el| {
                        el.child(section_heading(REVIEW_REQUESTED, cx))
                    })
                    .children(
                        docket
                            .review_requested
                            .iter()
                            .map(|pr| list_row(DocketItem::PullRequest(pr))),
                    )
                })
                .children(docket.pinned.iter().map(|item| list_row(*item)))
                .when(!docket.recent.is_empty(), |el| {
                    el.when(grouped, |el| el.child(section_heading(RECENT, cx)))
                        .children(docket.recent.iter().map(|item| list_row(*item)))
                }),
        )
    }
}

fn state_icon(pr: &PullRequest) -> (IconName, Color) {
    match (pr.state, pr.is_draft) {
        (PullRequestState::Merged, _) => (IconName::GitMerge, Color::Merged),
        (PullRequestState::Closed, _) => (IconName::GitPullRequestClosed, Color::Danger),
        (_, true) => (IconName::GitPullRequestDraft, Color::Muted),
        (_, false) => (IconName::GitPullRequest, Color::Success),
    }
}

fn pr_row(
    pr: &PullRequest,
    show_repository: bool,
    state: &Entity<AppState>,
    theme: &Theme,
) -> impl IntoElement {
    let (open, state) = (pr.clone(), state.clone());
    let (state_icon, state_color) = state_icon(pr);
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
        .test_selector(|| pr.id.clone())
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

fn branch_row(
    project: &Project,
    branch: &Branch,
    pinned: bool,
    show_repository: bool,
    state: &Entity<AppState>,
    theme: &Theme,
) -> impl IntoElement {
    let id = branch_id(project.id, &branch.name);
    let (project_id, name, state) = (project.id, branch.name.clone(), state.clone());
    div()
        .test_selector(|| id.clone())
        .id(SharedString::from(id.clone()))
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
                Icon::new(IconName::GitBranch)
                    .size(IconSize::Medium)
                    .color(Color::Muted),
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
                        .child(branch.name.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.text_muted)
                        .when(show_repository, |el| {
                            el.child(div().flex_none().child(project.repository.to_string()))
                        })
                        .child(div().truncate().child(branch.subject.clone()))
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.text_subtle)
                                .child(branch.updated_at.format("%b %-d").to_string()),
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
                .when(branch.worktree.is_some(), |el| {
                    el.child(Badge::new("Checked out").icon(IconName::FolderOpen))
                })
                .when(pinned, |el| {
                    el.child(Badge::new("Default branch").icon(IconName::Pin))
                })
                .when(!pinned && branch.worktree.is_none(), |el| {
                    el.child(Badge::new("No pull request"))
                }),
        )
        .on_click(move |_, _, cx| {
            state.update(cx, |state, cx| state.browse_branch(project_id, &name, cx));
        })
}
