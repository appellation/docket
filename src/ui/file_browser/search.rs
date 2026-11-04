//! The search atop the tree's sidebar: a field, the mode it searches in
//! (file names, contents, or symbols), and while it holds a query, the
//! results in the tree's place (`state::search` does the searching).
//!
//! ⌘P, ⌘⇧F, and ⌘⇧T focus the field in each mode, keeping the query, so
//! one search can be tried each way. Up and down move through the results
//! and Enter opens one; Escape clears the field and returns to the file.
//! A file opened by name closes the search, as a quick open would, while
//! a line or symbol keeps the results there to open the next.

use std::ops::Range;

use gpui::{
    AnyElement, App, Context, Entity, Focusable as _, FontWeight, HighlightStyle, KeyBinding,
    MouseButton, ScrollStrategy, SharedString, StyledText, Window, actions, div, prelude::*, px,
    uniform_list,
};

use super::{FileBrowserView, Reveal, SelectNext, SelectPrevious};
use crate::Cancel;
use crate::lsp::SymbolKind;
use crate::state::{BrowserSearch, CONTENT_RESULTS, SearchMode, SearchResults, matched_ranges};
use crate::syntax::LanguageRegistry;
use crate::ui::code::Definition;
use crate::ui::components::{
    ButtonSize, Icon, IconButton, IconName, IconSize, MONOSPACE, Spinner, TestSelector as _,
    TreeItem,
};
use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::ui::theme::{Color, Theme};

actions!(
    file_browser,
    [
        /// Focuses the search, for files by name.
        FindFile,
        /// Focuses the search, for text in files.
        FindInFiles,
        /// Focuses the search, for symbols.
        FindSymbol
    ]
);

pub(super) const SEARCH_CONTEXT: &str = "FileSearch";

/// A row of the results list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SearchRow {
    /// A file whose name matched, by its index in the results.
    File(usize),
    /// A file with matching lines, by its index in the results.
    HitFile(usize),
    /// A matching line: its file's index, then the line's.
    Hit(usize, usize),
    Symbol(usize),
}

/// The search's part of the view.
pub(super) struct SearchState {
    pub input: Entity<TextInput>,
    /// The `BrowserSearch::revision` `rows` were built from.
    revision: Option<u64>,
    pub rows: Vec<SearchRow>,
    /// The row Enter opens.
    pub selected: Option<usize>,
    scroll: gpui::UniformListScrollHandle,
}

pub(super) fn bind_keys(context: &'static str, cx: &mut App) {
    for context in [Some(context), Some(SEARCH_CONTEXT)] {
        cx.bind_keys([
            KeyBinding::new("cmd-p", FindFile, context),
            KeyBinding::new("cmd-shift-f", FindInFiles, context),
            KeyBinding::new("cmd-shift-t", FindSymbol, context),
        ]);
    }
    let context = Some(SEARCH_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
    ]);
}

fn placeholder(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Files => "Go to file",
        SearchMode::Contents => "Search in files",
        SearchMode::Symbols => "Go to symbol",
    }
}

impl SearchState {
    pub fn new(cx: &mut Context<FileBrowserView>) -> Self {
        let input = cx.new(|cx| TextInput::new(placeholder(SearchMode::Files), cx));
        cx.subscribe(
            &input,
            |this: &mut FileBrowserView, input, event, cx| match event {
                TextInputEvent::Changed => {
                    let query = input.read(cx).text().to_owned();
                    this.state
                        .update(cx, |state, cx| state.set_search_query(&query, cx));
                }
                TextInputEvent::Submit => this.open_search_row(None, cx),
            },
        )
        .detach();
        Self {
            input,
            revision: None,
            rows: Vec::new(),
            selected: None,
            scroll: gpui::UniformListScrollHandle::new(),
        }
    }
}

