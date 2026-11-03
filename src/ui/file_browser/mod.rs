//! A project's files as they are in its checkout: the directory tree on
//! the left, and the selected file, syntax-highlighted, on the right.
//!
//! Both are virtualized lists, so a large checkout or file only lays out
//! what's visible. The tree works from the keyboard: up and down move
//! through it, showing each file they reach; right and left expand and
//! collapse directories, or move into and out of them.

mod tree;

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, HighlightStyle, KeyBinding,
    ListHorizontalSizingBehavior, MouseButton, ScrollStrategy, StyledText, UniformListScrollHandle,
    Window, actions, div, prelude::*, px, uniform_list,
};

use self::tree::{TreeRow, parent_row, visible_rows};
use super::components::{
    Callout, CalloutKind, Icon, IconButton, IconName, IconSize, MONOSPACE, Spinner, display_path,
};
use super::diff_view::{display_width, expand_tabs};
use super::theme::{Color, Theme};
use crate::projects::files::{FileTree, LocalFile};
use crate::state::{AppState, FileLoad, OpenFile, ProjectBrowser, TreeLoad};
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
const TREE_WIDTH: f32 = 260.;
const TREE_ROW_HEIGHT: f32 = 24.;
const INDENT: f32 = 12.;
const LINE_HEIGHT: f32 = 20.;
/// About one digit of the monospace font at `text_xs`.
const DIGIT_WIDTH: f32 = 7.5;

