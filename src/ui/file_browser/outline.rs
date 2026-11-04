//! Go to a symbol in the open file: the find bar's Symbols mode (⌘⇧O),
//! which lists the file's outline from its language server
//! (`textDocument/documentSymbol`, as hunk headers use) under the bar.
//!
//! With no query the list is the whole outline, nested, starting at the
//! symbol the view is in; a query ranks the names as the tree's search
//! does. Up and down move through the list, scrolling the file to each
//! symbol with its name marked, and Enter (or a click) goes to it,
//! selecting the name, and closes the bar. Escape closes it and returns
//! the file to where it was.
//!
//! The outline is asked for once the file's server is running, and again
//! when its text changes. A file without one says why: no server for its
//! language, a branch that isn't checked out, or the server starting.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use futures::future::{Either, select};
use gpui::{
    AnyElement, Context, FontWeight, HighlightStyle, MouseButton, Pixels, Point, ScrollStrategy,
    SharedString, StyledText, Task, UniformListScrollHandle, Window, actions, deferred, div,
    prelude::*, px, uniform_list,
};

use super::FileBrowserView;
use super::find::FindMode;
use super::search::kind_label;
use crate::diff::TextSnapshot;
use crate::lsp::{Outline, PositionEncoding, SymbolKind, server_for};
use crate::projects::search::fuzzy_find;
use crate::state::{ServerState, matched_ranges};
use crate::ui::code::{Granularity, Selection, TextPoint};
use crate::ui::components::{IconName, Spinner, TestSelector as _, TreeItem};
use crate::ui::theme::{Color, Theme};

actions!(
    file_browser,
    [
        /// Opens the find bar over the open file, for its symbols.
        FindSymbolInFile
    ]
);

/// Tries at the outline, for servers that turn requests away while they
/// load, and how long each may take.
const OUTLINE_ATTEMPTS: u32 = 5;
const OUTLINE_RETRY_INTERVAL: Duration = Duration::from_secs(2);
const OUTLINE_TIMEOUT: Duration = Duration::from_secs(10);
/// Symbols a query lists, at most.
const SYMBOL_RESULTS: usize = 200;
/// A row of the list; as many show before it scrolls.
const ROW_HEIGHT: f32 = 24.;
const ROWS_SHOWN: usize = 12;

/// A symbol in the open file, where its name is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileSymbol {
    pub name: String,
    pub kind: SymbolKind,
    /// How deep it's nested in other symbols.
    pub depth: usize,
    /// Its name's line, and range in it.
    pub line: usize,
    pub name_range: Range<usize>,
}

pub(super) enum OutlineLoad {
    Loading,
    Ready(Arc<Vec<FileSymbol>>),
    /// No server to ask, and why.
    Unavailable(SharedString),
    Failed(SharedString),
}

#[derive(Default)]
pub(super) struct SymbolFind {
    pub outline: Option<OutlineLoad>,
    /// The text, and the server (by address), the outline is for.
    outline_for: Option<(Arc<TextSnapshot>, Option<usize>)>,
    _task: Option<Task<()>>,
    /// The listed symbols, by index into the outline, with the byte
    /// offsets of the characters the query matched in each name.
    pub rows: Vec<(usize, Vec<usize>)>,
    /// The query and outline (by address) `rows` are for.
    pub rows_for: Option<(String, usize)>,
    pub selected: Option<usize>,
    /// The selected symbol's name, marked in its line.
    pub mark: Vec<(usize, Range<usize>)>,
    /// Scroll the file to the selection once the rows are listed: the
    /// query changed.
    pub preview: bool,
    /// Where the file was scrolled before the list moved it, to return to
    /// on escape.
    pub preview_from: Option<Point<Pixels>>,
    scroll: UniformListScrollHandle,
}

