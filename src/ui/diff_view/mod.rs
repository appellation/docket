//! A pull request's Files tab: changed files on the left, and every file's
//! changes in one unified, syntax-highlighted list.
//!
//! Rows are derived from the diff once per load (see `rows.rs`) and drawn
//! with a virtualized list, so only visible lines are laid out. Highlights
//! are looked up per visible line, and fill in as background highlighting
//! finishes.
//!
//! Code can be selected and copied like text (`selection.rs`), and resting
//! the mouse on it shows the language server's hover (`hover.rs`). Both map
//! the mouse to text through the layouts of the lines last drawn.
//!
//! Hunk headers name the scopes their excerpt is in but doesn't show the
//! start of, like the function name `git diff` puts after `@@`, from the
//! file's outline. Each name hovers like code.

mod file_list;
mod hover;
mod rows;
mod selection;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::Duration;

use gpui::{
    AnyElement, App, ClipboardItem, Context, CursorStyle, DispatchPhase, Entity, FocusHandle,
    FontWeight, HighlightStyle, KeyBinding, ListHorizontalSizingBehavior, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollStrategy, StyledText, Task,
    TextLayout, UniformListScrollHandle, Window, actions, canvas, div, prelude::*, px,
    uniform_list,
};

use self::file_list::{file_list, stats, status_icon};
use self::hover::HoverState;
use self::rows::{DiffRows, DisplayRow, Notice, build_rows};
use self::selection::{
    DiffPoint, Granularity, Selection, Span, TabMap, expand_tabs, with_background, word_range,
};
use super::components::{
    Button, ButtonStyle, Callout, CalloutKind, Icon, IconName, IconSize, MONOSPACE, Spinner,
};
use super::theme::{Color, Theme};
use crate::diff::{
    DiffLine, Excerpt, FileContent, FileDiff, FileStatus, LineKind, PullRequestDiff, TextContent,
    TextSnapshot,
};
use crate::lsp::{Outline, Position, SymbolKind};
use crate::state::{AppState, DiffLoad, PrDetail, PrFiles};
use crate::syntax::{HighlightId, SyntaxSnapshot};

actions!(diff_view, [Copy, SelectAll, ClearSelection]);

const KEY_CONTEXT: &str = "DiffView";
const ROW_HEIGHT: f32 = 20.;
const LINE_NUMBER_WIDTH: f32 = 48.;
/// How often to scroll while a selection is dragged past the list's edge.
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(16);

pub struct DiffView {
    state: Entity<AppState>,
    /// The `PrFiles::revision` `rows` were built from.
    revision: Option<u64>,
    rows: DiffRows,
    collapsed: HashSet<usize>,
    /// The file at the top of the viewport, highlighted in the file list.
    current_file: Option<usize>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    /// The line rows laid out in the last frame, by row.
    line_hits: HashMap<usize, LineHit>,
    /// The hunk headers with scopes laid out in the last frame, by row.
    scope_hits: HashMap<usize, ScopeHit>,
    selection: Option<Selection>,
    /// Set from pressing on a line until the button comes up.
    dragging: bool,
    drag_position: Point<Pixels>,
    autoscrolling: bool,
    _autoscroll_task: Option<Task<()>>,
    hover: HoverState,
}

/// A drawn line's text layout, for mapping window positions to offsets in
/// its source text.
struct LineHit {
    layout: TextLayout,
    tabs: TabMap,
}

impl LineHit {
    /// The source offset whose boundary is nearest `x`, for selecting.
    fn closest_offset(&self, x: Pixels) -> usize {
        let Some(line) = self.layout.line_layout_for_index(0) else {
            return 0;
        };
        let x = x - self.layout.bounds().left();
        self.tabs
            .to_source(line.unwrapped_layout.closest_index_for_x(x))
    }

    /// The source offset of the character under `x`, if there is one.
    fn offset_under(&self, x: Pixels) -> Option<usize> {
        let line = self.layout.line_layout_for_index(0)?;
        let x = x - self.layout.bounds().left();
        if x < px(0.) {
            return None;
        }
        Some(self.tabs.source_at(line.unwrapped_layout.index_for_x(x)?))
    }

    /// The window x of a source offset.
    fn x_for_offset(&self, offset: usize) -> Pixels {
        let left = self.layout.bounds().left();
        self.layout.line_layout_for_index(0).map_or(left, |line| {
            left + line
                .unwrapped_layout
                .x_for_index(self.tabs.to_display(offset))
        })
    }
}

