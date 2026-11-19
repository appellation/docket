//! The open file's map, at the right of its code: the whole file drawn
//! small, a line of the map per row (or several rows to a line of pixels,
//! in a long file), with the diff's changes in their colors at its left
//! edge and the part of the file in view marked. Clicking it scrolls the
//! code to center there, and dragging keeps scrolling with the mouse.
//!
//! Once the file's language server is running, the map names the symbols
//! of its outline (`outline.rs`, the same as Go to Symbol's) where they
//! are, nesting one deep; where names would overlap, the outermost and
//! then the first keeps its place. Clicking a name goes to its symbol,
//! selecting it.
//!
//! Where the names go depends on the map's height, which is only known
//! once it's laid out, so they're placed by the height last painted, and
//! the view draws again when that changes.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, Entity, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollStrategy, Window, canvas, div, fill, point,
    prelude::*, px, size,
};

use super::FileBrowserView;
use super::changes::FileRow;
use super::outline::{FileSymbol, OutlineLoad};
use crate::diff::TextSnapshot;
use crate::ui::code::{
    CodeDefinition as _, CodeView as _, Granularity, ROW_HEIGHT, Selection, TextPoint,
    display_width,
};
use crate::ui::components::TestSelector as _;
use crate::ui::diff_view::scope_color;
use crate::ui::theme::Theme;

pub(super) const MAP_WIDTH: f32 = 128.;
/// The most of the map's height a row takes, so a short file isn't
/// stretched over all of it.
const MAX_ROW_HEIGHT: f32 = 3.;
/// Each column of a line, drawn.
const COLUMN_WIDTH: f32 = 1.;
/// Room at the map's left for the changes' marks, and at its right.
const LEFT_PADDING: f32 = 6.;
const RIGHT_PADDING: f32 = 4.;
/// A symbol's name on the map.
const LABEL_HEIGHT: f32 = 16.;
/// How deep the named symbols are nested, at most.
const LABEL_DEPTH: usize = 1;
/// The part of the file in view is marked at least this tall.
const MIN_THUMB_HEIGHT: f32 = 6.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Context,
    Added,
    Removed,
}

/// A row as the map draws it: the columns its text spans, past its
/// indentation, and what kind of row it is.
#[derive(Clone, Copy, Debug)]
struct SketchRow {
    start: u16,
    end: u16,
    kind: RowKind,
}

#[derive(Default)]
pub(super) struct MapState {
    /// The map's bounds as last painted, to place names and find the row
    /// under the mouse.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Pressed on the map until the button comes up.
    dragging: bool,
    sketch: Arc<Vec<SketchRow>>,
    /// The text and rows (by `rows_version`) `sketch` is of.
    sketch_for: Option<(Arc<TextSnapshot>, u64)>,
    /// Where the names go, and the outline, rows, and map height they were
    /// placed for.
    labels: Arc<Vec<MapLabel>>,
    labels_for: Option<(Arc<Vec<FileSymbol>>, u64, Pixels)>,
}

/// A symbol's name on the map.
struct MapLabel {
    /// Its index in the outline.
    symbol: usize,
    top: f32,
}

/// The map's height per row, for `rows` rows in `height`.
fn row_scale(height: Pixels, rows: usize) -> f32 {
    if rows == 0 {
        return MAX_ROW_HEIGHT;
    }
    (height / px(rows as f32)).min(MAX_ROW_HEIGHT)
}

fn sketch_row(source: &str, kind: RowKind) -> SketchRow {
    let indent = source.len() - source.trim_start().len();
    let start = display_width(&source[..indent]);
    let end = display_width(source.trim_end());
    let clamp = |n: usize| n.min(u16::MAX as usize) as u16;
    SketchRow {
        start: clamp(start),
        end: clamp(end.max(start)),
        kind,
    }
}

