//! Jumping to where a symbol is defined, in a code view.
//!
//! Command-clicking code, or F12 with the caret in it, asks the file's
//! server where the symbol there is defined. Holding command over code a
//! server can answer for underlines it, as a link. The view shows the
//! definition in place if it has it (`CodeView::reveal_definition`): the
//! diff when it's on a line the diff shows, the file browser when it's in
//! the checkout. Otherwise a peek popover shows the definition's code, read
//! from disk, below the symbol. Views remember where each jump came from,
//! for Go Back.
//!
//! Command-option-click (or option-F12) opens a definition in the project's
//! checkout in a new tab instead, browsing the project's files there and
//! leaving this view as it was.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use futures::future::{Either, select};
use gpui::{
    AnyElement, Context, Corner, HighlightStyle, Modifiers, Pixels, Point, SharedString, Task,
    Window, anchored, deferred, div, point, prelude::*, px,
};

use super::{
    CodeHover, CodeView, GoToDefinition, GoToDefinitionInNewTab, Marks, ROW_HEIGHT, styled_line,
    word_range,
};
use crate::diff::TextSnapshot;
use crate::lsp::{Location, PositionEncoding};
use crate::projects::files::{self, LocalFile};
use crate::syntax::{self, SyntaxSnapshot};
use crate::ui::components::{MONOSPACE, display_path};
use crate::ui::theme::Theme;
use crate::ui::windows;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Lines a peek shows above the definition.
const PEEK_CONTEXT: u32 = 1;
/// Lines a peek shows below the definition's name, at least, for servers
/// that don't say where a definition ends.
const PEEK_MIN_AFTER: u32 = 12;
/// Most lines a peek shows; the popover scrolls through them.
const PEEK_MAX_LINES: u32 = 80;
const MAX_WIDTH: f32 = 640.;
const MAX_HEIGHT: f32 = 320.;
/// About one digit of the monospace font at `text_xs`.
const DIGIT_WIDTH: f32 = 7.5;

/// Where a symbol is defined, as a server answered.
#[derive(Clone, Debug)]
pub struct Definition {
    pub location: Location,
    /// The path relative to where the server runs, if it's in there:
    /// the PR's worktree, or the browsed checkout.
    pub relative: Option<String>,
    /// How the server counts characters in `location`.
    pub encoding: PositionEncoding,
}

impl Definition {
    /// The line the definition's name is on.
    pub fn line(&self) -> u32 {
        self.location.range.0.line
    }

    /// The source range of what the server pointed at, on its line, whose
    /// text is `line`.
    pub fn name_in(&self, line: &str) -> Range<usize> {
        let (start, end) = self.location.range;
        let from = self.encoding.offset(line, start.character);
        let to = if end.line == start.line {
            self.encoding.offset(line, end.character).max(from)
        } else {
            line.len()
        };
        from..to
    }
}

#[derive(Default)]
pub struct DefinitionState {
    /// The word command-hovering marks as a link, by row and source range.
    link: Option<(usize, Range<usize>)>,
    peek: Option<Peek>,
    /// Asking for a definition, or reading one to peek at.
    _task: Option<Task<()>>,
}

impl DefinitionState {
    pub fn link(&self) -> Option<(usize, Range<usize>)> {
        self.link.clone()
    }

    pub fn is_peeking(&self) -> bool {
        self.peek.is_some()
    }
}

/// A definition's code, shown in a popover under the symbol it defines.
struct Peek {
    /// Window position of the symbol's start, at the top of its row.
    origin: Point<Pixels>,
    /// Where it is: the path, as near the project as it can say, and line.
    title: SharedString,
    text: Arc<TextSnapshot>,
    syntax: Option<Arc<SyntaxSnapshot>>,
    lines: Range<u32>,
    /// The name's line and source range there.
    name: (u32, Range<usize>),
}

/// What a peek shows, read off the UI thread.
struct PeekContent {
    title: SharedString,
    text: Arc<TextSnapshot>,
    syntax: Option<Arc<SyntaxSnapshot>>,
    lines: Range<u32>,
    name: (u32, Range<usize>),
}

/// Definitions for every [`CodeView`].
pub trait CodeDefinition: CodeHover {
    /// While command is held over code a server can answer for, marks the
    /// word as a link.
    fn update_link(
        &mut self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let link = (modifiers.secondary()
            && !self.code().dragging
            && self.list_bounds().contains(&position))
        .then(|| {
            let row = usize::try_from(self.row_at(position.y)).ok()?;
            let (offset, word) = self.hover_word_at(row, position.x, cx)?;
            self.lsp_request(row, offset, cx)?;
            Some((row, word))
        })
        .flatten();
        let definition = &mut self.code_mut().definition;
        if definition.link != link {
            definition.link = link;
            cx.notify();
        }
    }

