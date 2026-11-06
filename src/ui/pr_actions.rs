//! Merging and closing a pull request from its row in the docket, in the
//! sidebar or the full list: buttons that show while the mouse is over the
//! row (or always, on the open pull request's), each asking first.
//!
//! Merging follows the project's settings (`MergeSettings`): on GitHub, or
//! by leaving one of the project's comments, chosen at random, for a bot.

use gpui::{AnyElement, App, Entity, PromptLevel, SharedString, div, prelude::*, px};

use super::components::{
    Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconButton, IconName, IconSize,
    Spinner, TestSelector as _, Tooltip,
};
use super::theme::Color;
use crate::github::{PullRequest, PullRequestState};
use crate::projects::MergePlan;
use crate::state::{AppState, PrAction, PrActionKind, PrActionStatus};

/// The hover group a docket row names itself, for its buttons to show.
pub const ROW_GROUP: &str = "docket-row";

/// A row's merge button's test selector and element id.
pub fn merge_id(pr: &PullRequest) -> String {
    format!("merge-{}", pr.id)
}

/// A row's close button's test selector and element id.
pub fn close_id(pr: &PullRequest) -> String {
    format!("close-{}", pr.id)
}

/// What a row shows while it's being merged, closed, or commented on.
pub fn acting_id(pr: &PullRequest) -> String {
    format!("acting-{}", pr.id)
}

/// What a row shows once it's been commented on to merge.
pub fn commented_id(pr: &PullRequest) -> String {
    format!("commented-{}", pr.id)
}

/// A dismiss button for why an action on a pull request failed.
pub fn dismiss_id(pr_id: &str) -> String {
    format!("dismiss-action-{pr_id}")
}

/// Merge and close buttons for an open pull request's row: always shown
/// when `revealed`, else while the mouse is over the row (`ROW_GROUP`). A
/// spinner instead while one is running.
pub fn row_actions(
    pr: &PullRequest,
    revealed: bool,
    app: &Entity<AppState>,
    cx: &App,
) -> Option<AnyElement> {
    if pr.state != PullRequestState::Open {
        return None;
    }
    let state = app.read(cx);
    let action = state.pr_action(&pr.id);
    if action.is_some_and(PrAction::is_running) {
        let id = acting_id(pr);
        return Some(
            div()
                .test_selector(|| id.clone())
                .flex_none()
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .child(Spinner::new(SharedString::from(format!("{id}-spinner"))))
                .into_any_element(),
        );
    }

    let commented = action.and_then(|action| match (&action.kind, &action.status) {
        (PrActionKind::Comment(body), PrActionStatus::Commented) => {
            let (id, tooltip) = (commented_id(pr), format!("Commented “{body}”"));
            Some(
                div()
                    .test_selector(|| id.clone())
                    .id(SharedString::from(id.clone()))
                    .flex_none()
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .tooltip(move |_, cx| Tooltip::view(tooltip.clone().into(), cx))
                    .child(
                        Icon::new(IconName::MessageSquare)
                            .size(IconSize::Small)
                            .color(Color::Success),
                    ),
            )
        }
        _ => None,
    });

    let settings = state.merge_settings(pr);
    let by_comment = settings.is_some_and(|merge| merge.by_comment);
    let (merge_tooltip, can_merge) = if pr.is_draft {
        ("Drafts can't be merged", false)
    } else if by_comment && settings.is_some_and(|merge| merge.comments.is_empty()) {
        (
            "Add comments to merge with in the project's connection settings",
            false,
        )
    } else if by_comment {
        ("Merge by commenting…", true)
    } else {
        ("Merge…", true)
    };
    let merge = IconButton::new(SharedString::from(merge_id(pr)), IconName::GitMerge)
        .size(ButtonSize::Compact)
        .tooltip(merge_tooltip)
        .disabled(!can_merge)
        .on_click({
            let (pr, app) = (pr.clone(), app.clone());
            move |_, window, cx| {
                cx.stop_propagation();
                confirm_merge(&pr, &app, window, cx);
            }
        });
    let close = IconButton::new(
        SharedString::from(close_id(pr)),
        IconName::GitPullRequestClosed,
    )
    .size(ButtonSize::Compact)
    .tooltip("Close…")
    .on_click({
        let (pr, app) = (pr.clone(), app.clone());
        move |_, window, cx| {
            cx.stop_propagation();
            confirm_close(&pr, &app, window, cx);
        }
    });

    Some(
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_0p5()
            .children(commented)
            .child(
                div()
                    .id(SharedString::from(format!("actions-{}", pr.id)))
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .when(!revealed, |el| {
                        el.opacity(0.)
                            .group_hover(ROW_GROUP, |style| style.opacity(1.))
                    })
                    // A disabled button's click isn't the row's either.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(merge)
                    .child(close),
            )
            .into_any_element(),
    )
}

