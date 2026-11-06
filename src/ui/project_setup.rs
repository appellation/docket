//! Adding or editing a project: pick a local checkout, then confirm which
//! GitHub repository its PRs live in, and how they merge (on GitHub, or by
//! a comment for a bot). Editing is also where a project is removed.

use gpui::{
    AnyElement, App, Entity, FontWeight, PromptLevel, SharedString, Window, div, prelude::*, px,
};

use super::components::{
    Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconName, Spinner,
    TestSelector as _, display_path,
};
use super::text_input::TextInput;
use super::theme::Theme;
use crate::state::{AccessFix, CandidateSource, ProjectSetup, RepoCheck, Tab};

#[derive(IntoElement)]
pub struct ProjectSetupView {
    tab: Entity<Tab>,
    /// The comments to merge with, separated by commas.
    merge_comments: Entity<TextInput>,
}

impl ProjectSetupView {
    pub fn new(tab: Entity<Tab>, merge_comments: Entity<TextInput>) -> Self {
        Self {
            tab,
            merge_comments,
        }
    }
}

fn label(text: &'static str, theme: &Theme) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text_muted)
        .child(text)
}

impl RenderOnce for ProjectSetupView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let tab = self.tab.read(cx);
        let Some(setup) = &tab.setup else {
            return div().into_any_element();
        };
        let editing = setup.editing.and_then(|id| tab.app.read(cx).project(id));

        let title = match editing {
            Some(project) => format!("Edit {}", project.name),
            None => "Add project".to_owned(),
        };

        let folder = {
            let entity = self.tab.clone();
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
                        .pl_3()
                        .pr_1p5()
                        .py_1p5()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.elevated)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .child(Icon::new(IconName::FolderGit))
                                .child(div().truncate().child(display_path(shown))),
                        )
                        .child(
                            Button::new("change-folder", "Choose…")
                                .icon(IconName::FolderOpen)
                                .size(ButtonSize::Compact)
                                .style(ButtonStyle::Secondary)
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |tab, cx| tab.change_setup_folder(cx));
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
            .child(repository_body(setup, &self.tab, theme));

        let merging = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(label("Merging", theme))
            .child(merging_body(setup, &self.tab, &self.merge_comments, theme));

        let footer = {
            let cancel = self.tab.clone();
            let confirm = self.tab.clone();
            let ready = setup.root.is_some()
                && setup.confirmed_repository().is_some()
                && setup.merge_is_valid();
            let remove = editing.map(|project| {
                let (id, remove) = (project.id, self.tab.clone());
                let message = format!("Remove {} from Docket?", project.name);
                Button::new("remove-project", "Remove project")
                    .icon(IconName::Trash)
                    .style(ButtonStyle::Danger)
                    .on_click(move |_, window, cx| {
                        let answer = window.prompt(
                            PromptLevel::Warning,
                            &message,
                            Some("Your local checkout isn't affected."),
                            &["Remove", "Cancel"],
                            cx,
                        );
                        let remove = remove.clone();
                        cx.spawn(async move |cx| {
                            if answer.await == Ok(0) {
                                remove
                                    .update(cx, |tab, cx| {
                                        tab.app.update(cx, |app, cx| app.remove_project(id, cx))
                                    })
                                    .ok();
                            }
                        })
                        .detach();
                    })
            });
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(remove)
                .child(div().flex_1())
                .child(
                    Button::new("cancel-setup", "Cancel")
                        .style(ButtonStyle::Secondary)
                        .on_click(move |_, _, cx| {
                            cancel.update(cx, |tab, cx| tab.cancel_setup(cx));
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
                        confirm.update(cx, |tab, cx| tab.confirm_setup(cx));
                    }),
                )
        };

        div()
            .id("project-setup")
            .flex_1()
            .min_w_0()
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
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(div().text_color(theme.text_muted).child(
                                "Connect a local checkout to the GitHub repository its pull \
                                 requests live in. Docket uses the checkout to open files in \
                                 your editor and for code intelligence.",
                            )),
                    )
                    .child(folder)
                    .child(repository)
                    .child(merging)
                    .child(footer),
            )
            .into_any_element()
    }
}

