//! A pull request's Files tab: changed files on the left, and every file's
//! changes in one unified, syntax-highlighted list.
//!
//! Rows are derived from the diff once per load (see `rows.rs`) and drawn
//! with a virtualized list, so only visible lines are laid out. Highlights
//! are looked up per visible line, and fill in as background highlighting
//! finishes.

mod file_list;
mod rows;

use std::collections::HashSet;
use std::ops::Range;

use gpui::{
    AnyElement, App, Context, Entity, FontWeight, HighlightStyle, ListHorizontalSizingBehavior,
    ScrollStrategy, SharedString, StyledText, UniformListScrollHandle, Window, div, prelude::*, px,
    uniform_list,
};

use self::file_list::{file_list, stats, status_icon};
use self::rows::{DiffRows, DisplayRow, Notice, TAB_WIDTH, build_rows};
use super::components::{
    Button, ButtonStyle, Callout, CalloutKind, Icon, IconName, IconSize, MONOSPACE, Spinner,
};
use super::theme::{Color, Theme};
use crate::diff::{
    DiffLine, Excerpt, FileContent, FileDiff, FileStatus, LineKind, PullRequestDiff,
};
use crate::state::{AppState, DiffLoad, PrDetail, PrFiles};

const ROW_HEIGHT: f32 = 20.;
const LINE_NUMBER_WIDTH: f32 = 48.;

pub struct DiffView {
    state: Entity<AppState>,
    /// The `PrFiles::revision` `rows` were built from.
    revision: Option<u64>,
    rows: DiffRows,
    collapsed: HashSet<usize>,
    /// The file at the top of the viewport, highlighted in the file list.
    current_file: Option<usize>,
    scroll: UniformListScrollHandle,
}

impl DiffView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            revision: None,
            rows: DiffRows::default(),
            collapsed: HashSet::new(),
            current_file: None,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Scrolls the diff to a file's header.
    pub fn reveal_file(&mut self, file: usize, cx: &mut Context<Self>) {
        if let Some(&row) = self.rows.file_headers.get(file) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Top);
            self.current_file = Some(file);
            cx.notify();
        }
    }

    fn toggle_collapsed(&mut self, file: usize, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&file) {
            self.collapsed.insert(file);
        }
        // Rebuild on the next render.
        self.revision = None;
        cx.notify();
    }

    /// Rebuilds the rows if the diff or the collapsed files changed.
    fn sync_rows(&mut self, cx: &App) {
        let Some(files) = files(self.state.read(cx)) else {
            return;
        };
        if self.revision == Some(files.revision) {
            return;
        }
        self.rows = match &files.diff {
            DiffLoad::Ready(diff) => build_rows(diff, &self.collapsed),
            _ => DiffRows::default(),
        };
        self.revision = Some(files.revision);
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let top_file = self.rows.rows.get(range.start).map(row_file);
        if top_file.is_some() && top_file != self.current_file {
            self.current_file = top_file;
            cx.notify();
        }

        let entity = cx.entity();
        let Some(PrFiles {
            diff: DiffLoad::Ready(diff),
            ..
        }) = files(self.state.read(cx))
        else {
            return Vec::new();
        };
        let theme = Theme::get(cx);
        self.rows.rows[range]
            .iter()
            .map(|row| render_row(*row, diff, &self.collapsed, &entity, theme))
            .collect()
    }
}

fn files(state: &AppState) -> Option<&PrFiles> {
    state.detail.as_ref()?.files.as_ref()
}

fn row_file(row: &DisplayRow) -> usize {
    match *row {
        DisplayRow::FileHeader { file }
        | DisplayRow::HunkHeader { file, .. }
        | DisplayRow::Line { file, .. }
        | DisplayRow::Notice { file, .. } => file,
        // Spacers sit just above the next file.
        DisplayRow::Spacer => 0,
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_rows(cx);
        let entity = cx.entity();
        let state = self.state.read(cx);
        let theme = Theme::get(cx);
        let Some(detail) = &state.detail else {
            return div().flex_1();
        };
        let Some(files) = &detail.files else {
            return div().flex_1();
        };

        let body = match &files.diff {
            DiffLoad::Loading => centered(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_color(theme.text_muted)
                    .child(Spinner::new("diff-loading"))
                    .child("Loading the diff from your checkout…"),
            )
            .into_any_element(),
            DiffLoad::Failed(err) => div()
                .p_4()
                .child(
                    Callout::new(CalloutKind::Error, err.clone()).title("Couldn't load the diff"),
                )
                .into_any_element(),
            DiffLoad::NoProject => no_project(detail, &self.state).into_any_element(),
            DiffLoad::Ready(diff) if diff.files.is_empty() => {
                centered(div().text_color(theme.text_muted).child("No changes.")).into_any_element()
            }
            DiffLoad::Ready(diff) => div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(file_list(diff, self.current_file, entity.clone(), cx))
                .child(
                    uniform_list(
                        "diff-rows",
                        self.rows.rows.len(),
                        cx.processor(Self::render_rows),
                    )
                    .flex_1()
                    .h_full()
                    .track_scroll(self.scroll.clone())
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .with_width_from_item(self.rows.widest),
                )
                .into_any_element(),
        };

        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(body)
    }
}

