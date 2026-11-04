//! A branch's files, opened from the docket, or the project's checkout,
//! or a pull request's or branch's Browse tab: the directory tree in the
//! sidebar (`tree_sidebar`), and the selected file, syntax-highlighted,
//! beside it. A checked-out branch shows as it is on disk; any other as it
//! is at its tip, checked out in Docket's worktree (`state::browse`). On
//! its own it has a header of its own;
//! in a Browse tab, the pull request's or branch's is above it.
//!
//! The tree, the file, and the main panel's directory listing are
//! virtualized lists, so a large checkout or file only lays out what's
//! visible, and each directory is listed the first time it's expanded.
//! The tree works from the keyboard: up and down move through it, showing
//! each file they reach; right and left expand and collapse directories,
//! or move into and out of them.
//!
//! The main panel shows the open file's code, or while none is open, a
//! directory's entries: the root's to start, or a directory's clicked (or
//! entered) in the tree. Clicking an entry there opens it, a directory
//! listing in its place, and the path above (the open file's too) links to
//! each directory it's in, so the checkout can be walked from the main
//! panel alone. The tree follows, expanding to show where it is.
//!
//! The file's code selects, copies, and hovers as in the diff (`ui/code`),
//! with language servers running where the files are. Command-clicking
//! a symbol opens its definition here when it's in the checkout, selecting
//! it in the tree, and peeks at it otherwise (the standard library, a
//! package outside the project). Command-option-clicking opens it in a new
//! tab's browser.
//!
//! A search atop the tree finds files by name, lines of text, or symbols,
//! its results in the tree's place while it holds a query (`search.rs`).

mod find;
mod outline;
mod search;
mod tree;

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, HighlightStyle, KeyBinding,
    ListHorizontalSizingBehavior, MouseButton, Pixels, Point, ScrollStrategy,
    UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

use self::find::FindState;
use self::search::SearchState;
use self::tree::{TreeRow, parent_row, visible_rows};
use super::code::{
    self, Ask, CodeDefinition, CodeHover, CodeState, CodeView, Definition, GoBack, Granularity,
    Marks, ROW_HEIGHT, Selection, Span, TextPoint, display_width, drag_listeners, selected_newline,
    styled_line,
};
use super::components::{
    Callout, CalloutKind, Icon, IconButton, IconName, IconSize, MONOSPACE, Spinner,
    TestSelector as _, Tooltip, TreeItem, display_path,
};
use super::diff_view::server_status;
use super::sidebar_stack::{rail_header, rail_items, sidebar_header};
use super::theme::{Color, Theme};
use crate::projects::ProjectId;
use crate::projects::files::{EntryKind, LocalFile};
use crate::state::{BrowserScope, DirLoad, FileLoad, LspTarget, OpenFile, ProjectBrowser, Tab};
use crate::syntax::LanguageRegistry;

actions!(
    file_browser,
    [
        SelectNext,
        SelectPrevious,
        ExpandSelected,
        CollapseSelected,
        ToggleSelected
    ]
);

const KEY_CONTEXT: &str = "FileBrowser";
/// A row of the main panel's directory listing.
const ENTRY_HEIGHT: f32 = 32.;
/// About one digit of the monospace font at `text_xs`.
const DIGIT_WIDTH: f32 = 7.5;

pub struct FileBrowserView {
    state: Entity<Tab>,
    /// The `ProjectBrowser::revision` `rows` were built from.
    revision: Option<u64>,
    rows: Vec<TreeRow>,
    /// The path of the row the keyboard moves from.
    selected: Option<String>,
    tree_scroll: UniformListScrollHandle,
    /// The open file's scroll position, selection, hover, and the lines
    /// laid out in the last frame.
    code: CodeState,
    /// The open file's path and contents `widest` was measured for.
    measured: Option<(String, Arc<LocalFile>)>,
    /// The open file's widest line, which sizes its list horizontally.
    widest: Option<usize>,
    focus_handle: FocusHandle,
    focused: bool,
    /// Where each jump to a definition came from, latest last.
    back: Vec<BrowserPlace>,
    /// Where to go in a file once it's shown: its path, and what to show.
    pending: Option<(String, Reveal)>,
    /// A path to scroll the tree to once its row is listed.
    reveal_in_tree: Option<String>,
    /// The open file as last seen, to select one opened from elsewhere
    /// (the diff) in the tree.
    followed_file: Option<String>,
    /// The main panel's directory listing, and the directory it's of.
    dir_scroll: UniformListScrollHandle,
    scrolled_dir: Option<String>,
    /// The search atop the tree, and its results.
    search: SearchState,
    /// Find in the open file.
    find: FindState,
}

/// A place in the checkout to go back to.
struct BrowserPlace {
    path: String,
    scroll: Point<Pixels>,
    selection: Option<Span>,
}

/// What to show in a file once it's open.
enum Reveal {
    /// Scroll to the definition and select its name.
    Definition(Definition),
    /// Scroll to a line and select `range` of it, as a search result
    /// does.
    Range { line: u32, range: Range<usize> },
    /// Return to a place.
    Place {
        scroll: Point<Pixels>,
        selection: Option<Span>,
    },
}

impl FileBrowserView {
    pub fn bind_keys(cx: &mut App) {
        let context = Some(KEY_CONTEXT);
        cx.bind_keys([
            KeyBinding::new("down", SelectNext, context),
            KeyBinding::new("up", SelectPrevious, context),
            KeyBinding::new("right", ExpandSelected, context),
            KeyBinding::new("left", CollapseSelected, context),
            KeyBinding::new("enter", ToggleSelected, context),
        ]);
        code::bind_keys(KEY_CONTEXT, cx);
        search::bind_keys(KEY_CONTEXT, cx);
        find::bind_keys(KEY_CONTEXT, cx);
    }