/// Where each name goes on a map of `height`, top to bottom: each symbol
/// nested at most `LABEL_DEPTH` deep, centered on its row, unless it would
/// overlap one placed before it, outermost first.
fn place_labels(
    symbols: &[FileSymbol],
    row_of: impl Fn(usize) -> usize,
    scale: f32,
    height: f32,
) -> Vec<MapLabel> {
    let mut candidates: Vec<usize> = (0..symbols.len())
        .filter(|&ix| symbols[ix].depth <= LABEL_DEPTH)
        .collect();
    candidates.sort_by_key(|&ix| (symbols[ix].depth, symbols[ix].line));
    let bottom = (height - LABEL_HEIGHT).max(0.);
    let mut placed: Vec<MapLabel> = Vec::new();
    for symbol in candidates {
        let center = (row_of(symbols[symbol].line) as f32 + 0.5) * scale;
        let top = (center - LABEL_HEIGHT / 2.).clamp(0., bottom);
        if placed
            .iter()
            .all(|label| (label.top - top).abs() >= LABEL_HEIGHT)
        {
            placed.push(MapLabel { symbol, top });
        }
    }
    placed.sort_by(|a, b| a.top.total_cmp(&b.top));
    placed
}

impl FileBrowserView {
    /// Sketches the rows again when the text or rows change.
    fn sync_sketch(&mut self, cx: &Context<Self>) {
        let Some(text) = self.open_text(cx) else {
            self.map.sketch = Arc::default();
            self.map.sketch_for = None;
            return;
        };
        if self
            .map
            .sketch_for
            .as_ref()
            .is_some_and(|(for_text, version)| {
                Arc::ptr_eq(for_text, &text) && *version == self.rows_version
            })
        {
            return;
        }
        let changes = self.shown_changes().cloned();
        let sketch = (0..self.row_count())
            .filter_map(|row| {
                Some(match self.file_row(row)? {
                    FileRow::Line { line, added } if line < text.line_count() => {
                        let kind = if added {
                            RowKind::Added
                        } else {
                            RowKind::Context
                        };
                        sketch_row(text.line(line), kind)
                    }
                    FileRow::Removed { old_line } => {
                        let old = changes.as_ref()?.old.as_ref()?;
                        let source = if old_line < old.line_count() {
                            old.line(old_line)
                        } else {
                            ""
                        };
                        sketch_row(source, RowKind::Removed)
                    }
                    FileRow::Line { .. } => sketch_row("", RowKind::Context),
                })
            })
            .collect();
        self.map.sketch = Arc::new(sketch);
        self.map.sketch_for = Some((text, self.rows_version));
    }

    /// Scrolls the code so the row at window y `y` on the map is centered.
    fn scroll_from_map(&mut self, y: Pixels, cx: &mut Context<Self>) {
        let Some(bounds) = self.map.bounds.get() else {
            return;
        };
        let rows = self.map.sketch.len();
        if rows == 0 {
            return;
        }
        let scale = row_scale(bounds.size.height, rows);
        let row = ((y - bounds.top()) / px(scale)).clamp(0., (rows - 1) as f32);
        {
            let scroll = self.code.scroll.0.borrow();
            let handle = &scroll.base_handle;
            let viewport = handle.bounds().size.height;
            let max = handle.max_offset().height;
            let target = (px(ROW_HEIGHT) * (row + 0.5) - viewport / 2.).clamp(px(0.), max);
            let offset = handle.offset();
            handle.set_offset(point(offset.x, -target));
        }
        self.dismiss_popovers(cx);
        cx.notify();
    }