fn repository_body(setup: &ProjectSetup, tab: &Entity<Tab>, theme: &Theme) -> AnyElement {
    if setup.scanning {
        return div()
            .flex()
            .items_center()
            .gap_2()
            .text_color(theme.text_muted)
            .child(Spinner::new("scanning-remotes"))
            .child("Reading Git remotes…")
            .into_any_element();
    }
    if let Some(error) = &setup.error {
        return Callout::new(CalloutKind::Error, error.clone()).into_any_element();
    }
    if setup.candidates.is_empty() {
        return Callout::new(
            CalloutKind::Warning,
            "Add one (git remote add origin git@github.com:owner/repo.git), \
             then choose the folder again.",
        )
        .title("This checkout has no GitHub remotes.")
        .into_any_element();
    }

    // Not `overflow_hidden`, which cuts off a failed check's details (see
    // `merging_body`): the rows at its ends round their own corners.
    let last = setup.candidates.len() - 1;
    div()
        .test_selector(|| "repo-candidates".into())
        .flex()
        .flex_col()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.elevated)
        .children(setup.candidates.iter().enumerate().map(|(ix, candidate)| {
            let selected = setup.selected == Some(ix);
            let selectable = !matches!(candidate.check, RepoCheck::Failed { .. });
            let entity = tab.clone();

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
                    failure = Some(failure_details(ix, message.clone(), fix, tab, theme));
                    None
                }
            };

            let radio = radio(selected, theme).when(!selectable, |el| el.opacity(0.4));

            div()
                .test_selector(move || format!("repo-candidate-{ix}"))
                .id(("repo-candidate", ix))
                .flex()
                .items_start()
                .gap_3()
                .px_3()
                .py_2()
                .when(ix > 0, |el| {
                    el.border_t_1().border_color(theme.border_subtle)
                })
                .when(ix == 0, |el| el.rounded_t_md())
                .when(ix == last, |el| el.rounded_b_md())
                .when(selected, |el| el.bg(theme.accent_subtle))
                .when(selectable && !selected, |el| {
                    el.hover(|style| style.bg(theme.element_hover))
                })
                .when(selectable, |el| {
                    el.cursor_pointer().on_click(move |_, _, cx| {
                        entity.update(cx, |tab, cx| tab.select_candidate(ix, cx));
                    })
                })
                .child(div().h(px(20.)).flex().items_center().child(radio))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(candidate.repo.to_string()),
                        )
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

/// A radio button's dot.
fn radio(selected: bool, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(14.))
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .when(selected, |el| {
            el.border_color(theme.accent)
                .bg(theme.accent)
                .child(div().size(px(6.)).rounded_full().bg(theme.text_on_accent))
        })
}