/// The rows for `query`: the whole outline in order without one, or else
/// the best matches for it by name, best first.
pub(super) fn symbol_rows(symbols: &[FileSymbol], query: &str) -> Vec<(usize, Vec<usize>)> {
    if query.trim().is_empty() {
        return (0..symbols.len()).map(|ix| (ix, Vec::new())).collect();
    }
    let names: Vec<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
    fuzzy_find(&names, query, false, SYMBOL_RESULTS)
        .into_iter()
        .map(|found| (found.index, found.positions))
        .collect()
}

/// An outline's symbols, each with where its name is in `text`.
fn file_symbols(
    outline: &Outline,
    text: &TextSnapshot,
    encoding: PositionEncoding,
) -> Vec<FileSymbol> {
    let Some(last) = text.line_count().checked_sub(1) else {
        return Vec::new();
    };
    outline
        .flatten()
        .into_iter()
        .map(|(depth, symbol)| {
            let line = symbol.name_position.line.min(last);
            let source = text.line(line);
            let start = encoding.offset(source, symbol.name_position.character);
            // The name, where the server points at it; just its start if
            // it names it otherwise (`impl Widget`).
            let end = if source[start..].starts_with(symbol.name.as_ref()) {
                start + symbol.name.len()
            } else {
                start
            };
            FileSymbol {
                name: symbol.name.to_string(),
                kind: symbol.kind,
                depth,
                line: line as usize,
                name_range: start..end,
            }
        })
        .collect()
}

impl FileBrowserView {
    /// ⌘⇧O: opens the bar for the open file's symbols.
    pub(super) fn open_symbols(
        &mut self,
        _: &FindSymbolInFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_find_in(FindMode::Symbols, window, cx);
    }

    /// Where the file is scrolled, for returning to.
    pub(super) fn code_offset(&self) -> Point<Pixels> {
        self.code.scroll.0.borrow().base_handle.offset()
    }

    /// Escape: returns the file to where it was before the list moved it.
    pub(super) fn restore_symbol_preview(&mut self) {
        if let Some(offset) = self.find.symbols.preview_from.take() {
            self.code.scroll.0.borrow().base_handle.set_offset(offset);
        }
    }