/// Status text near the top of the body, like the inbox's empty state.
fn centered(child: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(96.))
        .child(child)
}

fn no_project(detail: &PrDetail, state: &Entity<AppState>) -> impl IntoElement {
    let add = state.clone();
    let url = detail.summary.url.clone();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .child(
            Callout::new(
                CalloutKind::Warning,
                format!(
                    "Docket reads diffs from your local checkout. Add a project for {} to \
                     view this pull request here.",
                    detail.summary.repository
                ),
            )
            .title("No project for this repository"),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("diff-add-project", "Add Project…")
                        .icon(IconName::Plus)
                        .on_click(move |_, _, cx| {
                            add.update(cx, |state, cx| state.add_project(cx));
                        }),
                )
                .child(
                    Button::new("diff-no-project-github", "Open on GitHub")
                        .icon(IconName::ExternalLink)
                        .style(ButtonStyle::Secondary)
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                ),
        )
}

fn render_row(
    row: DisplayRow,
    diff: &PullRequestDiff,
    collapsed: &HashSet<usize>,
    view: &Entity<DiffView>,
    theme: &Theme,
) -> AnyElement {
    let base = div()
        .flex()
        .items_center()
        .h(px(ROW_HEIGHT))
        .w_full()
        .whitespace_nowrap();
    match row {
        DisplayRow::Spacer => base.into_any_element(),
        DisplayRow::FileHeader { file } => file_header(
            file,
            &diff.files[file],
            collapsed.contains(&file),
            view,
            theme,
        ),
        DisplayRow::HunkHeader { file, excerpt } => {
            let FileContent::Text(text) = &diff.files[file].content else {
                return base.into_any_element();
            };
            base.bg(theme.diff_hunk_bg)
                .text_color(theme.text_muted)
                .font_family(MONOSPACE)
                .text_xs()
                .child(div().flex_none().w(px(LINE_NUMBER_WIDTH * 2. + 16.)))
                .child(hunk_header(&text.excerpts[excerpt]))
                .into_any_element()
        }
        DisplayRow::Line { file, line } => diff_line(base, &diff.files[file], line, theme),
        DisplayRow::Notice { notice, .. } => base
            .pl(px(LINE_NUMBER_WIDTH * 2. + 16.))
            .text_xs()
            .text_color(theme.text_subtle)
            .child(match notice {
                Notice::Binary => "Binary file not shown.".to_owned(),
                Notice::Submodule => "Submodule changed.".to_owned(),
                Notice::TooLarge { bytes } => format!(
                    "File not shown: {:.1} MB is too large to display.",
                    bytes as f64 / (1024. * 1024.)
                ),
                Notice::NoChanges => "No content changes.".to_owned(),
            })
            .into_any_element(),
    }
}

fn file_header(
    ix: usize,
    file: &FileDiff,
    collapsed: bool,
    view: &Entity<DiffView>,
    theme: &Theme,
) -> AnyElement {
    let (icon, color) = status_icon(file.status);
    let path = match (&file.status, &file.old_path, &file.new_path) {
        (FileStatus::Renamed { .. } | FileStatus::Copied { .. }, Some(old), Some(new)) => {
            format!("{old} → {new}")
        }
        _ => file.path().to_owned(),
    };
    let view = view.clone();
    div()
        .id(("diff-file-header", ix))
        .flex()
        .items_center()
        .gap_2()
        .h(px(ROW_HEIGHT))
        .w_full()
        .px_2()
        .whitespace_nowrap()
        .bg(theme.surface)
        .border_y_1()
        .border_color(theme.border_subtle)
        .cursor_pointer()
        .hover(|style| style.bg(theme.element_hover))
        .child(
            Icon::new(if collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            })
            .size(IconSize::XSmall),
        )
        .child(Icon::new(icon).size(IconSize::XSmall).color(color))
        .child(
            div()
                .text_xs()
                .font_family(MONOSPACE)
                .font_weight(FontWeight::MEDIUM)
                .child(path),
        )
        .child(stats(file.additions, file.deletions, theme))
        .on_click(move |_, _, cx| {
            view.update(cx, |view, cx| view.toggle_collapsed(ix, cx));
        })
        .into_any_element()
}