    fn clear_link(&mut self, cx: &mut Context<Self>) {
        if self.code_mut().definition.link.take().is_some() {
            cx.notify();
        }
    }

    /// Command-click: asks for the definition of the code under
    /// `position`, to show here or, with `new_tab`, in a new tab. Returns
    /// whether it asked, so the click doesn't select.
    fn go_to_definition_under(
        &mut self,
        position: Point<Pixels>,
        new_tab: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Ok(row) = usize::try_from(self.row_at(position.y)) else {
            return false;
        };
        let Some((offset, word)) = self.hover_word_at(row, position.x, cx) else {
            return false;
        };
        self.go_to_definition(row, offset, word, new_tab, cx)
    }

    /// F12: asks for the definition of the word at the start of the
    /// selection.
    fn go_to_definition_action(
        &mut self,
        _: &GoToDefinition,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_selected_definition(false, cx);
    }

    /// Option-F12: the same, opening it in a new tab.
    fn go_to_definition_in_new_tab_action(
        &mut self,
        _: &GoToDefinitionInNewTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_selected_definition(true, cx);
    }

    fn go_to_selected_definition(&mut self, new_tab: bool, cx: &mut Context<Self>) {
        let Some((start, _)) = self.code().selection.as_ref().map(|s| s.range()) else {
            return;
        };
        let Some(text) = self.line_text(start.row, cx) else {
            return;
        };
        let word = word_range(text, start.offset);
        self.go_to_definition(start.row, word.start, word, new_tab, cx);
    }

    /// Asks the server for the definition of `offset` in `row`, then shows
    /// it, in a new tab if `new_tab`. `word` is what the popovers point at.
    /// Returns whether there was a server to ask.
    fn go_to_definition(
        &mut self,
        row: usize,
        offset: usize,
        word: Range<usize>,
        new_tab: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((target, ask)) = self.lsp_request(row, offset, cx) else {
            return false;
        };
        self.dismiss_popovers(cx);
        let loading = target.busy.then_some(target.server.spec.name);
        let background = cx.background_executor().clone();
        let request = cx.background_executor().spawn(async move {
            let encoding = target.server.encoding;
            let position = ask.position(&target.text, encoding);
            target
                .server
                .open(&target.path, target.language_id, target.text.text())?;
            let definition = target.server.definition(&target.path, position);
            let timeout = background.timer(REQUEST_TIMEOUT);
            let locations = match select(Box::pin(definition), timeout).await {
                Either::Left((locations, _)) => locations?,
                Either::Right(_) => return Err(anyhow!("no answer within 10 seconds")),
            };
            // Servers may answer with the root's real path.
            let roots = [Some(target.root.clone()), target.root.canonicalize().ok()];
            Ok(locations
                .into_iter()
                .map(|location| Definition {
                    relative: roots
                        .iter()
                        .flatten()
                        .find_map(|root| relative_path(&location.path, root)),
                    location,
                    encoding,
                })
                .collect::<Vec<_>>())
        });
        let task = cx.spawn(async move |this, cx| {
            let found = request.await;
            this.update(cx, |this, cx| {
                this.show_definitions(row, word, found, loading, new_tab, cx)
            })
            .ok();
        });
        self.code_mut().definition._task = Some(task);
        true
    }

    /// Shows the first definition the view can, or else peeks at the
    /// first. With `new_tab`, opens the first in the checkout in a new tab
    /// instead, if there is one. With none, says why in the hover popover.
    fn show_definitions(
        &mut self,
        row: usize,
        word: Range<usize>,
        found: Result<Vec<Definition>>,
        loading: Option<&'static str>,
        new_tab: bool,
        cx: &mut Context<Self>,
    ) {
        self.code_mut().definition._task = None;
        let note = match (&found, loading) {
            (Ok(definitions), _) if !definitions.is_empty() => None,
            // Servers answer with nothing, or an error, until they've loaded.
            (_, Some(name)) => Some(format!("{name} is still loading this project…")),
            (Ok(_), None) => Some("No definition found.".to_owned()),
            (Err(err), None) => {
                eprintln!("definition failed: {err:#}");
                Some("Couldn't find the definition.".to_owned())
            }
        };
        if let Some(note) = note {
            self.show_note(row, word, note.into(), cx);
            return;
        }
        let Ok(mut definitions) = found else {
            return;
        };
        if new_tab && definitions.iter().any(|d| self.open_in_new_tab(d, cx)) {
            self.clear_link(cx);
            return;
        }
        if definitions.iter().any(|d| self.reveal_definition(d, cx)) {
            // The rows under it moved.
            self.clear_link(cx);
            return;
        }
        self.peek_definition(row, word, definitions.swap_remove(0), cx);
    }