/// A hunk header's scope names as laid out, for hovering them.
struct ScopeHit {
    layout: TextLayout,
    scopes: Vec<ScopeName>,
}

struct ScopeName {
    /// Where the name is in the header's scope text.
    range: Range<usize>,
    /// Where it's declared, to ask the server about it.
    position: Position,
}

impl ScopeHit {
    /// The scope whose name is under `x`.
    fn scope_under(&self, x: Pixels) -> Option<&ScopeName> {
        let line = self.layout.line_layout_for_index(0)?;
        let x = x - self.layout.bounds().left();
        if x < px(0.) {
            return None;
        }
        let index = line.unwrapped_layout.index_for_x(x)?;
        self.scopes
            .iter()
            .find(|scope| scope.range.contains(&index))
    }

    fn x_for_offset(&self, offset: usize) -> Pixels {
        let left = self.layout.bounds().left();
        self.layout.line_layout_for_index(0).map_or(left, |line| {
            left + line.unwrapped_layout.x_for_index(offset)
        })
    }
}

/// A row's text as laid out, if it has any to point at.
enum RowHit {
    Line(LineHit),
    Scopes(ScopeHit),
}

/// What's marked on the visible lines.
struct Marks<'a> {
    selection: Option<&'a Selection>,
    /// The hovered symbol: its row and source range.
    hover: Option<(usize, Range<usize>)>,
}

impl DiffView {
    pub fn bind_keys(cx: &mut App) {
        let context = Some(KEY_CONTEXT);
        cx.bind_keys([
            KeyBinding::new("cmd-c", Copy, context),
            KeyBinding::new("cmd-a", SelectAll, context),
            KeyBinding::new("escape", ClearSelection, context),
        ]);
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            revision: None,
            rows: DiffRows::default(),
            collapsed: HashSet::new(),
            current_file: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            line_hits: HashMap::new(),
            scope_hits: HashMap::new(),
            selection: None,
            dragging: false,
            drag_position: Point::default(),
            autoscrolling: false,
            _autoscroll_task: None,
            hover: HoverState::default(),
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
        // Rows moved, so positions in them mean something else now.
        self.selection = None;
        self.line_hits.clear();
        self.scope_hits.clear();
        self.hover = HoverState::default();
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

        // The list also renders a row to measure it before the visible
        // ones, so only the last call's lines are on screen.
        self.line_hits.clear();
        self.scope_hits.clear();
        let entity = cx.entity();
        let Some(files) = files(self.state.read(cx)) else {
            return Vec::new();
        };
        let DiffLoad::Ready(diff) = &files.diff else {
            return Vec::new();
        };
        let theme = Theme::get(cx);
        let marks = Marks {
            selection: self.selection.as_ref(),
            hover: self.hover.marked(),
        };
        range
            .map(|ix| {
                let (element, hit) = render_row(
                    ix,
                    self.rows.rows[ix],
                    files,
                    diff,
                    &self.collapsed,
                    &entity,
                    &marks,
                    theme,
                );
                match hit {
                    Some(RowHit::Line(hit)) => {
                        self.line_hits.insert(ix, hit);
                    }
                    Some(RowHit::Scopes(hit)) => {
                        self.scope_hits.insert(ix, hit);
                    }
                    None => {}
                }
                element
            })
            .collect()
    }