    pub fn new(state: Entity<Tab>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            revision: None,
            rows: Vec::new(),
            selected: None,
            tree_scroll: UniformListScrollHandle::new(),
            code: CodeState::default(),
            measured: None,
            widest: None,
            focus_handle: cx.focus_handle(),
            focused: false,
            back: Vec::new(),
            pending: None,
            reveal_in_tree: None,
            followed_file: None,
            dir_scroll: UniformListScrollHandle::new(),
            scrolled_dir: None,
            search: SearchState::new(cx),
            find: FindState::new(cx),
        }
    }

    /// Selects a file opened from elsewhere, like the diff, in the tree,
    /// and scrolls the tree to it.
    fn follow_open_file(&mut self, cx: &App) {
        let path = self
            .state
            .read(cx)
            .browser
            .as_ref()
            .and_then(|browser| browser.file.as_ref())
            .map(|file| file.path.clone());
        if path == self.followed_file {
            return;
        }
        if let Some(path) = &path
            && self.selected.as_ref() != Some(path)
        {
            self.selected = Some(path.clone());
            self.reveal_in_tree = Some(path.clone());
        }
        self.followed_file = path;
    }

    /// Goes to an entry from the main panel (its listing, or a link in the
    /// path above): lists a directory (`""` for the root) in its place, or
    /// opens a file. The tree expands to it and selects it.
    fn navigate(&mut self, path: String, is_dir: bool, cx: &mut Context<Self>) {
        self.dismiss_popovers(cx);
        self.state.update(cx, |state, cx| {
            for (ix, _) in path.match_indices('/') {
                state.set_browser_expanded(&path[..ix], true, cx);
            }
            if is_dir {
                if !path.is_empty() {
                    state.set_browser_expanded(&path, true, cx);
                }
                state.show_browser_dir(&path, cx);
            } else {
                state.open_browser_file(path.clone(), cx);
            }
        });
        if path.is_empty() {
            self.selected = None;
        } else {
            self.selected = Some(path.clone());
            self.reveal_in_tree = Some(path);
        }
        cx.notify();
    }

    /// Rebuilds the rows if a listing or the expanded directories changed.
    fn sync_rows(&mut self, cx: &App) {
        let Some(browser) = &self.state.read(cx).browser else {
            return;
        };
        if self.revision == Some(browser.revision) {
            return;
        }
        self.rows = visible_rows(&browser.dirs, &browser.expanded);
        self.revision = Some(browser.revision);
    }

    fn is_expanded(&self, path: &str, cx: &App) -> bool {
        self.state
            .read(cx)
            .browser
            .as_ref()
            .is_some_and(|browser| browser.expanded.contains(path))
    }

    fn selected_row(&self) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        self.rows.iter().position(|row| row.entry.path == selected)
    }

    /// Moves the selection to a row, keeping it in view, and shows it if
    /// it's a file.
    fn select_row(&mut self, ix: usize, strategy: ScrollStrategy, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else {
            return;
        };
        let entry = row.entry.clone();
        self.selected = Some(entry.path.clone());
        self.reveal_in_tree = None;
        self.tree_scroll.scroll_to_item(ix, strategy);
        if entry.kind != EntryKind::Dir {
            self.state
                .update(cx, |state, cx| state.open_browser_file(entry.path, cx));
        }
        cx.notify();
    }

    fn set_expanded(&mut self, path: &str, expanded: bool, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.set_browser_expanded(path, expanded, cx);
        });
        self.sync_rows(cx);
    }

    fn click_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.select_row(ix, ScrollStrategy::Top, cx);
        self.toggle_selected(&ToggleSelected, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let next = match self.selected_row() {
            Some(ix) => (ix + 1).min(self.rows.len().saturating_sub(1)),
            None => 0,
        };
        self.select_row(next, ScrollStrategy::Bottom, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        let previous = self.selected_row().map_or(0, |ix| ix.saturating_sub(1));
        self.select_row(previous, ScrollStrategy::Top, cx);
    }

    /// Right: expands a directory, or moves into one that's expanded.
    fn expand_selected(&mut self, _: &ExpandSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_row() else {
            return;
        };
        let row = self.rows[ix].clone();
        if row.entry.kind != EntryKind::Dir {
            return;
        }
        if !self.is_expanded(&row.entry.path, cx) {
            self.set_expanded(&row.entry.path, true, cx);
        } else if self
            .rows
            .get(ix + 1)
            .is_some_and(|next| next.depth > row.depth)
        {
            self.select_row(ix + 1, ScrollStrategy::Bottom, cx);
        }
    }

    /// Left: collapses an expanded directory, or moves to the one the
    /// selection is in.
    fn collapse_selected(&mut self, _: &CollapseSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_row() else {
            return;
        };
        let entry = self.rows[ix].entry.clone();
        if entry.kind == EntryKind::Dir && self.is_expanded(&entry.path, cx) {
            self.set_expanded(&entry.path, false, cx);
        } else if let Some(parent) = parent_row(&self.rows, ix) {
            self.select_row(parent, ScrollStrategy::Top, cx);
        }
    }

    /// Enter, or a click: expands or collapses a directory, and lists it
    /// in the main panel.
    fn toggle_selected(&mut self, _: &ToggleSelected, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_row() else {
            return;
        };
        let entry = self.rows[ix].entry.clone();
        if entry.kind == EntryKind::Dir {
            let expanded = self.is_expanded(&entry.path, cx);
            self.set_expanded(&entry.path, !expanded, cx);
            self.state
                .update(cx, |state, cx| state.show_browser_dir(&entry.path, cx));
        }
    }

    /// Re-measures the open file when its contents change, and starts a
    /// newly opened one at the top.
    fn sync_measured(&mut self, file: Option<&OpenFile>) {
        let content = file.and_then(|file| match &file.content {
            FileLoad::Ready(content) => Some((file.path.as_str(), content)),
            _ => None,
        });
        let Some((path, content)) = content else {
            // A directory is listed in its place.
            if file.is_none() && self.measured.take().is_some() {
                self.code = CodeState::default();
            }
            return;
        };
        if let Some((measured_path, measured)) = &self.measured
            && measured_path == path
            && Arc::ptr_eq(measured, content)
        {
            return;
        }
        // A new file starts at the top; new text in the same file keeps its
        // place, but not positions in the old text.
        if self.measured.as_ref().is_none_or(|(p, _)| p != path) {
            self.code = CodeState::default();
        } else {
            self.code.reset();
        }
        self.widest = match &**content {
            LocalFile::Text(text) => (0..text.line_count())
                .max_by_key(|&line| display_width(text.line(line)))
                .map(|line| line as usize),
            _ => None,
        };
        self.measured = Some((path.to_owned(), content.clone()));
    }

    /// Where the open file is scrolled to, and its selection.
    fn place(&self) -> Option<BrowserPlace> {
        let (path, _) = self.measured.as_ref()?;
        Some(BrowserPlace {
            path: path.clone(),
            scroll: self.code.scroll.0.borrow().base_handle.offset(),
            selection: self.code.selection.as_ref().map(Selection::range),
        })
    }

    /// Opens `path`, selecting it in the tree with the directories it's in
    /// expanded, and shows `reveal` in it once it's read.
    fn show(&mut self, path: String, reveal: Reveal, cx: &mut Context<Self>) {
        let dirs: Vec<String> = path
            .match_indices('/')
            .map(|(ix, _)| path[..ix].to_owned())
            .collect();
        self.state.update(cx, |state, cx| {
            for dir in &dirs {
                state.set_browser_expanded(dir, true, cx);
            }
            state.open_browser_file(path.clone(), cx);
        });
        self.selected = Some(path.clone());
        self.reveal_in_tree = Some(path.clone());
        self.pending = Some((path, reveal));
        cx.notify();
    }

    /// Applies what's pending for the open file, once it's shown. Drops it
    /// if another file was opened since.
    fn apply_pending(&mut self, file: Option<&OpenFile>) {
        let Some((path, _)) = &self.pending else {
            return;
        };
        if file.is_none_or(|file| file.path != *path) {
            self.pending = None;
            return;
        }
        let text = match &self.measured {
            Some((measured, content)) if measured == path => match &**content {
                LocalFile::Text(text) => text.clone(),
                _ => {
                    self.pending = None;
                    return;
                }
            },
            // Still reading it.
            _ => return,
        };
        let Some((_, reveal)) = self.pending.take() else {
            return;
        };
        match reveal {
            Reveal::Definition(definition) => {
                let Some(last) = text.line_count().checked_sub(1) else {
                    return;
                };
                let line = definition.line().min(last);
                let name = definition.name_in(text.line(line));
                let at = |offset| TextPoint {
                    row: line as usize,
                    offset,
                };
                self.code.selection = Some(Selection::new(
                    (at(name.start), at(name.end)),
                    Granularity::Character,
                ));
                self.code
                    .scroll
                    .scroll_to_item(line as usize, ScrollStrategy::Center);
            }
            Reveal::Range { line, range } => {
                let Some(last) = text.line_count().checked_sub(1) else {
                    return;
                };
                let line = line.min(last);
                let len = text.line(line).len();
                let at = |offset: usize| TextPoint {
                    row: line as usize,
                    offset: offset.min(len),
                };
                self.code.selection = Some(Selection::new(
                    (at(range.start), at(range.end)),
                    Granularity::Character,
                ));
                self.code
                    .scroll
                    .scroll_to_item(line as usize, ScrollStrategy::Center);
            }
            Reveal::Place { scroll, selection } => {
                self.code.scroll.0.borrow().base_handle.set_offset(scroll);
                self.code.selection =
                    selection.map(|span| Selection::new(span, Granularity::Character));
            }
        }
    }

    /// Opens the file a definition in the checkout is in, selecting its
    /// name.
    pub fn show_definition(&mut self, definition: Definition, cx: &mut Context<Self>) {
        if let Some(path) = definition.relative.clone() {
            self.show(path, Reveal::Definition(definition), cx);
        }
    }

    /// Returns to where the last jump to a definition came from.
    fn go_back(&mut self, _: &GoBack, _: &mut Window, cx: &mut Context<Self>) {
        let Some(place) = self.back.pop() else {
            return;
        };
        self.dismiss_popovers(cx);
        let reveal = Reveal::Place {
            scroll: place.scroll,
            selection: place.selection,
        };
        self.show(place.path, reveal, cx);
    }

    fn render_tree_rows(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(browser) = &self.state.read(cx).browser else {
            return Vec::new();
        };
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?;
                let entry = &row.entry;
                let selected = self.selected.as_deref() == Some(entry.path.as_str());
                let expanded = browser.expanded.contains(&entry.path);
                let icon = match entry.kind {
                    EntryKind::Dir if expanded => IconName::FolderOpen,
                    EntryKind::Dir => IconName::Folder,
                    EntryKind::Repository => IconName::FolderGit,
                    EntryKind::File if LanguageRegistry::for_path(&entry.path).is_some() => {
                        IconName::FileCode
                    }
                    EntryKind::File => IconName::File,
                };
                let mut item = TreeItem::new(("browser-tree-row", ix), row.depth)
                    .selector(format!("browser-tree-{}", entry.path))
                    .selected(selected)
                    .icon(icon, Color::Muted)
                    .child(entry.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.click_row(ix, cx)));
                // A directory being listed spins in place of its chevron;
                // one that couldn't be says why on hover.
                if entry.kind == EntryKind::Dir {
                    item = match browser.dirs.get(&entry.path) {
                        Some(DirLoad::Loading) if expanded => {
                            item.lead(Spinner::new(format!("browser-tree-loading-{ix}")))
                        }
                        Some(DirLoad::Failed(err)) if expanded => item
                            .lead(
                                Icon::new(IconName::TriangleAlert)
                                    .size(IconSize::XSmall)
                                    .color(Color::Warning),
                            )
                            .tooltip(err.clone()),
                        _ => item.disclosure(expanded),
                    };
                }
                Some(item.into_any_element())
            })
            .collect()
    }

    fn render_lines(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        // The list also renders a row to measure it before the visible
        // ones, so only the last call's lines are on screen.
        self.code.line_hits.clear();
        let state = self.state.read(cx);
        let Some(file) = state.browser.as_ref().and_then(|b| b.file.as_ref()) else {
            return Vec::new();
        };
        let FileLoad::Ready(content) = &file.content else {
            return Vec::new();
        };
        let LocalFile::Text(text) = &**content else {
            return Vec::new();
        };
        let theme = Theme::get(cx);
        let gutter = gutter_width(text.line_count());
        let (found, current_found) = self.find.marked();
        let marks = Marks {
            selection: self.code.selection.as_ref(),
            hover: self.code.hover.marked(),
            link: self.code.definition.link(),
            found,
            current_found,
        };
        let mut hits = Vec::new();
        let elements = range
            .filter(|&line| line < text.line_count() as usize)
            .map(|line| {
                let range = text.line_range(line as u32);
                let spans: Vec<(Range<usize>, HighlightStyle)> = file
                    .syntax
                    .as_ref()
                    .map(|syntax| {
                        syntax
                            .spans_in(range.clone())
                            .map(|(range, id)| (range, theme.syntax.style(id)))
                            .collect()
                    })
                    .unwrap_or_default();
                let styled = styled_line(line, &text.text()[range], spans, &marks, theme);
                hits.push((line, styled.hit));
                div()
                    .flex()
                    .items_center()
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .whitespace_nowrap()
                    .font_family(MONOSPACE)
                    .text_xs()
                    .cursor(marks.cursor(line))
                    .child(
                        div()
                            .flex_none()
                            .w(gutter)
                            .flex()
                            .justify_end()
                            .pr_4()
                            .text_color(theme.text_subtle)
                            .child((line + 1).to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .pr_4()
                            .text_color(theme.text)
                            .child(styled.text)
                            // The newline, when the selection carries on to
                            // the next line.
                            .when(styled.selection_continues, |el| {
                                el.child(selected_newline(theme.text_selection))
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        self.code.line_hits.extend(hits);
        elements
    }

    /// The tree, as the project's sidebar beside the file (`sidebar_stack`).
    /// Collapsed, its rail marks the open file. The workspace builds it
    /// before this view renders, so it brings the rows up to date.
    pub fn tree_sidebar(&mut self, collapsed: bool, cx: &mut Context<Self>) -> AnyElement {
        self.sync_rows(cx);
        self.follow_open_file(cx);
        if let Some(path) = &self.reveal_in_tree
            && let Some(ix) = self.rows.iter().position(|row| row.entry.path == *path)
        {
            self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Center);
            self.reveal_in_tree = None;
        }
        let theme = Theme::get(cx);
        // The tree is outside this view's element, so give the keyboard
        // back to the view when it's clicked.
        let focus = self.focus_handle.clone();
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, move |_, window, _| window.focus(&focus));
        let Some(browser) = &self.state.read(cx).browser else {
            return root.into_any_element();
        };

        if collapsed {
            let file = browser.file.as_ref().map(|file| {
                let path = file.path.clone();
                div()
                    .test_selector(|| "rail-browser-file".into())
                    .id("rail-browser-file")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .rounded_md()
                    .bg(theme.element_selected)
                    .child(Icon::new(IconName::FileCode).size(IconSize::Medium))
                    .tooltip(move |_, cx| Tooltip::view(path.clone().into(), cx))
            });
            return root
                .child(rail_header(
                    Icon::new(IconName::FolderTree).size(IconSize::Medium),
                    cx,
                ))
                .child(rail_items("browser-tree-rail").children(file))
                .into_any_element();
        }

        let ready = matches!(browser.root(), Some(DirLoad::Ready(_)));
        let search_bar = ready.then(|| self.render_search_bar(cx));
        let searching = ready && self.searching(cx);
        let results = searching.then(|| self.render_search_results(cx));
        let tab = self.state.read(cx);
        let Some(browser) = &tab.browser else {
            return root.into_any_element();
        };
        let theme = Theme::get(cx);
        root.child(
            sidebar_header(cx)
                .justify_between()
                .gap_2()
                .px_4()
                .text_xs()
                .text_color(theme.text_muted)
                .child(div().font_weight(FontWeight::MEDIUM).child("Files"))
                // The branch these are the files of, not the default
                // branch's on GitHub.
                .children(browser.head.clone().map(|head| {
                    div()
                        .truncate()
                        .font_family(MONOSPACE)
                        .text_color(theme.text_subtle)
                        .child(head)
                })),
        )
        .children(search_bar)
        .children(results)
        .when(ready && !searching, |el| {
            el.child(
                uniform_list(
                    "browser-tree",
                    self.rows.len(),
                    cx.processor(Self::render_tree_rows),
                )
                .flex_1()
                .py_2()
                .track_scroll(self.tree_scroll.clone()),
            )
        })
        // Language servers, for hover and symbols, once they're wanted.
        .children(
            tab.browser_code_intel()
                .map(|intel| server_status(&self.state, intel, tab.browser_intel_owner(), theme)),
        )
        .into_any_element()
    }

    fn code_pane(&mut self, browser: &ProjectBrowser, cx: &Context<Self>) -> AnyElement {
        let theme = Theme::get(cx);
        let Some(file) = &browser.file else {
            return self.dir_pane(browser, cx);
        };
        let notice = |message: String| {
            div()
                .px_4()
                .py_3()
                .text_xs()
                .text_color(theme.text_subtle)
                .child(message)
                .into_any_element()
        };
        let (body, details) = match &file.content {
            FileLoad::Loading => (
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .text_color(theme.text_muted)
                    .child(Spinner::new("browser-file-loading"))
                    .child("Reading the file…")
                    .into_any_element(),
                None,
            ),
            FileLoad::Failed(err) => (
                div()
                    .p_4()
                    .child(
                        Callout::new(CalloutKind::Error, err.clone())
                            .title("Couldn't read the file"),
                    )
                    .into_any_element(),
                None,
            ),
            FileLoad::Ready(content) => match &**content {
                LocalFile::Text(text) if text.line_count() == 0 => {
                    (notice("Empty file.".to_owned()), None)
                }
                LocalFile::Text(text) => {
                    let lines = match text.line_count() {
                        1 => "1 line".to_owned(),
                        n => format!("{n} lines"),
                    };
                    let details = match text.language {
                        Some(language) => format!("{} · {lines}", language.name),
                        None => lines,
                    };
                    // No padding: rows are found from the list's top edge.
                    let list = uniform_list(
                        "browser-lines",
                        text.line_count() as usize,
                        cx.processor(Self::render_lines),
                    )
                    .size_full()
                    .track_scroll(self.code.scroll.clone())
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .with_width_from_item(self.widest);
                    let body = div()
                        .id("browser-code")
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                        .on_mouse_move(cx.listener(Self::on_mouse_move))
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            if !hovered {
                                this.mouse_left(cx);
                            }
                        }))
                        .on_scroll_wheel(cx.listener(|this, _, _, cx| this.dismiss_popovers(cx)))
                        .child(list)
                        .when(self.code.dragging, |el| {
                            el.child(drag_listeners(cx.entity()))
                        });
                    (body.into_any_element(), Some(details))
                }
                LocalFile::Binary => (notice("Binary file not shown.".to_owned()), None),
                LocalFile::TooLarge { bytes } => (
                    notice(format!(
                        "File not shown: {:.1} MB is too large to display.",
                        *bytes as f64 / (1024. * 1024.)
                    )),
                    None,
                ),
                LocalFile::Directory => (
                    notice(
                        "A submodule or nested repository. Its files aren't listed here."
                            .to_owned(),
                    ),
                    None,
                ),
            },
        };

        let find_bar = matches!(
            &file.content,
            FileLoad::Ready(content) if matches!(&**content, LocalFile::Text(_))
        )
        .then(|| self.render_find_bar(cx))
        .flatten();
        let header = pane_header(theme)
            .child(self.breadcrumbs(browser, &file.path, cx))
            .children(details.map(|details| {
                div()
                    .flex_none()
                    .text_color(theme.text_subtle)
                    .child(details)
            }));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .children(find_bar)
            .child(body)
            .into_any_element()
    }
}