impl FileBrowserView {
    fn browser_search<'a>(&self, cx: &'a App) -> Option<&'a BrowserSearch> {
        Some(&self.state.read(cx).browser.as_ref()?.search)
    }

    /// Whether results show in the tree's place.
    pub(super) fn searching(&self, cx: &App) -> bool {
        self.browser_search(cx)
            .is_some_and(BrowserSearch::is_active)
    }

    /// Rebuilds the result rows when the results change, selecting the
    /// first.
    fn sync_search_rows(&mut self, cx: &App) {
        let Some(search) = self.browser_search(cx) else {
            return;
        };
        if self.search.revision == Some(search.revision) {
            return;
        }
        let rows: Vec<SearchRow> = match &search.results {
            SearchResults::Files(files) => (0..files.len()).map(SearchRow::File).collect(),
            SearchResults::Contents(found) => found
                .files
                .iter()
                .enumerate()
                .flat_map(|(file, hits)| {
                    std::iter::once(SearchRow::HitFile(file))
                        .chain((0..hits.lines.len()).map(move |line| SearchRow::Hit(file, line)))
                })
                .collect(),
            SearchResults::Symbols { symbols, .. } => {
                (0..symbols.len()).map(SearchRow::Symbol).collect()
            }
            SearchResults::None | SearchResults::Failed(_) => Vec::new(),
        };
        // A file's heading opens it, but the first line is what's wanted.
        self.search.selected = rows
            .iter()
            .position(|row| !matches!(row, SearchRow::HitFile(_)))
            .or((!rows.is_empty()).then_some(0));
        self.search.rows = rows;
        self.search.revision = Some(search.revision);
        if let Some(ix) = self.search.selected {
            self.search.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        }
    }

    /// ⌘P, ⌘⇧F, ⌘⇧T: focuses the field in `mode`, its text selected.
    fn find(&mut self, mode: SearchMode, window: &mut Window, cx: &mut Context<Self>) {
        self.set_search_mode(mode, cx);
        let input = self.search.input.clone();
        input.update(cx, |input, cx| input.select_all_text(cx));
        window.focus(&input.read(cx).focus_handle(cx));
        cx.notify();
    }

    fn set_search_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.set_search_mode(mode, cx);
            state.prepare_search(cx);
        });
        self.search
            .input
            .update(cx, |input, cx| input.set_placeholder(placeholder(mode), cx));
    }

    pub(super) fn find_file(&mut self, _: &FindFile, window: &mut Window, cx: &mut Context<Self>) {
        self.find(SearchMode::Files, window, cx);
    }

    pub(super) fn find_in_files(
        &mut self,
        _: &FindInFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.find(SearchMode::Contents, window, cx);
    }

    pub(super) fn find_symbol(
        &mut self,
        _: &FindSymbol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.find(SearchMode::Symbols, window, cx);
    }

    /// Escape: clears the search, and gives the keyboard back to the file.
    fn close_search(&mut self, cx: &mut Context<Self>) {
        // Clearing the field clears the search.
        self.search.input.update(cx, |input, cx| {
            input.take(cx);
        });
        self.focused = false;
        cx.notify();
    }

    fn move_search_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.search.rows.len();
        if len == 0 {
            return;
        }
        let ix = match self.search.selected {
            Some(ix) => ix.saturating_add_signed(delta).min(len - 1),
            None => 0,
        };
        self.search.selected = Some(ix);
        let strategy = if delta > 0 {
            ScrollStrategy::Bottom
        } else {
            ScrollStrategy::Top
        };
        self.search.scroll.scroll_to_item(ix, strategy);
        cx.notify();
    }

    /// Opens a row (the selected one, for `None`): a file, or a line or
    /// symbol in one, selected.
    fn open_search_row(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        let Some(ix) = ix.or(self.search.selected) else {
            return;
        };
        let Some(&row) = self.search.rows.get(ix) else {
            return;
        };
        self.search.selected = Some(ix);
        let Some(search) = self.browser_search(cx) else {
            return;
        };
        let place = self.place();
        match (row, &search.results) {
            (SearchRow::File(file), SearchResults::Files(files)) => {
                let path = files[file].path.clone();
                self.navigate(path, false, cx);
                // Opening a file by name is done with the search.
                self.close_search(cx);
            }
            (SearchRow::HitFile(file), SearchResults::Contents(found)) => {
                let path = found.files[file].path.clone();
                self.navigate(path, false, cx);
            }
            (SearchRow::Hit(file, line), SearchResults::Contents(found)) => {
                let path = found.files[file].path.clone();
                let hit = &found.files[file].lines[line];
                let reveal = Reveal::Range {
                    line: hit.line,
                    range: hit.first.clone(),
                };
                self.back.extend(place);
                self.show(path, reveal, cx);
            }
            (SearchRow::Symbol(symbol), SearchResults::Symbols { symbols, .. }) => {
                let symbol = &symbols[symbol];
                let definition = Definition {
                    location: symbol.location.clone(),
                    relative: Some(symbol.path.clone()),
                    encoding: symbol.encoding,
                };
                let path = symbol.path.clone();
                self.back.extend(place);
                self.show(path, Reveal::Definition(definition), cx);
            }
            _ => {}
        }
    }

    /// The field, the modes, and a summary of the results.
    pub(super) fn render_search_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.sync_search_rows(cx);
        let theme = Theme::get(cx);
        let Some(search) = self.browser_search(cx) else {
            return div().into_any_element();
        };
        let mode = search.mode;
        let input = self.search.input.clone();
        let mode_button = |id: &'static str, icon, tooltip: &'static str, button_mode, cx| {
            let input = input.clone();
            IconButton::new(id, icon)
                .size(ButtonSize::Compact)
                .tooltip(tooltip)
                .selected(mode == button_mode)
                .on_click(Context::listener(
                    cx,
                    move |this: &mut Self, _, window, cx| {
                        this.set_search_mode(button_mode, cx);
                        window.focus(&input.read(cx).focus_handle(cx));
                    },
                ))
        };
        let toggles = (mode == SearchMode::Contents).then(|| {
            div()
                .flex()
                .items_center()
                .child(
                    IconButton::new("search-case", IconName::CaseSensitive)
                        .size(ButtonSize::Compact)
                        .tooltip("Match case")
                        .selected(search.case_sensitive)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state
                                .update(cx, |state, cx| state.toggle_search_case(cx));
                        })),
                )
                .child(
                    IconButton::new("search-regex", IconName::Regex)
                        .size(ButtonSize::Compact)
                        .tooltip("Use regular expression")
                        .selected(search.regex)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state
                                .update(cx, |state, cx| state.toggle_search_regex(cx));
                        })),
                )
        });
        let summary = search.is_active().then(|| summary(search)).flatten();
        let spinning = search.is_active() && search.searching;

        div()
            .key_context(SEARCH_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_search_selection(1, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPrevious, _, cx| this.move_search_selection(-1, cx)),
            )
            // Escape is the app's Cancel, which reaches the field first.
            .on_action(cx.listener(|this, _: &Cancel, _, cx| this.close_search(cx)))
            .on_action(cx.listener(Self::find_file))
            .on_action(cx.listener(Self::find_in_files))
            .on_action(cx.listener(Self::find_symbol))
            // Keeps the keyboard in the field, rather than the sidebar
            // handing it to the file.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .flex()
            .flex_none()
            .flex_col()
            .gap_1()
            .px_2()
            .pt_2()
            .pb_1()
            .border_b_1()
            .border_color(theme.border_subtle)
            .child(
                div()
                    .test_selector(|| "browser-search-input".into())
                    .text_xs()
                    .child(self.search.input.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(mode_button(
                                "search-mode-files",
                                IconName::FileSearch,
                                "Files by name (⌘P)",
                                SearchMode::Files,
                                cx,
                            ))
                            .child(mode_button(
                                "search-mode-contents",
                                IconName::TextSearch,
                                "Text in files (⌘⇧F)",
                                SearchMode::Contents,
                                cx,
                            ))
                            .child(mode_button(
                                "search-mode-symbols",
                                IconName::Braces,
                                "Symbols in the project (⌘⇧T)",
                                SearchMode::Symbols,
                                cx,
                            ))
                            .children(toggles),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.text_subtle)
                            .when(spinning, |el| el.child(Spinner::new("browser-searching")))
                            .children(summary.map(|summary| {
                                div()
                                    .test_selector(|| "search-summary".into())
                                    .truncate()
                                    .child(summary)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// The results, in the tree's place.
    pub(super) fn render_search_results(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx);
        let Some(search) = self.browser_search(cx) else {
            return div().into_any_element();
        };
        let notice = |message: SharedString, icon: Option<(IconName, Color)>| {
            div()
                .test_selector(|| "search-notice".into())
                .flex()
                .items_start()
                .gap_2()
                .px_4()
                .py_3()
                .text_xs()
                .text_color(theme.text_muted)
                .children(icon.map(|(icon, color)| {
                    div()
                        .flex_none()
                        .pt(px(1.))
                        .child(Icon::new(icon).size(IconSize::XSmall).color(color))
                }))
                .child(div().flex_1().min_w_0().child(message))
        };
        let note = match &search.results {
            SearchResults::Failed(err) => {
                return notice(err.clone(), Some((IconName::TriangleAlert, Color::Warning)))
                    .into_any_element();
            }
            SearchResults::Symbols { note, .. } => note.clone(),
            _ => None,
        };
        if self.search.rows.is_empty() {
            if !search.results.is_for(search.mode) || search.searching && note.is_none() {
                return div().into_any_element();
            }
            let empty = match search.mode {
                SearchMode::Files => "No files match.",
                SearchMode::Contents => "Nothing in the files matches.",
                SearchMode::Symbols => "No symbols match.",
            };
            return div()
                .flex()
                .flex_col()
                .child(notice(empty.into(), None))
                .children(note.map(|note| notice(note, Some((IconName::Info, Color::Muted)))))
                .into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .children(note.map(|note| notice(note, Some((IconName::Info, Color::Muted)))))
            .child(
                uniform_list(
                    "browser-search-results",
                    self.search.rows.len(),
                    cx.processor(Self::render_search_rows),
                )
                .flex_1()
                .py_1()
                .track_scroll(self.search.scroll.clone()),
            )
            .into_any_element()
    }

    fn render_search_rows(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::get(cx);
        let Some(search) = self.browser_search(cx) else {
            return Vec::new();
        };
        let matched = HighlightStyle {
            color: Some(theme.accent),
            font_weight: Some(FontWeight::SEMIBOLD),
            ..Default::default()
        };
        let highlighted = |text: &str, ranges: Vec<Range<usize>>| {
            StyledText::new(text.to_owned())
                .with_highlights(ranges.into_iter().map(|r| (r, matched)))
        };
        // Given one child: a div's children stack, like lines.
        let dim = |text: AnyElement| {
            div()
                .flex_shrink()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(theme.text_subtle)
                .child(text)
        };
        let label = |ix: usize, children: Vec<AnyElement>| {
            div()
                .test_selector(move || format!("search-label-{ix}"))
                .flex()
                .items_baseline()
                .gap_1p5()
                .min_w_0()
                .overflow_hidden()
                .children(children)
        };
        range
            .filter_map(|ix| {
                let row = *self.search.rows.get(ix)?;
                let selected = self.search.selected == Some(ix);
                let item = |selector: String, depth| {
                    TreeItem::new(("browser-search-row", ix), depth)
                        .selector(selector)
                        .selected(selected)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.open_search_row(Some(ix), cx)),
                        )
                };
                let element = match (row, &search.results) {
                    (SearchRow::File(file), SearchResults::Files(files)) => {
                        let found = &files[file];
                        let path = found.path.as_str();
                        let name_start = path.rfind('/').map_or(0, |ix| ix + 1);
                        let ranges = matched_ranges(path, &found.positions);
                        let (dir_ranges, name_ranges) = split_ranges(&ranges, name_start);
                        let dir = &path[..name_start.saturating_sub(1)];
                        item(format!("search-file-{path}"), 0)
                            .icon(file_icon(path), Color::Muted)
                            .child(label(
                                ix,
                                vec![
                                    div()
                                        .flex_none()
                                        .child(highlighted(&path[name_start..], name_ranges))
                                        .into_any_element(),
                                    dim(highlighted(dir, dir_ranges).into_any_element())
                                        .into_any_element(),
                                ],
                            ))
                            .into_any_element()
                    }
                    (SearchRow::HitFile(file), SearchResults::Contents(found)) => {
                        let hits = &found.files[file];
                        let path = hits.path.as_str();
                        let name_start = path.rfind('/').map_or(0, |ix| ix + 1);
                        item(format!("search-hit-file-{path}"), 0)
                            .icon(file_icon(path), Color::Muted)
                            .child(label(
                                ix,
                                vec![
                                    div()
                                        .flex_none()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(path[name_start..].to_owned())
                                        .into_any_element(),
                                    dim(path[..name_start.saturating_sub(1)]
                                        .to_owned()
                                        .into_any_element())
                                    .into_any_element(),
                                ],
                            ))
                            .end_slot(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.text_subtle)
                                    .child(hits.lines.len().to_string()),
                            )
                            .into_any_element()
                    }
                    (SearchRow::Hit(file, line), SearchResults::Contents(found)) => {
                        let hits = &found.files[file];
                        let hit = &hits.lines[line];
                        item(format!("search-hit-{}:{}", hits.path, hit.line + 1), 1)
                            .child(label(
                                ix,
                                vec![
                                    div()
                                        .flex_none()
                                        .font_family(MONOSPACE)
                                        .text_xs()
                                        .text_color(theme.text_subtle)
                                        .child((hit.line + 1).to_string())
                                        .into_any_element(),
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(MONOSPACE)
                                        .text_xs()
                                        .child(highlighted(&hit.snippet, hit.ranges.clone()))
                                        .into_any_element(),
                                ],
                            ))
                            .into_any_element()
                    }
                    (SearchRow::Symbol(symbol), SearchResults::Symbols { symbols, .. }) => {
                        let found = &symbols[symbol];
                        let at = match &found.container {
                            Some(container) => format!("{container} · {}", found.path),
                            None => found.path.clone(),
                        };
                        item(format!("search-symbol-{}", found.name), 0)
                            .icon(IconName::Braces, Color::Muted)
                            .child(label(
                                ix,
                                vec![
                                    div()
                                        .flex_none()
                                        .child(highlighted(
                                            &found.name,
                                            matched_ranges(&found.name, &found.positions),
                                        ))
                                        .into_any_element(),
                                    dim(at.into_any_element()).into_any_element(),
                                ],
                            ))
                            .end_slot(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.text_subtle)
                                    .child(kind_label(found.kind)),
                            )
                            .into_any_element()
                    }
                    _ => return None,
                };
                Some(element)
            })
            .collect()
    }
}