    /// The source text of a line row.
    fn line_text<'a>(&self, row: usize, cx: &'a App) -> Option<&'a str> {
        let DisplayRow::Line { file, line } = *self.rows.rows.get(row)? else {
            return None;
        };
        let DiffLoad::Ready(diff) = &files(self.state.read(cx))?.diff else {
            return None;
        };
        let (text, _, number) = line_side(&diff.files[file], line)?;
        Some(text.line(number))
    }

    fn list_bounds(&self) -> gpui::Bounds<Pixels> {
        self.scroll.0.borrow().base_handle.bounds()
    }

    /// The row at window y `y`. Rows are all one height, so this holds
    /// above and below the list too.
    fn row_at(&self, y: Pixels) -> isize {
        let state = self.scroll.0.borrow();
        let handle = &state.base_handle;
        ((y - handle.bounds().top() - handle.offset().y) / px(ROW_HEIGHT)).floor() as isize
    }

    /// The window y of a row's top.
    fn row_top(&self, row: usize) -> Pixels {
        let state = self.scroll.0.borrow();
        let handle = &state.base_handle;
        handle.bounds().top() + handle.offset().y + px(ROW_HEIGHT) * row
    }

    /// The text position at `position`, if it's over a drawn line. With
    /// `clamp`, positions above or below the drawn lines land on the
    /// first or last of them, for dragging past the edge.
    fn point_at(&self, position: Point<Pixels>, clamp: bool) -> Option<DiffPoint> {
        let row = self.row_at(position.y);
        let row = if clamp {
            let first = *self.line_hits.keys().min()? as isize;
            let last = *self.line_hits.keys().max()? as isize;
            row.clamp(first, last) as usize
        } else {
            usize::try_from(row).ok()?
        };
        let hit = self.line_hits.get(&row)?;
        Some(DiffPoint {
            row,
            offset: hit.closest_offset(position.x),
        })
    }

    /// What a click at `point` selects.
    fn span_at(&self, point: DiffPoint, granularity: Granularity, cx: &App) -> Span {
        let text = self.line_text(point.row, cx).unwrap_or_default();
        let at = |offset| DiffPoint {
            row: point.row,
            offset,
        };
        match granularity {
            Granularity::Character => (point, point),
            Granularity::Word => {
                let word = word_range(text, point.offset);
                (at(word.start), at(word.end))
            }
            Granularity::Line => (at(0), at(text.len())),
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.dismiss_hover(cx);
        let Some(point) = self.point_at(event.position, false) else {
            self.selection = None;
            cx.notify();
            return;
        };
        let granularity = Granularity::for_click_count(event.click_count);
        let span = self.span_at(point, granularity, cx);
        match &mut self.selection {
            Some(selection) if event.modifiers.shift => selection.extend_to(span),
            _ => self.selection = Some(Selection::new(span, granularity)),
        }
        self.dragging = true;
        self.drag_position = event.position;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.dragging && event.pressed_button.is_none() {
            self.update_hover(event.position, cx);
        }
    }

    /// Mouse moves anywhere in the window while dragging a selection.
    fn drag_to(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        // The button came up somewhere we didn't hear about.
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        self.drag_position = event.position;
        self.extend_drag(cx);

        let bounds = self.list_bounds();
        let outside = event.position.y < bounds.top() || event.position.y > bounds.bottom();
        if outside && !self.autoscrolling {
            self.autoscrolling = true;
            self._autoscroll_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(AUTOSCROLL_INTERVAL).await;
                    match this.update(cx, |this, cx| this.autoscroll(cx)) {
                        Ok(true) => {}
                        _ => break,
                    }
                }
            }));
        }
    }

    fn extend_drag(&mut self, cx: &mut Context<Self>) {
        let Some(point) = self.point_at(self.drag_position, true) else {
            return;
        };
        let Some(granularity) = self.selection.as_ref().map(|s| s.granularity) else {
            return;
        };
        let span = self.span_at(point, granularity, cx);
        if let Some(selection) = &mut self.selection {
            selection.extend_to(span);
        }
        cx.notify();
    }

    /// Scrolls toward a drag past the list's edge, faster the further past
    /// it is. Returns whether to keep going.
    fn autoscroll(&mut self, cx: &mut Context<Self>) -> bool {
        let bounds = self.list_bounds();
        let y = self.drag_position.y;
        let past = if y < bounds.top() {
            y - bounds.top()
        } else if y > bounds.bottom() {
            y - bounds.bottom()
        } else {
            px(0.)
        };
        if !self.dragging || past == px(0.) {
            self.autoscrolling = false;
            return false;
        }
        let max_step = px(ROW_HEIGHT * 2.);
        let step = past.clamp(-max_step, max_step) * 0.5;
        {
            let state = self.scroll.0.borrow();
            let handle = &state.base_handle;
            let mut offset = handle.offset();
            offset.y = (offset.y - step).clamp(-handle.max_offset().height, px(0.));
            handle.set_offset(offset);
        }
        self.extend_drag(cx);
        cx.notify();
        true
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        self.dragging = false;
        self.autoscrolling = false;
        self._autoscroll_task = None;
        cx.notify();
    }

    fn selected_text(&self, selection: &Selection, cx: &App) -> String {
        let (start, end) = selection.range();
        let mut lines = Vec::new();
        for row in start.row..=end.row.min(self.rows.rows.len().saturating_sub(1)) {
            let Some(text) = self.line_text(row, cx) else {
                continue;
            };
            match selection.in_row(row, text.len()) {
                Some((range, _)) => lines.push(&text[range]),
                // Ending at the start of a line copies the newline before it.
                None if row == end.row && row > start.row => lines.push(""),
                None => {}
            }
        }
        lines.join("\n")
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = self.selection.as_ref().filter(|s| !s.is_empty()) else {
            return;
        };
        let text = self.selected_text(selection, cx);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let is_line = |row: &DisplayRow| matches!(row, DisplayRow::Line { .. });
        let first = self.rows.rows.iter().position(is_line);
        let last = self.rows.rows.iter().rposition(is_line);
        let (Some(first), Some(last)) = (first, last) else {
            return;
        };
        let len = self.line_text(last, cx).map_or(0, str::len);
        let span = (
            DiffPoint {
                row: first,
                offset: 0,
            },
            DiffPoint {
                row: last,
                offset: len,
            },
        );
        self.selection = Some(Selection::new(span, Granularity::Line));
        cx.notify();
    }

    /// Escape dismisses the hover, then the selection, then falls through
    /// to closing the pull request.
    fn clear_selection(&mut self, _: &ClearSelection, _: &mut Window, cx: &mut Context<Self>) {
        if self.hover.is_shown() {
            self.dismiss_hover(cx);
        } else if self.selection.take().is_some_and(|s| !s.is_empty()) {
            cx.notify();
        } else {
            cx.propagate();
        }
    }
}