impl FileBrowserView {
    /// The main panel while no file is open: the entries of the directory
    /// navigated to, directories first, each opening on a click.
    fn dir_pane(&mut self, browser: &ProjectBrowser, cx: &Context<Self>) -> AnyElement {
        let theme = Theme::get(cx);
        let dir = browser.dir.as_str();
        // Another directory starts at its top.
        if self.scrolled_dir.as_deref() != Some(dir) {
            self.dir_scroll = UniformListScrollHandle::new();
            self.scrolled_dir = Some(dir.to_owned());
        }
        let notice = |message: &'static str| {
            div()
                .px_4()
                .py_3()
                .text_xs()
                .text_color(theme.text_subtle)
                .child(message)
                .into_any_element()
        };
        let body = match browser.dirs.get(dir) {
            None | Some(DirLoad::Loading) => div()
                .flex()
                .items_center()
                .gap_2()
                .px_4()
                .py_3()
                .text_color(theme.text_muted)
                .child(Spinner::new("browser-dir-loading"))
                .child("Listing the directory…")
                .into_any_element(),
            Some(DirLoad::Failed(err)) => div()
                .p_4()
                .child(
                    Callout::new(CalloutKind::Error, err.clone())
                        .title("Couldn't list the directory"),
                )
                .into_any_element(),
            Some(DirLoad::Ready(entries)) if entries.is_empty() => notice("Empty directory."),
            Some(DirLoad::Ready(entries)) => div()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list(
                        "browser-entries",
                        entries.len(),
                        cx.processor(Self::render_entries),
                    )
                    .size_full()
                    .track_scroll(self.dir_scroll.clone()),
                )
                .into_any_element(),
        };
        let count = match browser.dirs.get(dir) {
            Some(DirLoad::Ready(entries)) => Some(match entries.len() {
                1 => "1 item".to_owned(),
                n => format!("{n} items"),
            }),
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                pane_header(theme)
                    .child(self.breadcrumbs(browser, dir, cx))
                    .children(
                        count.map(|count| {
                            div().flex_none().text_color(theme.text_subtle).child(count)
                        }),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn render_entries(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(browser) = &self.state.read(cx).browser else {
            return Vec::new();
        };
        let Some(DirLoad::Ready(entries)) = browser.dirs.get(&browser.dir) else {
            return Vec::new();
        };
        let theme = Theme::get(cx);
        let hover = theme.element_hover;
        range
            .filter_map(|ix| {
                let entry = entries.get(ix)?;
                let is_dir = entry.kind == EntryKind::Dir;
                let (icon, color) = match entry.kind {
                    EntryKind::Dir => (IconName::Folder, Color::Accent),
                    EntryKind::Repository => (IconName::FolderGit, Color::Muted),
                    EntryKind::File if LanguageRegistry::for_path(&entry.path).is_some() => {
                        (IconName::FileCode, Color::Muted)
                    }
                    EntryKind::File => (IconName::File, Color::Muted),
                };
                let path = entry.path.clone();
                Some(
                    div()
                        .test_selector(|| format!("browser-entry-{}", entry.path))
                        .id(("browser-entry", ix))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .h(px(ENTRY_HEIGHT))
                        .px_4()
                        .border_b_1()
                        .border_color(theme.border_subtle)
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .child(Icon::new(icon).size(IconSize::Small).color(color))
                        .child(div().min_w_0().truncate().child(entry.name.clone()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.navigate(path.clone(), is_dir, cx)
                        }))
                        .into_any_element(),
                )
            })
            .collect()
    }

    /// `path` from the project's root, each directory on the way a link
    /// to list it; the last part is where the main panel is.
    fn breadcrumbs(&self, browser: &ProjectBrowser, path: &str, cx: &Context<Self>) -> AnyElement {
        let theme = Theme::get(cx);
        let root = self
            .state
            .read(cx)
            .app
            .read(cx)
            .project(browser.project)
            .map_or_else(|| "Files".to_owned(), |project| project.name.clone());
        let mut crumbs = vec![(String::new(), root)];
        crumbs.extend(path.split('/').filter(|part| !part.is_empty()).scan(
            String::new(),
            |at, part| {
                if !at.is_empty() {
                    at.push('/');
                }
                at.push_str(part);
                Some((at.clone(), part.to_owned()))
            },
        ));
        let last = crumbs.len() - 1;
        let accent = theme.accent;
        let separator = || {
            div()
                .flex_none()
                .px_1()
                .text_color(theme.text_subtle)
                .child("/")
        };
        div()
            .flex()
            .items_center()
            .min_w_0()
            .font_family(MONOSPACE)
            .children(crumbs.into_iter().enumerate().map(|(ix, (at, name))| {
                if ix == last {
                    return div()
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM)
                        .child(name)
                        .into_any_element();
                }
                let selector = match at.as_str() {
                    "" => "browser-crumb-root".to_owned(),
                    at => format!("browser-crumb-{at}"),
                };
                div()
                    .flex()
                    .flex_shrink()
                    .min_w_0()
                    .child(
                        div()
                            .test_selector(|| selector)
                            .id(("browser-crumb", ix))
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_muted)
                            .cursor_pointer()
                            .hover(move |style| style.text_color(accent))
                            .child(name)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.navigate(at.clone(), true, cx)
                            })),
                    )
                    .child(separator())
                    .into_any_element()
            }))
            .into_any_element()
    }
}

