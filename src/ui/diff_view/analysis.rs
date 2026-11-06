//! The diff grouped by change: its groups' rows, the toolbar that switches
//! layouts and shows the analysis's progress, and the form to connect
//! Claude Code.

use std::collections::HashSet;

use gpui::{AnyElement, Context, Div, Entity, FontWeight, SharedString, div, prelude::*, px};

use super::rows::{GroupLine, IMPACT_LABEL};
use super::{DiffView, files};
use crate::ai::DiffAnalysis;
use crate::config::ANALYSIS_MODEL_NAME;
use crate::state::{AnalysisLoad, ClaudeStatus, DiffLoad};
use crate::ui::components::{
    Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconButton, IconName, IconSize,
    MONOSPACE, Spinner, TestSelector as _,
};
use crate::ui::theme::{Color, Theme};

/// Where a group's text starts in the diff, under its title rather than
/// its icon.
const GROUP_INDENT: f32 = 32.;

impl DiffView {
    /// Above the diff: file by file or grouped by change, and how grouping
    /// is going. Until Claude Code is connected, an offer to connect it.
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tab = self.state.read(cx);
        let files = files(tab)?;
        let DiffLoad::Ready(diff) = &files.diff else {
            return None;
        };
        if diff.files.is_empty() {
            return None;
        }
        let app = tab.app.read(cx);
        let theme = Theme::get(cx);
        let view = cx.entity();
        let bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(36.))
            .px_2()
            .border_b_1()
            .border_color(theme.border_subtle)
            .text_xs()
            .text_color(theme.text_muted);
        let status =
            |label: SharedString| div().flex().items_center().gap_1p5().px_2().child(label);

        let bar = match &app.claude {
            // Says nothing until it's known.
            ClaudeStatus::Restoring => return None,
            ClaudeStatus::Connecting => bar
                .child(Spinner::new("diff-claude-connecting"))
                .child(status("Connecting to Claude Code…".into())),
            ClaudeStatus::Disconnected { .. } => bar.child(
                Button::new("diff-group-connect", "Group Changes with Claude…")
                    .icon(IconName::Sparkles)
                    .style(ButtonStyle::Ghost)
                    .size(ButtonSize::Compact)
                    .on_click(move |_, _, cx| {
                        view.update(cx, |view, cx| {
                            view.connecting = true;
                            cx.notify();
                        });
                    }),
            ),
            ClaudeStatus::Connected(_) => {
                let grouped = app.group_changes;
                let layout = |id: &'static str, label: &'static str, icon, group: bool| {
                    let view = view.clone();
                    Button::new(id, label)
                        .icon(icon)
                        .size(ButtonSize::Compact)
                        .style(if grouped == group {
                            ButtonStyle::Secondary
                        } else {
                            ButtonStyle::Ghost
                        })
                        .on_click(move |_, _, cx| {
                            view.update(cx, |view, cx| view.set_group_changes(group, cx));
                        })
                };
                let progress = grouped.then(|| match &files.analysis {
                    AnalysisLoad::Running => status(
                        format!("Grouping changes with {ANALYSIS_MODEL_NAME} in Claude Code…")
                            .into(),
                    )
                    .child(Spinner::new("diff-analysis-running"))
                    .into_any_element(),
                    AnalysisLoad::Ready(analysis) => status(
                        match analysis.groups.len() {
                            1 => format!("1 change, grouped by {ANALYSIS_MODEL_NAME}"),
                            n => format!("{n} changes, grouped by {ANALYSIS_MODEL_NAME}"),
                        }
                        .into(),
                    )
                    .text_color(theme.text_subtle)
                    .into_any_element(),
                    // The callout below says why.
                    AnalysisLoad::Failed(_) | AnalysisLoad::Idle => div().into_any_element(),
                });
                bar.child(layout(
                    "diff-layout-files",
                    "By File",
                    IconName::FileDiff,
                    false,
                ))
                .child(layout(
                    "diff-layout-changes",
                    "By Change",
                    IconName::Sparkles,
                    true,
                ))
                .child(div().flex_1())
                .children(progress)
            }
        };
        Some(bar.into_any_element())
    }

    /// Why the diff couldn't be grouped, with a way to try again.
    pub(super) fn render_analysis_error(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tab = self.state.read(cx);
        let AnalysisLoad::Failed(error) = &files(tab)?.analysis else {
            return None;
        };
        if !tab.app.read(cx).groups_changes() {
            return None;
        }
        let state = self.state.clone();
        Some(
            div()
                .p_3()
                .child(
                    Callout::new(CalloutKind::Error, error.clone())
                        .title("Couldn't group the changes")
                        .action(
                            Button::new("diff-analysis-retry", "Try Again")
                                .icon(IconName::RefreshCw)
                                .style(ButtonStyle::Secondary)
                                .size(ButtonSize::Compact)
                                .on_click(move |_, _, cx| {
                                    state.update(cx, |state, cx| state.retry_analysis(cx));
                                }),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The form to connect Claude Code.
    pub(super) fn render_connect(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.connecting {
            return None;
        }
        let (checking, error) = match &self.state.read(cx).app.read(cx).claude {
            ClaudeStatus::Connected(_) => {
                self.connecting = false;
                return None;
            }
            ClaudeStatus::Connecting => (true, None),
            ClaudeStatus::Disconnected { error } => (false, error.clone()),
            ClaudeStatus::Restoring => (false, None),
        };
        let theme = Theme::get(cx);
        let view = cx.entity();
        let app = self.state.read(cx).app.clone();
        let note = |text: &'static str| div().text_xs().text_color(theme.text_muted).child(text);
        let code = |text: &'static str| {
            div()
                .font_family(MONOSPACE)
                .text_color(theme.text)
                .child(text)
        };

        let setup = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(theme.text_muted)
            .child("Docket runs your")
            .child(code("claude"))
            .child("and uses its sign-in. Install Claude Code and sign in with")
            .child(code("claude auth login"))
            .child("in a terminal first.");
        let actions = div().flex().items_center().gap_2().child(
            Button::new("claude-connect", "Use Claude Code")
                .icon(IconName::Terminal)
                .size(ButtonSize::Compact)
                .disabled(checking)
                .on_click(move |_, _, cx| app.update(cx, |app, cx| app.connect_claude(cx))),
        );
        let actions = match checking {
            true => actions
                .child(Spinner::new("claude-checking"))
                .child(note("Checking Claude Code's sign-in…")),
            false => actions,
        };

        Some(
            div()
                .test_selector(|| "claude-connect-form".to_owned())
                .flex_none()
                .m_3()
                .p_4()
                .flex()
                .flex_col()
                .gap_4()
                .max_w(px(640.))
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.elevated)
                .text_sm()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Icon::new(IconName::Sparkles).color(Color::Accent))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Group changes with Claude"),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("claude-connect-cancel", "Not Now")
                                .style(ButtonStyle::Ghost)
                                .size(ButtonSize::Compact)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |view, cx| {
                                        view.connecting = false;
                                        cx.notify();
                                    });
                                }),
                        ),
                )
                .child(note(
                    "Claude Haiku 5.5 groups the diff's related changes, across files, and \
                     summarizes what each one changes and what else it affects. It runs in \
                     Claude Code, in a copy of the repository at the diff's head, and reads the \
                     code around the changes as it needs to, with read-only file tools. Docket \
                     keeps each analysis until the diff changes.",
                ))
                .child(setup)
                .child(actions)
                .children(error.map(|error| Callout::new(CalloutKind::Error, error)))
                .into_any_element(),
        )
    }
}

