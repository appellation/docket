//! Left navigation: the signed-in user and projects. It's the top of the
//! sidebar stack (`sidebar_stack`), so once something deeper is open it
//! collapses to a rail of their icons.
//!
//! The selected project scopes the docket (`pr_list`). Each is badged with
//! its review requests.

use gpui::{App, Entity, FontWeight, Img, Window, div, img, prelude::*};

use super::components::{IconButton, IconName, ListItem, count_badge, section_heading};
use super::sidebar_stack::{rail_header, rail_items, sidebar_header};
use super::theme::{Color, Theme};
use crate::auth::CredentialSource;
use crate::github::User;
use crate::projects::Project;
use crate::state::{AppState, AuthStatus, SectionKind, Tab};

#[derive(IntoElement)]
pub struct Sidebar {
    tab: Entity<Tab>,
    collapsed: bool,
}

impl Sidebar {
    pub fn new(tab: Entity<Tab>, collapsed: bool) -> Self {
        Self { tab, collapsed }
    }
}

/// A project's icon: its folder, or what's wrong with it.
fn project_icon(project: &Project, state: &AppState) -> (IconName, Color) {
    if project.path_missing {
        (IconName::TriangleAlert, Color::Danger)
    } else if state.project_access_issue(project.id).is_some() {
        (IconName::TriangleAlert, Color::Warning)
    } else {
        (IconName::FolderGit, Color::Muted)
    }
}

fn avatar(user: &User, cx: &App) -> Img {
    img(user.avatar_url.clone())
        .flex_none()
        .size_6()
        .rounded_full()
        .bg(Theme::get(cx).element_selected)
}

impl RenderOnce for Sidebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.collapsed {
            render_rail(&self.tab, cx).into_any_element()
        } else {
            render_expanded(&self.tab, cx).into_any_element()
        }
    }
}

fn render_expanded(entity: &Entity<Tab>, cx: &App) -> impl IntoElement {
    let theme = Theme::get(cx);
    let tab = entity.read(cx);
    let state = tab.app.read(cx);

    let user = match &state.auth {
        AuthStatus::SignedIn { user, source, .. } => Some((user, *source)),
        _ => None,
    };
    // While setting up a project, nothing in the sidebar is "current".
    let in_setup = tab.setup.is_some();

    // Projects are badged with what's waiting on you: review requests.
    let review_count = |project: Option<&Project>| {
        state
            .pull_requests_in(SectionKind::ReviewRequested, project)
            .count()
    };

    let all_projects = {
        let entity = entity.clone();
        let selected = !in_setup && tab.selected_project.is_none();
        ListItem::new("project-all")
            .icon(IconName::Layers)
            .selected(selected)
            .child("All projects")
            .end_slot(count_badge(review_count(None), selected, cx))
            .on_click(move |_, _, cx| {
                entity.update(cx, |tab, cx| tab.select_project(None, cx));
            })
    };

    let projects = state.projects.iter().map(|project| {
        let entity = entity.clone();
        let id = project.id;
        let selected = !in_setup && tab.selected_project == Some(id);
        let (icon, color) = project_icon(project, state);
        let detail = match color {
            Color::Danger => div().text_color(theme.danger).child("Folder missing"),
            Color::Warning => div()
                .text_color(theme.warning)
                .child("Pull requests hidden"),
            _ => div()
                .text_color(theme.text_muted)
                .child(project.repository.to_string()),
        };
        ListItem::new(("project", id.0))
            .icon_color(icon, color)
            .selected(selected)
            .child(div().truncate().child(project.name.clone()))
            .child(detail.text_xs().truncate())
            .end_slot(count_badge(review_count(Some(project)), selected, cx))
            .on_click(move |_, _, cx| {
                entity.update(cx, |tab, cx| tab.select_project(Some(id), cx));
            })
    });

    let add_project = {
        let entity = entity.clone();
        ListItem::new("project-add")
            .icon(IconName::Plus)
            .selected(in_setup && tab.setup.as_ref().is_some_and(|s| s.editing.is_none()))
            .child(div().text_color(theme.text_muted).child("Add project…"))
            .on_click(move |_, _, cx| {
                entity.update(cx, |tab, cx| tab.add_project(cx));
            })
    };

    div()
        .size_full()
        .flex()
        .flex_col()
        .children(user.map(|(user, source)| {
            sidebar_header(cx)
                .gap_2()
                .px_3()
                .child(avatar(user, cx))
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
                .child(add_project),
        )
}

/// The same rows as icons. Each does what its row does, though resting on
/// the rail expands it first.
fn render_rail(entity: &Entity<Tab>, cx: &App) -> impl IntoElement {
    let tab = entity.read(cx);
    let state = tab.app.read(cx);
    let in_setup = tab.setup.is_some();

    let all_projects = {
        let entity = entity.clone();
        IconButton::new("rail-project-all", IconName::Layers)
            .tooltip("All projects")
            .selected(!in_setup && tab.selected_project.is_none())
            .on_click(move |_, _, cx| {
                entity.update(cx, |tab, cx| tab.select_project(None, cx));
            })
    };

    let projects = state.projects.iter().map(|project| {
        let entity = entity.clone();
        let id = project.id;
        let (icon, color) = project_icon(project, state);
        IconButton::new(("rail-project", id.0), icon)
            .icon_color(color)
            .tooltip(project.name.clone())
            .selected(!in_setup && tab.selected_project == Some(id))
            .on_click(move |_, _, cx| {
                entity.update(cx, |tab, cx| tab.select_project(Some(id), cx));
            })
    });

    let add_project = {
        let entity = entity.clone();
        IconButton::new("rail-project-add", IconName::Plus)
            .tooltip("Add project…")
            .on_click(move |_, _, cx| {
                entity.update(cx, |tab, cx| tab.add_project(cx));
            })
    };

    let user = match &state.auth {
        AuthStatus::SignedIn { user, .. } => Some(user),
        _ => None,
    };

    div()
        .size_full()
        .flex()
        .flex_col()
        .children(user.map(|user| rail_header(avatar(user, cx), cx)))
        .child(
            rail_items("sidebar-rail")
                .child(all_projects)
                .children(projects)
                .child(add_project),
        )
}
