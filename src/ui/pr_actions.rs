//! Merging, closing, and drafting the open pull request, from the side
//! panel of its Conversation tab: a button for each, merging and closing
//! asking first, and what came of the last one (running, commented on, or
//! why it failed).
//!
//! Merging follows the project's settings (`MergeSettings`): on GitHub, or
//! by leaving one of the project's comments, chosen at random, for a bot.

use gpui::{AnyElement, App, Entity, PromptLevel, SharedString, div, prelude::*};

use super::components::{
    Button, ButtonSize, ButtonStyle, Callout, CalloutKind, IconName, Spinner, TestSelector as _,
};
use super::theme::Theme;
use crate::github::{PullRequest, PullRequestDetail, PullRequestState};
use crate::projects::MergePlan;
use crate::state::{AppState, PrActionKind, PrActionStatus};

/// The merge button's test selector and element id.
pub fn merge_id(pr: &PullRequest) -> String {
    format!("merge-{}", pr.id)
}

/// The close button's test selector and element id.
pub fn close_id(pr: &PullRequest) -> String {
    format!("close-{}", pr.id)
}

/// The button that converts it to a draft, or marks it ready for review.
pub fn draft_id(pr: &PullRequest) -> String {
    format!("draft-{}", pr.id)
}

/// What shows while the pull request is being merged, closed, or
/// commented on.
pub fn acting_id(pr: &PullRequest) -> String {
    format!("acting-{}", pr.id)
}

/// What shows once it's been commented on to merge.
pub fn commented_id(pr: &PullRequest) -> String {
    format!("commented-{}", pr.id)
}

/// Dismisses why the last action on it failed.
pub fn dismiss_id(pr: &PullRequest) -> String {
    format!("dismiss-action-{}", pr.id)
}

