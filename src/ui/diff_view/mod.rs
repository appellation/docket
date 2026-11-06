//! A pull request's Files tab, or a branch's changes: every file's changes
//! in one unified, syntax-highlighted list, beside the changed files in the
//! sidebar (`files_sidebar`).
//!
//! Rows are derived from the diff once per load (see `rows.rs`) and drawn
//! with a virtualized list, so only visible lines are laid out. Highlights
//! are looked up per visible line, and fill in as background highlighting
//! finishes.
//!
//! Code can be selected and copied like text, resting the mouse on it
//! shows the language server's hover, and command-clicking it jumps to its
//! definition, as in every code view (`ui/code`). A definition on a line the
//! diff shows is scrolled to and selected (expanding its file if it's
//! collapsed); one elsewhere, at the diff's head or outside the project, is
//! peeked at instead.
//!
//! Each file's path in its header opens it in the Browse tab, as it is at
//! the head; the rest of the header collapses it. Marking a file as viewed
//! in its header collapses it too, as GitHub does, and it stays collapsed
//! (in every window, and after a relaunch) until what the diff shows of it
//! changes (`diff::viewed`). A file collapsed or expanded by hand goes back
//! to that default when it changes, too.
//!
//! Once Claude Code is connected, the diff shows grouped by change:
//! Claude's groups of related hunks (`state/analysis.rs`), each headed by
//! its title, a summary, and what it affects, then its hunks file by file.
//! The toolbar above switches back to file by file, and until Claude Code
//! is connected, offers to connect it.
//!
//! Hunk headers name the scopes their excerpt is in but doesn't show the
//! start of, like the function name `git diff` puts after `@@`, from the
//! file's outline. Each name hovers like code.

mod analysis;
mod file_list;
mod rows;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, HighlightStyle,
    ListHorizontalSizingBehavior, MouseButton, Pixels, ScrollStrategy, StyledText, TextLayout,
    Window, div, prelude::*, px, uniform_list,
};

use self::analysis::{group_header, group_line};
pub(super) use self::file_list::server_status;
use self::file_list::{DirKey, FileTree, stats, status_icon};
use self::rows::{DiffRows, DisplayRow, Notice, build_grouped_rows, build_rows};
use super::code::{
    self, Ask, CodeDefinition, CodeHover, CodeState, CodeView, Definition, GoBack, Granularity,
    Marks, ROW_HEIGHT, Selection, TextPoint, code_word_at, drag_listeners, link_style,
    selected_newline, styled_line, with_background, with_highlight,
};
use super::components::{
    Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconName, IconSize, MONOSPACE,
    Spinner, TestSelector as _, Tooltip,
};
use super::theme::{Color, Theme};
use crate::ai::DiffAnalysis;
use crate::diff::{
    DiffLine, Excerpt, FileContent, FileDiff, FileStatus, LineKind, PullRequestDiff, TextContent,
    TextSnapshot, ViewKey, Viewed, hunk_header,
};
use crate::lsp::{Outline, Position, SymbolKind};
use crate::projects::ProjectId;
use crate::state::{AnalysisLoad, AppState, DiffFiles, DiffLoad, LspTarget, PrDetail, Tab};
use crate::syntax::{HighlightId, SyntaxSnapshot};

const KEY_CONTEXT: &str = "DiffView";
const LINE_NUMBER_WIDTH: f32 = 48.;

pub struct DiffView {
    state: Entity<Tab>,
    /// The `DiffFiles::revision` `rows` were built from.
    revision: Option<u64>,
    /// The analysis they're grouped by, if they are, by address.
    grouped_by: Option<usize>,
    /// Showing the form to connect Anthropic.
    connecting: bool,
    rows: DiffRows,
    /// The files `rows` show collapsed: those marked as viewed, but for
    /// those `toggled`.
    collapsed: HashSet<usize>,
    /// Files collapsed or expanded by hand, against their default, by
    /// diff (`DiffFiles::viewed_scope`) and path; each with its key then,
    /// so it goes back to the default once it changes.
    toggled: HashMap<String, HashMap<String, ViewKey>>,
    /// The directories collapsed in the file tree. Only the tree changes;
    /// the diff still shows their files.
    collapsed_dirs: HashSet<DirKey>,
    /// The groups collapsed in the file tree, grouped by change.
    collapsed_groups: HashSet<usize>,
    /// The changed files beside the diff.
    tree: FileTree,
    /// The file at the top of the viewport, highlighted in the file tree.
    current_file: Option<usize>,
    /// Grouped by change, the group at the top of the viewport.
    current_group: Option<usize>,
    focus_handle: FocusHandle,
    /// Selection, hover, and the line rows laid out in the last frame.
    code: CodeState,
    /// The hunk headers with scopes laid out in the last frame, by row.
    scope_hits: HashMap<usize, ScopeHit>,
    /// Where each jump to a definition came from, latest last.
    back: Vec<DiffPlace>,
}