/// Merging on GitHub, or commenting for a bot to, with the comments to
/// choose from.
fn merging_body(
    setup: &ProjectSetup,
    tab: &Entity<Tab>,
    comments: &Entity<TextInput>,
    theme: &Theme,
) -> impl IntoElement {
    let by_comment = setup.merge.by_comment;
    // Each option is a value of `by_comment`.
    let option = |id: &'static str, value: bool, title: &'static str, detail: &'static str| {
        let (tab, selected) = (tab.clone(), by_comment == value);
        div()
            .test_selector(|| id.into())
            .id(id)
            .flex()
            .items_start()
            .gap_3()
            .px_3()
            .py_2()
            .when(selected, |el| el.bg(theme.accent_subtle))
            .when(!selected, |el| {
                el.cursor_pointer()
                    .hover(|style| style.bg(theme.element_hover))
                    .on_click(move |_, _, cx| {
                        tab.update(cx, |tab, cx| tab.set_merge_by_comment(value, cx));
                    })
            })
            .child(
                div()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .child(radio(selected, theme)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).child(title))
                    .child(
                        div()
                            .test_selector(move || format!("{id}-detail"))
                            .text_xs()
                            .text_color(theme.text_muted)
                            .child(detail),
                    ),
            )
    };
    let missing = by_comment && setup.merge.comments.is_empty();
    // Not `overflow_hidden` to round the selected row's fill with the box:
    // GPUI then sizes the box as if the descriptions didn't wrap, cutting
    // them off. The rows at its ends round their own corners instead.
    div()
        .test_selector(|| "merge-options".into())
        .flex()
        .flex_col()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.elevated)
        .child(
            option(
                "merge-on-github",
                false,
                "Merge on GitHub",
                "With your default merge method for the repository.",
            )
            .rounded_t_md(),
        )
        .child(
            option(
                "merge-by-comment",
                true,
                "Comment instead",
                "For repositories a bot merges. Docket leaves one of these comments, chosen \
                 at random each time.",
            )
            .border_t_1()
            .border_color(theme.border_subtle)
            .when(!by_comment, |el| el.rounded_b_md()),
        )
        .when(by_comment, |el| {
            el.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .pl(px(38.))
                    .pr_3()
                    .pb_3()
                    .rounded_b_md()
                    .bg(theme.accent_subtle)
                    .child(
                        div()
                            .test_selector(|| "merge-comments".into())
                            .child(comments.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if missing {
                                theme.danger
                            } else {
                                theme.text_muted
                            })
                            .child(if missing {
                                "Add a comment to merge with."
                            } else {
                                "Separate comments with commas."
                            }),
                    ),
            )
        })
}

/// The full error, plus a link to fix it on GitHub when there is one, and a
/// way to check again afterwards.
fn failure_details(
    ix: usize,
    message: SharedString,
    fix: &Option<AccessFix>,
    tab: &Entity<Tab>,
    theme: &Theme,
) -> impl IntoElement {
    let recheck = tab.clone();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .pt_1()
        .child(
            div()
                .test_selector(move || format!("repo-candidate-{ix}-error"))
                .text_xs()
                .text_color(theme.danger)
                .child(message),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .when_some(fix.as_ref(), |el, fix| {
                    let url = fix.url.clone();
                    el.child(
                        Button::new(("fix-access", ix), fix.label)
                            .icon(IconName::ExternalLink)
                            .size(ButtonSize::Compact)
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                })
                .child(
                    Button::new(("recheck", ix), "Check again")
                        .icon(IconName::RefreshCw)
                        .size(ButtonSize::Compact)
                        .style(ButtonStyle::Secondary)
                        .on_click(move |_, _, cx| {
                            recheck.update(cx, |tab, cx| tab.recheck_repositories(cx));
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{TestAppContext, px};

    use crate::github::FakeGithub;
    use crate::projects::ProjectId;
    use crate::test_support::{VisualTestContextExt as _, git, git_checkout, open_docket, project};

    #[gpui::test]
    fn a_repository_that_failed_shows_all_of_why(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        git_checkout(dir.path(), &[("README.md", "# Widgets\n")]);
        let origin = "git@github.com:acme/widgets.git";
        git(dir.path(), &["remote", "add", "origin", origin]);
        let upstream = "git@github.com:acme/gone.git";
        git(dir.path(), &["remote", "add", "upstream", upstream]);
        let github = Arc::new(FakeGithub::new());
        github.fail_repository(
            "acme/gone".parse().unwrap(),
            "Could not resolve to a Repository with the name 'acme/gone'. It may have been \
             deleted, renamed, or made private, or this sign-in may not have access to it.",
        );
        let projects = vec![project(1, dir.path(), "acme/widgets")];
        let (tab, cx) = open_docket(cx, github, projects);
        tab.update(cx, |tab, cx| tab.select_project(Some(ProjectId(1)), cx));
        cx.run_until_parked();
        cx.click_on("edit-project");

        // `origin`, then the failed `upstream` at the bottom.
        let candidates = cx.bounds_of("repo-candidates").unwrap();
        let error = cx.bounds_of("repo-candidate-1-error").unwrap();
        assert!(error.size.height > px(20.), "the error wraps");
        let row = cx.bounds_of("repo-candidate-1").unwrap();
        assert!(
            row.bottom() <= candidates.bottom(),
            "the box grows to fit it, with Check Again under it"
        );
    }
}