    /// Opens a definition in the project's checkout in a new tab of the
    /// active window, browsing the project's files there. Returns whether
    /// it's in the checkout to open.
    fn open_in_new_tab(&mut self, definition: &Definition, cx: &mut Context<Self>) -> bool {
        let (Some(project), Some(_)) = (self.definition_project(cx), &definition.relative) else {
            return false;
        };
        let opened = windows::open(cx.active_window(), true, cx, move |tab, cx| {
            tab.browse_project(project, cx)
        });
        let Some(opened) = opened else {
            return false;
        };
        let definition = definition.clone();
        opened
            .update(cx, |workspace, _, cx| {
                workspace.show_definition(definition, cx)
            })
            .is_ok()
    }

    /// Reads the definition's file and shows its code under `word`.
    fn peek_definition(
        &mut self,
        row: usize,
        word: Range<usize>,
        definition: Definition,
        cx: &mut Context<Self>,
    ) {
        let Some(x) = self.hover_x(row, word.start) else {
            return;
        };
        let origin = point(x, self.row_top(row));
        let task = cx.spawn(async move |this, cx| {
            let content = cx
                .background_executor()
                .spawn(async move { read_peek(&definition) })
                .await;
            this.update(cx, |this, cx| {
                let state = &mut this.code_mut().definition;
                state._task = None;
                match content {
                    Ok(content) => {
                        state.peek = Some(Peek {
                            origin,
                            title: content.title,
                            text: content.text,
                            syntax: content.syntax,
                            lines: content.lines,
                            name: content.name,
                        });
                        cx.notify();
                    }
                    Err(err) => this.show_note(row, word, format!("{err:#}").into(), cx),
                }
            })
            .ok();
        });
        self.code_mut().definition._task = Some(task);
    }

    fn dismiss_peek(&mut self, cx: &mut Context<Self>) {
        let definition = &mut self.code_mut().definition;
        definition._task = None;
        if definition.peek.take().is_some() {
            cx.notify();
        }
    }

    /// Hides the hover and peek, and stops asking for a definition, e.g.
    /// on scroll or click.
    fn dismiss_popovers(&mut self, cx: &mut Context<Self>) {
        self.dismiss_hover(cx);
        self.dismiss_peek(cx);
    }