/// A group's title above its hunks in the diff, with its place among the
/// groups and its size. Its summary and impact follow in `group_line`s.
pub(super) fn group_header(
    base: Div,
    group: usize,
    analysis: &DiffAnalysis,
    theme: &Theme,
) -> AnyElement {
    let Some(change) = analysis.groups.get(group) else {
        return base.into_any_element();
    };
    let files: HashSet<usize> = change.hunks.iter().map(|hunk| hunk.file).collect();
    let size = match (change.hunks.len(), files.len()) {
        (1, _) => "1 hunk".to_owned(),
        (hunks, 1) => format!("{hunks} hunks in 1 file"),
        (hunks, files) => format!("{hunks} hunks in {files} files"),
    };
    base.test_selector(|| format!("diff-group-{group}"))
        .gap_2()
        .px_3()
        .bg(theme.accent_subtle)
        .border_t_1()
        .border_color(theme.border_subtle)
        .child(
            Icon::new(IconName::Sparkles)
                .size(IconSize::XSmall)
                .color(Color::Accent),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(change.title.clone()),
        )
        .child(div().text_xs().text_color(theme.text_subtle).child(format!(
            "{} of {} · {size}",
            group + 1,
            analysis.groups.len()
        )))
        .into_any_element()
}

/// A line of a group's summary, or of its impact, under its header in the
/// diff.
pub(super) fn group_line(base: Div, line: &GroupLine, theme: &Theme) -> AnyElement {
    let text = match line.labeled {
        true => line.text.strip_prefix(IMPACT_LABEL).unwrap_or(&line.text),
        false => &line.text,
    };
    base.pl(px(GROUP_INDENT))
        .pr_4()
        .bg(theme.accent_subtle)
        .text_xs()
        .text_color(if line.impact {
            theme.text_muted
        } else {
            theme.text
        })
        .when(line.labeled, |el| {
            el.child(
                div()
                    .flex_none()
                    .pr_1()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(IMPACT_LABEL.trim_end().to_owned()),
            )
        })
        .child(text.to_owned())
        .into_any_element()
}

