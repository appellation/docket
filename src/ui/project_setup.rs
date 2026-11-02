//! Adding or editing a project: pick a local checkout, then confirm which
//! GitHub repository its PRs live in.

use gpui::{AnyElement, App, Entity, SharedString, Window, div, prelude::*};

use super::components::{Button, ButtonStyle, display_path};
use super::theme::Theme;
use crate::state::{AccessFix, AppState, CandidateSource, ProjectSetup, RepoCheck};

#[derive(IntoElement)]
pub struct ProjectSetupView {
    state: Entity<AppState>,
}

impl ProjectSetupView {
    pub fn new(state: Entity<AppState>) -> Self {
        Self { state }
    }
}

fn label(text: &'static str, theme: &Theme) -> impl IntoElement {
    div().text_xs().text_color(theme.text_muted).child(text)
}

impl RenderOnce for ProjectSetupView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let state = self.state.read(cx);
        let Some(setup) = &state.setup else {
            return div().into_any_element();
        };
        let editing = setup.editing.and_then(|id| state.project(id));

        let title = match editing {
            Some(project) => format!("Edit {}", project.name),
            None => "Add project".to_owned(),
        };

        let folder = {
            let entity = self.state.clone();
            let shown = setup.root.as_ref().unwrap_or(&setup.path);
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(label("Local folder", theme))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface)
                        .child(div().truncate().child(display_path(shown)))
                        .child(
                            Button::new("change-folder", "Choose…")
                                .style(ButtonStyle::Secondary)
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |state, cx| state.change_setup_folder(cx));
                                }),
                        ),
                )
                .when(
                    setup.root.as_ref().is_some_and(|root| *root != setup.path),
                    |el| {
                        el.child(div().text_xs().text_color(theme.text_muted).child(
                            "Using the root of the Git checkout containing the folder you chose.",
                        ))
                    },
                )
        };

        let repository = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(label("GitHub repository", theme))
            .child(repository_body(setup, &self.state, theme));

        let footer = {
            let cancel = self.state.clone();
            let confirm = self.state.clone();
            let ready = setup.root.is_some() && setup.confirmed_repository().is_some();
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("cancel-setup", "Cancel")
                        .style(ButtonStyle::Secondary)
                        .on_click(move |_, _, cx| {
                            cancel.update(cx, |state, cx| state.cancel_setup(cx));
                        }),
                )
                .child(
                    Button::new(
                        "confirm-setup",
                        if editing.is_some() {
                            "Save"
                        } else {
                            "Add project"
                        },
                    )
                    .disabled(!ready)
                    .on_click(move |_, _, cx| {
                        confirm.update(cx, |state, cx| state.confirm_setup(cx));
                    }),
                )
        };

        div()
            .id("project-setup")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(gpui::px(560.))
                    .flex()
                    .flex_col()
                    .gap_6()
                    .p_6()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xl().child(title))
                            .child(div().text_color(theme.text_muted).child(
                                "Connect a local checkout to the GitHub repository its pull \
                                 requests live in. Docket uses the checkout to open files in \
                                 your editor and for code intelligence.",
                            )),
                    )
                    .child(folder)
                    .child(repository)
                    .child(footer),
            )
            .into_any_element()
    }
}

fn repository_body(setup: &ProjectSetup, state: &Entity<AppState>, theme: &Theme) -> AnyElement {
    if setup.scanning {
        return div()
            .text_color(theme.text_muted)
            .child("Reading Git remotes…")
            .into_any_element();
    }
    if let Some(error) = &setup.error {
        return div()
            .text_color(theme.danger)
            .child(error.clone())
            .into_any_element();
    }
    if setup.candidates.is_empty() {
        return div()
            .flex()
            .flex_col()
            .gap_1()
            .child("This checkout has no GitHub remotes.")
            .child(div().text_color(theme.text_muted).child(
                "Add one (git remote add origin git@github.com:owner/repo.git), \
                 then choose the folder again.",
            ))
            .into_any_element();
    }

    div()
        .flex()
        .flex_col()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .children(setup.candidates.iter().enumerate().map(|(ix, candidate)| {
            let selected = setup.selected == Some(ix);
            let selectable = !matches!(candidate.check, RepoCheck::Failed { .. });
            let entity = state.clone();

            let source = match &candidate.source {
                CandidateSource::Remote(name) => format!("Remote “{name}”"),
                CandidateSource::ForkParent(fork) => format!("Parent of fork {fork}"),
                CandidateSource::Current => "Currently connected".to_owned(),
            };
            let mut failure = None;
            let status = match &candidate.check {
                RepoCheck::Checking => Some("Checking…".to_owned()),
                RepoCheck::Ok(repo) => {
                    let mut parts =
                        vec![if repo.is_private { "Private" } else { "Public" }.to_owned()];
                    if repo.parent.is_some() {
                        parts.push("Fork".to_owned());
                    }
                    if let Some(permission) = &repo.viewer_permission {
                        parts.push(format!("{} access", capitalize(permission)));
                    }
                    // GitHub follows renames and transfers.
                    if repo.repo != candidate.repo {
                        parts.push(format!("Now {}", repo.repo));
                    }
                    Some(parts.join(" · "))
                }
                RepoCheck::Failed { message, fix } => {
                    failure = Some(failure_details(ix, message.clone(), fix, state, theme));
                    None
                }
            };

            let radio = div()
                .flex_none()
                .size_3()
                .rounded_full()
                .border_1()
                .border_color(if selected { theme.accent } else { theme.border })
                .when(selected, |el| el.bg(theme.accent))
                .when(!selectable, |el| el.opacity(0.4));

            div()
                .id(("repo-candidate", ix))
                .flex()
                .items_start()
                .gap_3()
                .px_3()
                .py_2()
                .when(ix > 0, |el| el.border_t_1().border_color(theme.border))
                .when(selected, |el| el.bg(theme.surface_hover))
                .when(selectable, |el| {
                    el.cursor_pointer()
                        .hover(|style| style.bg(theme.surface_hover))
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |state, cx| state.select_candidate(ix, cx));
                        })
                })
                .child(div().pt_1().child(radio))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(candidate.repo.to_string())
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.text_muted)
                                .truncate()
                                .child(match status {
                                    Some(status) => format!("{source} · {status}"),
                                    None => source,
                                }),
                        )
                        .children(failure),
                )
        }))
        .into_any_element()
}

/// The full error, plus a link to fix it on GitHub when there is one, and a
/// way to check again afterwards.
fn failure_details(
    ix: usize,
    message: SharedString,
    fix: &Option<AccessFix>,
    state: &Entity<AppState>,
    theme: &Theme,
) -> impl IntoElement {
    let recheck = state.clone();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .pt_1()
        .child(div().text_xs().text_color(theme.danger).child(message))
        .child(
            div()
                .flex()
                .gap_2()
                .text_xs()
                .when_some(fix.as_ref(), |el, fix| {
                    let url = fix.url.clone();
                    el.child(
                        Button::new(("fix-access", ix), fix.label)
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                })
                .child(
                    Button::new(("recheck", ix), "Check again")
                        .style(ButtonStyle::Secondary)
                        .on_click(move |_, _, cx| {
                            recheck.update(cx, |state, cx| state.recheck_repositories(cx));
                        }),
                ),
        )
}

/// `WRITE` → `Write`
fn capitalize(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
