//! The docket for the tab's selected project (`AppState::docket`): pull requests
//! waiting on your review, then your other pull requests and branches,
//! most recently updated first. With a pull request or branch open, the
//! list moves into a sidebar beside it (`PrSidebar`).

use chrono::Duration;
use gpui::{App, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};

use super::components::{
    Badge, Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconButton, IconName,
    IconSize, ListItem, Spinner, TestSelector as _, Tooltip, display_path, section_heading,
};
use super::sidebar_stack::{rail_header, rail_items};
use super::theme::{Color, Theme};
use crate::github::{PullRequest, PullRequestState, ReviewDecision};
use crate::projects::{Branch, Project, ProjectId};
use crate::state::{AppState, DocketItem, Tab};

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
    tab: Entity<Tab>,
}

impl PrList {
    pub fn new(tab: Entity<Tab>) -> Self {
        Self { tab }
    }
}

impl RenderOnce for PrList {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let tab = self.tab.read(cx);
        let state = tab.app.read(cx);
        let project = tab.selected_project(cx);
        let docket = tab.docket(cx);
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
                pr_row(pr, show_repository, &self.tab, theme).into_any_element()
            }
            DocketItem::Branch {
                project,
                branch,
                pinned,
            } => {
                // The default branch pulls from the project's remote.
                let pull = pinned
                    .then(|| state.base_branch(project.id)?.remote.clone())
                    .flatten()
                    .filter(|_| !project.path_missing)
                    .map(|remote| Pull {
                        remote,
                        pulling: state.is_pulling(project.id),
                        app: tab.app.clone(),
                    });
                branch_row(
                    project,
                    branch,
                    pinned,
                    pull,
                    show_repository,
                    &self.tab,
                    theme,
                )
                .into_any_element()
            }
        };
        let pull_errors: Vec<_> = state
            .projects
            .iter()
            .filter(|p| project.is_none_or(|selected| selected.id == p.id))
            .filter_map(|p| Some((p, state.pull_error(p.id)?.clone())))
            .collect();
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
                let recheck = self.tab.clone();
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
                                    recheck.update(cx, |tab, cx| tab.refresh(cx));
                                }),
                        ),
                )
            }))
            .children(pull_errors.into_iter().map(|(failed, error)| {
                let name = state
                    .base_branch(failed.id)
                    .map_or("the default branch", |base| base.name.as_str());
                let title = if project.is_some() {
                    format!("Couldn't pull {name}")
                } else {
                    format!("Couldn't pull {name} in {}", failed.repository)
                };
                let (retry, dismiss, id) = (tab.app.clone(), tab.app.clone(), failed.id);
                div().px_4().pt_4().pb_2().child(
                    Callout::new(CalloutKind::Error, error)
                        .title(title)
                        .action(
                            Button::new(
                                SharedString::from(format!("retry-pull-{}", id.0)),
                                "Try again",
                            )
                            .icon(IconName::RefreshCw)
                            .size(ButtonSize::Compact)
                            .on_click(move |_, _, cx| {
                                retry.update(cx, |state, cx| state.pull_default_branch(id, cx));
                            }),
                        )
                        .action(
                            Button::new(
                                SharedString::from(format!("dismiss-pull-{}", id.0)),
                                "Dismiss",
                            )
                            .size(ButtonSize::Compact)
                            .style(ButtonStyle::Secondary)
                            .on_click(move |_, _, cx| {
                                dismiss.update(cx, |state, cx| state.dismiss_pull_error(id, cx));
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
                        .map(|pr| pr_row(pr, show_repository, &self.tab, theme)),
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
                    match (project, docket.older) {
                        (_, 1..) => format!("No branches updated in {}.", weeks_back(tab.weeks)),
                        (Some(project), 0) => {
                            format!("Nothing pending in {}.", project.repository)
                        }
                        (None, 0) => "Nothing pending.".to_owned(),
                    },
                    theme,
                ))
            })
            .when(!docket.recent.is_empty(), |el| {
                el.when(grouped, |el| el.child(group(RECENT, docket.recent.len())))
                    .children(docket.recent.iter().map(|item| item_row(*item)))
            })
            .when(docket.older > 0, |el| {
                el.child(show_more(&self.tab, "show-more", cx).py_4())
            });

        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header(project, loading, &self.tab, theme))
            .child(body)
    }
}

