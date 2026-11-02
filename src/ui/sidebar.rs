//! Left navigation: signed-in user, projects, and PR inbox sections.
//!
//! The selected project scopes the inbox counts and the PR list.

use gpui::{App, Entity, FontWeight, Window, div, img, prelude::*, px};

use super::components::{IconName, ListItem, count_badge, section_heading};
use super::theme::{Color, Theme};
use crate::auth::CredentialSource;
use crate::projects::Project;
use crate::state::{AppState, AuthStatus, SectionKind};

#[derive(IntoElement)]
pub struct Sidebar {
    state: Entity<AppState>,
}

impl Sidebar {
    pub fn new(state: Entity<AppState>) -> Self {
        Self { state }
    }
}

fn inbox_icon(kind: SectionKind) -> IconName {
    match kind {
        SectionKind::ReviewRequested => IconName::Inbox,
        SectionKind::Authored => IconName::GitPullRequest,
        SectionKind::Involved => IconName::Users,
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let state = self.state.read(cx);

        let user = match &state.auth {
            AuthStatus::SignedIn { user, source, .. } => Some((user, *source)),
            _ => None,
        };
        // While setting up a project, nothing in the sidebar is "current".
        let in_setup = state.setup.is_some();

        // Projects are badged with what's waiting on you: review requests.
        let review_count = |project: Option<&Project>| {
            state
                .pull_requests_in(SectionKind::ReviewRequested, project)
                .count()
        };

        let all_projects = {
            let entity = self.state.clone();
            let selected = !in_setup && state.selected_project.is_none();
            ListItem::new("project-all")
                .icon(IconName::Layers)
                .selected(selected)
                .child("All projects")
                .end_slot(count_badge(review_count(None), selected, cx))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |state, cx| state.select_project(None, cx));
                })
        };

        let projects = state.projects.iter().map(|project| {
            let entity = self.state.clone();
            let id = project.id;
            let selected = !in_setup && state.selected_project == Some(id);
            let (item, detail) = if project.path_missing {
                (
                    ListItem::new(("project", id.0))
                        .icon_color(IconName::TriangleAlert, Color::Danger),
                    div().text_color(theme.danger).child("Folder missing"),
                )
            } else if state.project_access_issue(id).is_some() {
                (
                    ListItem::new(("project", id.0))
                        .icon_color(IconName::TriangleAlert, Color::Warning),
                    div()
                        .text_color(theme.warning)
                        .child("Pull requests hidden"),
                )
            } else {
                (
                    ListItem::new(("project", id.0)).icon(IconName::FolderGit),
                    div()
                        .text_color(theme.text_muted)
                        .child(project.repository.to_string()),
                )
            };
            item.selected(selected)
                .child(div().truncate().child(project.name.clone()))
                .child(detail.text_xs().truncate())
                .end_slot(count_badge(review_count(Some(project)), selected, cx))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |state, cx| state.select_project(Some(id), cx));
                })
        });

        let add_project = {
            let entity = self.state.clone();
            ListItem::new("project-add")
                .icon(IconName::Plus)
                .selected(in_setup && state.setup.as_ref().is_some_and(|s| s.editing.is_none()))
                .child(div().text_color(theme.text_muted).child("Add project…"))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |state, cx| state.add_project(cx));
                })
        };

        let inboxes = SectionKind::ALL.into_iter().map(|kind| {
            let entity = self.state.clone();
            let selected = !in_setup && state.selected == kind;
            ListItem::new(kind.label())
                .icon(inbox_icon(kind))
                .selected(selected)
                .child(kind.label())
                .end_slot(count_badge(
                    state.visible_pull_requests(kind).count(),
                    selected,
                    cx,
                ))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |state, cx| state.select(kind, cx));
                })
        });

        div()
            .w(px(240.))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.border)
            .children(user.map(|(user, source)| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(52.))
                    .px_3()
                    .border_b_1()
                    .border_color(theme.border_subtle)
                    .child(
                        img(user.avatar_url.clone())
                            .flex_none()
                            .size_6()
                            .rounded_full()
                            .bg(theme.element_selected),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(user.name.clone().unwrap_or_else(|| user.login.clone())),
                            )
                            // Only call out the non-default sign-in methods.
                            .when(source != CredentialSource::OAuth, |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.text_muted)
                                        .truncate()
                                        .child(format!("via {}", source.label())),
                                )
                            }),
                    )
            }))
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .px_2()
                    .pb_2()
                    .gap_px()
                    .child(section_heading("Projects", cx))
                    .child(all_projects)
                    .children(projects)
                    .child(add_project)
                    .child(section_heading("Inbox", cx))
                    .children(inboxes),
            )
    }
}