/// A place in the diff to go back to. By row rather than index, so
/// expanding or collapsing a file in between doesn't move it.
struct DiffPlace {
    /// The first row at the top of the viewport that isn't a spacer.
    top: Option<DisplayRow>,
    /// How far down from `top`'s top the viewport starts.
    within: Pixels,
    /// The selection's ends, by row and offset.
    selection: Option<((DisplayRow, usize), (DisplayRow, usize))>,
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
    Line(code::LineHit),
    Scopes(ScopeHit),
}

impl DiffView {
    pub fn bind_keys(cx: &mut App) {
        code::bind_keys(KEY_CONTEXT, cx);
    }

    pub fn new(state: Entity<Tab>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            revision: None,
            grouped_by: None,
            connecting: false,
            rows: DiffRows::default(),
            collapsed: HashSet::new(),
            toggled: HashMap::new(),
            collapsed_dirs: HashSet::new(),
            collapsed_groups: HashSet::new(),
            tree: FileTree::new(),
            current_file: None,
            current_group: None,
            focus_handle: cx.focus_handle(),
            code: CodeState::default(),
            scope_hits: HashMap::new(),
            back: Vec::new(),
        }
    }

    /// Scrolls the diff to a file's header.
    pub fn reveal_file(&mut self, file: usize, cx: &mut Context<Self>) {
        if let Some(&row) = self.rows.file_headers.get(file) {
            self.code.scroll.scroll_to_item(row, ScrollStrategy::Top);
            self.current_file = Some(file);
            cx.notify();
        }
    }

    /// Scrolls the diff to a group's header.
    fn reveal_group(&mut self, group: usize, cx: &mut Context<Self>) {
        if let Some(&row) = self.rows.group_headers.get(group) {
            self.code.scroll.scroll_to_item(row, ScrollStrategy::Top);
            self.current_group = Some(group);
            cx.notify();
        }
    }

    /// Scrolls the diff to a file's hunks in a group.
    fn reveal_group_file(&mut self, group: usize, file: usize, cx: &mut Context<Self>) {
        if let Some(&row) = self.rows.group_file_headers.get(&(group, file)) {
            self.code.scroll.scroll_to_item(row, ScrollStrategy::Top);
            self.current_group = Some(group);
            self.current_file = Some(file);
            cx.notify();
        }
    }

    /// Collapses or expands a directory in the file tree.
    fn toggle_directory(&mut self, key: DirKey, cx: &mut Context<Self>) {
        if !self.collapsed_dirs.remove(&key) {
            self.collapsed_dirs.insert(key);
        }
        cx.notify();
    }

    /// Collapses or expands a group in the file tree.
    fn toggle_group(&mut self, group: usize, cx: &mut Context<Self>) {
        if !self.collapsed_groups.remove(&group) {
            self.collapsed_groups.insert(group);
        }
        cx.notify();
    }

    /// Opens a file in the pull request's or branch's Browse tab.
    fn browse_file(&mut self, path: String, cx: &mut Context<Self>) {
        self.dismiss_popovers(cx);
        self.state
            .update(cx, |state, cx| state.browse_diff_file(&path, cx));
    }

    /// Collapses or expands a file by hand.
    fn toggle_collapsed(&mut self, file: usize, cx: &mut Context<Self>) {
        let Some((scope, path, key)) = file_identity(self.state.read(cx), file) else {
            return;
        };
        let toggled = self.toggled.entry(scope).or_default();
        if toggled.get(&path) == Some(&key) {
            toggled.remove(&path);
        } else {
            toggled.insert(path, key);
        }
        cx.notify();
    }

    /// Marks a file as viewed, which collapses it, or unmarks it, which
    /// expands it.
    fn set_viewed(&mut self, file: usize, viewed: bool, cx: &mut Context<Self>) {
        let Some((scope, path, key)) = file_identity(self.state.read(cx), file) else {
            return;
        };
        if let Some(toggled) = self.toggled.get_mut(&scope) {
            toggled.remove(&path);
        }
        let tab = self.state.read(cx);
        let app = tab.app.clone();
        let pull_request = tab.viewed_pull_request_id().map(str::to_owned);
        app.update(cx, |app, cx| {
            app.set_viewed(
                &scope,
                &path,
                viewed.then_some(key),
                pull_request.as_deref(),
                cx,
            );
        });
    }

    /// Why GitHub refused a file being marked as viewed, with a button to
    /// try again.
    fn render_viewed_error(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tab = self.state.read(cx);
        let scope = files(tab)?.viewed_scope.clone();
        let error = tab.app.read(cx).viewed_error(&scope)?.clone();
        let app = tab.app.clone();
        Some(
            div()
                .p_3()
                .child(
                    Callout::new(CalloutKind::Error, error)
                        .title("Couldn't mark files as viewed on GitHub")
                        .action(
                            Button::new("diff-viewed-retry", "Try Again")
                                .icon(IconName::RefreshCw)
                                .style(ButtonStyle::Secondary)
                                .size(ButtonSize::Compact)
                                .on_click(move |_, _, cx| {
                                    app.update(cx, |app, cx| app.retry_viewed(&scope, cx));
                                }),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The files to show collapsed.
    fn collapsed_files(&self, app: &AppState, files: &DiffFiles) -> HashSet<usize> {
        let DiffLoad::Ready(diff) = &files.diff else {
            return HashSet::new();
        };
        let toggled = self.toggled.get(&files.viewed_scope);
        diff.files
            .iter()
            .enumerate()
            .filter(|(_, file)| {
                let viewed = app.viewed(&files.viewed_scope, file) == Viewed::Yes;
                let toggled = toggled.and_then(|t| t.get(file.path())) == Some(&file.view_key);
                viewed != toggled
            })
            .map(|(ix, _)| ix)
            .collect()
    }

    /// Switches between file by file and grouped by change.
    fn set_group_changes(&mut self, group: bool, cx: &mut Context<Self>) {
        let app = self.state.read(cx).app.clone();
        app.update(cx, |app, cx| app.set_group_changes(group, cx));
    }

    /// Rebuilds the rows if the diff, its grouping, or the collapsed files
    /// changed.
    fn sync_rows(&mut self, cx: &App) {
        let tab = self.state.read(cx);
        let Some(files) = files(tab) else {
            return;
        };
        let app = tab.app.read(cx);
        let analysis = shown_analysis(app, files);
        let grouped_by = analysis.map(|a| Arc::as_ptr(a) as usize);
        let collapsed = self.collapsed_files(app, files);
        let same_diff = self.revision == Some(files.revision) && self.grouped_by == grouped_by;
        if same_diff && self.collapsed == collapsed {
            return;
        }
        self.collapsed = collapsed;
        self.rows = match (&files.diff, analysis) {
            (DiffLoad::Ready(diff), Some(analysis)) => {
                build_grouped_rows(diff, analysis, &self.collapsed)
            }
            (DiffLoad::Ready(diff), None) => build_rows(diff, &self.collapsed),
            _ => DiffRows::default(),
        };
        self.grouped_by = grouped_by;
        // A new diff, not just a file collapsing.
        if !same_diff && self.revision.is_some() {
            self.back.clear();
        }
        self.revision = Some(files.revision);
        // Rows moved, so positions in them mean something else now.
        self.code.reset();
        self.scope_hits.clear();
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let top_file = self.rows.rows.get(range.start).and_then(row_file);
        let top_group = self.rows.group_at(range.start);
        if (top_file.is_some() && top_file != self.current_file) || top_group != self.current_group
        {
            self.current_file = top_file.or(self.current_file);
            self.current_group = top_group;
            cx.notify();
        }

        // The list also renders a row to measure it before the visible
        // ones, so only the last call's lines are on screen.
        self.code.line_hits.clear();
        self.scope_hits.clear();
        let entity = cx.entity();
        let tab = self.state.read(cx);
        let app = tab.app.read(cx);
        let Some(files) = files(tab) else {
            return Vec::new();
        };
        let DiffLoad::Ready(diff) = &files.diff else {
            return Vec::new();
        };
        let analysis = match &files.analysis {
            AnalysisLoad::Ready(analysis) => Some(&**analysis),
            _ => None,
        };
        let theme = Theme::get(cx);
        // Fields, not a method, so the line hits stay free to fill in.
        let marks = Marks {
            selection: self.code.selection.as_ref(),
            hover: self.code.hover.marked(),
            link: self.code.definition.link(),
            found: &[],
            current_found: None,
        };
        range
            .map(|ix| {
                let (element, hit) = render_row(
                    ix,
                    self.rows.rows[ix],
                    &self.rows,
                    analysis,
                    files,
                    diff,
                    app,
                    &self.collapsed,
                    &entity,
                    &marks,
                    theme,
                );
                match hit {
                    Some(RowHit::Line(hit)) => {
                        self.code.line_hits.insert(ix, hit);
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

    fn place(&self) -> DiffPlace {
        let scrolled = -self.code.scroll.0.borrow().base_handle.offset().y;
        let first = (scrolled / px(ROW_HEIGHT)).floor().max(0.) as usize;
        let top =
            (first..self.rows.rows.len()).find(|&ix| self.rows.rows[ix] != DisplayRow::Spacer);
        let row_at = |point: TextPoint| Some((*self.rows.rows.get(point.row)?, point.offset));
        DiffPlace {
            top: top.map(|ix| self.rows.rows[ix]),
            within: scrolled - px(ROW_HEIGHT) * top.unwrap_or(first),
            selection: self.code.selection.as_ref().and_then(|selection| {
                let (start, end) = selection.range();
                row_at(start).zip(row_at(end))
            }),
        }
    }

    /// Returns to where the last jump to a definition came from.
    fn go_back(&mut self, _: &GoBack, _: &mut Window, cx: &mut Context<Self>) {
        let Some(place) = self.back.pop() else {
            return;
        };
        self.dismiss_popovers(cx);
        let find = |row: DisplayRow| self.rows.rows.iter().position(|r| *r == row);
        if let Some(ix) = place.top.and_then(find) {
            let state = self.code.scroll.0.borrow();
            let handle = &state.base_handle;
            let mut offset = handle.offset();
            let y = px(ROW_HEIGHT) * ix + place.within;
            offset.y = -y.clamp(px(0.), handle.max_offset().height);
            handle.set_offset(offset);
        }
        self.code.selection = place.selection.and_then(|((start, from), (end, to))| {
            let start = TextPoint {
                row: find(start)?,
                offset: from,
            };
            let end = TextPoint {
                row: find(end)?,
                offset: to,
            };
            Some(Selection::new((start, end), Granularity::Character))
        });
        cx.notify();
    }
}

impl CodeView for DiffView {
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
        self.rows.rows.len()
    }

    fn line_text<'a>(&'a self, row: usize, cx: &'a App) -> Option<&'a str> {
        let DisplayRow::Line { file, line } = *self.rows.rows.get(row)? else {
            return None;
        };
        let DiffLoad::Ready(diff) = &files(self.state.read(cx))?.diff else {
            return None;
        };
        let (text, _, number) = line_side(&diff.files[file], line)?;
        Some(text.line(number))
    }

    /// Code on the new side (context and added lines), or a scope name in a
    /// hunk header (offset into its scope text, and the whole name). The
    /// server runs in a worktree at the diff's head, where removed lines
    /// don't exist.
    fn hover_word_at(&self, row: usize, x: Pixels, cx: &App) -> Option<(usize, Range<usize>)> {
        match self.rows.rows.get(row)? {
            DisplayRow::Line { line, .. } if line.kind != LineKind::Removed => {
                code_word_at(self, row, x, cx)
            }
            DisplayRow::HunkHeader { .. } => {
                let scope = self.scope_hits.get(&row)?.scope_under(x)?;
                Some((scope.range.start, scope.range.clone()))
            }
            _ => None,
        }
    }

    fn hover_x(&self, row: usize, offset: usize) -> Option<Pixels> {
        match (self.code.line_hits.get(&row), self.scope_hits.get(&row)) {
            (Some(hit), _) => Some(hit.x_for_offset(offset)),
            (None, Some(hit)) => Some(hit.x_for_offset(offset)),
            (None, None) => None,
        }
    }

    fn lsp_request(&self, row: usize, offset: usize, cx: &App) -> Option<(LspTarget, Ask)> {
        let (file, ask) = match *self.rows.rows.get(row)? {
            DisplayRow::Line { file, line } => (
                file,
                Ask::Code {
                    line: line.new_line?,
                    offset,
                },
            ),
            DisplayRow::HunkHeader { file, .. } => {
                let scopes = &self.scope_hits.get(&row)?.scopes;
                let scope = scopes.iter().find(|s| s.range.contains(&offset))?;
                (file, Ask::Declaration(scope.position))
            }
            _ => return None,
        };
        Some((files(self.state.read(cx))?.lsp_target(file)?, ask))
    }

    /// A definition on a line the diff shows, in a file on the new side.
    fn reveal_definition(&mut self, definition: &Definition, cx: &mut Context<Self>) -> bool {
        let Some(relative) = definition.relative.as_deref() else {
            return false;
        };
        let line = definition.line();
        let found = files(self.state.read(cx)).and_then(|files| {
            let DiffLoad::Ready(diff) = &files.diff else {
                return None;
            };
            diff.files.iter().enumerate().find_map(|(ix, file)| {
                let FileContent::Text(text) = &file.content else {
                    return None;
                };
                let new = text.new.as_ref()?;
                let shown = text.excerpts.iter().any(|e| e.new.contains(&line));
                (file.new_path.as_deref() == Some(relative) && shown && line < new.line_count())
                    .then(|| (ix, definition.name_in(new.line(line))))
            })
        });
        let Some((file, name)) = found else {
            return false;
        };
        let place = self.place();
        if self.collapsed.contains(&file) {
            self.toggle_collapsed(file, cx);
            self.sync_rows(cx);
        }
        let row = self.rows.rows.iter().position(|row| {
            matches!(row, DisplayRow::Line { file: f, line: l } if *f == file && l.new_line == Some(line))
        });
        let Some(row) = row else {
            return false;
        };
        self.back.push(place);
        let at = |offset| TextPoint { row, offset };
        self.code.selection = Some(Selection::new(
            (at(name.start), at(name.end)),
            Granularity::Character,
        ));
        self.code.scroll.scroll_to_item(row, ScrollStrategy::Center);
        cx.notify();
        true
    }

    /// The project the diff comes from. Its checkout may be at another
    /// commit than the diff's head, but it's the closest to browse.
    fn definition_project(&self, cx: &App) -> Option<ProjectId> {
        files(self.state.read(cx))?.project
    }
}

fn files(state: &Tab) -> Option<&DiffFiles> {
    state.diff_files()
}

/// A file of the open diff by what outlasts it: its diff's scope, path,
/// and view key.
fn file_identity(state: &Tab, file: usize) -> Option<(String, String, ViewKey)> {
    let files = files(state)?;
    let DiffLoad::Ready(diff) = &files.diff else {
        return None;
    };
    let file = diff.files.get(file)?;
    Some((
        files.viewed_scope.clone(),
        file.path().to_owned(),
        file.view_key.clone(),
    ))
}

/// The analysis the diff shows grouped by, if it does.
pub(super) fn shown_analysis<'a>(
    app: &AppState,
    files: &'a DiffFiles,
) -> Option<&'a Arc<DiffAnalysis>> {
    match &files.analysis {
        AnalysisLoad::Ready(analysis) if app.groups_changes() => Some(analysis),
        _ => None,
    }
}

/// The file a row is in. A group's rows aren't in one.
fn row_file(row: &DisplayRow) -> Option<usize> {
    match *row {
        DisplayRow::FileHeader { file }
        | DisplayRow::HunkHeader { file, .. }
        | DisplayRow::Line { file, .. }
        | DisplayRow::Notice { file, .. } => Some(file),
        // Spacers sit just above the next file.
        DisplayRow::Spacer => Some(0),
        DisplayRow::GroupHeader { .. } | DisplayRow::GroupText { .. } => None,
    }
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_rows(cx);
        let hover = self.render_hover(cx);
        let peek = self.render_peek(window, cx);
        let entity = cx.entity();
        let toolbar = self.render_toolbar(cx);
        let connect = self.render_connect(cx);
        let analysis_error = self.render_analysis_error(cx);
        let viewed_error = self.render_viewed_error(cx);
        let state = self.state.read(cx);
        let theme = Theme::get(cx);
        let Some(files) = files(state) else {
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
            // A branch is always in a project.
            DiffLoad::NoProject => match &state.detail {
                Some(detail) => no_project(detail, &self.state).into_any_element(),
                None => div().into_any_element(),
            },
            DiffLoad::Ready(diff) if diff.files.is_empty() => {
                centered(div().text_color(theme.text_muted).child("No changes.")).into_any_element()
            }
            DiffLoad::Ready(_) => div()
                .flex()
                .flex_1()
                .min_h_0()
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
                        .on_scroll_wheel(cx.listener(|this, _, _, cx| this.dismiss_popovers(cx)))
                        .child(
                            uniform_list(
                                "diff-rows",
                                self.rows.rows.len(),
                                cx.processor(Self::render_rows),
                            )
                            .size_full()
                            .track_scroll(self.code.scroll.clone())
                            .with_horizontal_sizing_behavior(
                                ListHorizontalSizingBehavior::Unconstrained,
                            )
                            .with_width_from_item(self.rows.widest),
                        )
                        .when(self.code.dragging, |el| {
                            el.child(drag_listeners(entity.clone()))
                        }),
                )
                .into_any_element(),
        };

        div()
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::clear_selection))
            .on_action(cx.listener(Self::go_to_definition_action))
            .on_action(cx.listener(Self::go_to_definition_in_new_tab_action))
            .on_action(cx.listener(Self::go_back))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .children(toolbar)
            .children(connect)
            .children(analysis_error)
            .children(viewed_error)
            .child(body)
            .children(hover)
            .children(peek)
    }
}

/// Status text near the top of the body, like the docket's empty state.
fn centered(child: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(96.))
        .child(child)
}