/// The bar above the main panel, naming what's shown.
fn pane_header(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_4()
        .h(px(32.))
        .px_4()
        .bg(theme.surface)
        .border_b_1()
        .border_color(theme.border_subtle)
        .text_xs()
}

/// Wide enough for the last line's number.
fn gutter_width(lines: u32) -> gpui::Pixels {
    let digits = lines.max(1).ilog10() + 1;
    px(digits.max(3) as f32 * DIGIT_WIDTH + 28.)
}

/// Status text near the top of a pane, like the docket's empty state.
fn centered(child: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .pt(px(96.))
        .child(child)
}

impl Render for FileBrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Take the keyboard when opened, so the arrows move through the tree.
        if !self.focused {
            window.focus(&self.focus_handle);
            self.focused = true;
        }
        self.sync_find(cx);
        let hover = self.render_hover(cx);
        let peek = self.render_peek(window, cx);
        let state = self.state.clone();
        let state = state.read(cx);
        let Some(browser) = &state.browser else {
            return div().flex_1();
        };
        self.follow_open_file(cx);
        self.sync_measured(browser.file.as_ref());
        self.apply_pending(browser.file.as_ref());
        let theme = Theme::get(cx);

        // In a Browse tab, the pull request's or branch's header is above.
        let standalone = browser.scope == BrowserScope::Project;
        let header = standalone.then(|| {
            let (back, refresh) = (self.state.clone(), self.state.clone());
            let project = state.app.read(cx).project(browser.project);
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap_4()
                .h(px(48.))
                .pl_2()
                .pr_4()
                .border_b_1()
                .border_color(theme.border_subtle)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(
                            IconButton::new("browser-back", IconName::ArrowLeft)
                                .tooltip("Back (Esc)")
                                .on_click(move |_, _, cx| {
                                    back.update(cx, |state, cx| state.close_browser(cx));
                                }),
                        )
                        .children(project.map(|project| {
                            div()
                                .flex()
                                .items_baseline()
                                .gap_3()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex_none()
                                        .text_base()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(project.name.clone()),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.text_subtle)
                                        .children(match browser.folder() {
                                        _ if browser.in_docket_worktree() => Some(
                                            "Not checked out here: Docket's checkout of its tip"
                                                .into(),
                                        ),
                                        Some(path) => Some(display_path(path)),
                                        None => None,
                                    }),
                                )
                        })),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .when(browser.reloading, |el| {
                            el.child(Spinner::new("browser-refreshing"))
                        })
                        .child(
                            IconButton::new("browser-refresh", IconName::RefreshCw)
                                .tooltip("Refresh (⌘R)")
                                .disabled(browser.is_busy())
                                .on_click(move |_, _, cx| {
                                    refresh.update(cx, |state, cx| state.refresh(cx));
                                }),
                        ),
                )
        });

        let body = match browser.root() {
            None | Some(DirLoad::Loading) => centered(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_color(theme.text_muted)
                    .child(Spinner::new("browser-loading"))
                    .child("Listing files…"),
            )
            .into_any_element(),
            Some(DirLoad::Failed(err)) => div()
                .p_4()
                .child(Callout::new(CalloutKind::Error, err.clone()).title("Couldn't list files"))
                .into_any_element(),
            Some(DirLoad::Ready(root)) if root.is_empty() => {
                centered(div().text_color(theme.text_muted).child("No files.")).into_any_element()
            }
            Some(DirLoad::Ready(_)) => div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.code_pane(browser, cx))
                .into_any_element(),
        };

        div()
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::expand_selected))
            .on_action(cx.listener(Self::collapse_selected))
            .on_action(cx.listener(|this, action, _, cx| this.toggle_selected(action, cx)))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::clear_selection))
            .on_action(cx.listener(Self::go_to_definition_action))
            .on_action(cx.listener(Self::go_to_definition_in_new_tab_action))
            .on_action(cx.listener(Self::go_back))
            .on_action(cx.listener(Self::find_file))
            .on_action(cx.listener(Self::find_in_files))
            .on_action(cx.listener(Self::find_symbol))
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(Self::open_symbols))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_previous))
            .on_action(cx.listener(Self::cancel_find))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| window.focus(&this.focus_handle)),
            )
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .children(header)
            .child(body)
            .children(hover)
            .children(peek)
    }
}

