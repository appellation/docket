//! What views that show code a line per row share: selecting and copying it
//! like text, and the language server's hover and definitions. The diff
//! and the file browser both implement [`CodeView`] over their own rows.
//!
//! Code can be selected by dragging (shift-click extends, double-click
//! takes a word, triple-click a line), and copied as the file's own text.
//! Resting the mouse on code shows its hover (`hover.rs`); command-clicking
//! it jumps to its definition (`definition.rs`). All of these map the mouse
//! to text through the layouts of the lines last drawn.

mod definition;
mod hover;
mod selection;

use std::collections::HashMap;
use std::ops::Range;
use std::time::Duration;

use gpui::{
    App, ClipboardItem, Context, CursorStyle, DispatchPhase, Entity, FocusHandle, HighlightStyle,
    Hsla, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, StyledText, Task, TextLayout, UnderlineStyle, UniformListScrollHandle, Window, actions,
    canvas, div, prelude::*, px,
};

pub use self::definition::{CodeDefinition, Definition, DefinitionState};
pub use self::hover::{Ask, CodeHover, HoverState};
pub use self::selection::{
    Granularity, Selection, Span, TabMap, TextPoint, display_width, expand_tabs, with_background,
    with_highlight, word_range,
};
use super::theme::Theme;
use crate::projects::ProjectId;
use crate::state::LspTarget;

actions!(
    code,
    [
        Copy,
        SelectAll,
        ClearSelection,
        GoToDefinition,
        GoToDefinitionInNewTab,
        GoBack
    ]
);

/// Every row of a code view is this tall, so it can be virtualized and the
/// row under the mouse found by division.
pub const ROW_HEIGHT: f32 = 20.;
/// How often to scroll while a selection is dragged past the list's edge.
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(16);

/// Binds copying, selecting all, clearing the selection, and jumping to
/// definitions and back in a code view's key context.
pub fn bind_keys(context: &'static str, cx: &mut App) {
    let context = Some(context);
    cx.bind_keys([
        gpui::KeyBinding::new("cmd-c", Copy, context),
        gpui::KeyBinding::new("cmd-a", SelectAll, context),
        gpui::KeyBinding::new("escape", ClearSelection, context),
        gpui::KeyBinding::new("f12", GoToDefinition, context),
        gpui::KeyBinding::new("alt-f12", GoToDefinitionInNewTab, context),
        gpui::KeyBinding::new("ctrl--", GoBack, context),
    ]);
}

/// A drawn line's text layout, for mapping window positions to offsets in
/// its source text.
pub struct LineHit {
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
    pub fn offset_under(&self, x: Pixels) -> Option<usize> {
        let line = self.layout.line_layout_for_index(0)?;
        let x = x - self.layout.bounds().left();
        if x < px(0.) {
            return None;
        }
        Some(self.tabs.source_at(line.unwrapped_layout.index_for_x(x)?))
    }

    /// The window x of a source offset.
    pub fn x_for_offset(&self, offset: usize) -> Pixels {
        let left = self.layout.bounds().left();
        self.layout.line_layout_for_index(0).map_or(left, |line| {
            left + line
                .unwrapped_layout
                .x_for_index(self.tabs.to_display(offset))
        })
    }
}

/// What's marked on the visible lines.
pub struct Marks<'a> {
    pub selection: Option<&'a Selection>,
    /// The hovered symbol: its row and source range.
    pub hover: Option<(usize, Range<usize>)>,
    /// What command-click would jump from: its row and source range.
    pub link: Option<(usize, Range<usize>)>,
    /// What a find in the file matched: each match's row and source
    /// range, in order.
    pub found: &'a [(usize, Range<usize>)],
    /// Which of `found` the find is on.
    pub current_found: Option<usize>,
}

impl Marks<'_> {
    /// The pointer over `row`: a hand while it holds a link.
    pub fn cursor(&self, row: usize) -> CursorStyle {
        match &self.link {
            Some((link_row, _)) if *link_row == row => CursorStyle::PointingHand,
            _ => CursorStyle::IBeam,
        }
    }
}