    fn map_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.map.dragging = true;
        self.scroll_from_map(event.position.y, cx);
    }

    fn drag_map(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button == Some(MouseButton::Left) {
            self.scroll_from_map(event.position.y, cx);
        } else {
            self.end_map_drag(cx);
        }
    }

    fn end_map_drag(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.map.dragging) {
            cx.notify();
        }
    }

    /// A name on the map: goes to its symbol, selecting its name.
    pub(super) fn go_to_map_symbol(&mut self, symbol: usize, cx: &mut Context<Self>) {
        let Some(OutlineLoad::Ready(outline)) = &self.find.symbols.outline else {
            return;
        };
        let Some(symbol) = outline.get(symbol).cloned() else {
            return;
        };
        self.select_line_range(symbol.line, symbol.name_range, cx);
    }

    /// Selects `range` of `line`, scrolling it to the center.
    pub(super) fn select_line_range(
        &mut self,
        line: usize,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let row = self.row_of_line(line);
        let at = |offset| TextPoint { row, offset };
        self.dismiss_popovers(cx);
        self.code.selection = Some(Selection::new(
            (at(range.start), at(range.end)),
            Granularity::Character,
        ));
        self.code.scroll.scroll_to_item(row, ScrollStrategy::Center);
        cx.notify();
    }

    /// The map, beside the open file's code.
    pub(super) fn render_map(&mut self, cx: &Context<Self>) -> AnyElement {
        self.sync_sketch(cx);
        let theme = Theme::get(cx);
        let rows = self.map.sketch.len();
        let drawn = self.map.bounds.get();
        let labels = match (&self.find.symbols.outline, drawn) {
            (Some(OutlineLoad::Ready(outline)), Some(bounds)) => {
                let height = bounds.size.height;
                let placed = matches!(
                    &self.map.labels_for,
                    Some((for_outline, version, for_height))
                        if Arc::ptr_eq(for_outline, outline)
                            && *version == self.rows_version
                            && *for_height == height
                );
                if !placed {
                    let scale = row_scale(height, rows);
                    let labels = place_labels(
                        outline,
                        |line| self.row_of_line(line),
                        scale,
                        height / px(1.),
                    );
                    self.map.labels = Arc::new(labels);
                    self.map.labels_for = Some((outline.clone(), self.rows_version, height));
                }
                self.map
                    .labels
                    .iter()
                    .map(|label| {
                        let symbol = &outline[label.symbol];
                        let ix = label.symbol;
                        let hover = theme.element_hover;
                        div()
                            .test_selector(|| format!("file-map-symbol-{}", symbol.name))
                            .id(("file-map-symbol", ix))
                            .absolute()
                            .top(px(label.top))
                            .left(px(LEFT_PADDING + symbol.depth as f32 * 8.))
                            .max_w(px(MAP_WIDTH
                                - LEFT_PADDING
                                - RIGHT_PADDING
                                - symbol.depth as f32 * 8.))
                            .h(px(LABEL_HEIGHT))
                            .flex()
                            .items_center()
                            .px_1()
                            .rounded_sm()
                            .bg(theme.elevated.alpha(0.85))
                            .text_size(px(10.))
                            .line_height(px(LABEL_HEIGHT))
                            .text_color(scope_color(symbol.kind, theme))
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .child(div().min_w_0().truncate().child(symbol.name.clone()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    // Not the map's, which scrolls.
                                    cx.stop_propagation();
                                    window.focus(&this.focus_handle);
                                    this.go_to_map_symbol(ix, cx);
                                }),
                            )
                            .into_any_element()
                    })
                    .collect()
            }
            _ => Vec::new(),
        };

        let sketch = self.map.sketch.clone();
        let scroll = self.code.scroll.clone();
        let bounds_cell = self.map.bounds.clone();
        let view = cx.entity_id();
        let colors = SketchColors {
            context: theme.text_subtle.alpha(0.4),
            added: theme.success,
            removed: theme.danger,
            thumb: theme.element_hover,
        };
        let sketch_canvas = canvas(
            move |bounds, _, cx| {
                // Names were placed for the last height.
                if bounds_cell.replace(Some(bounds)).map(|b| b.size) != Some(bounds.size) {
                    cx.notify(view);
                }
            },
            move |bounds, _, window, _| {
                let scroll = scroll.0.borrow();
                let handle = &scroll.base_handle;
                let (offset, viewport) = (handle.offset(), handle.bounds().size.height);
                paint_sketch(&sketch, bounds, offset, viewport, &colors, window);
            },
        )
        .absolute()
        .size_full();

        div()
            .test_selector(|| "file-map".into())
            .id("file-map")
            .relative()
            .flex_none()
            .w(px(MAP_WIDTH))
            .h_full()
            .overflow_hidden()
            .border_l_1()
            .border_color(theme.border_subtle)
            .bg(theme.surface)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::map_mouse_down))
            .child(sketch_canvas)
            .children(labels)
            .when(self.map.dragging, |el| {
                el.child(map_drag_listeners(cx.entity()))
            })
            .into_any_element()
    }
}

struct SketchColors {
    context: Hsla,
    added: Hsla,
    removed: Hsla,
    thumb: Hsla,
}