pub struct FileBrowserView {
    state: Entity<AppState>,
    /// The `ProjectBrowser::revision` `rows` were built from; `None` when
    /// they need rebuilding.
    revision: Option<u64>,
    rows: Vec<TreeRow>,
    /// Paths of the expanded directories, so they stay open across a
    /// refresh's new tree.
    expanded: HashSet<String>,
    /// The path of the row the keyboard moves from.
    selected: Option<String>,
    tree_scroll: UniformListScrollHandle,
    code_scroll: UniformListScrollHandle,
    /// The open file's path and contents `widest` was measured for.
    measured: Option<(String, Arc<LocalFile>)>,
    /// The open file's widest line, which sizes its list horizontally.
    widest: Option<usize>,
    focus_handle: FocusHandle,
    focused: bool,
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
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            revision: None,
            rows: Vec::new(),
            expanded: HashSet::new(),
            selected: None,
            tree_scroll: UniformListScrollHandle::new(),
            code_scroll: UniformListScrollHandle::new(),
            measured: None,
            widest: None,
            focus_handle: cx.focus_handle(),
            focused: false,
        }
    }

    fn tree(&self, cx: &App) -> Option<Arc<FileTree>> {
        match &self.state.read(cx).browser.as_ref()?.tree {
            TreeLoad::Ready(tree) => Some(tree.clone()),
            _ => None,
        }
    }

    /// Rebuilds the rows if the tree or the expanded directories changed.
    fn sync_rows(&mut self, cx: &App) {
        let Some(browser) = &self.state.read(cx).browser else {
            return;
        };
        if self.revision == Some(browser.revision) {
            return;
        }
        self.rows = match &browser.tree {
            TreeLoad::Ready(tree) => visible_rows(tree, &self.expanded),
            _ => Vec::new(),
        };
        self.revision = Some(browser.revision);
    }

    fn selected_row(&self, tree: &FileTree) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        self.rows
            .iter()
            .position(|row| tree.node(row.node).path == selected)
    }

    /// Moves the selection to a row, keeping it in view, and shows it if
    /// it's a file.
    fn select_row(&mut self, ix: usize, strategy: ScrollStrategy, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cx) else {
            return;
        };
        let Some(row) = self.rows.get(ix) else {
            return;
        };
        let node = tree.node(row.node);
        self.selected = Some(node.path.clone());
        self.tree_scroll.scroll_to_item(ix, strategy);
        if !node.is_dir() {
            let path = node.path.clone();
            self.state
                .update(cx, |state, cx| state.open_browser_file(path, cx));
        }
        cx.notify();
    }

    fn set_expanded(&mut self, path: &str, expanded: bool, cx: &mut Context<Self>) {
        let changed = if expanded {
            self.expanded.insert(path.to_owned())
        } else {
            self.expanded.remove(path)
        };
        if changed {
            // Rebuild on the next render.
            self.revision = None;
            self.sync_rows(cx);
            cx.notify();
        }
    }

    fn click_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.select_row(ix, ScrollStrategy::Top, cx);
        self.toggle_selected(&ToggleSelected, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cx) else {
            return;
        };
        let next = match self.selected_row(&tree) {
            Some(ix) => (ix + 1).min(self.rows.len().saturating_sub(1)),
            None => 0,
        };
        self.select_row(next, ScrollStrategy::Bottom, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cx) else {
            return;
        };
        let previous = self
            .selected_row(&tree)
            .map_or(0, |ix| ix.saturating_sub(1));
        self.select_row(previous, ScrollStrategy::Top, cx);
    }

    /// Right: expands a directory, or moves into one that's expanded.
    fn expand_selected(&mut self, _: &ExpandSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cx) else {
            return;
        };
        let Some(ix) = self.selected_row(&tree) else {
            return;
        };
        let node = tree.node(self.rows[ix].node);
        if !node.is_dir() {
            return;
        }
        if !self.expanded.contains(&node.path) {
            self.set_expanded(&node.path, true, cx);
        } else if self
            .rows
            .get(ix + 1)
            .is_some_and(|next| next.depth > self.rows[ix].depth)
        {
            self.select_row(ix + 1, ScrollStrategy::Bottom, cx);
        }
    }

    /// Left: collapses an expanded directory, or moves to the one the
    /// selection is in.
    fn collapse_selected(&mut self, _: &CollapseSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cx) else {
            return;
        };
        let Some(ix) = self.selected_row(&tree) else {
            return;
        };
        let node = tree.node(self.rows[ix].node);
        if node.is_dir() && self.expanded.contains(&node.path) {
            self.set_expanded(&node.path, false, cx);
        } else if let Some(parent) = parent_row(&self.rows, ix) {
            self.select_row(parent, ScrollStrategy::Top, cx);
        }
    }

    /// Enter, or a click: expands or collapses a directory.
    fn toggle_selected(&mut self, _: &ToggleSelected, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cx) else {
            return;
        };
        let Some(ix) = self.selected_row(&tree) else {
            return;
        };
        let node = tree.node(self.rows[ix].node);
        if node.is_dir() {
            let expanded = self.expanded.contains(&node.path);
            self.set_expanded(&node.path, !expanded, cx);
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
        if self.measured.as_ref().is_none_or(|(p, _)| p != path) {
            self.code_scroll = UniformListScrollHandle::new();
        }
        self.widest = match &**content {
            LocalFile::Text(text) => (0..text.line_count())
                .max_by_key(|&line| display_width(text.line(line)))
                .map(|line| line as usize),
            _ => None,
        };
        self.measured = Some((path.to_owned(), content.clone()));
    }

    fn render_tree_rows(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(tree) = self.tree(cx) else {
            return Vec::new();
        };
        let theme = Theme::get(cx);
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?;
                let node = tree.node(row.node);
                let selected = self.selected.as_deref() == Some(node.path.as_str());
                let expanded = self.expanded.contains(&node.path);
                let (chevron, icon) = match node.is_dir() {
                    true if expanded => (Some(IconName::ChevronDown), IconName::FolderOpen),
                    true => (Some(IconName::ChevronRight), IconName::Folder),
                    false if LanguageRegistry::for_path(&node.path).is_some() => {
                        (None, IconName::FileCode)
                    }
                    false => (None, IconName::File),
                };
                let color = if selected {
                    Color::Default
                } else {
                    Color::Muted
                };
                let hover = theme.element_hover;
                Some(
                    div()
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
                                .child(div().flex_none().w(px(12.)).children(chevron.map(
                                    |chevron| {
                                        Icon::new(chevron).size(IconSize::XSmall).color(color)
                                    },
                                )))
                                .child(Icon::new(icon).size(IconSize::Small).color(color))
                                .child(div().truncate().child(node.name.clone())),
                        )
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
        range
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
                let (content, spans, _) = expand_tabs(&text.text()[range], spans);
                div()
                    .flex()
                    .items_center()
                    .h(px(LINE_HEIGHT))
                    .w_full()
                    .whitespace_nowrap()
                    .font_family(MONOSPACE)
                    .text_xs()
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
                            .pr_4()
                            .text_color(theme.text)
                            .child(StyledText::new(content).with_highlights(spans)),
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn tree_column(&self, tree: &FileTree, cx: &Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx);
        div()
            .flex_none()
            .flex()
            .flex_col()
            .w(px(TREE_WIDTH))
            .h_full()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.border_subtle)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_4()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .text_color(theme.text_muted)
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(match tree.file_count {
                                1 => "1 file".to_owned(),
                                n => format!("{n} files"),
                            }),
                    )
                    // The checkout's branch: these are its files, not the
                    // default branch's on GitHub.
                    .children(tree.head.clone().map(|head| {
                        div()
                            .truncate()
                            .font_family(MONOSPACE)
                            .text_color(theme.text_subtle)
                            .child(head)
                    })),
            )
            .child(
                uniform_list(
                    "browser-tree",
                    self.rows.len(),
                    cx.processor(Self::render_tree_rows),
                )
                .flex_1()
                .pb_2()
                .track_scroll(self.tree_scroll.clone()),
            )
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
                    let list = uniform_list(
                        "browser-lines",
                        text.line_count() as usize,
                        cx.processor(Self::render_lines),
                    )
                    .size_full()
                    .py_2()
                    .track_scroll(self.code_scroll.clone())
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .with_width_from_item(self.widest);
                    (
                        div().flex_1().min_h_0().child(list).into_any_element(),
                        Some(details),
                    )
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

/// Status text near the top of a pane, like the inbox's empty state.
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
        self.sync_rows(cx);
        let state = self.state.clone();
        let state = state.read(cx);
        let Some(browser) = &state.browser else {
            return div().flex_1();
        };
        self.sync_measured(browser.file.as_ref());
        let theme = Theme::get(cx);

        let header = {
            let (back, refresh) = (self.state.clone(), self.state.clone());
            let project = state.project(browser.project);
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
                                        .child(display_path(&project.path)),
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

        let body = match &browser.tree {
            TreeLoad::Loading => centered(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_color(theme.text_muted)
                    .child(Spinner::new("browser-loading"))
                    .child("Listing the files in your checkout…"),
            )
            .into_any_element(),
            TreeLoad::Failed(err) => div()
                .p_4()
                .child(Callout::new(CalloutKind::Error, err.clone()).title("Couldn't list files"))
                .into_any_element(),
            TreeLoad::Ready(tree) if tree.file_count == 0 => {
                centered(div().text_color(theme.text_muted).child("No files.")).into_any_element()
            }
            TreeLoad::Ready(tree) => div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.tree_column(tree, cx))
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
    }
}