/// While dragging a selection, follows the mouse outside the list too.
fn drag_listeners(view: Entity<DiffView>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            window.on_mouse_event({
                let view = view.clone();
                move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        view.update(cx, |view, cx| view.drag_to(event, cx));
                    }
                }
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    view.update(cx, |view, cx| view.end_drag(cx));
                }
            });
        },
    )
    .absolute()
    .size_full()
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
        let hover = self.render_hover(cx);
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
                .child(file_list(
                    &self.state,
                    files,
                    diff,
                    self.current_file,
                    entity.clone(),
                    cx,
                ))
                .child(
                    div()
                        .id("diff-body")
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                        .on_mouse_move(cx.listener(Self::on_mouse_move))
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            if !hovered {
                                this.mouse_left(cx);
                            }
                        }))
                        .on_scroll_wheel(cx.listener(|this, _, _, cx| this.dismiss_hover(cx)))
                        .child(
                            uniform_list(
                                "diff-rows",
                                self.rows.rows.len(),
                                cx.processor(Self::render_rows),
                            )
                            .size_full()
                            .track_scroll(self.scroll.clone())
                            .with_horizontal_sizing_behavior(
                                ListHorizontalSizingBehavior::Unconstrained,
                            )
                            .with_width_from_item(self.rows.widest),
                        )
                        .when(self.dragging, |el| el.child(drag_listeners(entity.clone()))),
                )
                .into_any_element(),
        };

        div()
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::clear_selection))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(body)
            .children(hover)
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

#[allow(clippy::too_many_arguments)]
fn render_row(
    ix: usize,
    row: DisplayRow,
    files: &PrFiles,
    diff: &PullRequestDiff,
    collapsed: &HashSet<usize>,
    view: &Entity<DiffView>,
    marks: &Marks,
    theme: &Theme,
) -> (AnyElement, Option<RowHit>) {
    let base = div()
        .flex()
        .items_center()
        .h(px(ROW_HEIGHT))
        .w_full()
        .whitespace_nowrap();
    if let DisplayRow::Line { file, line } = row {
        let (element, hit) = diff_line(base, ix, &diff.files[file], line, marks, theme);
        return (element, hit.map(RowHit::Line));
    }
    let element = match row {
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
                return (base.into_any_element(), None);
            };
            let excerpt = &text.excerpts[excerpt];
            let scopes = files
                .outline(file)
                .and_then(|outline| hunk_scopes(ix, text, excerpt, outline, marks, theme));
            let (scopes, hit) = scopes.unzip();
            let element = base
                .bg(theme.diff_hunk_bg)
                .text_color(theme.text_muted)
                .font_family(MONOSPACE)
                .text_xs()
                .child(div().flex_none().w(px(LINE_NUMBER_WIDTH * 2. + 16.)))
                .child(hunk_header(excerpt))
                .children(scopes.map(|scopes| div().pl_3().child(scopes)))
                .into_any_element();
            return (element, hit.map(RowHit::Scopes));
        }
        DisplayRow::Line { .. } => unreachable!("rendered above"),
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
    };
    (element, None)
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

