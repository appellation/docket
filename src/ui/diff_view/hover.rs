//! Language server hover in the diff. Resting the mouse on code for a
//! moment asks the file's server about the symbol there, and the answer
//! shows in a popover above the line.
//!
//! Only lines on the new side (context and added) can be asked about: the
//! server runs in a worktree at the PR's head, where removed lines don't
//! exist. The scope names in hunk headers can be too, about where each is
//! declared.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures::future::{Either, select};
use gpui::{
    AnyElement, Context, Corner, Pixels, Point, SharedString, Task, anchored, deferred, div, point,
    prelude::*, px,
};

use super::rows::DisplayRow;
use super::selection::word_range;
use super::{DiffView, ROW_HEIGHT, files};
use crate::diff::LineKind;
use crate::lsp::Position;
use crate::markdown::Document;
use crate::ui::components::Markdown;
use crate::ui::theme::Theme;

/// How long the mouse rests on a symbol before asking about it.
const HOVER_DELAY: Duration = Duration::from_millis(350);
/// Grace period for moving from the symbol onto the popover.
const HIDE_DELAY: Duration = Duration::from_millis(200);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WIDTH: f32 = 560.;
const MAX_HEIGHT: f32 = 320.;

#[derive(Default)]
pub(super) struct HoverState {
    /// The word under the mouse, by row and source range, while waiting
    /// to ask about it or for the answer.
    candidate: Option<(usize, Range<usize>)>,
    shown: Option<ShownHover>,
    popover_hovered: bool,
    hide_pending: bool,
    _request_task: Option<Task<()>>,
    _hide_task: Option<Task<()>>,
}

struct ShownHover {
    row: usize,
    /// The source range it describes: the server's, or else the word.
    range: Range<usize>,
    /// Window position of the range's start, at the top of its row.
    origin: Point<Pixels>,
    content: HoverContent,
}

enum HoverContent {
    /// The answer, and a caveat if the server was still loading.
    Docs(Arc<Document>, Option<SharedString>),
    /// No answer, and why it may come later.
    Note(SharedString),
}

/// Where to ask a server about.
#[derive(Clone, Copy)]
enum Ask {
    /// A source offset on a new-side line.
    Code { line: u32, offset: usize },
    /// A scope, where its name is declared.
    Declaration(Position),
}

/// A hover answer, parsed off the UI thread: the docs and the source range
/// they describe, if the server gave one on this line.
type Answer = Result<Option<(Arc<Document>, Option<Range<usize>>)>>;

impl HoverState {
    pub fn is_shown(&self) -> bool {
        self.shown.is_some()
    }

    /// The hovered symbol to mark: its row and source range.
    pub fn marked(&self) -> Option<(usize, Range<usize>)> {
        let shown = self.shown.as_ref()?;
        matches!(shown.content, HoverContent::Docs(..)).then(|| (shown.row, shown.range.clone()))
    }
}