/// `@@ -a,b +c,d @@`, numbered as Git does: 1-based, and an empty range is
/// numbered by the line before it.
fn hunk_header(excerpt: &Excerpt) -> String {
    let range = |r: &Range<u32>| match r.len() {
        0 => format!("{},0", r.start),
        1 => format!("{}", r.start + 1),
        len => format!("{},{len}", r.start + 1),
    };
    format!("@@ -{} +{} @@", range(&excerpt.old), range(&excerpt.new))
}

fn diff_line(base: gpui::Div, file: &FileDiff, line: DiffLine, theme: &Theme) -> AnyElement {
    let FileContent::Text(text) = &file.content else {
        return base.into_any_element();
    };
    // Context and added lines read from the new side, removed ones from
    // the old.
    let (snapshot, syntax, number) = match line.kind {
        LineKind::Removed => (text.old.as_ref(), text.old_syntax.as_ref(), line.old_line),
        LineKind::Context | LineKind::Added => {
            (text.new.as_ref(), text.new_syntax.as_ref(), line.new_line)
        }
    };
    let (Some(snapshot), Some(number)) = (snapshot, number) else {
        return base.into_any_element();
    };
    let range = snapshot.line_range(number);
    let spans: Vec<(Range<usize>, HighlightStyle)> = syntax
        .map(|syntax| {
            syntax
                .spans_in(range.clone())
                .map(|(range, id)| (range, theme.syntax.style(id)))
                .collect()
        })
        .unwrap_or_default();
    let (content, spans) = expand_tabs(&snapshot.text()[range], spans);

    let (bg, gutter, marker) = match line.kind {
        LineKind::Context => (None, None, " "),
        LineKind::Added => (
            Some(theme.diff_added_bg),
            Some(theme.diff_added_gutter),
            "+",
        ),
        LineKind::Removed => (
            Some(theme.diff_removed_bg),
            Some(theme.diff_removed_gutter),
            "−",
        ),
    };
    let line_number = |n: Option<u32>| {
        div()
            .flex_none()
            .w(px(LINE_NUMBER_WIDTH))
            .h_full()
            .flex()
            .items_center()
            .justify_end()
            .pr_2()
            .text_color(theme.text_subtle)
            .when_some(gutter, |el, gutter| el.bg(gutter))
            .children(n.map(|n| (n + 1).to_string()))
    };

    base.font_family(MONOSPACE)
        .text_xs()
        .when_some(bg, |el, bg| el.bg(bg))
        .child(line_number(line.old_line))
        .child(line_number(line.new_line))
        .child(
            div()
                .flex_none()
                .w(px(16.))
                .flex()
                .justify_center()
                .text_color(match line.kind {
                    LineKind::Context => theme.text_subtle,
                    LineKind::Added => theme.success,
                    LineKind::Removed => theme.danger,
                })
                .child(marker),
        )
        .child(
            div()
                .pr_4()
                .text_color(Color::Default.hsla(theme))
                .child(StyledText::new(content).with_highlights(spans)),
        )
        .into_any_element()
}

/// Expands tabs to spaces (GPUI doesn't lay them out as tab stops),
/// remapping highlight ranges to match.
fn expand_tabs(
    text: &str,
    spans: Vec<(Range<usize>, HighlightStyle)>,
) -> (SharedString, Vec<(Range<usize>, HighlightStyle)>) {
    if !text.contains('\t') {
        return (SharedString::from(text.to_owned()), spans);
    }
    let mut expanded = String::with_capacity(text.len() + TAB_WIDTH * 2);
    // Byte offset in `text` -> byte offset in `expanded`.
    let mut offsets = Vec::with_capacity(text.len() + 1);
    let mut column = 0;
    for c in text.chars() {
        let start = expanded.len();
        if c == '\t' {
            let width = TAB_WIDTH - column % TAB_WIDTH;
            expanded.extend(std::iter::repeat_n(' ', width));
            column += width;
        } else {
            expanded.push(c);
            column += 1;
        }
        offsets.extend(std::iter::repeat_n(start, c.len_utf8()));
    }
    offsets.push(expanded.len());
    let spans = spans
        .into_iter()
        .map(|(range, style)| (offsets[range.start]..offsets[range.end], style))
        .collect();
    (expanded.into(), spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_hunk_headers_like_git() {
        let header = |old: Range<u32>, new: Range<u32>| hunk_header(&Excerpt { old, new });
        assert_eq!(header(1..8, 1..9), "@@ -2,7 +2,8 @@");
        assert_eq!(header(0..0, 0..2), "@@ -0,0 +1,2 @@");
        assert_eq!(header(4..5, 4..5), "@@ -5 +5 @@");
    }

    #[test]
    fn expands_tabs_to_tab_stops() {
        let style = HighlightStyle::default();
        let (text, spans) = expand_tabs("a\tbc\td", vec![(2..4, style)]);
        assert_eq!(text.as_ref(), "a   bc  d");
        assert_eq!(spans[0].0, 4..6);
    }
}
