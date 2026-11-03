//! A branch's files, opened from the docket, or the project's checkout:
//! the directory tree in the sidebar (`tree_sidebar`), and the selected
//! file, syntax-highlighted, beside it. A checked-out branch shows as it is
//! on disk; any other as it is at its tip (`state::browse`).
//!
//! Both are virtualized lists, so a large checkout or file only lays out
//! what's visible, and each directory is listed the first time it's
//! expanded. The tree works from the keyboard: up and down move
//! through it, showing each file they reach; right and left expand and
//! collapse directories, or move into and out of them.
//!
//! The file's code selects, copies, and hovers as in the diff (`ui/code`),
//! with language servers running in the checkout (or worktree) itself. A
//! branch read from Git has none. Command-clicking
//! a symbol opens its definition here when it's in the checkout, selecting
//! it in the tree, and peeks at it otherwise (the standard library, a
//! package outside the project). Command-option-clicking opens it in a new
//! tab's browser.

mod tree;

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, HighlightStyle, KeyBinding,
    ListHorizontalSizingBehavior, MouseButton, Pixels, Point, ScrollStrategy,
    UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

use self::tree::{TreeRow, parent_row, visible_rows};
use super::code::{
    self, Ask, CodeDefinition, CodeHover, CodeState, CodeView, Definition, GoBack, Granularity,
    Marks, ROW_HEIGHT, Selection, Span, TextPoint, display_width, drag_listeners, selected_newline,
    styled_line,
};
use super::components::{
    Callout, CalloutKind, Icon, IconButton, IconName, IconSize, MONOSPACE, Spinner,
    TestSelector as _, Tooltip, display_path,
};
use super::diff_view::server_status;
use super::sidebar_stack::{rail_header, rail_items, sidebar_header};
use super::theme::{Color, Theme};
use crate::projects::ProjectId;
use crate::projects::files::{EntryKind, LocalFile};
use crate::state::{DirLoad, FileLoad, IntelOwner, LspTarget, OpenFile, ProjectBrowser, Tab};
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
const TREE_ROW_HEIGHT: f32 = 24.;
const INDENT: f32 = 12.;
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
        }
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

    /// Enter, or a click: expands or collapses a directory.
    fn toggle_selected(&mut self, _: &ToggleSelected, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_row() else {
            return;
        };
        let entry = self.rows[ix].entry.clone();
        if entry.kind == EntryKind::Dir {
            let expanded = self.is_expanded(&entry.path, cx);
            self.set_expanded(&entry.path, !expanded, cx);
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
        let theme = Theme::get(cx);
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?;
                let entry = &row.entry;
                let selected = self.selected.as_deref() == Some(entry.path.as_str());
                let expanded = browser.expanded.contains(&entry.path);
                let color = if selected {
                    Color::Default
                } else {
                    Color::Muted
                };
                let icon = match entry.kind {
                    EntryKind::Dir if expanded => IconName::FolderOpen,
                    EntryKind::Dir => IconName::Folder,
                    EntryKind::Repository => IconName::FolderGit,
                    EntryKind::File if LanguageRegistry::for_path(&entry.path).is_some() => {
                        IconName::FileCode
                    }
                    EntryKind::File => IconName::File,
                };
                // A directory being listed spins in place of its chevron;
                // one that couldn't be says why on hover.
                let mut error = None;
                let lead = match (entry.kind, browser.dirs.get(&entry.path)) {
                    (EntryKind::Dir, Some(DirLoad::Loading)) if expanded => {
                        Some(Spinner::new(format!("browser-tree-loading-{ix}")).into_any_element())
                    }
                    (EntryKind::Dir, Some(DirLoad::Failed(err))) if expanded => {
                        error = Some(err.clone());
                        Some(
                            Icon::new(IconName::TriangleAlert)
                                .size(IconSize::XSmall)
                                .color(Color::Warning)
                                .into_any_element(),
                        )
                    }
                    (EntryKind::Dir, _) => Some(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(IconSize::XSmall)
                        .color(color)
                        .into_any_element(),
                    ),
                    _ => None,
                };
                let hover = theme.element_hover;
                Some(
                    div()
                        .test_selector(|| format!("browser-tree-{}", entry.path))
                        .id(("browser-tree-row", ix))
                        .h(px(TREE_ROW_HEIGHT))
                        .px_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .size_full()
                                .pl(px(4. + INDENT * row.depth as f32))
                                .pr_2()
                                .rounded_md()
                                .cursor_pointer()
                                .when(selected, |el| el.bg(theme.element_selected))
                                .when(!selected, |el| el.hover(move |s| s.bg(hover)))
                                .child(
                                    div()
                                        .flex_none()
                                        .w(px(12.))
                                        .flex()
                                        .items_center()
                                        .children(lead),
                                )
                                .child(Icon::new(icon).size(IconSize::Small).color(color))
                                .child(div().truncate().child(entry.name.clone())),
                        )
                        .when_some(error, |el, error| {
                            el.tooltip(move |_, cx| Tooltip::view(error.clone(), cx))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.click_row(ix, cx)))
                        .into_any_element(),
                )
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
        let marks = Marks {
            selection: self.code.selection.as_ref(),
            hover: self.code.hover.marked(),
            link: self.code.definition.link(),
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
        .when(ready, |el| {
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
        // Hover's servers, once a file in their language has opened.
        .children(
            browser
                .code_intel
                .as_ref()
                .map(|intel| server_status(&self.state, intel, IntelOwner::Browser, theme)),
        )
        .into_any_element()
    }

    fn code_pane(&self, browser: &ProjectBrowser, cx: &Context<Self>) -> AnyElement {
        let theme = Theme::get(cx);
        let Some(file) = &browser.file else {
            return centered(
                div()
                    .text_color(theme.text_muted)
                    .child("Select a file to view it."),
            )
            .into_any_element();
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

        let (dir, name) = match file.path.rsplit_once('/') {
            Some((dir, name)) => (Some(format!("{dir}/")), name),
            None => (None, file.path.as_str()),
        };
        let header = div()
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
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .font_family(MONOSPACE)
                    .children(dir.map(|dir| {
                        div()
                            .flex_shrink()
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(dir)
                    }))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::MEDIUM)
                            .child(name.to_owned()),
                    ),
            )
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
            .child(body)
            .into_any_element()
    }
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
        let hover = self.render_hover(cx);
        let peek = self.render_peek(window, cx);
        let state = self.state.clone();
        let state = state.read(cx);
        let Some(browser) = &state.browser else {
            return div().flex_1();
        };
        self.sync_measured(browser.file.as_ref());
        self.apply_pending(browser.file.as_ref());
        let theme = Theme::get(cx);

        let header = {
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
                                        .child(match browser.working_copy() {
                                            Some(path) => display_path(path),
                                            None => "Not checked out: as of its last commit".into(),
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
        };

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
            .child(header)
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
        let target = self.state.read(cx).browser.as_ref()?.lsp_target()?;
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
}