/// A group in the file tree: its title, which scrolls the diff to it, and
/// a chevron that collapses its files. Its summary is in the diff.
pub(super) fn group_row(
    view: &Entity<DiffView>,
    group: usize,
    analysis: &DiffAnalysis,
    expanded: bool,
    current: bool,
    theme: &Theme,
) -> AnyElement {
    let Some(change) = analysis.groups.get(group) else {
        return div().into_any_element();
    };
    let toggle = {
        let view = view.clone();
        IconButton::new(
            ("diff-group-toggle", group),
            if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            },
        )
        .size(ButtonSize::Compact)
        .tooltip(if expanded { "Collapse" } else { "Expand" })
        .on_click(move |_, _, cx| {
            // Not a click on the row, which scrolls to the group.
            cx.stop_propagation();
            view.update(cx, |view, cx| view.toggle_group(group, cx));
        })
    };
    let view = view.clone();
    div()
        .test_selector(|| format!("diff-tree-group-{group}"))
        .id(("diff-tree-group", group))
        .flex()
        .items_start()
        .gap_1()
        .mx_2()
        .mt(px(if group == 0 { 0. } else { 6. }))
        .px_1()
        .py_0p5()
        .rounded_md()
        .border_l_2()
        .border_color(if current {
            theme.accent
        } else {
            gpui::transparent_black()
        })
        .cursor_pointer()
        .hover(|style| style.bg(theme.element_hover))
        .child(toggle)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pt(px(4.))
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(change.title.clone()),
        )
        .on_click(move |_, _, cx| {
            view.update(cx, |view, cx| view.reveal_group(group, cx));
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{Entity, TestAppContext, VisualTestContext};
    use serde_json::json;

    use crate::ai::{ClaudeCode, FakeClaude};
    use crate::github::FakeGithub;
    use crate::state::{AnalysisLoad, AppState, Tab};
    use crate::test_support::{
        self, VisualTestContextExt as _, git_checkout, git_commit, open_docket, project,
    };

    fn numbered(lines: u32) -> String {
        (1..=lines).map(|n| format!("line {n}\n")).collect()
    }

    /// A checkout whose `feature` branch changes two files, the first in
    /// two places: three hunks.
    fn checkout(root: &std::path::Path) {
        git_checkout(root, &[("a.txt", &numbered(30)), ("b.txt", "one\n")]);
        test_support::git(root, &["checkout", "--quiet", "feature"]);
        let a = numbered(30)
            .replace("line 2\n", "line two\n")
            .replace("line 25\n", "line twenty-five\n");
        std::fs::write(root.join("a.txt"), a).unwrap();
        std::fs::write(root.join("b.txt"), "two\n").unwrap();
        git_commit(root, "Change things", "2026-01-02T00:00:00Z");
        test_support::git(root, &["checkout", "--quiet", "main"]);
    }

    fn connect(cx: &mut VisualTestContext, fake: &Arc<FakeClaude>) {
        let claude = ClaudeCode::fake(fake.clone());
        cx.update(|_, cx| {
            AppState::global(cx).update(cx, |app, cx| app.connect_claude_for_test(claude, cx));
        });
        cx.run_until_parked();
    }

    fn group_titles(tab: &Entity<Tab>, cx: &mut VisualTestContext) -> Vec<String> {
        tab.read_with(cx, |tab, _| match &tab.diff_files().unwrap().analysis {
            AnalysisLoad::Ready(analysis) => {
                analysis.groups.iter().map(|g| g.title.clone()).collect()
            }
            _ => Vec::new(),
        })
    }

    #[gpui::test]
    fn groups_a_diffs_changes_until_it_changes(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        checkout(root);
        let projects = vec![project(1, root, "acme/widgets")];
        let (tab, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        let claude = Arc::new(FakeClaude::new());
        claude.set_answer(json!({ "groups": [
            { "title": "Rename lines", "summary": "Spells out two numbers.",
              "impact": "Anything matching the old text.", "hunks": ["H1", "H3"] },
            { "title": "Tidy", "summary": "Another number.", "impact": "", "hunks": ["H2"] },
        ]}));
        connect(cx, &claude);

        // The diff is grouped as it opens.
        cx.click_on("branch-1-feature");
        assert_eq!(claude.requests(), 1);
        let prompt = claude.last_prompt().unwrap();
        assert!(prompt.contains("## a.txt (modified, +2 -2)"), "{prompt}");
        assert!(
            prompt.contains("### H3 @@ -1 +1 @@\n-one\n+two"),
            "{prompt}"
        );
        // Claude reads the code at the branch's tip, in a worktree of
        // Docket's, while the checkout stays on main.
        let dir = claude.last_dir().unwrap();
        assert_ne!(dir, root);
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "two\n");
        assert_eq!(
            std::fs::read_to_string(root.join("b.txt")).unwrap(),
            "one\n"
        );
        assert_eq!(group_titles(&tab, cx), ["Rename lines", "Tidy"]);
        assert!(cx.is_drawn("diff-group-0"));
        assert!(cx.is_drawn("diff-group-1"));
        // The tree lists each group with its files: a.txt under both.
        assert!(cx.is_drawn("diff-tree-group-0"));
        assert!(cx.is_drawn("diff-group-0-file-0"));
        assert!(cx.is_drawn("diff-group-0-file-1"));
        assert!(cx.is_drawn("diff-group-1-file-0"));
        assert!(!cx.is_drawn("diff-file-0"), "not file by file");
        // Collapsing a group hides its files.
        cx.click_on("diff-group-toggle-0");
        assert!(!cx.is_drawn("diff-group-0-file-0"));
        assert!(cx.is_drawn("diff-tree-group-1"));
        cx.click_on("diff-group-toggle-0");
        assert!(cx.is_drawn("diff-group-0-file-0"));

        // File by file, then grouped again, without asking again.
        cx.click_on("diff-layout-files");
        assert!(!cx.is_drawn("diff-group-0"));
        assert!(!cx.is_drawn("diff-tree-group-0"));
        assert!(cx.is_drawn("diff-file-0"));
        cx.click_on("diff-layout-changes");
        assert!(cx.is_drawn("diff-group-0"));

        // Opening it again reuses the analysis.
        cx.simulate_keystrokes("escape");
        cx.click_on("branch-1-feature");
        assert_eq!(claude.requests(), 1);
        assert!(cx.is_drawn("diff-group-0"));

        // A new commit changes the diff, so it's asked about again.
        test_support::git(root, &["checkout", "--quiet", "feature"]);
        std::fs::write(root.join("c.txt"), "new\n").unwrap();
        git_commit(root, "Add c", "2026-01-03T00:00:00Z");
        test_support::git(root, &["checkout", "--quiet", "main"]);
        cx.simulate_keystrokes("cmd-r");
        cx.run_until_parked();
        assert_eq!(claude.requests(), 2);
        assert!(claude.last_prompt().unwrap().contains("## c.txt (added"));
        // It wasn't in a group the fake answered with.
        assert_eq!(
            group_titles(&tab, cx),
            ["Rename lines", "Tidy", "Other changes"]
        );
    }

    #[gpui::test]
    fn offers_to_connect_and_retries_failures(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        checkout(dir.path());
        let projects = vec![project(1, dir.path(), "acme/widgets")];
        let (tab, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);

        // Not connected, the diff is file by file, with an offer.
        cx.click_on("branch-1-feature");
        assert!(!cx.is_drawn("diff-group-0"));
        cx.click_on("diff-group-connect");
        assert!(cx.is_drawn("claude-connect-form"));
        assert!(cx.is_drawn("claude-connect"));
        cx.click_on("claude-connect-cancel");
        assert!(!cx.is_drawn("claude-connect-form"));

        // Connecting groups the open diff; a failure says so.
        let claude = Arc::new(FakeClaude::new());
        claude.set_error("overloaded");
        connect(cx, &claude);
        assert!(tab.read_with(cx, |tab, _| matches!(
            tab.diff_files().unwrap().analysis,
            AnalysisLoad::Failed(_)
        )));
        assert!(cx.is_drawn("diff-analysis-retry"));

        claude.set_answer(json!({ "groups": [
            { "title": "Everything", "summary": "All of it.", "impact": "None.",
              "hunks": ["H1", "H2", "H3"] },
        ]}));
        cx.click_on("diff-analysis-retry");
        assert_eq!(claude.requests(), 2);
        assert_eq!(group_titles(&tab, cx), ["Everything"]);
        assert!(cx.is_drawn("diff-group-0"));
        assert!(!cx.is_drawn("diff-analysis-retry"));
    }
}