    fn render_peek(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let peek = self.code().definition.peek.as_ref()?;
        let theme = Theme::get(cx);
        let view = cx.entity();
        // Below the symbol, unless that's too close to the bottom of the
        // window.
        let below = point(peek.origin.x, peek.origin.y + px(ROW_HEIGHT));
        let (corner, position) = if below.y + px(MAX_HEIGHT) > window.viewport_size().height {
            (Corner::BottomLeft, peek.origin)
        } else {
            (Corner::TopLeft, below)
        };
        let digits = peek.lines.end.max(1).ilog10() + 1;
        let gutter = px(digits as f32 * DIGIT_WIDTH + 16.);
        let lines = peek.lines.clone().map(|line| {
            let range = peek.text.line_range(line);
            let spans: Vec<(Range<usize>, HighlightStyle)> = peek
                .syntax
                .as_ref()
                .map(|syntax| {
                    syntax
                        .spans_in(range.clone())
                        .map(|(range, id)| (range, theme.syntax.style(id)))
                        .collect()
                })
                .unwrap_or_default();
            let (name_line, name) = &peek.name;
            let marks = Marks {
                selection: None,
                hover: (line == *name_line).then(|| (line as usize, name.clone())),
                link: None,
            };
            let styled = styled_line(
                line as usize,
                &peek.text.text()[range],
                spans,
                &marks,
                theme,
            );
            div()
                .flex()
                .items_center()
                .h(px(ROW_HEIGHT))
                .whitespace_nowrap()
                .child(
                    div()
                        .flex_none()
                        .w(gutter)
                        .flex()
                        .justify_end()
                        .pr_3()
                        .text_color(theme.text_subtle)
                        .child((line + 1).to_string()),
                )
                .child(div().pr_3().text_color(theme.text).child(styled.text))
        });
        let popover = div()
            .id("code-peek")
            .occlude()
            .flex()
            .flex_col()
            .max_w(px(MAX_WIDTH))
            .max_h(px(MAX_HEIGHT))
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow_lg()
            .text_xs()
            .font_family(MONOSPACE)
            // Only the peek: the click may be asking for another definition.
            .on_mouse_down_out(move |_, _, cx| {
                view.update(cx, |view, cx| {
                    if view.code_mut().definition.peek.take().is_some() {
                        cx.notify();
                    }
                });
            })
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .border_b_1()
                    .border_color(theme.border_subtle)
                    .text_color(theme.text_muted)
                    .truncate()
                    .child(peek.title.clone()),
            )
            .child(
                div()
                    .id("code-peek-lines")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    // Lines as wide as their text, to scroll sideways.
                    .items_start()
                    .py_1()
                    .overflow_scroll()
                    .children(lines),
            );
        Some(
            deferred(
                anchored()
                    .anchor(corner)
                    .position(position)
                    .snap_to_window_with_margin(px(8.))
                    .child(popover),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

impl<V: CodeView> CodeDefinition for V {}

/// `path` relative to `root`, with forward slashes, if it's inside it.
fn relative_path(path: &Path, root: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    (!relative.is_empty()).then_some(relative)
}

/// Reads and highlights the lines around a definition, for a peek.
fn read_peek(definition: &Definition) -> Result<PeekContent> {
    let path: &Path = &definition.location.path;
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        bail!("Couldn't read {}.", path.display());
    };
    let text = match files::read(dir, &name.to_string_lossy())? {
        LocalFile::Text(text) => text,
        _ => bail!("{} can't be shown.", display_path(path)),
    };
    let count = text.line_count();
    if count == 0 {
        bail!("{} is empty.", display_path(path));
    }
    let line = definition.line().min(count - 1);
    let (extent_start, extent_end) = definition.location.extent;
    let start = extent_start.line.min(line).saturating_sub(PEEK_CONTEXT);
    let end = extent_end
        .line
        .max(line + PEEK_MIN_AFTER)
        .min(start + PEEK_MAX_LINES - 1)
        .min(count - 1);
    let syntax = text
        .language
        .and_then(|language| syntax::highlight(text.text(), language))
        .map(Arc::new);
    let place = definition
        .relative
        .clone()
        .unwrap_or_else(|| display_path(path));
    Ok(PeekContent {
        title: format!("{place}:{}", line + 1).into(),
        name: (line, definition.name_in(text.line(line))),
        lines: start..end + 1,
        text,
        syntax,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::lsp::Position;

    fn definition(path: &str, range: (u32, u32, u32), extent: (u32, u32)) -> Definition {
        let (line, start, end) = range;
        let at = |line, character| Position { line, character };
        Definition {
            location: Location {
                path: PathBuf::from(path),
                range: (at(line, start), at(line, end)),
                extent: (at(extent.0, 0), at(extent.1, 0)),
            },
            relative: None,
            encoding: PositionEncoding::Utf16,
        }
    }

    #[test]
    fn finds_the_name_on_its_line() {
        let found = definition("/a.rs", (0, 7, 10), (0, 0));
        assert_eq!(found.name_in("pub fn run() {}"), 7..10);
        // Past the end of a line that changed since the server read it.
        assert_eq!(found.name_in("fn"), 2..2);
    }

    #[test]
    fn relativizes_paths_inside_the_root() {
        let root = Path::new("/work/tree");
        assert_eq!(
            relative_path(Path::new("/work/tree/src/lib.rs"), root).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(relative_path(Path::new("/work/other.rs"), root), None);
        assert_eq!(relative_path(root, root), None);
    }

    #[test]
    fn peeks_at_the_lines_around_a_definition() {
        let dir = std::env::temp_dir().join(format!("docket-peek-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lib.rs");
        let source: String = (0..200).map(|n| format!("fn f{n}() {{}}\n")).collect();
        std::fs::write(&path, source).unwrap();
        let path = path.to_string_lossy().into_owned();

        // A definition the server gave the extent of.
        let peek = read_peek(&definition(&path, (20, 3, 6), (19, 40))).unwrap();
        assert_eq!(peek.lines, 18..41);
        assert_eq!(peek.name, (20, 3..6));
        assert!(peek.title.ends_with("lib.rs:21"));
        assert!(peek.syntax.is_some());

        // Just a name: some lines after it, but not past the end.
        let peek = read_peek(&definition(&path, (195, 3, 7), (195, 195))).unwrap();
        assert_eq!(peek.lines, 194..200);

        // A long definition is cut short.
        let peek = read_peek(&definition(&path, (0, 3, 5), (0, 199))).unwrap();
        assert_eq!(peek.lines, 0..PEEK_MAX_LINES);

        std::fs::remove_dir_all(&dir).ok();
    }
}