impl CodeView for FileBrowserView {
    fn code(&self) -> &CodeState {
        &self.code
    }

    fn code_mut(&mut self) -> &mut CodeState {
        &mut self.code
    }

    fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    fn row_count(&self) -> usize {
        match self.measured.as_ref().map(|(_, content)| &**content) {
            Some(LocalFile::Text(text)) => text.line_count() as usize,
            _ => 0,
        }
    }

    fn line_text<'a>(&'a self, row: usize, cx: &'a App) -> Option<&'a str> {
        let file = self.state.read(cx).browser.as_ref()?.file.as_ref()?;
        let FileLoad::Ready(content) = &file.content else {
            return None;
        };
        let LocalFile::Text(text) = &**content else {
            return None;
        };
        (row < text.line_count() as usize).then(|| text.line(row as u32))
    }

    fn lsp_request(&self, row: usize, offset: usize, cx: &App) -> Option<(LspTarget, Ask)> {
        let target = self.state.read(cx).browser_lsp_target()?;
        let ask = Ask::Code {
            line: u32::try_from(row).ok()?,
            offset,
        };
        Some((target, ask))
    }

    /// A definition in the checkout, which opens here.
    fn reveal_definition(&mut self, definition: &Definition, cx: &mut Context<Self>) -> bool {
        if definition.relative.is_none() {
            return false;
        }
        if let Some(place) = self.place() {
            self.back.push(place);
        }
        self.show_definition(definition.clone(), cx);
        true
    }