    /// Asks for the outline when the file's text or server changes, and
    /// lists its symbols again when it or the query does.
    pub(super) fn sync_symbols(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.open_text(cx) else {
            let symbols = &mut self.find.symbols;
            symbols.outline = None;
            symbols.outline_for = None;
            symbols._task = None;
            symbols.rows.clear();
            symbols.rows_for = None;
            symbols.selected = None;
            symbols.mark.clear();
            return;
        };
        let target = self
            .state
            .read(cx)
            .browser
            .as_ref()
            .and_then(|browser| browser.lsp_target());
        let server = target
            .as_ref()
            .map(|target| Arc::as_ptr(&target.server) as usize);
        let current = matches!(
            &self.find.symbols.outline_for,
            Some((for_text, for_server)) if Arc::ptr_eq(for_text, &text) && *for_server == server
        );
        if !current {
            self.find.symbols.outline_for = Some((text.clone(), server));
            self.find.symbols.rows_for = None;
            match target {
                Some(target) => {
                    self.find.symbols.outline = Some(OutlineLoad::Loading);
                    self.find.symbols._task = Some(cx.spawn(async move |this, cx| {
                        let background = cx.background_executor().clone();
                        let found = background
                            .clone()
                            .spawn(async move {
                                let server = &target.server;
                                server.open(
                                    &target.path,
                                    target.language_id,
                                    target.text.text(),
                                )?;
                                let mut attempt = 1;
                                loop {
                                    let request = server.outline(&target.path);
                                    let timeout = background.timer(OUTLINE_TIMEOUT);
                                    let answer = match select(Box::pin(request), timeout).await {
                                        Either::Left((answer, _)) => answer,
                                        Either::Right(_) => {
                                            Err(anyhow!("no answer within 10 seconds"))
                                        }
                                    };
                                    match answer {
                                        Ok(outline) => {
                                            return Ok(file_symbols(
                                                &outline,
                                                &target.text,
                                                server.encoding,
                                            ));
                                        }
                                        Err(_) if attempt < OUTLINE_ATTEMPTS => {
                                            attempt += 1;
                                            background.timer(OUTLINE_RETRY_INTERVAL).await;
                                        }
                                        Err(err) => return Err(err),
                                    }
                                }
                            })
                            .await;
                        this.update(cx, |this, cx| {
                            let symbols = &mut this.find.symbols;
                            symbols.outline = Some(match found {
                                Ok(found) => OutlineLoad::Ready(Arc::new(found)),
                                Err(err) => OutlineLoad::Failed(
                                    format!("Couldn't list the file's symbols: {err:#}").into(),
                                ),
                            });
                            cx.notify();
                        })
                        .ok();
                    }));
                }
                None => {
                    self.find.symbols._task = None;
                    self.find.symbols.outline = Some(OutlineLoad::Unavailable(
                        self.symbols_unavailable(&text, cx),
                    ));
                }
            }
        } else if matches!(self.find.symbols.outline, Some(OutlineLoad::Unavailable(_))) {
            // A server starting says so, then fails or runs.
            self.find.symbols.outline = Some(OutlineLoad::Unavailable(
                self.symbols_unavailable(&text, cx),
            ));
        }

        let query = self.find.input.read(cx).text().to_owned();
        let outline = match &self.find.symbols.outline {
            Some(OutlineLoad::Ready(outline)) => Some(outline.clone()),
            _ => None,
        };
        let outline_key = outline.as_ref().map_or(0, |o| Arc::as_ptr(o) as usize);
        let rows_for = Some((query.clone(), outline_key));
        if self.find.symbols.rows_for == rows_for {
            return;
        }
        self.find.symbols.rows_for = rows_for;
        let rows = outline
            .as_ref()
            .map(|outline| symbol_rows(outline, &query))
            .unwrap_or_default();
        // Without a query, from the symbol the view is in.
        let selected = match (&outline, query.trim().is_empty()) {
            (Some(outline), true) => {
                let top = self.visible_rows().0;
                Some(
                    rows.iter()
                        .rposition(|(ix, _)| outline[*ix].line <= top)
                        .unwrap_or(0),
                )
            }
            _ => Some(0),
        }
        .filter(|_| !rows.is_empty());
        self.find.symbols.rows = rows;
        self.select_symbol(selected, ScrollStrategy::Center);
        if std::mem::take(&mut self.find.symbols.preview) && !query.trim().is_empty() {
            self.preview_symbol();
        }
    }

    /// Why there's no outline to list.
    fn symbols_unavailable(&self, text: &TextSnapshot, cx: &Context<Self>) -> SharedString {
        let none = "Symbols come from a language server, and Docket has none for this file.";
        let Some(spec) = text.language.and_then(server_for) else {
            return none.into();
        };
        let Some(browser) = &self.state.read(cx).browser else {
            return none.into();
        };
        if browser.working_copy().is_none() {
            return "Symbols come from a language server, which runs only where the branch is \
                    checked out."
                .into();
        }
        let slot = browser.code_intel.as_ref().and_then(|intel| {
            intel
                .servers
                .iter()
                .find(|slot| std::ptr::eq(slot.spec, spec))
        });
        match slot.map(|slot| &slot.state) {
            Some(ServerState::NotInstalled { .. }) => {
                format!("{} isn't installed; see below the tree.", spec.name).into()
            }
            Some(ServerState::Failed(_)) => {
                format!("{} stopped; see below the tree.", spec.name).into()
            }
            _ => format!("Waiting for {}…", spec.name).into(),
        }
    }

    /// Selects a row of the list, marking the symbol's name and keeping
    /// the row in view.
    fn select_symbol(&mut self, row: Option<usize>, strategy: ScrollStrategy) {
        let symbols = &mut self.find.symbols;
        symbols.selected = row;
        symbols.mark.clear();
        let outline = match &symbols.outline {
            Some(OutlineLoad::Ready(outline)) => outline.clone(),
            _ => return,
        };
        if let Some(row) = row
            && let Some((ix, _)) = symbols.rows.get(row)
        {
            let symbol = &outline[*ix];
            symbols.mark.push((symbol.line, symbol.name_range.clone()));
            symbols.scroll.scroll_to_item(row, strategy);
        }
    }