/// The side panel's merge, draft, and close buttons for an open pull
/// request, or
/// what's running instead, with what came of the last action under them.
pub fn pr_actions(pr: &PullRequestDetail, app: &Entity<AppState>, cx: &App) -> Option<AnyElement> {
    if pr.state != PullRequestState::Open {
        return None;
    }
    let theme = Theme::get(cx);
    let summary = &pr.summary;
    let state = app.read(cx);
    let action = state.pr_action(&summary.id);
    let note = |text: String| div().text_xs().text_color(theme.text_muted).child(text);
    let root = div()
        .test_selector(|| "pr-actions".into())
        .flex()
        .flex_col()
        .gap_2()
        .pb_4()
        .border_b_1()
        .border_color(theme.border_subtle);

    if let Some(action) = action.filter(|action| action.is_running()) {
        let id = acting_id(summary);
        let doing = match action.kind {
            PrActionKind::Merge => "Merging…",
            PrActionKind::Comment(_) => "Commenting…",
            PrActionKind::Close => "Closing…",
            PrActionKind::SetDraft(true) => "Converting to draft…",
            PrActionKind::SetDraft(false) => "Marking ready for review…",
        };
        return Some(
            root.child(
                div()
                    .test_selector(|| id.clone())
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_color(theme.text_muted)
                    .child(Spinner::new(SharedString::from(format!("{id}-spinner"))))
                    .child(doing),
            )
            .into_any_element(),
        );
    }

    let settings = state.merge_settings(summary);
    let by_comment = settings.is_some_and(|merge| merge.by_comment);
    let is_draft = state.is_draft(summary);
    let blocked = if is_draft {
        Some("Drafts can't be merged.")
    } else if by_comment && settings.is_some_and(|merge| merge.comments.is_empty()) {
        Some("Add comments to merge with in the project's connection settings.")
    } else {
        None
    };
    let merge = Button::new(
        SharedString::from(merge_id(summary)),
        if by_comment {
            "Comment to merge"
        } else {
            "Merge"
        },
    )
    .icon(IconName::GitMerge)
    .size(ButtonSize::Compact)
    .disabled(blocked.is_some())
    .on_click({
        let (pr, app) = (summary.clone(), app.clone());
        move |_, window, cx| confirm_merge(&pr, &app, window, cx)
    });
    let draft = Button::new(
        SharedString::from(draft_id(summary)),
        if is_draft {
            "Ready for review"
        } else {
            "Convert to draft"
        },
    )
    .icon(if is_draft {
        IconName::Eye
    } else {
        IconName::GitPullRequestDraft
    })
    .size(ButtonSize::Compact)
    .style(ButtonStyle::Secondary)
    .on_click({
        let (pr, app) = (summary.clone(), app.clone());
        move |_, _, cx| app.update(cx, |app, cx| app.set_draft(&pr, !is_draft, cx))
    });
    let close = Button::new(SharedString::from(close_id(summary)), "Close")
        .icon(IconName::GitPullRequestClosed)
        .size(ButtonSize::Compact)
        .style(ButtonStyle::Danger)
        .on_click({
            let (pr, app) = (summary.clone(), app.clone());
            move |_, window, cx| confirm_close(&pr, &app, window, cx)
        });

    let outcome = action.and_then(|action| match (&action.kind, &action.status) {
        (PrActionKind::Comment(body), PrActionStatus::Commented) => {
            let id = commented_id(summary);
            Some(
                note(format!("Commented “{body}” for a bot to merge it."))
                    .test_selector(|| id.clone())
                    .into_any_element(),
            )
        }
        (kind, PrActionStatus::Failed(error)) => {
            let title = match kind {
                PrActionKind::Merge => "Couldn't merge",
                PrActionKind::Comment(_) => "Couldn't comment",
                PrActionKind::Close => "Couldn't close",
                PrActionKind::SetDraft(true) => "Couldn't convert to draft",
                PrActionKind::SetDraft(false) => "Couldn't mark ready for review",
            };
            let (dismiss, id) = (app.clone(), summary.id.clone());
            Some(
                Callout::new(CalloutKind::Error, error.clone())
                    .title(title)
                    .action(
                        Button::new(SharedString::from(dismiss_id(summary)), "Dismiss")
                            .size(ButtonSize::Compact)
                            .style(ButtonStyle::Secondary)
                            .on_click(move |_, _, cx| {
                                dismiss.update(cx, |app, cx| app.dismiss_pr_action(&id, cx));
                            }),
                    )
                    .into_any_element(),
            )
        }
        _ => None,
    });

    Some(
        root.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(merge)
                .child(draft)
                .child(close),
        )
        .children(blocked.map(|reason| note(reason.to_owned())))
        .children(outcome)
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{Entity, TestAppContext, VisualTestContext};

    use super::{close_id, commented_id, dismiss_id, draft_id, merge_id};
    use crate::github::{FakeAction, FakeGithub, PullRequest, PullRequestState};
    use crate::projects::{MergeSettings, ProjectId};
    use crate::state::{SectionKind, Tab};
    use crate::test_support::{
        VisualTestContextExt as _, detail, git, git_checkout, open_docket, project, pull_request,
    };

    /// A docket with one pull request of yours, in a project for
    /// `acme/widgets` that merges as `merge` says, open to its
    /// conversation.
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
        assert!(
            !cx.is_drawn(&merge_id(&pr)),
            "the docket's rows don't merge"
        );
        cx.click_on(&pr.id);
        assert!(tab.read_with(cx, |tab, _| tab.detail.is_some()));
        (github, pr, tab, cx, dir)
    }

    #[gpui::test]
    fn merges_from_the_conversation_once_confirmed(cx: &mut TestAppContext) {
        let (github, pr, tab, cx, _dir) = open_with_pr(cx, MergeSettings::default());
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
            !cx.is_drawn(&pr.id),
            "it leaves the docket, though GitHub's search hasn't caught up"
        );
        assert!(
            tab.read_with(cx, |tab, _| tab.detail.is_some()),
            "it stays open"
        );
    }

    #[gpui::test]
    fn closes_from_the_conversation_once_confirmed(cx: &mut TestAppContext) {
        let (github, pr, _, cx, _dir) = open_with_pr(cx, MergeSettings::default());
        cx.click_on(&close_id(&pr));
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert!(github.actions().is_empty());
        assert!(cx.is_drawn(&pr.id));

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
    fn converts_to_a_draft_and_back(cx: &mut TestAppContext) {
        let (github, pr, tab, cx, _dir) = open_with_pr(cx, MergeSettings::default());
        let is_draft = |cx: &mut VisualTestContext| {
            tab.read_with(cx, |tab, cx| {
                let detail = tab.detail.as_ref().unwrap().loaded.as_ref().unwrap();
                tab.app.read(cx).is_draft(&detail.pr.summary)
            })
        };

        cx.click_on(&draft_id(&pr));
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt(), "it's undone as easily");
        assert_eq!(
            github.actions(),
            vec![FakeAction::SetDraft {
                id: pr.id.clone(),
                draft: true,
            }]
        );
        assert!(
            is_draft(cx),
            "it's a draft at once, though GitHub hasn't caught up"
        );
        assert!(cx.is_drawn(&pr.id), "it stays in the docket");
        cx.click_on(&merge_id(&pr));
        assert!(!cx.has_pending_prompt(), "drafts can't be merged");

        cx.click_on(&draft_id(&pr));
        cx.run_until_parked();
        assert_eq!(
            github.actions().last(),
            Some(&FakeAction::SetDraft {
                id: pr.id.clone(),
                draft: false,
            })
        );
        assert!(!is_draft(cx));

        // Once GitHub shows it changed since, it's as GitHub says.
        cx.click_on(&draft_id(&pr));
        cx.run_until_parked();
        assert!(is_draft(cx));
        let mut changed = pr.clone();
        changed.updated_at = chrono::Utc::now() + chrono::Duration::hours(1);
        github.set_detail("acme/widgets".parse().unwrap(), detail(&changed));
        cx.click_on("pr-refresh");
        assert!(!is_draft(cx));
    }

    #[gpui::test]
    fn says_why_converting_to_a_draft_failed(cx: &mut TestAppContext) {
        let (github, pr, _, cx, _dir) = open_with_pr(cx, MergeSettings::default());
        github.fail_actions(Some("Only the author can convert it"));
        cx.click_on(&draft_id(&pr));
        cx.run_until_parked();
        assert!(cx.is_drawn(&dismiss_id(&pr)));
        assert!(cx.is_drawn(&draft_id(&pr)), "it can be tried again");
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
        let (github, pr, _, cx, _dir) = open_with_pr(cx, merge);
        cx.click_on(&merge_id(&pr));
        assert!(!cx.has_pending_prompt());
        assert!(github.actions().is_empty());
    }

    #[gpui::test]
    fn says_why_merging_failed(cx: &mut TestAppContext) {
        let (github, pr, _, cx, _dir) = open_with_pr(cx, MergeSettings::default());
        github.fail_actions(Some("Pull request is not mergeable"));
        cx.click_on(&merge_id(&pr));
        cx.simulate_prompt_answer("Merge");
        cx.run_until_parked();
        assert!(cx.is_drawn(&dismiss_id(&pr)));
        assert!(cx.is_drawn(&merge_id(&pr)), "it can be tried again");

        cx.click_on(&dismiss_id(&pr));
        assert!(!cx.is_drawn(&dismiss_id(&pr)));
    }

    #[gpui::test]
    fn a_merged_pull_request_has_nothing_to_merge(cx: &mut TestAppContext) {
        let (github, pr, _, cx, _dir) = open_with_pr(cx, MergeSettings::default());
        let mut merged = detail(&pr);
        merged.state = PullRequestState::Merged;
        github.set_detail("acme/widgets".parse().unwrap(), merged);
        cx.click_on("pr-refresh");
        assert!(!cx.is_drawn("pr-actions"));
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
        // Its descriptions wrap, and the box around them grows to fit.
        let options = cx.bounds_of("merge-options").unwrap();
        let last = cx.bounds_of("merge-by-comment-detail").unwrap();
        assert!(last.size.height > cx.bounds_of("merge-on-github-detail").unwrap().size.height);
        assert!(last.bottom() <= options.bottom());
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