    fn definition_project(&self, cx: &App) -> Option<ProjectId> {
        Some(self.state.read(cx).browser.as_ref()?.project)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use crate::github::FakeGithub;
    use crate::test_support::{VisualTestContextExt as _, git_checkout, open_docket, project};
    use crate::ui::sidebar_stack::{PEEK_DELAY, RAIL_WIDTH, SIDEBAR_WIDTH};

    #[gpui::test]
    fn arrow_keys_move_through_the_tree(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        git_checkout(
            checkout.path(),
            &[
                ("README.md", "# Widgets\n"),
                ("docs/guide.md", "Turn it on.\n"),
            ],
        );
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);

        cx.click_on("project-1");
        cx.click_on("branch-1-main");
        let browser = |cx: &mut gpui::VisualTestContext| {
            state.read_with(cx, |state, _| {
                let browser = state.browser.as_ref().expect("the browser is open");
                let file = browser.file.as_ref().map(|file| file.path.clone());
                (browser.expanded.contains("docs"), file)
            })
        };
        assert_eq!(browser(cx), (false, None));

        // Directories come first: `docs/`, then `README.md`.
        cx.simulate_keystrokes("down right");
        assert_eq!(browser(cx), (true, None));

        cx.simulate_keystrokes("down");
        assert_eq!(browser(cx), (true, Some("docs/guide.md".into())));

        // Left moves up to `docs/`, then collapses it.
        cx.simulate_keystrokes("left left");
        assert_eq!(browser(cx), (false, Some("docs/guide.md".into())));

        cx.simulate_keystrokes("down");
        assert_eq!(browser(cx), (false, Some("README.md".into())));
    }

    #[gpui::test]
    fn the_main_panel_lists_and_navigates_directories(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        git_checkout(
            checkout.path(),
            &[
                ("README.md", "# Widgets\n"),
                ("docs/guide.md", "Turn it on.\n"),
                ("docs/api/ref.md", "Reference.\n"),
            ],
        );
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("project-1");
        cx.click_on("branch-1-main");
        let shown = |cx: &mut VisualTestContext| {
            state.read_with(cx, |state, _| {
                let browser = state.browser.as_ref().unwrap();
                let file = browser.file.as_ref().map(|file| file.path.clone());
                (browser.dir.clone(), file)
            })
        };

        // Until a file opens, the root's entries.
        assert_eq!(shown(cx), (String::new(), None));
        assert!(cx.is_drawn("browser-entry-docs"));
        assert!(cx.is_drawn("browser-entry-README.md"));

        // A directory lists in its place, and the tree expands to it.
        cx.click_on("browser-entry-docs");
        assert_eq!(shown(cx), ("docs".into(), None));
        assert!(cx.is_drawn("browser-entry-docs/api"));
        assert!(!cx.is_drawn("browser-entry-README.md"));
        assert!(cx.is_drawn("browser-tree-docs/guide.md"));

        cx.click_on("browser-entry-docs/api");
        cx.click_on("browser-entry-docs/api/ref.md");
        assert_eq!(shown(cx).1.as_deref(), Some("docs/api/ref.md"));
        assert!(cx.is_drawn("browser-tree-docs/api/ref.md"));

        // The file's path links to each directory it's in.
        cx.click_on("browser-crumb-docs");
        assert_eq!(shown(cx), ("docs".into(), None));
        cx.click_on("browser-crumb-root");
        assert_eq!(shown(cx), (String::new(), None));
        assert!(cx.is_drawn("browser-entry-README.md"));

        // Clicking a directory in the tree lists it too. The keyboard
        // moving onto one leaves the main panel be.
        cx.click_on("browser-tree-README.md");
        cx.click_on("browser-tree-docs/api");
        assert_eq!(shown(cx), ("docs/api".into(), None));
        // Up past `docs/guide.md` to `docs/api/`.
        cx.click_on("browser-tree-README.md");
        cx.simulate_keystrokes("up up");
        assert_eq!(shown(cx).1.as_deref(), Some("docs/guide.md"));
        cx.simulate_keystrokes("enter");
        assert_eq!(shown(cx), ("docs/api".into(), None), "enter lists it");
    }

    #[gpui::test]
    fn the_tree_is_a_sidebar_beside_the_file(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        git_checkout(
            checkout.path(),
            &[
                ("README.md", "# Widgets\n"),
                ("docs/guide.md", "Turn it on.\n"),
            ],
        );
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("project-1");
        cx.click_on("branch-1-main");
        let width =
            |cx: &mut VisualTestContext, selector| cx.bounds_of(selector).unwrap().size.width;
        assert_eq!(width(cx, "sidebar-navigation"), RAIL_WIDTH);
        assert_eq!(
            width(cx, "sidebar-pull-requests"),
            RAIL_WIDTH,
            "beside the docket"
        );
        assert_eq!(width(cx, "sidebar-project-files"), SIDEBAR_WIDTH);

        // Clicking the tree opens the file, and takes the keyboard back.
        cx.update(|window, _| window.blur());
        cx.click_on("browser-tree-README.md");
        let open = |cx: &mut VisualTestContext| {
            state.read_with(cx, |state, _| {
                let browser = state.browser.as_ref().unwrap();
                let file = browser.file.as_ref().map(|file| file.path.clone());
                (browser.expanded.contains("docs"), file)
            })
        };
        assert_eq!(open(cx), (false, Some("README.md".into())));
        cx.simulate_keystrokes("up right");
        assert_eq!(open(cx), (true, Some("README.md".into())));

        // Peeking at navigation swaps it with the tree, leaving the file put.
        let file = cx.bounds_of("browser-back").unwrap().origin;
        let rail = cx.bounds_of("sidebar-navigation").unwrap().center();
        cx.simulate_mouse_move(rail, None, Modifiers::none());
        cx.executor().advance_clock(PEEK_DELAY);
        cx.run_until_parked();
        assert!(cx.is_drawn("project-1"));
        assert!(!cx.is_drawn("browser-tree-README.md"));
        assert!(cx.is_drawn("rail-browser-file"));
        assert_eq!(width(cx, "sidebar-project-files"), RAIL_WIDTH);
        assert_eq!(cx.bounds_of("browser-back").unwrap().origin, file);
    }

    fn search_results(
        state: &gpui::Entity<crate::state::Tab>,
        cx: &mut VisualTestContext,
    ) -> Vec<String> {
        use crate::state::SearchResults;
        state.read_with(cx, |state, _| {
            let search = &state.browser.as_ref().unwrap().search;
            match &search.results {
                SearchResults::Files(files) => files.iter().map(|f| f.path.clone()).collect(),
                SearchResults::Contents(found) => found
                    .files
                    .iter()
                    .flat_map(|file| {
                        file.lines
                            .iter()
                            .map(move |hit| format!("{}:{}", file.path, hit.line + 1))
                    })
                    .collect(),
                SearchResults::Symbols { symbols, .. } => {
                    symbols.iter().map(|s| s.name.clone()).collect()
                }
                SearchResults::Failed(err) => vec![format!("failed: {err}")],
                SearchResults::None => Vec::new(),
            }
        })
    }