    /// Scrolls the file to the selected symbol, remembering where it was.
    fn preview_symbol(&mut self) {
        let Some(&(line, _)) = self.find.symbols.mark.first() else {
            return;
        };
        if self.find.symbols.preview_from.is_none() {
            self.find.symbols.preview_from = Some(self.code_offset());
        }
        self.reveal_row(line);
    }

    /// Up and down: moves through the list, showing each symbol.
    pub(super) fn step_symbol(&mut self, by: isize, cx: &mut Context<Self>) {
        let len = self.find.symbols.rows.len();
        if len == 0 {
            return;
        }
        let row = match self.find.symbols.selected {
            Some(row) => row.saturating_add_signed(by).min(len - 1),
            None => 0,
        };
        let strategy = if by > 0 {
            ScrollStrategy::Bottom
        } else {
            ScrollStrategy::Top
        };
        self.select_symbol(Some(row), strategy);
        self.preview_symbol();
        cx.notify();
    }

    /// Enter, or a click: goes to a symbol (the selected one, for `None`),
    /// selecting its name, and closes the bar.
    pub(super) fn go_to_symbol(&mut self, row: Option<usize>, cx: &mut Context<Self>) {
        if let Some(row) = row {
            self.select_symbol(Some(row), ScrollStrategy::Top);
        }
        let Some(&(line, ref name)) = self.find.symbols.mark.first() else {
            return;
        };
        let at = |offset| TextPoint { row: line, offset };
        self.code.selection = Some(Selection::new(
            (at(name.start), at(name.end)),
            Granularity::Character,
        ));
        self.code
            .scroll
            .scroll_to_item(line, ScrollStrategy::Center);
        self.find.symbols.preview_from = None;
        self.find.open = false;
        // Back to the file.
        self.focused = false;
        cx.notify();
    }

    /// The list, under the bar and over the file's code.
    pub(super) fn render_symbol_list(&self, cx: &Context<Self>) -> AnyElement {
        let theme = Theme::get(cx);
        let symbols = &self.find.symbols;
        let has_query = !self.find.input.read(cx).text().trim().is_empty();
        let notice: Option<(SharedString, bool)> = match &symbols.outline {
            None | Some(OutlineLoad::Loading) => Some(("Listing the file's symbols…".into(), true)),
            Some(OutlineLoad::Unavailable(why) | OutlineLoad::Failed(why)) => {
                Some((why.clone(), false))
            }
            Some(OutlineLoad::Ready(_)) if symbols.rows.is_empty() => Some((
                if has_query {
                    "No symbols match.".into()
                } else {
                    "No symbols in this file.".into()
                },
                false,
            )),
            Some(OutlineLoad::Ready(_)) => None,
        };
        let height = match notice {
            Some(_) => px(36.),
            None => px(symbols.rows.len().min(ROWS_SHOWN) as f32 * ROW_HEIGHT + 8.),
        };
        let body = match notice {
            Some((message, loading)) => div()
                .test_selector(|| "find-symbols-notice".into())
                .flex()
                .items_center()
                .gap_2()
                .h_full()
                .px_3()
                .text_color(theme.text_muted)
                .when(loading, |el| el.child(Spinner::new("find-symbols-loading")))
                .child(div().min_w_0().truncate().child(message))
                .into_any_element(),
            None => uniform_list(
                "find-symbols",
                symbols.rows.len(),
                cx.processor(Self::render_symbol_rows),
            )
            .size_full()
            .py_1()
            .track_scroll(symbols.scroll.clone())
            .into_any_element(),
        };
        deferred(
            div()
                .test_selector(|| "find-symbols".into())
                .occlude()
                .absolute()
                .top_full()
                .left_2()
                .mt_1()
                .w(px(420.))
                .h(height)
                .bg(theme.elevated)
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .shadow_lg()
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(body),
        )
        .with_priority(1)
        .into_any_element()
    }