/// A pull request with no project to read it from: its Files and Browse
/// tabs.
pub(super) fn no_project(detail: &PrDetail, state: &Entity<Tab>) -> impl IntoElement {
    let add = state.clone();
    let url = detail.url();
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
                    detail.repo
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
    rows: &DiffRows,
    analysis: Option<&DiffAnalysis>,
    files: &DiffFiles,
    diff: &PullRequestDiff,
    app: &AppState,
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
            app.viewed(&files.viewed_scope, &diff.files[file]),
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
            let linked = marks.link.as_ref().is_some_and(|(row, _)| *row == ix);
            let element = base
                .test_selector(|| format!("diff-hunk-{file}-{}", excerpt.old.start))
                .when(linked, |el| el.cursor_pointer())
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
        DisplayRow::GroupHeader { group } => match analysis {
            Some(analysis) => group_header(base, group, analysis, theme),
            None => base.into_any_element(),
        },
        DisplayRow::GroupText { group, line } => {
            match rows.group_text.get(group).and_then(|lines| lines.get(line)) {
                Some(line) => group_line(base, line, theme),
                None => base.into_any_element(),
            }
        }

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
    viewed: Viewed,
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
    // A deleted file isn't at the head to browse.
    let browse = (file.status != FileStatus::Deleted).then(|| file.path().to_owned());
    let label = div()
        .text_xs()
        .font_family(MONOSPACE)
        .font_weight(FontWeight::MEDIUM)
        .child(path);
    let label = match browse {
        Some(browse) => {
            let (view, accent) = (view.clone(), theme.accent);
            label
                .test_selector(|| format!("diff-browse-{browse}"))
                .id(("diff-file-browse", ix))
                .hover(move |style| style.text_color(accent))
                .tooltip(|_, cx| Tooltip::view("Browse the file".into(), cx))
                .on_click(move |_, _, cx| {
                    // Not a click on the header, which collapses it.
                    cx.stop_propagation();
                    let path = browse.clone();
                    view.update(cx, |view, cx| view.browse_file(path, cx));
                })
                .into_any_element()
        }
        None => label.into_any_element(),
    };
    let toggle_viewed = viewed_toggle(ix, viewed, view, theme);
    let view = view.clone();
    div()
        .id(("diff-file-header", ix))
        .test_selector(|| format!("diff-file-header-{ix}"))
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
        .child(label)
        .child(stats(file.additions, file.deletions, theme))
        .child(toggle_viewed)
        .when(viewed == Viewed::Changed, |el| {
            el.child(
                div()
                    .test_selector(|| format!("diff-viewed-changed-{ix}"))
                    .text_xs()
                    .text_color(theme.warning)
                    .child("Changed since viewed"),
            )
        })
        .on_click(move |_, _, cx| {
            view.update(cx, |view, cx| view.toggle_collapsed(ix, cx));
        })
        .into_any_element()
}