    fn open_file(
        state: &gpui::Entity<crate::state::Tab>,
        cx: &mut VisualTestContext,
    ) -> Option<String> {
        state.read_with(cx, |state, _| {
            let browser = state.browser.as_ref().unwrap();
            browser.file.as_ref().map(|file| file.path.clone())
        })
    }

    #[gpui::test]
    fn finds_files_by_name(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        git_checkout(
            checkout.path(),
            &[
                ("README.md", "# Widgets\n"),
                ("docs/guide.md", "Turn it on.\n"),
                ("docs/api/ref.md", "Reference.\n"),
            ],
        );
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("project-1");
        cx.click_on("branch-1-main");
        assert!(cx.is_drawn("browser-search-input"));

        cx.simulate_keystrokes("cmd-p");
        cx.simulate_input("gde");
        cx.run_until_parked();

        assert_eq!(search_results(&state, cx), ["docs/guide.md"]);
        assert!(cx.is_drawn("search-file-docs/guide.md"));
        assert!(
            !cx.is_drawn("browser-tree-README.md"),
            "results replace the tree"
        );

        // Opening one is done with the search: the tree is back, showing it.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(open_file(&state, cx).as_deref(), Some("docs/guide.md"));
        assert!(search_results(&state, cx).is_empty());
        assert!(cx.is_drawn("browser-tree-docs/guide.md"));

        // Down moves through the results; clicking one opens it.
        cx.simulate_keystrokes("cmd-p");
        cx.simulate_input("md");
        cx.run_until_parked();
        let found = search_results(&state, cx);
        assert_eq!(found.len(), 3);
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        assert_eq!(open_file(&state, cx), Some(found[1].clone()));
        cx.simulate_keystrokes("cmd-p");
        cx.simulate_input("ref");
        cx.run_until_parked();
        cx.click_on("search-file-docs/api/ref.md");
        assert_eq!(open_file(&state, cx).as_deref(), Some("docs/api/ref.md"));

        // Escape clears it, leaving the browser open.
        cx.simulate_keystrokes("cmd-p");
        cx.simulate_input("read");
        cx.run_until_parked();
        assert_eq!(search_results(&state, cx), ["README.md"]);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(search_results(&state, cx).is_empty());
        assert!(cx.is_drawn("browser-tree-README.md"));
        assert_eq!(open_file(&state, cx).as_deref(), Some("docs/api/ref.md"));
    }

    #[gpui::test]
    fn finds_in_the_open_file(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        git_checkout(
            checkout.path(),
            &[
                ("notes.txt", "one two one\nthree\nOne more\n"),
                ("other.txt", "one\n"),
            ],
        );
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("project-1");
        cx.click_on("branch-1-main");
        cx.click_on("browser-tree-notes.txt");
        cx.run_until_parked();
        let windows = crate::test_support::docket_windows(cx);
        let view = cx.read(|cx| windows[0].read(cx).unwrap().file_browser().unwrap());
        let found = |cx: &mut VisualTestContext| {
            view.read_with(cx, |view, _| {
                let (matches, current) = view.find.marked();
                (matches.to_vec(), current)
            })
        };

        cx.simulate_keystrokes("cmd-f");
        assert!(cx.is_drawn("browser-find-input"));
        cx.simulate_input("one");
        cx.run_until_parked();
        assert_eq!(
            found(cx),
            (vec![(0, 0..3), (0, 8..11), (2, 0..3)], Some(0)),
            "ignoring case, from the top"
        );
        assert!(cx.is_drawn("find-count"));

        // Enter and down go forward, wrapping; shift-enter and cmd-shift-g
        // go back.
        cx.simulate_keystrokes("enter down");
        assert_eq!(found(cx).1, Some(2));
        cx.simulate_keystrokes("enter");
        assert_eq!(found(cx).1, Some(0));
        cx.simulate_keystrokes("shift-enter");
        assert_eq!(found(cx).1, Some(2));
        cx.simulate_keystrokes("cmd-shift-g");
        assert_eq!(found(cx).1, Some(1));

        cx.click_on("find-case");
        cx.run_until_parked();
        assert_eq!(found(cx).0, [(0, 0..3), (0, 8..11)]);

        // It stays open for the next file.
        cx.click_on("browser-tree-other.txt");
        cx.run_until_parked();
        assert_eq!(found(cx).0, [(0, 0..3)]);

        // Escape closes it, leaving the file open; escape again closes
        // the browser.
        cx.simulate_keystrokes("escape");
        assert!(!cx.is_drawn("browser-find-input"));
        assert_eq!(found(cx).0, []);
        assert_eq!(open_file(&state, cx).as_deref(), Some("other.txt"));
        cx.simulate_keystrokes("cmd-f escape");
        assert!(state.read_with(cx, |state, _| state.browser.is_some()));
        cx.simulate_keystrokes("escape");
        assert!(state.read_with(cx, |state, _| state.browser.is_none()));
    }

    #[gpui::test]
    fn goes_to_symbols_in_the_open_file(cx: &mut TestAppContext) {
        use super::find::FindMode;
        use super::outline::FileSymbol;
        use crate::lsp::SymbolKind;

        let checkout = tempfile::tempdir().unwrap();
        git_checkout(
            checkout.path(),
            &[("notes.txt", "fn alpha\n\n   fn beta\n\nfn gamma\n")],
        );
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("project-1");
        cx.click_on("branch-1-main");
        cx.click_on("browser-tree-notes.txt");
        cx.run_until_parked();
        let windows = crate::test_support::docket_windows(cx);
        let view = cx.read(|cx| windows[0].read(cx).unwrap().file_browser().unwrap());

        // Text has no language server to ask.
        cx.simulate_keystrokes("cmd-shift-o");
        assert!(cx.is_drawn("find-symbols-notice"));
        assert!(view.read_with(cx, |view, _| view.find.mode == FindMode::Symbols));

        let symbol = |name: &str, depth, line, start: usize| FileSymbol {
            name: name.to_owned(),
            kind: SymbolKind::Function,
            depth,
            line,
            name_range: start..start + name.len(),
        };
        view.update(cx, |view, cx| {
            view.set_outline(
                vec![
                    symbol("alpha", 0, 0, 3),
                    symbol("beta", 1, 2, 6),
                    symbol("gamma", 0, 4, 3),
                ],
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.is_drawn("find-symbol-beta"), "the whole outline");
        let listed = |cx: &mut VisualTestContext| {
            view.read_with(cx, |view, _| {
                let symbols = &view.find.symbols;
                (symbols.rows.len(), symbols.selected, symbols.mark.clone())
            })
        };
        assert_eq!(listed(cx), (3, Some(0), vec![(0, 3..8)]));

        // Down moves through them, marking each name.
        cx.simulate_keystrokes("down");
        assert_eq!(listed(cx), (3, Some(1), vec![(2, 6..10)]));

        cx.simulate_input("gam");
        cx.run_until_parked();
        assert_eq!(listed(cx), (1, Some(0), vec![(4, 3..8)]));
        assert!(!cx.is_drawn("find-symbol-beta"));

        // Enter goes to it, selecting its name, and closes the bar.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let selection = view.read_with(cx, |view, _| {
            let (start, end) = view.code.selection.as_ref().unwrap().range();
            (
                view.find.open,
                (start.row, start.offset),
                (end.row, end.offset),
            )
        });
        assert_eq!(selection, (false, (4, 3), (4, 8)));
        assert_eq!(open_file(&state, cx).as_deref(), Some("notes.txt"));

        // The bar switches between text and symbols.
        cx.simulate_keystrokes("cmd-f");
        assert!(view.read_with(cx, |view, _| view.find.mode == FindMode::Text));
        assert!(!cx.is_drawn("find-symbols"));
        cx.click_on("find-mode-symbols");
        assert!(cx.is_drawn("find-symbols"));
        cx.simulate_keystrokes("escape");
        assert!(!cx.is_drawn("find-symbols"));
        assert!(state.read_with(cx, |state, _| state.browser.is_some()));
    }

