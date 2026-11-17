//! A commit opened from a pull request's or branch's Commits tab: a bar
//! naming it, with the way back to the list and to the commits either side
//! of it, above what it changes (`state/commit.rs`). Its diff is a
//! `DiffView` of its own, which groups its changes by Claude's analysis as
//! the Files tab does, with its changed files in the sidebar.

use gpui::{App, Entity, FontWeight, Window, div, prelude::*, px};

use super::components::{Icon, IconButton, IconName, MONOSPACE, TestSelector as _, relative_time};
use super::diff_view::DiffView;
use super::theme::{Color, Theme};
use crate::state::Tab;

#[derive(IntoElement)]
pub struct CommitView {
    state: Entity<Tab>,
    diff_view: Option<Entity<DiffView>>,
}

impl CommitView {
    pub fn new(state: Entity<Tab>, diff_view: Option<Entity<DiffView>>) -> Self {
        Self { state, diff_view }
    }
}

impl RenderOnce for CommitView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let root = div().flex_1().min_w_0().min_h_0().flex().flex_col();
        let tab = self.state.read(cx);
        let Some(open) = tab.shown_commit() else {
            return root;
        };
        let commit = &open.commit;
        let commits = tab.listed_commits();
        let position = commits.iter().position(|c| c.id == commit.id);
        let count = commits.len();
        let author = commit.author.as_deref().unwrap_or("Someone");
        let (back, previous, next) = (self.state.clone(), self.state.clone(), self.state.clone());

        let bar = div()
            .test_selector(|| "commit-view".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(44.))
            .pl_2()
            .pr_4()
            .border_b_1()
            .border_color(theme.border_subtle)
            .child(
                IconButton::new("commit-back", IconName::ArrowLeft)
                    .tooltip("All commits (Esc)")
                    .on_click(move |_, _, cx| {
                        back.update(cx, |tab, cx| tab.close_commit(cx));
                    }),
            )
            .child(Icon::new(IconName::GitCommit).color(Color::Muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(commit.headline.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(MONOSPACE)
                            .text_xs()
                            .text_color(theme.text_subtle)
                            .child(commit.short_oid.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.text_muted)
                            .child(format!(
                                "{author} committed {}",
                                relative_time(commit.committed_at)
                            )),
                    ),
            )
            .children(position.map(|ix| {
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.text_subtle)
                    .child(format!("{} of {count}", ix + 1))
            }))
            .child(
                IconButton::new("commit-previous", IconName::ChevronUp)
                    .tooltip("Previous commit")
                    .disabled(position.is_none_or(|ix| ix == 0))
                    .on_click(move |_, _, cx| {
                        previous.update(cx, |tab, cx| tab.open_adjacent_commit(-1, cx));
                    }),
            )
            .child(
                IconButton::new("commit-next", IconName::ChevronDown)
                    .tooltip("Next commit")
                    .disabled(position.is_none_or(|ix| ix + 1 >= count))
                    .on_click(move |_, _, cx| {
                        next.update(cx, |tab, cx| tab.open_adjacent_commit(1, cx));
                    }),
            )
            .children(commit.url.clone().map(|url| {
                IconButton::new("commit-open-github", IconName::ExternalLink)
                    .tooltip("Open on GitHub")
                    .on_click(move |_, _, cx| crate::ui::open_in_browser(&url, cx))
            }));
        root.child(bar).children(self.diff_view)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use chrono::{TimeZone as _, Utc};
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use serde_json::json;

    use crate::ai::{ClaudeCode, FakeClaude};
    use crate::diff::Viewed;
    use crate::github::{Commit, FakeGithub, FileViewedState, TimelineItem};
    use crate::state::{AnalysisLoad, AppState, DiffLoad, DiffSlot, PrTab, SectionKind, Tab};
    use crate::test_support::{
        self, VisualTestContextExt as _, detail, git_checkout, git_commit, open_docket, project,
        pull_request,
    };

    fn rev(root: &Path, reference: &str) -> String {
        crate::projects::git::git(root, &["rev-parse", reference])
            .unwrap()
            .trim()
            .to_owned()
    }

    /// A checkout whose `feature` branch changes `a.txt` in one commit,
    /// then `b.txt` in the next. Their object names, in order.
    fn checkout(root: &Path) -> (String, String) {
        git_checkout(root, &[("a.txt", "one\n"), ("b.txt", "one\n")]);
        test_support::git(root, &["checkout", "--quiet", "feature"]);
        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        git_commit(root, "Change a", "2026-01-02T00:00:00Z");
        std::fs::write(root.join("b.txt"), "two\n").unwrap();
        git_commit(root, "Change b", "2026-01-03T00:00:00Z");
        test_support::git(root, &["checkout", "--quiet", "main"]);
        (rev(root, "feature~1"), rev(root, "feature"))
    }

    /// The paths in `slot`'s diff, once it's loaded.
    fn paths(tab: &Entity<Tab>, slot: DiffSlot, cx: &mut VisualTestContext) -> Vec<String> {
        tab.read_with(cx, |tab, _| match &tab.diff_files_in(slot).unwrap().diff {
            DiffLoad::Ready(diff) => diff.files.iter().map(|f| f.path().to_owned()).collect(),
            _ => panic!("the diff is loaded"),
        })
    }

    fn opened(tab: &Entity<Tab>, cx: &mut VisualTestContext) -> Option<String> {
        tab.read_with(cx, |tab, _| {
            tab.opened_commit().map(|open| open.commit.oid.clone())
        })
    }

    #[gpui::test]
    fn a_branchs_commits_open_on_their_own(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (first, second) = checkout(root);
        let projects = vec![project(1, root, "acme/widgets")];
        let (tab, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("branch-1-feature");
        cx.click_on("branch-tab-commits");
        assert!(!cx.is_drawn("sidebar-files"), "the list has no files");

        cx.click_on(&format!("commit-{first}"));
        cx.run_until_parked();
        assert!(cx.is_drawn("commit-view"));
        assert_eq!(opened(&tab, cx), Some(first.clone()));
        assert_eq!(paths(&tab, DiffSlot::Commit, cx), ["a.txt"]);
        let head = tab.read_with(cx, |tab, _| match &tab.diff_files().unwrap().diff {
            DiffLoad::Ready(diff) => diff.head_oid.clone(),
            _ => unreachable!(),
        });
        assert_eq!(head, first);
        // Its files are in the sidebar, and its diff is drawn.
        assert!(cx.is_drawn("sidebar-files"));
        assert!(cx.is_drawn("diff-file-header-0"));
        // The branch's diff is left as it was.
        assert_eq!(paths(&tab, DiffSlot::Main, cx), ["a.txt", "b.txt"]);

        cx.click_on("commit-next");
        cx.run_until_parked();
        assert_eq!(opened(&tab, cx), Some(second.clone()));
        assert_eq!(paths(&tab, DiffSlot::Commit, cx), ["b.txt"]);
        // It's the last.
        cx.click_on("commit-next");
        assert_eq!(opened(&tab, cx), Some(second.clone()));
        cx.click_on("commit-previous");
        cx.run_until_parked();
        assert_eq!(opened(&tab, cx), Some(first.clone()));

        // The Files tab shows the branch's diff, and coming back, the
        // commit still.
        cx.click_on("branch-tab-files");
        assert!(!cx.is_drawn("commit-view"));
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.diff_files().unwrap().revision),
            tab.read_with(cx, |tab, _| tab
                .diff_files_in(DiffSlot::Main)
                .unwrap()
                .revision),
        );
        cx.click_on("branch-tab-commits");
        assert!(cx.is_drawn("commit-view"));

        // Escape goes back to the list, then out of the branch.
        cx.simulate_keystrokes("escape");
        assert_eq!(opened(&tab, cx), None);
        assert!(cx.is_drawn(&format!("commit-{first}")));
        cx.simulate_keystrokes("escape");
        assert!(tab.read_with(cx, |tab, _| tab.branch.is_none()));
    }

    #[gpui::test]
    fn a_commit_is_grouped_by_change(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (first, _) = checkout(root);
        let projects = vec![project(1, root, "acme/widgets")];
        let (tab, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        let claude = Arc::new(FakeClaude::new());
        claude.set_answer(json!({ "groups": [
            { "title": "Change a", "summary": "Spells it out.", "impact": "", "hunks": ["H1"] },
        ]}));
        cx.update(|_, cx| {
            AppState::global(cx).update(cx, |app, cx| {
                app.connect_claude_for_test(ClaudeCode::fake(claude.clone()), cx)
            });
        });
        cx.run_until_parked();

        cx.click_on("branch-1-feature");
        assert_eq!(claude.requests(), 1, "the branch's diff");
        cx.click_on("branch-tab-commits");
        cx.click_on(&format!("commit-{first}"));
        cx.run_until_parked();
        assert_eq!(claude.requests(), 2);
        let prompt = claude.last_prompt().unwrap();
        assert!(prompt.contains("## a.txt (modified, +1 -1)"), "{prompt}");
        assert!(!prompt.contains("b.txt"), "{prompt}");
        // Claude reads the code as of the commit.
        let dir = claude.last_dir().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "two\n");
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "one\n");
        assert_eq!(claude.last_trusted(), Some(true));
        let titles = tab.read_with(cx, |tab, _| match &tab.diff_files().unwrap().analysis {
            AnalysisLoad::Ready(analysis) => {
                analysis.groups.iter().map(|g| g.title.clone()).collect()
            }
            _ => Vec::new(),
        });
        assert_eq!(titles, ["Change a"]);
        assert!(cx.is_drawn("diff-group-0"));
        assert!(cx.is_drawn("diff-tree-group-0"));

        // Opening it again reuses the analysis.
        cx.simulate_keystrokes("escape");
        cx.click_on(&format!("commit-{first}"));
        cx.run_until_parked();
        assert_eq!(claude.requests(), 2);
        assert!(cx.is_drawn("diff-group-0"));
    }

    #[gpui::test]
    fn a_pull_requests_commit_is_only_read(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (first, second) = checkout(root);
        let mut pr = pull_request("acme/widgets", 7, "Change things");
        pr.base_ref_oid = rev(root, "main");
        pr.head_ref_oid = second.clone();
        let mut detail = detail(&pr);
        let commit = |oid: &str, headline: &str| {
            TimelineItem::Commit(Commit {
                id: format!("PC_{headline}"),
                url: Some(format!("{}/commits/{oid}", pr.url)),
                oid: oid.to_owned(),
                short_oid: oid[..7].to_owned(),
                headline: headline.into(),
                author: Some("hubot".into()),
                committed_at: Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap(),
            })
        };
        detail.timeline = vec![commit(&first, "a"), commit(&second, "b")];
        detail.commit_count = 2;
        let github = Arc::new(FakeGithub::new());
        github.set_search(
            SectionKind::ReviewRequested.search_query(),
            vec![pr.clone()],
        );
        github.set_detail(pr.repository.parse().unwrap(), detail);
        let projects = vec![project(1, root, "acme/widgets")];
        let (tab, cx) = open_docket(cx, github.clone(), projects);
        cx.click_on(&pr.id);
        cx.click_on("pr-tab-commits");
        cx.click_on("commit-PC_b");
        cx.run_until_parked();
        assert!(cx.is_drawn("commit-view"));
        assert!(cx.is_drawn("commit-open-github"));
        assert_eq!(paths(&tab, DiffSlot::Commit, cx), ["b.txt"]);

        // Reviews are of the pull request's diff, so not from here.
        assert!(!cx.is_drawn("diff-review-approve"));
        assert!(tab.read_with(cx, |tab, cx| tab.reviewable(cx).is_none()));
        // Marking a file viewed here leaves GitHub's marks alone.
        cx.click_on("diff-viewed-0");
        assert_eq!(
            github.viewed_state(&pr.id, "b.txt"),
            FileViewedState::Unviewed
        );
        let viewed = |tab: &Entity<Tab>, slot, cx: &mut VisualTestContext| {
            tab.read_with(cx, |tab, cx| {
                let files = tab.diff_files_in(slot).unwrap();
                let DiffLoad::Ready(diff) = &files.diff else {
                    unreachable!()
                };
                let file = diff.files.iter().find(|f| f.path() == "b.txt").unwrap();
                tab.app.read(cx).viewed(&files.viewed_scope, file)
            })
        };
        assert_eq!(viewed(&tab, DiffSlot::Commit, cx), Viewed::Yes);

        cx.click_on("pr-tab-files");
        cx.run_until_parked();
        assert_eq!(viewed(&tab, DiffSlot::Main, cx), Viewed::No);
        assert!(cx.is_drawn("diff-review-approve"));
        let detail_tab = tab.read_with(cx, |tab, _| tab.detail.as_ref().unwrap().tab);
        assert_eq!(detail_tab, PrTab::Files);
    }
}