/// Draws the rows, and marks the part in view: `offset` is the code's
/// scroll offset, and `viewport` its height.
fn paint_sketch(
    sketch: &[SketchRow],
    bounds: Bounds<Pixels>,
    offset: Point<Pixels>,
    viewport: Pixels,
    colors: &SketchColors,
    window: &mut Window,
) {
    let rows = sketch.len();
    if rows == 0 {
        return;
    }
    let scale = row_scale(bounds.size.height, rows);
    let left = bounds.left() + px(LEFT_PADDING);
    let columns = ((MAP_WIDTH - LEFT_PADDING - RIGHT_PADDING) / COLUMN_WIDTH) as u16;

    // Rows to each line of pixels, or a row to several.
    let lines = ((rows as f32 * scale).ceil() as usize).max(1);
    let line_height = (scale.max(1.) * 0.7).max(1.);
    let mut row = 0;
    for line in 0..lines {
        let (first, last) = if scale >= 1. {
            if line >= rows {
                break;
            }
            (line, line + 1)
        } else {
            let last = (((line + 1) as f32 / scale) as usize).clamp(row + 1, rows);
            (row, last)
        };
        if first >= rows {
            break;
        }
        row = last;
        let rows = &sketch[first..last];
        let kind = if rows.iter().any(|r| r.kind == RowKind::Removed) {
            RowKind::Removed
        } else if rows.iter().any(|r| r.kind == RowKind::Added) {
            RowKind::Added
        } else {
            RowKind::Context
        };
        let start = rows.iter().map(|r| r.start).min().unwrap_or(0).min(columns);
        let end = rows.iter().map(|r| r.end).max().unwrap_or(0).min(columns);
        let top = bounds.top() + px(first as f32 * scale);
        let color = match kind {
            RowKind::Context => colors.context,
            RowKind::Added => colors.added,
            RowKind::Removed => colors.removed,
        };
        if kind != RowKind::Context {
            let mark = Bounds::new(
                point(bounds.left() + px(1.), top),
                size(px(3.), px(scale.max(1.))),
            );
            window.paint_quad(fill(mark, color));
        }
        if end > start {
            let bar = Bounds::new(
                point(left + px(start as f32 * COLUMN_WIDTH), top),
                size(px((end - start) as f32 * COLUMN_WIDTH), px(line_height)),
            );
            let color = match kind {
                RowKind::Context => color,
                _ => color.alpha(0.6),
            };
            window.paint_quad(fill(bar, color));
        }
    }

    // The part in view.
    let top_row = (-offset.y / px(ROW_HEIGHT)).max(0.);
    let shown = viewport / px(ROW_HEIGHT);
    let thumb = Bounds::new(
        point(bounds.left(), bounds.top() + px(top_row * scale)),
        size(bounds.size.width, px((shown * scale).max(MIN_THUMB_HEIGHT))),
    );
    window.paint_quad(fill(thumb, colors.thumb));
}

/// While dragging on the map, follows the mouse outside it too.
fn map_drag_listeners(view: Entity<FileBrowserView>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            window.on_mouse_event({
                let view = view.clone();
                move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        view.update(cx, |view, cx| view.drag_map(event, cx));
                    }
                }
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    view.update(cx, |view, cx| view.end_map_drag(cx));
                }
            });
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::SymbolKind;

    fn symbol(name: &str, depth: usize, line: usize) -> FileSymbol {
        FileSymbol {
            name: name.to_owned(),
            kind: SymbolKind::Function,
            depth,
            line,
            name_range: 0..name.len(),
        }
    }

    #[test]
    fn names_outer_symbols_first_where_they_overlap() {
        let symbols = [
            symbol("Widget", 0, 0),
            symbol("spin", 1, 2),
            symbol("inner", 2, 40),
            symbol("Gadget", 0, 3),
            symbol("turn", 1, 50),
        ];
        // 3px a row: rows 0, 2, and 3 are within a name's height.
        let placed: Vec<&str> = place_labels(&symbols, |line| line, 3., 300.)
            .iter()
            .map(|label| symbols[label.symbol].name.as_str())
            .collect();
        assert_eq!(placed, ["Widget", "turn"]);
    }

    #[test]
    fn sketches_a_row_past_its_indentation() {
        let row = sketch_row("    let x = 1;  ", RowKind::Added);
        assert_eq!((row.start, row.end, row.kind), (4, 14, RowKind::Added));
        let blank = sketch_row("   ", RowKind::Context);
        assert_eq!((blank.start, blank.end), (3, 3));
    }
}