    fn render_symbol_rows(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::get(cx);
        let symbols = &self.find.symbols;
        let Some(OutlineLoad::Ready(outline)) = &symbols.outline else {
            return Vec::new();
        };
        let nested = self.find.input.read(cx).text().trim().is_empty();
        let matched = HighlightStyle {
            color: Some(theme.accent),
            font_weight: Some(FontWeight::SEMIBOLD),
            ..Default::default()
        };
        range
            .filter_map(|row| {
                let (ix, positions) = symbols.rows.get(row)?;
                let symbol = &outline[*ix];
                let ranges = matched_ranges(&symbol.name, positions);
                let name = StyledText::new(symbol.name.clone())
                    .with_highlights(ranges.into_iter().map(|r| (r, matched)));
                Some(
                    TreeItem::new(("find-symbol", row), if nested { symbol.depth } else { 0 })
                        .selector(format!("find-symbol-{}", symbol.name))
                        .selected(symbols.selected == Some(row))
                        .icon(IconName::Braces, Color::Muted)
                        .child(name)
                        .end_slot(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme.text_subtle)
                                .child(kind_label(symbol.kind)),
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.go_to_symbol(Some(row), cx)),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn set_outline(&mut self, symbols: Vec<FileSymbol>, cx: &mut Context<Self>) {
        self.find.symbols.outline = Some(OutlineLoad::Ready(Arc::new(symbols)));
        self.find.symbols.rows_for = None;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(name: &str, depth: usize, line: usize) -> FileSymbol {
        FileSymbol {
            name: name.to_owned(),
            kind: SymbolKind::Function,
            depth,
            line,
            name_range: 3..3 + name.len(),
        }
    }

    #[test]
    fn lists_the_outline_or_the_best_matches() {
        let symbols = [
            symbol("Widget", 0, 0),
            symbol("spin", 1, 2),
            symbol("render_widget", 1, 6),
        ];
        let rows = |query| -> Vec<&str> {
            symbol_rows(&symbols, query)
                .into_iter()
                .map(|(ix, _)| symbols[ix].name.as_str())
                .collect()
        };
        assert_eq!(rows(""), ["Widget", "spin", "render_widget"]);
        assert_eq!(rows("widg"), ["Widget", "render_widget"]);
        assert_eq!(rows("rw"), ["render_widget"]);
        assert!(rows("zz").is_empty());
    }

    #[test]
    fn finds_each_name_in_its_line() {
        let text = TextSnapshot::new(
            b"struct Widget;\nimpl Widget {\n    fn spin() {}\n}\n".to_vec(),
            "w.rs",
        );
        let outline = Outline::from_response(serde_json::json!([
            {
                "name": "Widget", "kind": 23,
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 14}},
                "selectionRange": {"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 13}},
            },
            {
                "name": "impl Widget", "kind": 19,
                "range": {"start": {"line": 1, "character": 0}, "end": {"line": 3, "character": 1}},
                "selectionRange": {"start": {"line": 1, "character": 5}, "end": {"line": 1, "character": 11}},
                "children": [{
                    "name": "spin", "kind": 12,
                    "range": {"start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 16}},
                    "selectionRange": {"start": {"line": 2, "character": 7}, "end": {"line": 2, "character": 11}},
                }],
            },
        ]))
        .unwrap();
        let found = file_symbols(&outline, &text, PositionEncoding::Utf16);
        let at: Vec<_> = found
            .iter()
            .map(|s| (s.name.as_str(), s.depth, s.line, s.name_range.clone()))
            .collect();
        assert_eq!(
            at,
            [
                ("Widget", 0, 0, 7..13),
                // Named for more than its name: just where it points.
                ("impl Widget", 0, 1, 5..5),
                ("spin", 1, 2, 7..11),
            ]
        );
    }
}