/// The checkbox in a file's header that marks it as viewed. After the
/// file's stats rather than at the end of the row: rows are as wide as the
/// diff's widest line, which can be off screen.
fn viewed_toggle(ix: usize, viewed: Viewed, view: &Entity<DiffView>, theme: &Theme) -> AnyElement {
    let checked = viewed == Viewed::Yes;
    let view = view.clone();
    div()
        .test_selector(|| format!("diff-viewed-{ix}"))
        .id(("diff-file-viewed", ix))
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .ml_2()
        .px_1()
        .rounded_sm()
        .text_xs()
        .text_color(if checked {
            theme.text
        } else {
            theme.text_muted
        })
        .hover(|style| style.bg(theme.element_hover))
        .child(
            Icon::new(if checked {
                IconName::SquareCheck
            } else {
                IconName::Square
            })
            .size(IconSize::XSmall)
            .color(if checked { Color::Accent } else { Color::Muted }),
        )
        .child("Viewed")
        .tooltip(move |_, cx| {
            Tooltip::view(
                if checked {
                    "Mark as not viewed".into()
                } else {
                    "Mark as viewed, collapsing it until it changes".into()
                },
                cx,
            )
        })
        .on_click(move |_, _, cx| {
            // Not a click on the header, which collapses it.
            cx.stop_propagation();
            view.update(cx, |view, cx| view.set_viewed(ix, !checked, cx));
        })
        .into_any_element()
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
    if let Some((link_row, range)) = &marks.link
        && *link_row == row
    {
        spans = with_highlight(spans, range.clone(), link_style(theme), content.len());
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
) -> (AnyElement, Option<code::LineHit>) {
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
    let styled = styled_line(row, &snapshot.text()[range], spans, marks, theme);

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
        .cursor(marks.cursor(row))
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
                .child(styled.text)
                // The newline, when the selection carries on to the next row.
                .when(styled.selection_continues, |el| {
                    el.child(selected_newline(theme.text_selection))
                }),
        )
        .into_any_element();
    (element, Some(styled.hit))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use gpui::{Entity, TestAppContext, VisualTestContext};

    use crate::diff::Viewed;
    use crate::github::{FakeGithub, FileViewedState};
    use crate::state::{AppState, DiffLoad, PULL_REQUEST_INTERVAL, SectionKind, Tab};
    use crate::test_support::{
        self, VisualTestContextExt as _, detail, git_checkout, git_commit, open_docket, project,
        pull_request,
    };

    /// Commits `files` on `feature`.
    fn commit_on_feature(root: &Path, files: &[(&str, &str)], date: &str) {
        test_support::git(root, &["checkout", "--quiet", "feature"]);
        for (path, text) in files {
            std::fs::write(root.join(path), text).unwrap();
        }
        git_commit(root, "Change things", date);
        test_support::git(root, &["checkout", "--quiet", "main"]);
    }

    fn viewed(tab: &Entity<Tab>, file: usize, cx: &mut VisualTestContext) -> Viewed {
        tab.read_with(cx, |tab, cx| {
            let files = tab.diff_files().unwrap();
            let DiffLoad::Ready(diff) = &files.diff else {
                panic!("the diff isn't loaded");
            };
            AppState::global(cx)
                .read(cx)
                .viewed(&files.viewed_scope, &diff.files[file])
        })
    }

    fn refresh(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("cmd-r");
        cx.run_until_parked();
    }

    #[gpui::test]
    fn viewed_files_stay_collapsed_until_they_change(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_checkout(root, &[("a.txt", "one\n"), ("b.txt", "one\n")]);
        commit_on_feature(
            root,
            &[("a.txt", "two\n"), ("b.txt", "two\n")],
            "2026-01-02T00:00:00Z",
        );
        let projects = vec![project(1, root, "acme/widgets")];
        let (tab, cx) = open_docket(cx, Arc::new(FakeGithub::new()), projects);
        cx.click_on("branch-1-feature");
        assert!(cx.is_drawn("diff-hunk-0-0"));
        assert!(cx.is_drawn("diff-hunk-1-0"));
        assert!(!cx.is_drawn("diff-files-viewed"));

        // Marking a file as viewed collapses it, and the tree shows it.
        cx.click_on("diff-viewed-0");
        assert_eq!(viewed(&tab, 0, cx), Viewed::Yes);
        assert!(!cx.is_drawn("diff-hunk-0-0"));
        assert!(cx.is_drawn("diff-hunk-1-0"));
        assert!(cx.is_drawn("diff-file-0-viewed"));
        assert!(!cx.is_drawn("diff-file-1-viewed"));
        assert!(cx.is_drawn("diff-files-viewed"));

        // Expanding it by hand leaves it viewed.
        cx.click_on("diff-file-header-0");
        assert!(cx.is_drawn("diff-hunk-0-0"));
        assert_eq!(viewed(&tab, 0, cx), Viewed::Yes);
        cx.click_on("diff-file-header-0");
        assert!(!cx.is_drawn("diff-hunk-0-0"));

        // It's still collapsed when the branch opens again.
        cx.simulate_keystrokes("escape");
        cx.click_on("branch-1-feature");
        assert!(!cx.is_drawn("diff-hunk-0-0"));
        assert!(cx.is_drawn("diff-hunk-1-0"));

        // And after a commit that changes only the other file, which was
        // collapsed by hand, and so goes back to expanded.
        cx.click_on("diff-file-header-1");
        assert!(!cx.is_drawn("diff-hunk-1-0"));
        commit_on_feature(root, &[("b.txt", "three\n")], "2026-01-03T00:00:00Z");
        refresh(cx);
        assert!(!cx.is_drawn("diff-hunk-0-0"));
        assert!(cx.is_drawn("diff-hunk-1-0"));
        assert_eq!(viewed(&tab, 0, cx), Viewed::Yes);

        // Once it changes, it's no longer viewed, but changed since.
        commit_on_feature(root, &[("a.txt", "three\n")], "2026-01-04T00:00:00Z");
        refresh(cx);
        assert_eq!(viewed(&tab, 0, cx), Viewed::Changed);
        assert!(cx.is_drawn("diff-hunk-0-0"));
        assert!(cx.is_drawn("diff-viewed-changed-0"));
        assert!(!cx.is_drawn("diff-file-0-viewed"));
        assert!(!cx.is_drawn("diff-files-viewed"));

        // Marking it again collapses it, and unmarking it expands it.
        cx.click_on("diff-viewed-0");
        assert_eq!(viewed(&tab, 0, cx), Viewed::Yes);
        assert!(!cx.is_drawn("diff-hunk-0-0"));
        assert!(!cx.is_drawn("diff-viewed-changed-0"));
        cx.click_on("diff-viewed-0");
        assert_eq!(viewed(&tab, 0, cx), Viewed::No);
        assert!(cx.is_drawn("diff-hunk-0-0"));
    }

    /// The commit `reference` is at.
    fn tip(root: &Path, reference: &str) -> String {
        let out = std::process::Command::new("git")
            .args(["rev-parse", reference])
            .current_dir(root)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// Waits out a check of the open pull request.
    fn poll(cx: &mut VisualTestContext) {
        cx.executor().advance_clock(PULL_REQUEST_INTERVAL);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn pull_requests_viewed_files_are_githubs(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_checkout(root, &[("a.txt", "one\n"), ("b.txt", "one\n")]);
        commit_on_feature(
            root,
            &[("a.txt", "two\n"), ("b.txt", "two\n")],
            "2026-01-02T00:00:00Z",
        );
        let mut pr = pull_request("acme/widgets", 7, "Change things");
        pr.base_ref_oid = tip(root, "main");
        pr.head_ref_oid = tip(root, "feature");
        let github = Arc::new(FakeGithub::new());
        github.set_search(
            SectionKind::ReviewRequested.search_query(),
            vec![pr.clone()],
        );
        github.set_detail(pr.repository.parse().unwrap(), detail(&pr));
        // Marked as viewed on github.com.
        github.set_viewed_state(&pr.id, "a.txt", FileViewedState::Viewed);
        let projects = vec![project(1, root, "acme/widgets")];
        let (tab, cx) = open_docket(cx, github.clone(), projects);
        cx.click_on(&pr.id);
        cx.click_on("pr-tab-files");
        assert_eq!(viewed(&tab, 0, cx), Viewed::Yes);
        assert!(!cx.is_drawn("diff-hunk-0-0"));
        assert!(cx.is_drawn("diff-hunk-1-0"));
        let fetches = github.detail_fetches();

        // Marking one in Docket marks it on GitHub, without fetching the
        // detail again.
        cx.click_on("diff-viewed-1");
        cx.run_until_parked();
        assert_eq!(
            github.viewed_state(&pr.id, "b.txt"),
            FileViewedState::Viewed
        );
        assert!(!cx.is_drawn("diff-hunk-1-0"));
        poll(cx);
        assert_eq!(github.detail_fetches(), fetches);
        assert!(!cx.is_drawn("diff-hunk-1-0"));

        // Unmarked on github.com, it expands once the pull request is
        // checked; changed since viewed there, it shows so.
        github.set_viewed_state(&pr.id, "a.txt", FileViewedState::Unviewed);
        poll(cx);
        assert_eq!(viewed(&tab, 0, cx), Viewed::No);
        assert!(cx.is_drawn("diff-hunk-0-0"));
        github.set_viewed_state(&pr.id, "a.txt", FileViewedState::Dismissed);
        poll(cx);
        assert_eq!(viewed(&tab, 0, cx), Viewed::Changed);
        assert!(cx.is_drawn("diff-viewed-changed-0"));

        // A mark GitHub refuses stays in Docket, with a way to send it
        // again.
        github.fail_viewed(Some("Resource not accessible by personal access token"));
        cx.click_on("diff-viewed-0");
        cx.run_until_parked();
        assert_eq!(
            github.viewed_state(&pr.id, "a.txt"),
            FileViewedState::Dismissed
        );
        assert_eq!(viewed(&tab, 0, cx), Viewed::Yes);
        assert!(!cx.is_drawn("diff-hunk-0-0"));
        assert!(cx.is_drawn("diff-viewed-retry"));
        poll(cx);
        assert_eq!(viewed(&tab, 0, cx), Viewed::Yes, "GitHub's doesn't win");
        github.fail_viewed(None);
        cx.click_on("diff-viewed-retry");
        cx.run_until_parked();
        assert_eq!(
            github.viewed_state(&pr.id, "a.txt"),
            FileViewedState::Viewed
        );
        assert!(!cx.is_drawn("diff-viewed-retry"));

        // With more files than come with the version, they're fetched.
        github.set_many_files(true);
        github.set_viewed_state(&pr.id, "b.txt", FileViewedState::Unviewed);
        poll(cx);
        assert_eq!(viewed(&tab, 1, cx), Viewed::No);
        assert!(cx.is_drawn("diff-hunk-1-0"));
        assert!(!cx.is_drawn("diff-hunk-0-0"));
    }
}