    #[gpui::test]
    fn clicking_twice_or_three_times_selects_a_word_or_all(cx: &mut TestAppContext) {
        use gpui::{MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, point, px};

        let checkout = tempfile::tempdir().unwrap();
        git_checkout(checkout.path(), &[("README.md", "# Widgets\n")]);
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (_state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("project-1");
        cx.click_on("branch-1-main");
        cx.simulate_keystrokes("cmd-p");
        cx.simulate_input("models/applica");
        cx.run_until_parked();

        let windows = crate::test_support::docket_windows(cx);
        let view = cx.read(|cx| windows[0].read(cx).unwrap().file_browser().unwrap());
        let selected = |cx: &mut VisualTestContext| {
            let input = view.read_with(cx, |view, _| view.search.input.clone());
            input.read_with(cx, |input, _| input.selected_text().to_owned())
        };
        let field = cx.bounds_of("browser-search-input").unwrap();
        // In the first letter, past the field's padding; and past the end.
        let start = point(field.left() + px(14.), field.center().y);
        let end = point(field.right() - px(8.), field.center().y);
        let press = |cx: &mut VisualTestContext, at, click_count| {
            cx.simulate_event(MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::none(),
                click_count,
                first_mouse: false,
            });
        };
        let release = |cx: &mut VisualTestContext, at| {
            cx.simulate_event(MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::none(),
                click_count: 1,
            });
        };

        press(cx, start, 1);
        release(cx, start);
        press(cx, start, 2);
        assert_eq!(selected(cx), "models");
        // Dragging on goes a word at a time.
        cx.simulate_event(MouseMoveEvent {
            position: end,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::none(),
        });
        assert_eq!(selected(cx), "models/applica");
        release(cx, end);

        press(cx, end, 2);
        release(cx, end);
        assert_eq!(selected(cx), "applica");
        press(cx, end, 3);
        release(cx, end);
        assert_eq!(selected(cx), "models/applica");

        // Typing replaces the selection, as ever.
        press(cx, start, 2);
        release(cx, start);
        cx.simulate_input("views");
        let input = view.read_with(cx, |view, _| view.search.input.clone());
        assert_eq!(
            input.read_with(cx, |input, _| input.text().to_owned()),
            "views/applica"
        );
    }

    #[gpui::test]
    fn search_results_fit_the_sidebar(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        let deep = &format!("{}/application.py", ["discord_api/models"; 12].join("/"));
        git_checkout(
            checkout.path(),
            &[(deep, "pass\n"), ("README.md", "# Widgets\n")],
        );
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (_state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("project-1");
        cx.click_on("branch-1-main");

        cx.simulate_keystrokes("cmd-p");
        cx.simulate_input("applica");
        cx.run_until_parked();
        // A long directory is cut off, rather than widening its row (and
        // its selection) past the sidebar.
        let sidebar = cx.bounds_of("sidebar-project-files").unwrap();
        let row = cx.bounds_of(&format!("search-file-{deep}")).unwrap();
        assert!(row.right() <= sidebar.right(), "{row:?} in {sidebar:?}");
        assert_eq!(row.left(), sidebar.left());
        // Its name and directory are one line, centered in the row.
        let label = cx.bounds_of("search-label-0").unwrap();
        assert!(
            label.top() >= row.top() && label.bottom() <= row.bottom(),
            "{label:?} in {row:?}"
        );
        let tree_row = |cx: &mut VisualTestContext| cx.bounds_of("browser-tree-README.md").unwrap();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(
            tree_row(cx).size.width,
            row.size.width,
            "as wide as the tree's"
        );
    }

    #[gpui::test]
    fn searches_file_contents(cx: &mut TestAppContext) {
        use crate::state::SEARCH_DEBOUNCE;

        let checkout = tempfile::tempdir().unwrap();
        git_checkout(
            checkout.path(),
            &[
                ("notes/a.md", "alpha\nbeta widget\n"),
                ("b.txt", "WIDGET\n"),
                ("c.md", "nothing here\n"),
            ],
        );
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("project-1");
        cx.click_on("branch-1-main");

        cx.simulate_keystrokes("cmd-shift-f");
        cx.simulate_input("widget");
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(search_results(&state, cx), ["b.txt:1", "notes/a.md:2"]);
        assert!(cx.is_drawn("search-hit-file-notes/a.md"));
        assert!(cx.is_drawn("search-hit-notes/a.md:2"));
        assert!(cx.is_drawn("search-summary"));

        // Matching case leaves out `WIDGET`.
        cx.click_on("search-case");
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(search_results(&state, cx), ["notes/a.md:2"]);

        // Enter opens the first line; the results stay to open the next.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(open_file(&state, cx).as_deref(), Some("notes/a.md"));
        assert!(cx.is_drawn("search-hit-notes/a.md:2"));

        // A regex, and one that doesn't parse.
        cx.click_on("search-case");
        cx.click_on("search-regex");
        cx.simulate_keystrokes("cmd-shift-f");
        cx.simulate_input("^(alpha|nothing)");
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(search_results(&state, cx), ["c.md:1", "notes/a.md:1"]);
        cx.simulate_keystrokes("cmd-shift-f");
        cx.simulate_input("(alpha");
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        cx.run_until_parked();
        assert!(search_results(&state, cx)[0].starts_with("failed: "));
        assert!(cx.is_drawn("search-notice"));

        // The same query, by name.
        cx.simulate_keystrokes("cmd-shift-f");
        cx.simulate_input("nothing");
        cx.click_on("search-mode-files");
        cx.run_until_parked();
        assert!(search_results(&state, cx).is_empty());
        cx.click_on("search-mode-contents");
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(search_results(&state, cx), ["c.md:1"]);
    }

    #[gpui::test]
    fn symbols_need_a_language_server(cx: &mut TestAppContext) {
        let checkout = tempfile::tempdir().unwrap();
        git_checkout(checkout.path(), &[("README.md", "# Widgets\n")]);
        let projects = vec![project(1, checkout.path(), "acme/widgets")];
        let (state, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("project-1");
        cx.click_on("branch-1-main");

        cx.simulate_keystrokes("cmd-shift-t");
        cx.simulate_input("widget");
        cx.run_until_parked();
        assert!(search_results(&state, cx).is_empty());
        let note = state.read_with(cx, |state, _| {
            match &state.browser.as_ref().unwrap().search.results {
                crate::state::SearchResults::Symbols { note, .. } => note.clone(),
                _ => None,
            }
        });
        assert!(note.unwrap().contains("no language server"));
        assert!(cx.is_drawn("search-notice"));
    }
}