/// What the results come to, beside the modes.
fn summary(search: &BrowserSearch) -> Option<String> {
    let plural = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{} {many}", thousands(n)),
    };
    Some(match &search.results {
        SearchResults::Files(files) => plural(files.len(), "file", "files"),
        SearchResults::Contents(found) => {
            let lines = plural(found.lines, "line", "lines");
            let lines = if found.truncated && found.lines >= CONTENT_RESULTS {
                format!("First {lines}")
            } else {
                lines
            };
            format!("{lines} in {}", plural(found.files.len(), "file", "files"))
        }
        SearchResults::Symbols { symbols, .. } => plural(symbols.len(), "symbol", "symbols"),
        SearchResults::None | SearchResults::Failed(_) => return None,
    })
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (ix, c) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `ranges` in a path split at its name, which starts at `name_start`
/// (after a `/`): those in the directory, and those in the name, from its
/// start.
fn split_ranges(
    ranges: &[Range<usize>],
    name_start: usize,
) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let dir_end = name_start.saturating_sub(1);
    let dir = ranges
        .iter()
        .filter(|r| r.start < dir_end)
        .map(|r| r.start..r.end.min(dir_end))
        .collect();
    let name = ranges
        .iter()
        .filter(|r| r.end > name_start)
        .map(|r| (r.start.max(name_start) - name_start)..(r.end - name_start))
        .collect();
    (dir, name)
}

fn file_icon(path: &str) -> IconName {
    if LanguageRegistry::for_path(path).is_some() {
        IconName::FileCode
    } else {
        IconName::File
    }
}

pub(super) fn kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Module | SymbolKind::Namespace | SymbolKind::Package => "module",
        SymbolKind::Class => "class",
        SymbolKind::Method => "method",
        SymbolKind::Property | SymbolKind::Field => "field",
        SymbolKind::Constructor => "constructor",
        SymbolKind::Enum => "enum",
        SymbolKind::Interface => "interface",
        SymbolKind::Function => "function",
        SymbolKind::Variable => "variable",
        SymbolKind::Constant => "constant",
        SymbolKind::Object => "impl",
        SymbolKind::EnumMember => "variant",
        SymbolKind::Struct => "struct",
        SymbolKind::Other => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_highlights_at_the_name() {
        // `src/ui/view.rs`: the name starts at 7.
        let (dir, name) = split_ranges(&[0..2, 5..8, 9..10], 7);
        assert_eq!(dir, [0..2, 5..6]);
        assert_eq!(name, [0..1, 2..3]);
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(12), "12");
    }
}