/// What the docket is of: the selected project, or all of them. And a
/// refresh button, though the docket follows GitHub by itself.
fn header(
    project: Option<&Project>,
    loading: bool,
    tab: &Entity<Tab>,
    theme: &Theme,
) -> impl IntoElement {
    let refresh = tab.clone();
    let (title, actions) = match project {
        Some(project) => (
            project_title(project, theme).into_any_element(),
            Some(edit_project_button(project, tab)),
        ),
        None => (all_projects_title(theme).into_any_element(), None),
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
        .child(title)
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .when(loading, |el| el.child(Spinner::new("docket-refreshing")))
                .child(
                    IconButton::new("docket-refresh", IconName::RefreshCw)
                        .tooltip("Refresh (⌘R)")
                        .disabled(loading)
                        .on_click(move |_, _, cx| {
                            refresh.update(cx, |tab, cx| tab.refresh(cx));
                        }),
                )
                .children(actions),
        )
}

fn all_projects_title(theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .min_w_0()
        .child(Icon::new(IconName::Layers).size(IconSize::Large))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("All projects"),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(theme.text_muted)
                        .child("Reviews waiting on you, and your work in flight"),
                ),
        )
}

/// The selected project: its name, repository, and folder (which shows in
/// Finder when clicked).
fn project_title(project: &Project, theme: &Theme) -> impl IntoElement {
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
        )
}

/// Opens the project's connection settings.
fn edit_project_button(project: &Project, tab: &Entity<Tab>) -> IconButton {
    let id = project.id;
    let edit = tab.clone();
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
            edit.update(cx, |tab, cx| tab.edit_project(id, cx));
        })
}

/// "the last week", "the last 3 weeks"
fn weeks_back(weeks: u32) -> String {
    match weeks {
        1 => "the last week".to_owned(),
        weeks => format!("the last {weeks} weeks"),
    }
}

/// Shows branches from another week back, under those shown, while there
/// are older ones.
fn show_more(tab: &Entity<Tab>, id: &'static str, cx: &App) -> gpui::Div {
    let theme = Theme::get(cx);
    let back_to = tab.read(cx).since() - Duration::weeks(1);
    let tab = tab.clone();
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .child(
            Button::new(id, "Show more")
                .style(ButtonStyle::Secondary)
                .size(ButtonSize::Compact)
                .on_click(move |_, _, cx| tab.update(cx, |tab, cx| tab.show_more(cx))),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.text_subtle)
                .child(format!("Back to {}", back_to.format("%b %-d"))),
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
    tab: Entity<Tab>,
    collapsed: bool,
}

impl PrSidebar {
    pub fn new(tab: Entity<Tab>, collapsed: bool) -> Self {
        Self { tab, collapsed }
    }
}

/// What clicking an item opens.
enum Open {
    PullRequest(Box<PullRequest>),
    Branch {
        project: ProjectId,
        name: String,
        pinned: bool,
    },
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
        let tab = self.tab.read(cx);
        let state = tab.app.read(cx);
        let project = tab.selected_project(cx);
        let open_pr = tab
            .detail
            .as_ref()
            .and_then(|detail| detail.summary.as_ref())
            .map(|pr| &pr.id);
        let shown_branch = match (&tab.branch, &tab.browser) {
            (Some(open), _) => Some((open.project, Some(open.branch.name.clone()))),
            (None, Some(browser)) => Some(browser.key()),
            (None, None) => None,
        };
        let docket = tab.docket(cx);
        let root = div().size_full().flex().flex_col();

        let row = |item: DocketItem| match item {
            DocketItem::PullRequest(pr) => SidebarRow {
                id: pr.id.clone(),
                icon: state_icon(pr),
                title: pr.title.clone(),
                detail: match (project, &pr.author) {
                    (Some(_), Some(author)) => {
                        format!("#{} · {}", pr.number, author.display_name())
                    }
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
                open: shown_branch.as_ref().is_some_and(|(id, name)| {
                    *id == in_project.id && name.as_ref() == Some(&branch.name)
                }),
            },
        };
        let on_click = |item: DocketItem| {
            let entity = self.tab.clone();
            let target = match item {
                DocketItem::PullRequest(pr) => Open::PullRequest(Box::new(pr.clone())),
                DocketItem::Branch {
                    project,
                    branch,
                    pinned,
                } => Open::Branch {
                    project: project.id,
                    name: branch.name.clone(),
                    pinned,
                },
            };
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |tab, cx| match &target {
                    Open::PullRequest(pr) => tab.open_pull_request(pr, cx),
                    Open::Branch {
                        project,
                        name,
                        pinned,
                    } => open_branch(tab, *project, name, *pinned, cx),
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
                })
                .when(docket.older > 0, |el| {
                    el.child(show_more(&self.tab, "pr-sidebar-show-more", cx).py_2())
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
    tab: &Entity<Tab>,
    theme: &Theme,
) -> impl IntoElement {
    let (open, tab) = (pr.clone(), tab.clone());
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
                        .children(pr.author.as_ref().map(|a| a.display_name().to_owned()))
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
            tab.update(cx, |tab, cx| tab.open_pull_request(&open, cx));
        })
}

/// Pulling the default branch from its remote, from its row.
struct Pull {
    remote: String,
    pulling: bool,
    app: Entity<AppState>,
}

/// A pinned default branch's pull button's test selector and element id.
pub fn pull_id(project: ProjectId) -> String {
    format!("pull-{}", project.0)
}

fn branch_row(
    project: &Project,
    branch: &Branch,
    pinned: bool,
    pull: Option<Pull>,
    show_repository: bool,
    tab: &Entity<Tab>,
    theme: &Theme,
) -> impl IntoElement {
    let id = branch_id(project.id, &branch.name);
    let (project_id, name, tab) = (project.id, branch.name.clone(), tab.clone());
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
                })
                .when_some(pull, |el, pull| {
                    let (project, selector, app) = (project.id, pull_id(project.id), pull.app);
                    el.child(if pull.pulling {
                        div()
                            .test_selector(|| format!("{selector}-pulling"))
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Spinner::new(format!("{selector}-spinner")))
                            .into_any_element()
                    } else {
                        IconButton::new(SharedString::from(selector), IconName::ArrowDownToLine)
                            .size(ButtonSize::Compact)
                            .tooltip(format!("Pull from {} and rebase", pull.remote))
                            .on_click(move |_, _, cx| {
                                // Not a click on the row, which browses it.
                                cx.stop_propagation();
                                app.update(cx, |app, cx| app.pull_default_branch(project, cx));
                            })
                            .into_any_element()
                    })
                }),
        )
        .on_click(move |_, _, cx| {
            tab.update(cx, |tab, cx| {
                open_branch(tab, project_id, &name, pinned, cx)
            });
        })
}

