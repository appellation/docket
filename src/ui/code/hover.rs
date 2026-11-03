//! Language server hover in a code view. Resting the mouse on code for a
//! moment asks the file's server about the symbol there, and the answer
//! shows in a popover above the line.
//!
//! The view says what can be asked about (`CodeView::hover_request`): in
//! the diff, lines on the new side and the scope names in hunk headers; in
//! the file browser, any line of the open file.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures::future::{Either, select};
use gpui::{
    AnyElement, Context, Corner, Pixels, Point, SharedString, Task, anchored, deferred, div, point,
    prelude::*, px,
};

use super::{CodeView, ROW_HEIGHT};
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
pub struct HoverState {
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
pub enum Ask {
    /// A source offset on a line of the target's text.
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

/// Hover for every [`CodeView`].
pub trait CodeHover: CodeView {
    /// Tracks the mouse over the rows: keeps a shown hover while the mouse
    /// is on its symbol, and asks about a new symbol once the mouse rests.
    fn update_hover(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let target = usize::try_from(self.row_at(position.y))
            .ok()
            .and_then(|row| Some((row, self.hover_word_at(row, position.x, cx)?)));
        let on_symbol = self.code().hover.shown.as_ref().map(|shown| {
            target
                .as_ref()
                .is_some_and(|(row, (offset, _))| *row == shown.row && shown.range.contains(offset))
        });
        match on_symbol {
            Some(true) => {
                self.code_mut().hover.hide_pending = false;
                return;
            }
            Some(false) => self.schedule_hide(cx),
            None => {}
        }

        let hover = &mut self.code_mut().hover;
        let Some((row, (offset, word))) = target else {
            hover.candidate = None;
            hover._request_task = None;
            return;
        };
        let candidate = (row, word);
        if hover.candidate.as_ref() == Some(&candidate) {
            return;
        }
        hover.candidate = Some(candidate.clone());
        hover._request_task = Some(cx.spawn(async move |this, cx| {
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

    /// Sends a hover request for `offset` in `row`, if its file has a
    /// running server. Also returns the server's name if it's still
    /// loading, since its answer may be missing or incomplete.
    fn request_hover(
        &mut self,
        row: usize,
        offset: usize,
        cx: &mut Context<Self>,
    ) -> Option<(Task<Answer>, Option<&'static str>)> {
        let (target, ask) = self.hover_request(row, offset, cx)?;
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
        if self.code().hover.candidate.as_ref() != Some(&(row, word.clone())) {
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
        let Some(x) = self.hover_x(row, range.start) else {
            return;
        };
        let origin = point(x, self.row_top(row));
        let hover = &mut self.code_mut().hover;
        hover.shown = Some(ShownHover {
            row,
            origin,
            range,
            content,
        });
        hover.hide_pending = false;
        cx.notify();
    }

    /// Hides the hover soon, unless the mouse comes back to its symbol or
    /// onto the popover first.
    fn schedule_hide(&mut self, cx: &mut Context<Self>) {
        let hover = &mut self.code_mut().hover;
        if hover.shown.is_none() || hover.hide_pending {
            return;
        }
        hover.hide_pending = true;
        hover._hide_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIDE_DELAY).await;
            this.update(cx, |this, cx| {
                let hover = &mut this.code_mut().hover;
                if hover.hide_pending && !hover.popover_hovered {
                    hover.shown = None;
                    cx.notify();
                }
                hover.hide_pending = false;
            })
            .ok();
        }));
    }

    /// Hides the hover now and forgets the symbol under the mouse, e.g. on
    /// scroll or click.
    fn dismiss_hover(&mut self, cx: &mut Context<Self>) {
        let hover = &mut self.code_mut().hover;
        let shown = hover.shown.is_some();
        *hover = HoverState::default();
        if shown {
            cx.notify();
        }
    }

    /// The mouse left the rows: stop waiting to ask, and hide the hover
    /// unless it went onto the popover.
    fn mouse_left(&mut self, cx: &mut Context<Self>) {
        let hover = &mut self.code_mut().hover;
        hover.candidate = None;
        hover._request_task = None;
        self.schedule_hide(cx);
    }

    fn set_popover_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.code_mut().hover.popover_hovered = hovered;
        if !hovered {
            self.schedule_hide(cx);
        }
    }

    fn render_hover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let shown = self.code().hover.shown.as_ref()?;
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
                .child(Markdown::new("code-hover-docs", docs.clone()))
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
            .id("code-hover")
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

impl<V: CodeView> CodeHover for V {}