impl DiffView {
    /// Tracks the mouse over the rows: keeps a shown hover while the mouse
    /// is on its symbol, and asks about a new symbol once the mouse rests.
    pub(super) fn update_hover(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let target = self.hover_target_at(position, cx);
        if let Some(shown) = &self.hover.shown {
            let on_symbol = target
                .as_ref()
                .is_some_and(|(row, offset, _)| *row == shown.row && shown.range.contains(offset));
            if on_symbol {
                self.hover.hide_pending = false;
                return;
            }
            self.schedule_hide(cx);
        }

        let Some((row, offset, word)) = target else {
            self.hover.candidate = None;
            self.hover._request_task = None;
            return;
        };
        let candidate = (row, word);
        if self.hover.candidate.as_ref() == Some(&candidate) {
            return;
        }
        self.hover.candidate = Some(candidate.clone());
        self.hover._request_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            let Ok(Some((request, loading))) =
                this.update(cx, |this, cx| this.request_hover(row, offset, cx))
            else {
                return;
            };
            let answer = request.await;
            this.update(cx, |this, cx| {
                this.show_hover(candidate, answer, loading, cx)
            })
            .ok();
        }));
    }

    /// The row, source offset, and word under `position`, if it's code on
    /// the new side that isn't whitespace, or a scope name in a hunk header
    /// (offset into its scope text, and the whole name).
    fn hover_target_at(
        &self,
        position: Point<Pixels>,
        cx: &gpui::App,
    ) -> Option<(usize, usize, Range<usize>)> {
        let row = usize::try_from(self.row_at(position.y)).ok()?;
        let line = match self.rows.rows.get(row)? {
            DisplayRow::Line { line, .. } => line,
            DisplayRow::HunkHeader { .. } => {
                let scope = self.scope_hits.get(&row)?.scope_under(position.x)?;
                return Some((row, scope.range.start, scope.range.clone()));
            }
            _ => return None,
        };
        if line.kind == LineKind::Removed {
            return None;
        }
        let offset = self.line_hits.get(&row)?.offset_under(position.x)?;
        let text = self.line_text(row, cx)?;
        if text[offset..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
        {
            return None;
        }
        Some((row, offset, word_range(text, offset)))
    }

    /// Sends a hover request for `offset` in `row`, if its file has a
    /// running server. Also returns the server's name if it's still
    /// loading, since its answer may be missing or incomplete.
    fn request_hover(
        &mut self,
        row: usize,
        offset: usize,
        cx: &mut Context<Self>,
    ) -> Option<(Task<Answer>, Option<&'static str>)> {
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
        let target = files(self.state.read(cx))?.hover_target(file)?;
        let loading = target.busy.then_some(target.server.spec.name);
        let background = cx.background_executor().clone();
        let request = cx.background_executor().spawn(async move {
            let encoding = target.server.encoding;
            let position = match ask {
                Ask::Code { line, offset } => Position {
                    line,
                    character: encoding.character(target.text.line(line), offset),
                },
                Ask::Declaration(position) => position,
            };
            target
                .server
                .open(&target.path, target.language_id, target.text.text())?;
            let hover = target.server.hover(&target.path, position);
            let timeout = background.timer(REQUEST_TIMEOUT);
            let hover = match select(Box::pin(hover), timeout).await {
                Either::Left((hover, _)) => hover?,
                Either::Right(_) => return Err(anyhow!("no answer within 10 seconds")),
            };
            Ok(hover.map(|hover| {
                // A scope name keeps its whole name marked.
                let range = hover.range.zip(match ask {
                    Ask::Code { line, offset } => Some((line, offset)),
                    Ask::Declaration(_) => None,
                });
                let range = range.and_then(|((start, end), (number, offset))| {
                    let line = target.text.line(number);
                    let range = encoding.offset(line, start.character)
                        ..encoding.offset(line, end.character);
                    (start.line == number && end.line == number && range.contains(&offset))
                        .then_some(range)
                });
                (Arc::new(Document::parse(&hover.markdown)), range)
            }))
        });
        Some((request, loading))
    }

    fn show_hover(
        &mut self,
        (row, word): (usize, Range<usize>),
        answer: Answer,
        loading: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        // The mouse moved on while waiting.
        if self.hover.candidate.as_ref() != Some(&(row, word.clone())) {
            return;
        }
        let (content, range) = match (answer, loading) {
            // Answered from a partial view of the project, so imports may
            // show as `any`.
            (Ok(Some((docs, range))), loading) => {
                let caveat = loading.map(|name| {
                    format!("{name} is still loading this project, so types may be incomplete.")
                        .into()
                });
                (HoverContent::Docs(docs, caveat), range.unwrap_or(word))
            }
            // Servers answer with nothing, or an error, until they've loaded.
            (Ok(None) | Err(_), Some(name)) => (
                HoverContent::Note(format!("{name} is still loading this project…").into()),
                word,
            ),
            (Ok(None), None) => return,
            (Err(err), None) => {
                eprintln!("hover failed: {err:#}");
                return;
            }
        };
        let x = match (self.line_hits.get(&row), self.scope_hits.get(&row)) {
            (Some(hit), _) => hit.x_for_offset(range.start),
            (None, Some(hit)) => hit.x_for_offset(range.start),
            (None, None) => return,
        };
        self.hover.shown = Some(ShownHover {
            row,
            origin: point(x, self.row_top(row)),
            range,
            content,
        });
        self.hover.hide_pending = false;
        cx.notify();
    }

    /// Hides the hover soon, unless the mouse comes back to its symbol or
    /// onto the popover first.
    fn schedule_hide(&mut self, cx: &mut Context<Self>) {
        if self.hover.shown.is_none() || self.hover.hide_pending {
            return;
        }
        self.hover.hide_pending = true;
        self.hover._hide_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIDE_DELAY).await;
            this.update(cx, |this, cx| {
                if this.hover.hide_pending && !this.hover.popover_hovered {
                    this.hover.shown = None;
                    cx.notify();
                }
                this.hover.hide_pending = false;
            })
            .ok();
        }));
    }

    /// Hides the hover now and forgets the symbol under the mouse, e.g. on
    /// scroll or click.
    pub(super) fn dismiss_hover(&mut self, cx: &mut Context<Self>) {
        let shown = self.hover.shown.is_some();
        self.hover = HoverState::default();
        if shown {
            cx.notify();
        }
    }

    /// The mouse left the rows: stop waiting to ask, and hide the hover
    /// unless it went onto the popover.
    pub(super) fn mouse_left(&mut self, cx: &mut Context<Self>) {
        self.hover.candidate = None;
        self.hover._request_task = None;
        self.schedule_hide(cx);
    }

    fn set_popover_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.hover.popover_hovered = hovered;
        if !hovered {
            self.schedule_hide(cx);
        }
    }

    pub(super) fn render_hover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let shown = self.hover.shown.as_ref()?;
        let theme = Theme::get(cx);
        let view = cx.entity();
        // Above the line, unless that's too close to the top of the window.
        let (corner, position) = if shown.origin.y > px(MAX_HEIGHT * 0.75) {
            (Corner::BottomLeft, shown.origin)
        } else {
            (
                Corner::TopLeft,
                point(shown.origin.x, shown.origin.y + px(ROW_HEIGHT)),
            )
        };
        let content = match &shown.content {
            HoverContent::Docs(docs, caveat) => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Markdown::new("diff-hover-docs", docs.clone()))
                .when_some(caveat.clone(), |el, caveat| {
                    el.child(
                        div()
                            .pt_2()
                            .border_t_1()
                            .border_color(theme.border_subtle)
                            .text_color(theme.text_muted)
                            .child(caveat),
                    )
                })
                .into_any_element(),
            HoverContent::Note(note) => div()
                .text_color(theme.text_muted)
                .child(note.clone())
                .into_any_element(),
        };
        let popover = div()
            .id("diff-hover")
            .occlude()
            .max_w(px(MAX_WIDTH))
            .max_h(px(MAX_HEIGHT))
            .overflow_y_scroll()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow_lg()
            .text_xs()
            .on_hover(move |hovered, _, cx| {
                view.update(cx, |view, cx| view.set_popover_hovered(*hovered, cx));
            })
            .child(content);
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
