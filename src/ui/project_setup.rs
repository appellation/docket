//! Adding or editing a project: pick a local checkout, then confirm which
//! GitHub repository its PRs live in, and which of its accounts post
//! automated comments. Editing is also where a project is removed.

use gpui::{
    AnyElement, App, Entity, FontWeight, PromptLevel, SharedString, Window, div, prelude::*, px,
};

use super::components::{
    Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconName, Spinner,
    TestSelector as _, display_path,
};
use super::text_input::{TextInput, TextInputEvent};
use super::theme::Theme;
use crate::state::{AccessFix, CandidateSource, ProjectSetup, RepoCheck, Tab};

#[derive(IntoElement)]
pub struct ProjectSetupView {
    tab: Entity<Tab>,
}

impl ProjectSetupView {
    pub fn new(tab: Entity<Tab>) -> Self {
        Self { tab }
    }
}

fn label(text: &'static str, theme: &Theme) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text_muted)
        .child(text)
}

impl ProjectSetupView {
    /// The automated accounts field, kept across frames for its selection
    /// and focus. It starts from the setup's text, and writes back to it.
    fn authors_input(&self, window: &mut Window, cx: &mut App) -> Option<Entity<TextInput>> {
        let setup = self.tab.read(cx).setup.as_ref()?;
        let key = setup.editing.map_or(0, |id| id.0 as usize + 1);
        let text = setup.automated_authors.clone();
        let tab = self.tab.clone();
        Some(
            window.use_keyed_state(("automated-authors", key), cx, move |_, cx| {
                let mut input = TextInput::new("deploy-user, ci-runner", cx);
                input.set_text(&text, cx);
                cx.subscribe_self(move |input: &mut TextInput, event, cx| {
                    if let TextInputEvent::Changed = event {
                        let text = input.text().to_owned();
                        tab.update(cx, |tab, _| {
                            if let Some(setup) = &mut tab.setup {
                                setup.automated_authors = text;
                            }
                        });
                    }
                })
                .detach();
                input
            }),
        )
    }
}

impl RenderOnce for ProjectSetupView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let authors_input = self.authors_input(window, cx);
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

        let automated = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(label("Automated accounts", theme))
            .child(
                div()
                    .test_selector(|| "automated-authors".into())
                    .children(authors_input),
            )
            .child(div().text_xs().text_color(theme.text_muted).child(
                "Comments from these GitHub accounts collapse with bot comments: for \
                 automation that posts as a user rather than a GitHub App. Separate \
                 logins with commas.",
            ));

        let footer = {
            let cancel = self.tab.clone();
            let confirm = self.tab.clone();
            let ready = setup.root.is_some() && setup.confirmed_repository().is_some();
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
                    .child(automated)
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

    div()
        .flex()
        .flex_col()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.elevated)
        .overflow_hidden()
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

            let radio = div()
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
                .when(!selectable, |el| el.opacity(0.4));

            div()
                .id(("repo-candidate", ix))
                .flex()
                .items_start()
                .gap_3()
                .px_3()
                .py_2()
                .when(ix > 0, |el| {
                    el.border_t_1().border_color(theme.border_subtle)
                })
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
        .child(div().text_xs().text_color(theme.danger).child(message))
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