/// A code view's selection, hover, and what it last drew.
#[derive(Default)]
pub struct CodeState {
    pub scroll: UniformListScrollHandle,
    /// The line rows laid out in the last frame, by row.
    pub line_hits: HashMap<usize, LineHit>,
    pub selection: Option<Selection>,
    /// Set from pressing on a line until the button comes up.
    pub dragging: bool,
    drag_position: Point<Pixels>,
    autoscrolling: bool,
    _autoscroll_task: Option<Task<()>>,
    pub hover: HoverState,
    pub definition: DefinitionState,
}

impl CodeState {
    /// Forgets the selection, hover, peek, and drawn lines, for when the
    /// rows change and positions in them mean something else.
    pub fn reset(&mut self) {
        self.selection = None;
        self.line_hits.clear();
        self.hover = HoverState::default();
        self.definition = DefinitionState::default();
    }
}

/// A view whose rows, all [`ROW_HEIGHT`] tall in one uniform list, are
/// lines of code (or other rows, which hold no text).
pub trait CodeView: Sized + 'static {
    fn code(&self) -> &CodeState;
    fn code_mut(&mut self) -> &mut CodeState;
    fn focus_handle(&self) -> &FocusHandle;
    fn row_count(&self) -> usize;
    /// The source text of a line row; `None` for other rows.
    fn line_text<'a>(&'a self, row: usize, cx: &'a App) -> Option<&'a str>;
    /// What to ask a language server about (for hover, or a definition)
    /// for `offset` in `row`, if its file has a running one.
    fn lsp_request(&self, row: usize, offset: usize, cx: &App) -> Option<(LspTarget, Ask)>;
    /// Shows a definition in the view itself, if it has it, remembering
    /// where it was for Go Back. Returns whether it did; if not, it's
    /// peeked at in a popover instead.
    fn reveal_definition(&mut self, definition: &Definition, cx: &mut Context<Self>) -> bool;
    /// The project whose checkout a definition in these files can be
    /// browsed in, to open it in a new tab.
    fn definition_project(&self, cx: &App) -> Option<ProjectId>;

    /// The source offset and word under `x` in `row`, to hover. By default
    /// any non-whitespace on a line row.
    fn hover_word_at(&self, row: usize, x: Pixels, cx: &App) -> Option<(usize, Range<usize>)> {
        code_word_at(self, row, x, cx)
    }

    /// The window x of `offset` in a drawn `row`, to place a hover.
    fn hover_x(&self, row: usize, offset: usize) -> Option<Pixels> {
        Some(self.code().line_hits.get(&row)?.x_for_offset(offset))
    }

    fn list_bounds(&self) -> gpui::Bounds<Pixels> {
        self.code().scroll.0.borrow().base_handle.bounds()
    }

    /// The row at window y `y`. Rows are all one height, so this holds
    /// above and below the list too.
    fn row_at(&self, y: Pixels) -> isize {
        let state = self.code().scroll.0.borrow();
        let handle = &state.base_handle;
        ((y - handle.bounds().top() - handle.offset().y) / px(ROW_HEIGHT)).floor() as isize
    }

    /// The window y of a row's top.
    fn row_top(&self, row: usize) -> Pixels {
        let state = self.code().scroll.0.borrow();
        let handle = &state.base_handle;
        handle.bounds().top() + handle.offset().y + px(ROW_HEIGHT) * row
    }

    /// The text position at `position`, if it's over a drawn line. With
    /// `clamp`, positions above or below the drawn lines land on the
    /// first or last of them, for dragging past the edge.
    fn point_at(&self, position: Point<Pixels>, clamp: bool) -> Option<TextPoint> {
        let hits = &self.code().line_hits;
        let row = self.row_at(position.y);
        let row = if clamp {
            let first = *hits.keys().min()? as isize;
            let last = *hits.keys().max()? as isize;
            row.clamp(first, last) as usize
        } else {
            usize::try_from(row).ok()?
        };
        let hit = hits.get(&row)?;
        Some(TextPoint {
            row,
            offset: hit.closest_offset(position.x),
        })
    }

    /// What a click at `point` selects.
    fn span_at(&self, point: TextPoint, granularity: Granularity, cx: &App) -> Span {
        let text = self.line_text(point.row, cx).unwrap_or_default();
        let at = |offset| TextPoint {
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
        window.focus(self.focus_handle());
        self.dismiss_popovers(cx);
        // Command-option-click opens it in a new tab.
        if event.modifiers.secondary()
            && event.click_count == 1
            && self.go_to_definition_under(event.position, event.modifiers.alt, cx)
        {
            return;
        }
        let Some(point) = self.point_at(event.position, false) else {
            self.code_mut().selection = None;
            cx.notify();
            return;
        };
        let granularity = Granularity::for_click_count(event.click_count);
        let span = self.span_at(point, granularity, cx);
        let code = self.code_mut();
        match &mut code.selection {
            Some(selection) if event.modifiers.shift => selection.extend_to(span),
            _ => code.selection = Some(Selection::new(span, granularity)),
        }
        code.dragging = true;
        code.drag_position = event.position;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.code().dragging && event.pressed_button.is_none() {
            self.update_hover(event.position, cx);
            self.update_link(event.position, event.modifiers, cx);
        }
    }

    /// Command pressed or released with the mouse still.
    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_link(window.mouse_position(), event.modifiers, cx);
    }

    /// Mouse moves anywhere in the window while dragging a selection.
    fn drag_to(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        // The button came up somewhere we didn't hear about.
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        self.code_mut().drag_position = event.position;
        self.extend_drag(cx);

        let bounds = self.list_bounds();
        let outside = event.position.y < bounds.top() || event.position.y > bounds.bottom();
        if outside && !self.code().autoscrolling {
            let task = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(AUTOSCROLL_INTERVAL).await;
                    match this.update(cx, |this, cx| this.autoscroll(cx)) {
                        Ok(true) => {}
                        _ => break,
                    }
                }
            });
            let code = self.code_mut();
            code.autoscrolling = true;
            code._autoscroll_task = Some(task);
        }
    }

    fn extend_drag(&mut self, cx: &mut Context<Self>) {
        let Some(point) = self.point_at(self.code().drag_position, true) else {
            return;
        };
        let Some(granularity) = self.code().selection.as_ref().map(|s| s.granularity) else {
            return;
        };
        let span = self.span_at(point, granularity, cx);
        if let Some(selection) = &mut self.code_mut().selection {
            selection.extend_to(span);
        }
        cx.notify();
    }

    /// Scrolls toward a drag past the list's edge, faster the further past
    /// it is. Returns whether to keep going.
    fn autoscroll(&mut self, cx: &mut Context<Self>) -> bool {
        let bounds = self.list_bounds();
        let y = self.code().drag_position.y;
        let past = if y < bounds.top() {
            y - bounds.top()
        } else if y > bounds.bottom() {
            y - bounds.bottom()
        } else {
            px(0.)
        };
        if !self.code().dragging || past == px(0.) {
            self.code_mut().autoscrolling = false;
            return false;
        }
        let max_step = px(ROW_HEIGHT * 2.);
        let step = past.clamp(-max_step, max_step) * 0.5;
        {
            let state = self.code().scroll.0.borrow();
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
        let code = self.code_mut();
        code.dragging = false;
        code.autoscrolling = false;
        code._autoscroll_task = None;
        cx.notify();
    }

    fn selected_text(&self, selection: &Selection, cx: &App) -> String {
        let (start, end) = selection.range();
        let mut lines = Vec::new();
        for row in start.row..=end.row.min(self.row_count().saturating_sub(1)) {
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
        let Some(selection) = self.code().selection.as_ref().filter(|s| !s.is_empty()) else {
            return;
        };
        let text = self.selected_text(selection, cx);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let mut lines = (0..self.row_count()).filter(|&row| self.line_text(row, cx).is_some());
        let Some(first) = lines.next() else {
            return;
        };
        let last = lines.last().unwrap_or(first);
        let len = self.line_text(last, cx).map_or(0, str::len);
        let span = (
            TextPoint {
                row: first,
                offset: 0,
            },
            TextPoint {
                row: last,
                offset: len,
            },
        );
        self.code_mut().selection = Some(Selection::new(span, Granularity::Line));
        cx.notify();
    }

    /// Escape dismisses a peek or hover, then the selection, then falls
    /// through to whatever escape does around the view.
    fn clear_selection(&mut self, _: &ClearSelection, _: &mut Window, cx: &mut Context<Self>) {
        if self.code().definition.is_peeking() {
            self.dismiss_peek(cx);
        } else if self.code().hover.is_shown() {
            self.dismiss_hover(cx);
        } else if self
            .code_mut()
            .selection
            .take()
            .is_some_and(|s| !s.is_empty())
        {
            cx.notify();
        } else {
            cx.propagate();
        }
    }
}

/// The source offset and word under `x` on a drawn line row, unless it's
/// whitespace.
pub fn code_word_at<V: CodeView>(
    view: &V,
    row: usize,
    x: Pixels,
    cx: &App,
) -> Option<(usize, Range<usize>)> {
    let offset = view.code().line_hits.get(&row)?.offset_under(x)?;
    let text = view.line_text(row, cx)?;
    if text[offset..]
        .chars()
        .next()
        .is_none_or(char::is_whitespace)
    {
        return None;
    }
    Some((offset, word_range(text, offset)))
}

/// While dragging a selection, follows the mouse outside the list too.
pub fn drag_listeners<V: CodeView>(view: Entity<V>) -> impl IntoElement {
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

/// A line of code as drawn: its text, with tabs expanded and the hovered
/// symbol and selection marked over its highlights.
pub struct StyledLine {
    pub text: StyledText,
    /// For mapping the mouse back to the line's text.
    pub hit: LineHit,
    /// Whether the selection carries on past the line's end, so its
    /// newline should look selected.
    pub selection_continues: bool,
}

pub fn styled_line(
    row: usize,
    source: &str,
    spans: Vec<(Range<usize>, HighlightStyle)>,
    marks: &Marks,
    theme: &Theme,
) -> StyledLine {
    let (content, mut spans, tabs) = expand_tabs(source, spans);
    let len = content.len();
    let display = |range: &Range<usize>| tabs.to_display(range.start)..tabs.to_display(range.end);
    if let Some((hover_row, range)) = &marks.hover
        && *hover_row == row
    {
        spans = with_background(spans, display(range), theme.element_selected, len);
    }
    if let Some((link_row, range)) = &marks.link
        && *link_row == row
    {
        spans = with_highlight(spans, display(range), link_style(theme), len);
    }
    let first = marks
        .found
        .partition_point(|(found_row, _)| *found_row < row);
    for (ix, (found_row, range)) in marks.found.iter().enumerate().skip(first) {
        if *found_row != row {
            break;
        }
        let color = if marks.current_found == Some(ix) {
            theme.find_match_current
        } else {
            theme.find_match
        };
        spans = with_background(spans, display(range), color, len);
    }
    let selected = marks
        .selection
        .and_then(|selection| selection.in_row(row, source.len()));
    if let Some((range, _)) = &selected {
        spans = with_background(spans, display(range), theme.text_selection, len);
    }
    let text = StyledText::new(content).with_highlights(spans);
    StyledLine {
        hit: LineHit {
            layout: text.layout().clone(),
            tabs,
        },
        text,
        selection_continues: selected.is_some_and(|(_, continues)| continues),
    }
}

/// How a link to a definition looks while command is held over it.
pub fn link_style(theme: &Theme) -> HighlightStyle {
    HighlightStyle {
        color: Some(theme.accent),
        underline: Some(UnderlineStyle {
            thickness: px(1.),
            color: Some(theme.accent),
            wavy: false,
        }),
        ..Default::default()
    }
}

/// The selected newline at the end of a line.
pub fn selected_newline(color: Hsla) -> impl IntoElement {
    div().w(px(6.)).h(px(ROW_HEIGHT - 4.)).bg(color)
}