/// The scopes `excerpt` is in whose names are above it, outermost first,
/// as `impl Foo › bar`, colored as the highlighter colors those kinds of
/// names.
fn hunk_scopes(
    row: usize,
    text: &TextContent,
    excerpt: &Excerpt,
    outline: &Outline,
    marks: &Marks,
    theme: &Theme,
) -> Option<(StyledText, ScopeHit)> {
    let (line, visible_from) = text.scope_anchor(excerpt)?;
    let symbols = outline.hidden_scopes(line, visible_from);
    if symbols.is_empty() {
        return None;
    }
    let mut content = String::new();
    let mut spans = Vec::new();
    let mut scopes = Vec::new();
    let mut push = |content: &mut String, text: &str, color| {
        let start = content.len();
        // Some servers name things with source text, newlines and all.
        content.extend(text.chars().map(|c| if c.is_control() { ' ' } else { c }));
        let range = start..content.len();
        spans.push((
            range.clone(),
            HighlightStyle {
                color: Some(color),
                ..Default::default()
            },
        ));
        range
    };
    for (depth, symbol) in symbols.into_iter().enumerate() {
        if depth > 0 {
            push(&mut content, " › ", theme.text_subtle);
        }
        let range = push(&mut content, &symbol.name, scope_color(symbol.kind, theme));
        scopes.push(ScopeName {
            range,
            position: symbol.name_position,
        });
    }
    if let Some((hover_row, range)) = &marks.hover
        && *hover_row == row
    {
        spans = with_background(spans, range.clone(), theme.element_selected, content.len());
    }
    let text = StyledText::new(content).with_highlights(spans);
    let hit = ScopeHit {
        layout: text.layout().clone(),
        scopes,
    };
    Some((text, hit))
}

/// A scope's name in the color the highlighter gives that kind of name.
fn scope_color(kind: SymbolKind, theme: &Theme) -> gpui::Hsla {
    let capture = match kind {
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => "function",
        SymbolKind::Class
        | SymbolKind::Struct
        | SymbolKind::Enum
        | SymbolKind::Interface
        | SymbolKind::Object => "type",
        SymbolKind::Module | SymbolKind::Namespace | SymbolKind::Package => "namespace",
        SymbolKind::Constant | SymbolKind::EnumMember => "constant",
        SymbolKind::Property | SymbolKind::Field => "property",
        SymbolKind::Variable | SymbolKind::Other => "variable",
    };
    HighlightId::for_capture(capture)
        .and_then(|id| theme.syntax.style(id).color)
        .unwrap_or(theme.text)
}

/// The side of the file a line row shows, its highlights, and its line
/// number there. Context and added lines read from the new side, removed
/// ones from the old.
fn line_side(
    file: &FileDiff,
    line: DiffLine,
) -> Option<(&TextSnapshot, Option<&SyntaxSnapshot>, u32)> {
    let FileContent::Text(text) = &file.content else {
        return None;
    };
    let (snapshot, syntax, number) = match line.kind {
        LineKind::Removed => (text.old.as_ref(), text.old_syntax.as_ref(), line.old_line),
        LineKind::Context | LineKind::Added => {
            (text.new.as_ref(), text.new_syntax.as_ref(), line.new_line)
        }
    };
    Some((snapshot?, syntax.map(|s| &**s), number?))
}

fn diff_line(
    base: gpui::Div,
    row: usize,
    file: &FileDiff,
    line: DiffLine,
    marks: &Marks,
    theme: &Theme,
) -> (AnyElement, Option<LineHit>) {
    let Some((snapshot, syntax, number)) = line_side(file, line) else {
        return (base.into_any_element(), None);
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
    let source = &snapshot.text()[range];
    let (content, mut spans, tabs) = expand_tabs(source, spans);
    let len = content.len();
    let display = |range: &Range<usize>| tabs.to_display(range.start)..tabs.to_display(range.end);
    if let Some((hover_row, range)) = &marks.hover
        && *hover_row == row
    {
        spans = with_background(spans, display(range), theme.element_selected, len);
    }
    let selected = marks
        .selection
        .and_then(|selection| selection.in_row(row, source.len()));
    if let Some((range, _)) = &selected {
        spans = with_background(spans, display(range), theme.text_selection, len);
    }
    let text = StyledText::new(content).with_highlights(spans);
    let hit = LineHit {
        layout: text.layout().clone(),
        tabs,
    };

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

    let element = base
        .font_family(MONOSPACE)
        .text_xs()
        .cursor(CursorStyle::IBeam)
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
                .flex()
                .items_center()
                .pr_4()
                .text_color(Color::Default.hsla(theme))
                .child(text)
                // The newline, when the selection carries on to the next row.
                .when(selected.is_some_and(|(_, continues)| continues), |el| {
                    el.child(
                        div()
                            .w(px(6.))
                            .h(px(ROW_HEIGHT - 4.))
                            .bg(theme.text_selection),
                    )
                }),
        )
        .into_any_element();
    (element, Some(hit))
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
}
