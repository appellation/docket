//! Left navigation: the signed-in user and projects. It's the top of the
//! sidebar stack (`sidebar_stack`), so once something deeper is open it
//! collapses to a rail of their icons.
//!
//! The selected project scopes the docket (`pr_list`). Each is badged with
//! its review requests. Below them is where an update to Docket stands,
//! when there's something to say (`state/updates.rs`).

use gpui::{App, Entity, FontWeight, Img, SharedString, Window, div, img, prelude::*};

use super::components::{
    Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconButton, IconName, IconSize,
    ListItem, Spinner, TestSelector as _, count_badge, section_heading,
};
use super::sidebar_stack::{rail_header, rail_items, sidebar_header};
use super::theme::{Color, Theme};
use crate::auth::CredentialSource;
use crate::github::User;
use crate::projects::Project;
use crate::state::{AppState, AuthStatus, SectionKind, Tab, UpdateStatus};

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
                                .child(user.display_name().to_owned()),
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
        .children(update_footer(&tab.app, cx))
}

/// Where an update to Docket stands, if there's anything to show.
fn update_footer(app: &Entity<AppState>, cx: &App) -> Option<impl IntoElement> {
    let theme = Theme::get(cx);
    let status_line = |icon: gpui::AnyElement, text: String| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .text_xs()
            .text_color(theme.text_muted)
            .child(icon)
            .child(div().flex_1().min_w_0().child(text))
    };
    let dismiss = |app: &Entity<AppState>| {
        let app = app.clone();
        IconButton::new("update-dismiss", IconName::X)
            .size(ButtonSize::Compact)
            .tooltip("Dismiss")
            .on_click(move |_, _, cx| app.update(cx, |state, cx| state.dismiss_update(cx)))
    };
    let open_release = |id: &'static str, url: &SharedString| {
        let url = url.clone();
        Button::new(id, "Open Release")
            .style(ButtonStyle::Secondary)
            .size(ButtonSize::Compact)
            .icon(IconName::ExternalLink)
            .on_click(move |_, _, cx| cx.open_url(&url))
    };

    let content = match app.read(cx).update_status() {
        UpdateStatus::Idle => return None,
        UpdateStatus::Checking => status_line(
            Spinner::new("update-checking").into_any_element(),
            "Checking for updates…".into(),
        )
        .into_any_element(),
        UpdateStatus::UpToDate => status_line(
            Icon::new(IconName::CircleCheck)
                .size(IconSize::Small)
                .color(Color::Success)
                .into_any_element(),
            format!("Docket {} is up to date", app.read(cx).build_name()),
        )
        .into_any_element(),
        UpdateStatus::Downloading(update) => status_line(
            Spinner::new("update-downloading").into_any_element(),
            format!("Downloading Docket {update}…"),
        )
        .into_any_element(),
        UpdateStatus::Ready(update) => {
            let app = app.clone();
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(status_line(
                    Icon::new(IconName::ArrowDownToLine)
                        .size(IconSize::Small)
                        .color(Color::Accent)
                        .into_any_element(),
                    format!("Docket {update} is ready"),
                ))
                .child(
                    Button::new("update-restart", "Restart to Update")
                        .full_width()
                        .on_click(move |_, _, cx| {
                            app.update(cx, |state, cx| state.restart_to_update(cx));
                        }),
                )
                .into_any_element()
        }
        UpdateStatus::Available { update, url } => div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div().flex_1().min_w_0().child(status_line(
                            Icon::new(IconName::ArrowDownToLine)
                                .size(IconSize::Small)
                                .color(Color::Accent)
                                .into_any_element(),
                            format!("Docket {update} is available"),
                        )),
                    )
                    .child(dismiss(app)),
            )
            .child(open_release("update-open-release", url))
            .into_any_element(),
        UpdateStatus::Failed { message, url } => Callout::new(CalloutKind::Error, message.clone())
            .action(dismiss(app))
            .when_some(url.as_ref(), |callout, url| {
                callout.action(open_release("update-open-release", url))
            })
            .into_any_element(),
    };
    Some(
        div()
            .test_selector(|| "update-status".into())
            .flex_none()
            .p_2()
            .border_t_1()
            .border_color(theme.border_subtle)
            .child(content),
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

    // Only a ready update shows on the rail: the rest can wait for the
    // sidebar to expand.
    let restart_to_update = match state.update_status() {
        UpdateStatus::Ready(update) => {
            let app = tab.app.clone();
            Some(
                div().flex().justify_center().py_2().child(
                    IconButton::new("rail-update-restart", IconName::ArrowDownToLine)
                        .icon_color(Color::Accent)
                        .tooltip(format!("Restart to update to Docket {update}"))
                        .on_click(move |_, _, cx| {
                            app.update(cx, |state, cx| state.restart_to_update(cx));
                        }),
                ),
            )
        }
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
        .children(restart_to_update)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use gpui::TestAppContext;

    use crate::CheckForUpdates;
    use crate::github::FakeGithub;
    use crate::state::{UPDATE_INTERVAL, UpdateStatus};
    use crate::test_support::{VisualTestContextExt as _, open_docket};
    use crate::updater::tests::{fake_bundle, main_build, stable_build, zip_bundle};
    use crate::updater::{Channel, MAIN_TAG, Update, Version, asset_name, releases_repo};

    #[gpui::test]
    fn installs_a_new_release_and_offers_to_restart(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let installed = fake_bundle(
            &dir.path().join("Applications"),
            stable_build(&Version::current().to_string()),
            "old",
        );
        let github = Arc::new(FakeGithub::new());
        let (tab, cx) = open_docket(cx, github.clone(), Vec::new());
        let app = tab.read_with(cx, |tab, _| tab.app.clone());
        app.update(cx, |state, cx| {
            state.set_update_bundle(installed.clone(), cx)
        });
        cx.run_until_parked();
        // Nothing's released yet, and checks on their own say nothing.
        assert!(!cx.is_drawn("update-status"));

        let new = fake_bundle(&dir.path().join("build"), stable_build("99.0.0"), "new");
        github.set_latest_release(
            releases_repo(),
            "v99.0.0",
            vec![
                ("Docket-macos-other.zip", Vec::new()),
                (&asset_name(), zip_bundle(&new)),
            ],
        );
        cx.executor().advance_clock(UPDATE_INTERVAL);
        cx.run_until_parked();

        let ready = UpdateStatus::Ready(Update::Release(Version::parse("99.0.0").unwrap()));
        assert_eq!(
            app.read_with(cx, |state, _| state.update_status().clone()),
            ready
        );
        assert!(cx.is_drawn("update-restart"));
        let exe = fs::read_to_string(installed.join("Contents/MacOS/docket")).unwrap();
        assert!(exe.contains("new"));
    }

    #[gpui::test]
    fn a_main_build_follows_main(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let installed = fake_bundle(
            &dir.path().join("Applications"),
            main_build("aaa111"),
            "old",
        );
        let github = Arc::new(FakeGithub::new());
        let repo = releases_repo();
        let stable = fake_bundle(&dir.path().join("stable"), stable_build("99.0.0"), "stable");
        github.set_latest_release(
            repo.clone(),
            "v99.0.0",
            vec![(&asset_name(), zip_bundle(&stable))],
        );
        let main = fake_bundle(&dir.path().join("main"), main_build("aaa111"), "main");
        github.set_release(
            repo.clone(),
            MAIN_TAG,
            "aaa111",
            true,
            vec![(&asset_name(), zip_bundle(&main))],
        );
        let (tab, cx) = open_docket(cx, github.clone(), Vec::new());
        let app = tab.read_with(cx, |tab, _| tab.app.clone());
        let status = |cx: &mut gpui::VisualTestContext| {
            app.read_with(cx, |state, _| state.update_status().clone())
        };
        app.update(cx, |state, cx| {
            state.set_update_channel(Channel::Main, "aaa111");
            state.set_update_bundle(installed.clone(), cx);
        });
        cx.run_until_parked();
        // Main is where this build is, and a stable release isn't for it,
        // however new.
        assert_eq!(status(cx), UpdateStatus::Idle);

        let main = fake_bundle(&dir.path().join("main"), main_build("bbb222"), "newer main");
        github.set_release(
            repo,
            MAIN_TAG,
            "bbb222",
            true,
            vec![(&asset_name(), zip_bundle(&main))],
        );
        cx.executor().advance_clock(UPDATE_INTERVAL);
        cx.run_until_parked();
        let update = Update::Main {
            commit: "bbb222".into(),
        };
        assert_eq!(status(cx), UpdateStatus::Ready(update));
        let exe = fs::read_to_string(installed.join("Contents/MacOS/docket")).unwrap();
        assert!(exe.contains("newer main"));
    }

    #[gpui::test]
    fn checking_by_hand_shows_what_it_found(cx: &mut TestAppContext) {
        let github = Arc::new(FakeGithub::new());
        let (tab, cx) = open_docket(cx, github.clone(), Vec::new());
        let app = tab.read_with(cx, |tab, _| tab.app.clone());
        let status = |cx: &mut gpui::VisualTestContext| {
            app.read_with(cx, |state, _| state.update_status().clone())
        };

        // Up to date, for a moment.
        github.set_latest_release(releases_repo(), &format!("v{}", Version::current()), vec![]);
        cx.dispatch_action(CheckForUpdates);
        cx.run_until_parked();
        assert_eq!(status(cx), UpdateStatus::UpToDate);
        assert!(cx.is_drawn("update-status"));
        cx.executor().advance_clock(UPDATE_INTERVAL);
        cx.run_until_parked();
        assert!(!cx.is_drawn("update-status"));

        // Newer, but `cargo run` has no bundle to install it into: it links
        // to the release instead.
        github.set_latest_release(releases_repo(), "v99.0.0", vec![]);
        cx.dispatch_action(CheckForUpdates);
        cx.run_until_parked();
        assert!(matches!(status(cx), UpdateStatus::Available { .. }));
        assert!(cx.is_drawn("update-open-release"));
        cx.click_on("update-dismiss");
        assert_eq!(status(cx), UpdateStatus::Idle);
        assert!(!cx.is_drawn("update-status"));
    }
}
