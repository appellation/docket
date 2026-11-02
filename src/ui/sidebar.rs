//! Left navigation: signed-in user, projects, and PR inbox sections.
//!
//! The selected project scopes the inbox counts and the PR list.

use gpui::{App, Div, Entity, SharedString, Stateful, Window, div, img, prelude::*, px};

use super::theme::Theme;
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

fn heading(label: &'static str, theme: &Theme) -> impl IntoElement {
    div()
        .px_2()
        .pt_2()
        .text_xs()
        .text_color(theme.text_muted)
        .child(label)
}

fn row(id: impl Into<SharedString>, selected: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex()
        .justify_between()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |el| el.bg(theme.surface_hover))
        .hover(|style| style.bg(theme.surface_hover))
}

fn count(n: usize, theme: &Theme) -> impl IntoElement {
    div()
        .flex_none()
        .text_color(theme.text_muted)
        .when(n > 0, |el| el.child(n.to_string()))
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
            row(
                "project-all",
                !in_setup && state.selected_project.is_none(),
                theme,
            )
            .child("All projects")
            .child(count(review_count(None), theme))
            .on_click(move |_, _, cx| {
                entity.update(cx, |state, cx| state.select_project(None, cx));
            })
        };

        let projects = state.projects.iter().map(|project| {
            let entity = self.state.clone();
            let id = project.id;
            let selected = !in_setup && state.selected_project == Some(id);
            let detail = if project.path_missing {
                div().text_color(theme.danger).child("Folder missing")
            } else {
                div()
                    .text_color(theme.text_muted)
                    .child(project.repository.to_string())
            };
            row(format!("project-{}", id.0), selected, theme)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().truncate().child(project.name.clone()))
                        .child(detail.text_xs().truncate()),
                )
                .child(count(review_count(Some(project)), theme))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |state, cx| state.select_project(Some(id), cx));
                })
        });

        let add_project = {
            let entity = self.state.clone();
            row(
                "project-add",
                in_setup && state.setup.as_ref().is_some_and(|s| s.editing.is_none()),
                theme,
            )
            .text_color(theme.text_muted)
            .child("Add project…")
            .on_click(move |_, _, cx| {
                entity.update(cx, |state, cx| state.add_project(cx));
            })
        };

        let inboxes = SectionKind::ALL.into_iter().map(|kind| {
            let entity = self.state.clone();
            let selected = !in_setup && state.selected == kind;
            row(kind.label(), selected, theme)
                .child(kind.label())
                .child(count(state.visible_pull_requests(kind).count(), theme))
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
                    .p_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(img(user.avatar_url.clone()).size_6().rounded_full())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
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
                    .p_2()
                    .gap_1()
                    .child(heading("Projects", theme))
                    .child(all_projects)
                    .children(projects)
                    .child(add_project)
                    .child(heading("Inbox", theme))
                    .children(inboxes),
            )
    }
}