/// Asks whether to merge `pr` as its project says, then does.
fn confirm_merge(
    pr: &PullRequest,
    app: &Entity<AppState>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    // Chosen now, so the prompt says which comment it'll be.
    let Some(plan) = app.read(cx).merge_plan(pr) else {
        return;
    };
    let label = format!("{}#{}", pr.repository, pr.number);
    let (message, detail, verb) = match &plan {
        MergePlan::Merge => (
            format!("Merge {label}?"),
            format!(
                "“{}” merges into {} on GitHub, with your default merge method for {}.",
                pr.title, pr.base_ref_name, pr.repository
            ),
            "Merge",
        ),
        MergePlan::Comment(body) => (
            format!("Comment to merge {label}?"),
            format!(
                "Docket comments “{body}” on “{}”, for a bot to merge it.",
                pr.title
            ),
            "Comment",
        ),
    };
    let answer = window.prompt(
        PromptLevel::Info,
        &message,
        Some(&detail),
        &[verb, "Cancel"],
        cx,
    );
    let (pr, app) = (pr.clone(), app.clone());
    cx.spawn(async move |cx| {
        if answer.await == Ok(0) {
            app.update(cx, |app, cx| app.merge_pull_request(&pr, plan, cx))
                .ok();
        }
    })
    .detach();
}

/// Asks whether to close `pr`, then does.
fn confirm_close(
    pr: &PullRequest,
    app: &Entity<AppState>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    let message = format!("Close {}#{}?", pr.repository, pr.number);
    let detail = format!(
        "“{}” closes without merging. It can be reopened on GitHub.",
        pr.title
    );
    let answer = window.prompt(
        PromptLevel::Warning,
        &message,
        Some(&detail),
        &["Close", "Cancel"],
        cx,
    );
    let (pr, app) = (pr.clone(), app.clone());
    cx.spawn(async move |cx| {
        if answer.await == Ok(0) {
            app.update(cx, |app, cx| app.close_pull_request(&pr, cx))
                .ok();
        }
    })
    .detach();
}