/// Opens a branch from the docket: the default branch's files, or another
/// branch's changes from it.
fn open_branch(tab: &mut Tab, project: ProjectId, name: &str, pinned: bool, cx: &mut Context<Tab>) {
    if pinned {
        tab.browse_branch(project, name, cx);
    } else {
        tab.open_branch(project, name, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use gpui::TestAppContext;

    use super::pull_id;
    use crate::github::FakeGithub;
    use crate::projects::ProjectId;
    use crate::test_support::{
        VisualTestContextExt as _, git, git_checkout, git_commit, open_docket, project,
    };

    /// A clone of a checkout made by `git_checkout`, at `dir/local`, whose
    /// `origin` reads as `acme/widgets` on GitHub but is `origin_path`.
    fn clone_as_github(dir: &Path, origin_path: &Path) -> std::path::PathBuf {
        let local = dir.join("local");
        git(dir, &["clone", "--quiet", "origin", "local"]);
        git(&local, &["config", "user.email", "you@example.com"]);
        let url = "git@github.com:acme/widgets.git";
        git(&local, &["remote", "set-url", "origin", url]);
        let rewrite = format!("url.{}.insteadOf", origin_path.display());
        git(&local, &["config", &rewrite, url]);
        local
    }

    #[gpui::test]
    fn pulls_the_default_branch_from_its_row(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        git_checkout(&origin, &[("README.md", "# Widgets\n")]);
        let local = clone_as_github(dir.path(), &origin);
        std::fs::write(origin.join("README.md"), "# Widgets!\n").unwrap();
        git_commit(&origin, "Upstream work", "2026-01-02T00:00:00Z");

        let github = Arc::new(FakeGithub::new());
        let (tab, cx) = open_docket(cx, github, vec![project(1, &local, "acme/widgets")]);
        let subject = |cx: &mut gpui::VisualTestContext| {
            tab.read_with(cx, |tab, cx| {
                let state = tab.app.read(cx);
                state.branch(ProjectId(1), "main").unwrap().subject.clone()
            })
        };
        assert_eq!(subject(cx), "Add files");

        cx.click_on(&pull_id(ProjectId(1)));
        cx.run_until_parked();
        assert_eq!(subject(cx), "Upstream work");
        assert_eq!(
            std::fs::read_to_string(local.join("README.md")).unwrap(),
            "# Widgets!\n"
        );
        assert!(
            tab.read_with(cx, |tab, _| tab.browser.is_none()),
            "pulling doesn't browse the branch"
        );
    }

    #[gpui::test]
    fn says_why_a_pull_failed(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        git_checkout(&origin, &[("README.md", "# Widgets\n")]);
        let local = clone_as_github(dir.path(), &dir.path().join("gone"));

        let github = Arc::new(FakeGithub::new());
        let (_, cx) = open_docket(cx, github, vec![project(1, &local, "acme/widgets")]);
        cx.click_on(&pull_id(ProjectId(1)));
        cx.run_until_parked();
        assert!(cx.is_drawn("dismiss-pull-1"));
        assert!(cx.is_drawn(&pull_id(ProjectId(1))), "it can be tried again");

        cx.click_on("dismiss-pull-1");
        assert!(!cx.is_drawn("dismiss-pull-1"));
    }
}