/// Why merging, closing, or commenting failed, each with a Dismiss.
pub fn action_errors(app: &Entity<AppState>, cx: &App) -> Vec<Callout> {
    let mut errors: Vec<_> = app.read(cx).pr_action_errors().collect();
    errors.sort_by(|a, b| a.1.label.cmp(&b.1.label));
    errors
        .into_iter()
        .map(|(id, action, error)| {
            let label = &action.label;
            let title = match action.kind {
                PrActionKind::Merge => format!("Couldn't merge {label}"),
                PrActionKind::Comment(_) => format!("Couldn't comment on {label}"),
                PrActionKind::Close => format!("Couldn't close {label}"),
            };
            let (dismiss, id) = (app.clone(), id.to_owned());
            Callout::new(CalloutKind::Error, error.clone())
                .title(title)
                .action(
                    Button::new(SharedString::from(dismiss_id(&id)), "Dismiss")
                        .size(ButtonSize::Compact)
                        .style(ButtonStyle::Secondary)
                        .on_click(move |_, _, cx| {
                            dismiss.update(cx, |app, cx| app.dismiss_pr_action(&id, cx));
                        }),
                )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{Entity, TestAppContext, VisualTestContext};

    use super::{close_id, commented_id, dismiss_id, merge_id};
    use crate::github::{FakeAction, FakeGithub, PullRequest};
    use crate::projects::{MergeSettings, ProjectId};
    use crate::state::{SectionKind, Tab};
    use crate::test_support::{
        VisualTestContextExt as _, detail, git, git_checkout, open_docket, project, pull_request,
    };

    /// A docket with one pull request of yours, in a project for
    /// `acme/widgets` that merges as `merge` says.
    fn open_with_pr(
        cx: &mut TestAppContext,
        merge: MergeSettings,
    ) -> (
        Arc<FakeGithub>,
        PullRequest,
        Entity<Tab>,
        &mut VisualTestContext,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        git_checkout(dir.path(), &[("README.md", "# Widgets\n")]);
        let github = Arc::new(FakeGithub::new());
        let pr = pull_request("acme/widgets", 7, "Add sprockets");
        github.set_search(SectionKind::Authored.search_query(), vec![pr.clone()]);
        github.set_detail("acme/widgets".parse().unwrap(), detail(&pr));
        let mut project = project(1, dir.path(), "acme/widgets");
        project.merge = merge;
        let (tab, cx) = open_docket(cx, github.clone(), vec![project]);
        (github, pr, tab, cx, dir)
    }

    #[gpui::test]
    fn merges_from_the_sidebar_once_confirmed(cx: &mut TestAppContext) {
        let (github, pr, tab, cx, _dir) = open_with_pr(cx, MergeSettings::default());
        cx.click_on(&pr.id);
        assert!(tab.read_with(cx, |tab, _| tab.detail.is_some()));
        assert!(
            cx.is_drawn(&merge_id(&pr)),
            "the open pull request's row shows it"
        );

        cx.click_on(&merge_id(&pr));
        assert!(cx.has_pending_prompt(), "merging asks first");
        cx.simulate_prompt_answer("Merge");
        cx.run_until_parked();
        assert_eq!(
            github.actions(),
            vec![FakeAction::Merged {
                id: pr.id.clone(),
                head: pr.head_ref_oid.clone(),
            }]
        );
        assert!(
            !cx.is_drawn(&merge_id(&pr)),
            "it leaves the docket, though GitHub's search hasn't caught up"
        );
        assert!(
            tab.read_with(cx, |tab, _| tab.detail.is_some()),
            "it stays open"
        );
    }

    #[gpui::test]
    fn closes_from_the_docket_once_confirmed(cx: &mut TestAppContext) {
        let (github, pr, _, cx, _dir) = open_with_pr(cx, MergeSettings::default());
        cx.click_on(&close_id(&pr));
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert!(github.actions().is_empty());
        assert!(cx.is_drawn(&pr.id), "clicking the button didn't open it");

        cx.click_on(&close_id(&pr));
        cx.simulate_prompt_answer("Close");
        cx.run_until_parked();
        assert_eq!(
            github.actions(),
            vec![FakeAction::Closed { id: pr.id.clone() }]
        );
        assert!(!cx.is_drawn(&pr.id));
    }

    #[gpui::test]
    fn merges_by_comment_when_the_project_says(cx: &mut TestAppContext) {
        let comments = vec!["ship it".to_owned(), "send it".to_owned()];
        let merge = MergeSettings {
            by_comment: true,
            comments: comments.clone(),
        };
        let (github, pr, _, cx, _dir) = open_with_pr(cx, merge);
        cx.click_on(&merge_id(&pr));
        cx.simulate_prompt_answer("Comment");
        cx.run_until_parked();
        let actions = github.actions();
        let [FakeAction::Commented { id, body }] = actions.as_slice() else {
            panic!("expected a comment, got {actions:?}");
        };
        assert_eq!(*id, pr.id);
        assert!(comments.contains(body), "{body:?} is one of the project's");
        assert!(cx.is_drawn(&pr.id), "it stays until the bot merges it");
        assert!(cx.is_drawn(&commented_id(&pr)));
    }

    #[gpui::test]
    fn without_comments_it_cant_merge_by_comment(cx: &mut TestAppContext) {
        let merge = MergeSettings {
            by_comment: true,
            comments: Vec::new(),
        };
        let (github, pr, tab, cx, _dir) = open_with_pr(cx, merge);
        cx.click_on(&merge_id(&pr));
        assert!(!cx.has_pending_prompt());
        assert!(github.actions().is_empty());
        assert!(
            tab.read_with(cx, |tab, _| tab.detail.is_none()),
            "the disabled button's click isn't the row's"
        );
    }

    #[gpui::test]
    fn says_why_merging_failed(cx: &mut TestAppContext) {
        let (github, pr, _, cx, _dir) = open_with_pr(cx, MergeSettings::default());
        github.fail_actions(Some("Pull request is not mergeable"));
        cx.click_on(&merge_id(&pr));
        cx.simulate_prompt_answer("Merge");
        cx.run_until_parked();
        assert!(cx.is_drawn(&dismiss_id(&pr.id)));
        assert!(cx.is_drawn(&merge_id(&pr)), "it can be tried again");

        cx.click_on(&dismiss_id(&pr.id));
        assert!(!cx.is_drawn(&dismiss_id(&pr.id)));
    }

    #[gpui::test]
    fn a_project_is_set_to_merge_by_comment(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        git_checkout(dir.path(), &[("README.md", "# Widgets\n")]);
        let remote = "git@github.com:acme/widgets.git";
        git(dir.path(), &["remote", "add", "origin", remote]);
        let github = Arc::new(FakeGithub::new());
        let projects = vec![project(1, dir.path(), "acme/widgets")];
        let (tab, cx) = open_docket(cx, github, projects);
        let merge = |cx: &mut VisualTestContext| {
            tab.read_with(cx, |tab, cx| {
                tab.app
                    .read(cx)
                    .project(ProjectId(1))
                    .unwrap()
                    .merge
                    .clone()
            })
        };

        tab.update(cx, |tab, cx| tab.select_project(Some(ProjectId(1)), cx));
        cx.run_until_parked();
        cx.click_on("edit-project");
        cx.click_on("merge-by-comment");
        cx.click_on("confirm-setup");
        assert!(
            tab.read_with(cx, |tab, _| tab.setup.is_some()),
            "it needs a comment first"
        );
        cx.click_on("merge-comments");
        cx.simulate_input("ship it, , send it,");
        cx.click_on("confirm-setup");
        assert!(tab.read_with(cx, |tab, _| tab.setup.is_none()));
        assert_eq!(
            merge(cx),
            MergeSettings {
                by_comment: true,
                comments: vec!["ship it".into(), "send it".into()],
            }
        );

        // Merging on GitHub again keeps the comments for next time.
        cx.click_on("edit-project");
        assert!(cx.is_drawn("merge-comments"));
        cx.click_on("merge-on-github");
        assert!(!cx.is_drawn("merge-comments"));
        cx.click_on("confirm-setup");
        assert_eq!(
            merge(cx),
            MergeSettings {
                by_comment: false,
                comments: vec!["ship it".into(), "send it".into()],
            }
        );
    }
}
