//! Reviewing a pull request from its diff.
//!
//! Selecting lines shows a Comment button in the gutter of the last; it
//! opens a box under them to write a comment in (`Composer`), which is
//! left for the review (`state/pending_review.rs`) and shows under its
//! lines from then on, to edit or delete. Comments can span lines of one
//! hunk, as on GitHub.
//!
//! The toolbar above the diff, which stays put as it scrolls, has the
//! review's actions: Comment, Request Changes, and Approve. Each opens a
//! panel under it to add a summary and submit the review, with every
//! comment left. GitHub refuses approving, or requesting changes on, your
//! own pull request, so those are disabled there.
//!
//! The box is drawn over rows set aside for it (`COMPOSER_ROWS`), since
//! the diff's rows are all one height: from whichever of them is first on
//! screen, shifted up by how far into them that is.

use std::hash::{Hash as _, Hasher as _};

use gpui::{
    AnyElement, App, Context, Entity, Focusable as _, FontWeight, MouseButton, Subscription,
    Window, div, prelude::*, px,
};

use super::rows::{COMPOSER_ROWS, CommentRows, DisplayRow, Notes, line_ref};
use super::{DiffView, LINE_NUMBER_WIDTH, files};
use crate::Cancel;
use crate::diff::{LineKind, PullRequestDiff};
use crate::github::{DiffLineRef, DiffSide, ReviewComment, ReviewEvent};
use crate::state::{AppState, DiffLoad, PendingReview, Reviewable, line_label};
use crate::ui::code::{ClearSelection, CodeDefinition as _, ROW_HEIGHT};
use crate::ui::components::{
    Badge, Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconButton, IconName,
    IconSize, Spinner, TestSelector as _, Tooltip,
};
use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::ui::theme::{Color, Theme};

/// Where code starts in a line row, past both line numbers and the marker.
const CODE_LEFT: f32 = LINE_NUMBER_WIDTH * 2. + 16.;
/// How wide comments and the box they're written in are.
const COMMENT_WIDTH: f32 = 720.;
/// The lines of text the comment box shows.
const COMPOSER_LINES: usize = 4;

/// The lines a comment is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CommentTarget {
    pub file: usize,
    pub path: String,
    /// The last.
    pub line: DiffLineRef,
    /// The first, when there's more than one.
    pub start: Option<DiffLineRef>,
}

/// The box a comment is being written in.
pub(super) struct Composer {
    pub target: CommentTarget,
    /// The comment it changes, if it isn't a new one.
    pub editing: Option<u64>,
    pub input: Entity<TextInput>,
    _subscription: Subscription,
}

/// The panel that submits the review, opened from one of its actions.
pub(super) struct ReviewPanel {
    pub event: ReviewEvent,
    pub input: Entity<TextInput>,
    /// Sent, and waiting on GitHub.
    sent: bool,
    _subscription: Subscription,
}

impl DiffView {
    /// The open pull request, when the diff is its.
    pub(super) fn reviewable(&self, cx: &App) -> Option<Reviewable> {
        self.state.read(cx).reviewable(cx)
    }

    /// The review being written of the open pull request, if one is.
    fn pending_review<'a>(
        &self,
        reviewable: &Reviewable,
        cx: &'a App,
    ) -> Option<&'a PendingReview> {
        AppState::global(cx)
            .read(cx)
            .pending_review(&reviewable.pull_request)
    }

    /// What to show after the diff's lines: its review's comments, unless
    /// they're on another commit, and the comment box. With a hash of
    /// them, to tell when they change.
    pub(super) fn notes(&self, diff: &PullRequestDiff, cx: &App) -> (Notes, u64) {
        let mut notes = Notes::default();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(composer) = &self.composer {
            notes.composer = Some((composer.target.file, composer.target.line));
            (composer.target.file, composer.target.line).hash(&mut hasher);
        }
        let review = self
            .reviewable(cx)
            .and_then(|r| self.pending_review(&r, cx));
        let Some(review) = review else {
            return (notes, hasher.finish());
        };
        if review
            .commit_oid
            .as_ref()
            .is_some_and(|oid| *oid != diff.head_oid)
        {
            return (notes, hasher.finish());
        }
        let editing = self.composer.as_ref().and_then(|c| c.editing);
        for draft in &review.comments {
            if Some(draft.id) == editing {
                continue;
            }
            let comment = &draft.comment;
            let Some(file) = diff.files.iter().position(|f| f.path() == comment.path) else {
                continue;
            };
            let label = line_label(comment.line, comment.start);
            notes.add_comment(file, comment.line, draft.id, label, &comment.body);
            (draft.id, &comment.body, file, comment.line).hash(&mut hasher);
        }
        (notes, hasher.finish())
    }

    /// The lines selected, to comment on, and the row of the last: in one
    /// hunk, ending on the last selected, as GitHub allows.
    pub(super) fn comment_target(&self, cx: &App) -> Option<(usize, CommentTarget)> {
        if self.code.dragging || self.composer.is_some() {
            return None;
        }
        self.reviewable(cx)?;
        let selection = self.code.selection.as_ref().filter(|s| !s.is_empty())?;
        let (start, end) = selection.range();
        let rows = &self.rows.rows;
        // Ending at the start of a line selects up to the one before.
        let last = if end.offset == 0 && end.row > start.row {
            end.row - 1
        } else {
            end.row
        };
        let end_row = (start.row..=last.min(rows.len().checked_sub(1)?))
            .rev()
            .find(|&ix| matches!(rows[ix], DisplayRow::Line { .. }))?;
        let DisplayRow::Line { file, line } = rows[end_row] else {
            return None;
        };
        let end_ref = line_ref(line)?;
        // Back to the selection's start, within the hunk.
        let mut first = end_row;
        let mut ix = end_row;
        while ix > start.row {
            ix -= 1;
            match rows[ix] {
                DisplayRow::Line { file: f, .. } if f == file => first = ix,
                DisplayRow::Comment { .. } | DisplayRow::Composer { .. } => {}
                _ => break,
            }
        }
        // A range that ends on the base's side starts there too.
        let start_ref = (first..end_row).find_map(|ix| {
            let DisplayRow::Line { line, .. } = rows[ix] else {
                return None;
            };
            match (end_ref.side, line.kind) {
                (DiffSide::Left, LineKind::Added) => None,
                (DiffSide::Left, _) => Some(DiffLineRef {
                    side: DiffSide::Left,
                    line: line.old_line? + 1,
                }),
                (DiffSide::Right, _) => line_ref(line),
            }
        });
        let DiffLoad::Ready(diff) = &files(self.state.read(cx))?.diff else {
            return None;
        };
        let target = CommentTarget {
            file,
            path: diff.files.get(file)?.path().to_owned(),
            line: end_ref,
            start: start_ref,
        };
        Some((end_row, target))
    }

    /// Opens the box to comment on the selected lines.
    fn comment_on_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, target)) = self.comment_target(cx) else {
            return;
        };
        self.open_composer(target, None, "", window, cx);
    }

    fn open_composer(
        &mut self,
        target: CommentTarget,
        editing: Option<u64>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_popovers(cx);
        let input = cx.new(|cx| {
            let mut input = TextInput::new("Leave a comment", cx).multiline(COMPOSER_LINES);
            input.set_text(text, cx);
            input
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                TextInputEvent::Submit => this.save_comment(window, cx),
                TextInputEvent::Changed => cx.notify(),
            });
        window.focus(&input.read(cx).focus_handle(cx));
        self.composer = Some(Composer {
            target,
            editing,
            input,
            _subscription: subscription,
        });
        self.code.selection = None;
        cx.notify();
    }

    /// Leaves the comment in the box for the review.
    fn save_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(composer) = &self.composer else {
            return;
        };
        let body = composer.input.read(cx).text().trim_end().to_owned();
        if body.trim().is_empty() {
            return;
        }
        let Some(reviewable) = self.reviewable(cx) else {
            return;
        };
        let DiffLoad::Ready(diff) = &files(self.state.read(cx))
            .map(|f| &f.diff)
            .unwrap_or(&DiffLoad::Loading)
        else {
            return;
        };
        let head = diff.head_oid.clone();
        let target = composer.target.clone();
        let editing = composer.editing;
        let comment = ReviewComment {
            path: target.path,
            line: target.line,
            start: target.start,
            body,
        };
        AppState::global(cx).update(cx, |app, cx| {
            app.save_review_comment(&reviewable.pull_request, &head, editing, comment, cx);
        });
        self.close_composer(window, cx);
    }

    fn close_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.take().is_some() {
            window.focus(&self.focus_handle);
            cx.notify();
        }
    }

    /// Escape in the box: closes it if nothing's written, rather than
    /// losing what is, or backing out of the pull request.
    fn cancel_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let blank = self
            .composer
            .as_ref()
            .is_none_or(|c| c.input.read(cx).is_blank());
        if blank {
            self.close_composer(window, cx);
        }
    }

    fn edit_comment(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(reviewable) = self.reviewable(cx) else {
            return;
        };
        let Some(draft) = self
            .pending_review(&reviewable, cx)
            .and_then(|r| r.comments.iter().find(|c| c.id == id))
            .cloned()
        else {
            return;
        };
        let DiffLoad::Ready(diff) = &files(self.state.read(cx))
            .map(|f| &f.diff)
            .unwrap_or(&DiffLoad::Loading)
        else {
            return;
        };
        let Some(file) = diff
            .files
            .iter()
            .position(|f| f.path() == draft.comment.path)
        else {
            return;
        };
        let target = CommentTarget {
            file,
            path: draft.comment.path.clone(),
            line: draft.comment.line,
            start: draft.comment.start,
        };
        self.open_composer(target, Some(id), &draft.comment.body, window, cx);
    }

    fn delete_comment(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(reviewable) = self.reviewable(cx) else {
            return;
        };
        AppState::global(cx).update(cx, |app, cx| {
            app.remove_review_comment(&reviewable.pull_request, id, cx);
        });
    }

    /// Opens the panel to submit the review as `event`.
    fn open_review_panel(
        &mut self,
        event: ReviewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = &mut self.review_panel {
            panel.event = event;
            window.focus(&panel.input.read(cx).focus_handle(cx));
            cx.notify();
            return;
        }
        let input = cx.new(|cx| TextInput::new("Leave a summary", cx).multiline(3));
        let subscription = cx.subscribe_in(&input, window, |this, _, event, _, cx| match event {
            TextInputEvent::Submit => this.submit_review(cx),
            TextInputEvent::Changed => cx.notify(),
        });
        window.focus(&input.read(cx).focus_handle(cx));
        self.review_panel = Some(ReviewPanel {
            event,
            input,
            sent: false,
            _subscription: subscription,
        });
        if let Some(reviewable) = self.reviewable(cx) {
            AppState::global(cx).update(cx, |app, cx| {
                app.dismiss_review_error(&reviewable.pull_request, cx);
            });
        }
        cx.notify();
    }

    fn close_review_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review_panel.take().is_some() {
            window.focus(&self.focus_handle);
            cx.notify();
        }
    }

    fn submit_review(&mut self, cx: &mut Context<Self>) {
        let Some(reviewable) = self.reviewable(cx) else {
            return;
        };
        let Some(panel) = &mut self.review_panel else {
            return;
        };
        let body = panel.input.read(cx).text().trim().to_owned();
        let review = AppState::global(cx)
            .read(cx)
            .pending_review(&reviewable.pull_request);
        let needs_summary = review.is_none_or(|r| r.needs_summary(panel.event));
        let busy = review.is_some_and(|r| r.submitting.is_some());
        let refused = reviewable.is_own && panel.event != ReviewEvent::Comment;
        if (needs_summary && body.is_empty()) || busy || refused {
            return;
        }
        panel.sent = true;
        let event = panel.event;
        AppState::global(cx).update(cx, |app, cx| {
            app.submit_review(
                &reviewable.pull_request,
                Some(reviewable.head_oid.clone()),
                event,
                body,
                cx,
            );
        });
        cx.notify();
    }

    /// Closes the panel once the review it sent is submitted.
    pub(super) fn sync_review_panel(&mut self, cx: &App) {
        let Some(panel) = &mut self.review_panel else {
            return;
        };
        if !panel.sent {
            return;
        }
        let review = self.state.read(cx).reviewable(cx).and_then(|r| {
            AppState::global(cx)
                .read(cx)
                .pending_review(&r.pull_request)
        });
        match review {
            Some(review) if review.submitting.is_some() => {}
            Some(review) if review.error.is_some() => panel.sent = false,
            _ => self.review_panel = None,
        }
    }

    /// The review's actions, for the toolbar's end.
    pub(super) fn render_review_actions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let reviewable = self.reviewable(cx)?;
        let review = self.pending_review(&reviewable, cx);
        let theme = Theme::get(cx);
        let view = cx.entity();
        let submitting = review.and_then(|r| r.submitting);
        let pending = review.map_or(0, |r| r.comments.len());
        let action = |id: &'static str, label: &'static str, icon, event, style| {
            let view = view.clone();
            let refused = reviewable.is_own && event != ReviewEvent::Comment;
            let open = self.review_panel.as_ref().is_some_and(|p| p.event == event);
            Button::new(id, label)
                .icon(icon)
                .size(ButtonSize::Compact)
                .style(if open { ButtonStyle::Primary } else { style })
                .disabled(refused || submitting.is_some())
                .on_click(move |_, window, cx| {
                    view.update(cx, |view, cx| view.open_review_panel(event, window, cx));
                })
        };
        let status = match (submitting, review.and_then(|r| r.submitted)) {
            (Some(_), _) => Some(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(Spinner::new("diff-review-submitting"))
                    .child("Submitting review…")
                    .into_any_element(),
            ),
            (None, Some(event)) if pending == 0 => Some(
                div()
                    .test_selector(|| "diff-review-submitted".into())
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(theme.success)
                    .child(
                        Icon::new(IconName::CircleCheck)
                            .size(IconSize::XSmall)
                            .color(Color::Success),
                    )
                    .child(match event {
                        ReviewEvent::Approve => "Approved",
                        ReviewEvent::RequestChanges => "Requested changes",
                        ReviewEvent::Comment => "Review submitted",
                    })
                    .into_any_element(),
            ),
            _ => None,
        };
        let pending_label = (pending > 0 && submitting.is_none()).then(|| {
            div()
                .test_selector(|| "diff-review-pending".into())
                .px_1()
                .child(match pending {
                    1 => "1 pending comment".to_owned(),
                    n => format!("{n} pending comments"),
                })
        });
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .children(status)
                .children(pending_label)
                .child(action(
                    "diff-review-comment",
                    "Comment",
                    IconName::MessageSquare,
                    ReviewEvent::Comment,
                    ButtonStyle::Ghost,
                ))
                .child(action(
                    "diff-review-request-changes",
                    "Request Changes",
                    IconName::MessageSquareWarning,
                    ReviewEvent::RequestChanges,
                    ButtonStyle::Ghost,
                ))
                .child(action(
                    "diff-review-approve",
                    "Approve",
                    IconName::Check,
                    ReviewEvent::Approve,
                    ButtonStyle::Secondary,
                ))
                .into_any_element(),
        )
    }

    /// The panel that submits the review, under the toolbar.
    pub(super) fn render_review_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.review_panel.as_ref()?;
        let reviewable = self.reviewable(cx)?;
        let review = self.pending_review(&reviewable, cx);
        let theme = Theme::get(cx);
        let view = cx.entity();
        let event = panel.event;
        let comments = review.map_or(0, |r| r.comments.len());
        let needs_summary = review.is_none_or(|r| r.needs_summary(event));
        let submitting = review.is_some_and(|r| r.submitting.is_some());
        let blank = panel.input.read(cx).is_blank();
        let (title, submit) = match event {
            ReviewEvent::Approve => ("Approve", "Approve"),
            ReviewEvent::RequestChanges => ("Request changes", "Request Changes"),
            ReviewEvent::Comment => ("Comment", "Submit Review"),
        };
        let detail = match (comments, needs_summary) {
            (0, true) => "Say what you think: GitHub needs a summary to submit this.".to_owned(),
            (0, false) => "Submits your review with an optional summary.".to_owned(),
            (1, _) => "Submits your review with your comment.".to_owned(),
            (n, _) => format!("Submits your review with your {n} comments."),
        };
        let error = review.and_then(|r| r.error.clone()).map(|error| {
            Callout::new(CalloutKind::Error, error).title("Couldn't submit the review")
        });
        let (cancel, send) = (view.clone(), view);
        Some(
            div()
                .id("diff-review-panel")
                .test_selector(|| "diff-review-panel".into())
                .flex()
                .flex_none()
                .flex_col()
                .gap_2()
                .p_3()
                .border_b_1()
                .border_color(theme.border_subtle)
                .bg(theme.surface)
                .text_sm()
                .line_height(px(20.))
                // Not the diff's, which would take focus from the summary.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                    this.close_review_panel(window, cx);
                }))
                .on_action(cx.listener(|this, _: &ClearSelection, window, cx| {
                    this.close_review_panel(window, cx);
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(title),
                        )
                        .child(div().text_xs().text_color(theme.text_muted).child(detail)),
                )
                .child(div().max_w(px(COMMENT_WIDTH)).child(panel.input.clone()))
                .children(error)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("diff-review-submit", submit)
                                .size(ButtonSize::Compact)
                                .disabled(submitting || (needs_summary && blank))
                                .on_click(move |_, _, cx| {
                                    send.update(cx, |view, cx| view.submit_review(cx));
                                }),
                        )
                        .child(
                            Button::new("diff-review-cancel", "Cancel")
                                .style(ButtonStyle::Ghost)
                                .size(ButtonSize::Compact)
                                .on_click(move |_, window, cx| {
                                    cancel
                                        .update(cx, |view, cx| view.close_review_panel(window, cx));
                                }),
                        )
                        .children(submitting.then(|| Spinner::new("diff-review-panel-spinner")))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.text_subtle)
                                .child("⌘↩ to submit"),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The comment box, over the rows set aside for it, from `line` of
    /// them: the first on screen.
    pub(super) fn render_composer(&self, line: usize, cx: &mut Context<Self>) -> AnyElement {
        let row = div().relative().h(px(ROW_HEIGHT)).w_full();
        let Some(composer) = &self.composer else {
            return row.into_any_element();
        };
        let theme = Theme::get(cx);
        let target = &composer.target;
        let blank = composer.input.read(cx).is_blank();
        let view = cx.entity();
        let (cancel, save) = (view.clone(), view);
        let card = div()
            .id("diff-composer")
            .test_selector(|| "diff-composer".into())
            .absolute()
            .top(px(4. - ROW_HEIGHT * line as f32))
            .left(px(CODE_LEFT))
            .w(px(COMMENT_WIDTH))
            .h(px(ROW_HEIGHT * COMPOSER_ROWS as f32 - 8.))
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .text_sm()
            .line_height(px(20.))
            .whitespace_normal()
            .cursor_default()
            // Not the diff's: that would select code, and take focus.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.cancel_composer(window, cx)))
            .on_action(
                cx.listener(|this, _: &ClearSelection, window, cx| {
                    this.cancel_composer(window, cx)
                }),
            )
            .child(div().text_xs().text_color(theme.text_muted).child(format!(
                "{} on {}",
                if composer.editing.is_some() {
                    "Edit comment"
                } else {
                    "Comment"
                },
                line_label(target.line, target.start).to_lowercase()
            )))
            .child(composer.input.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new(
                            "diff-composer-save",
                            if composer.editing.is_some() {
                                "Update Comment"
                            } else {
                                "Add Comment"
                            },
                        )
                        .size(ButtonSize::Compact)
                        .disabled(blank)
                        .on_click(move |_, window, cx| {
                            save.update(cx, |view, cx| view.save_comment(window, cx));
                        }),
                    )
                    .child(
                        Button::new("diff-composer-cancel", "Cancel")
                            .style(ButtonStyle::Ghost)
                            .size(ButtonSize::Compact)
                            .on_click(move |_, window, cx| {
                                cancel.update(cx, |view, cx| view.close_composer(window, cx));
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.text_subtle)
                            .child("⌘↩ to add"),
                    ),
            );
        row.child(card).into_any_element()
    }
}

/// The Comment button in the gutter of the last selected line.
pub(super) fn comment_button(view: &Entity<DiffView>, theme: &Theme) -> AnyElement {
    let view = view.clone();
    div()
        .absolute()
        .top(px(2.))
        .left(px(4.))
        .h(px(ROW_HEIGHT - 4.))
        .child(
            div()
                .id("diff-comment")
                .test_selector(|| "diff-comment".into())
                .flex()
                .items_center()
                .gap_1()
                .h_full()
                .px_1p5()
                .rounded_sm()
                .bg(theme.accent)
                .hover(|style| style.bg(theme.accent_hover))
                .text_color(theme.text_on_accent)
                .text_xs()
                .cursor_pointer()
                .child(
                    Icon::new(IconName::MessageSquare)
                        .size(IconSize::XSmall)
                        .color(Color::OnAccent),
                )
                .child("Comment")
                .tooltip(|_, cx| Tooltip::view("Comment on the selected lines".into(), cx))
                // Not the diff's, which would select where it's pressed.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, window, cx| {
                    view.update(cx, |view, cx| view.comment_on_selection(window, cx));
                }),
        )
        .into_any_element()
}

/// A row of a comment left for the review: its header, or a line of it.
pub(super) fn comment_row(
    base: gpui::Div,
    comment: &CommentRows,
    line: usize,
    view: &Entity<DiffView>,
    theme: &Theme,
) -> AnyElement {
    let last = line == comment.lines.len();
    let card = div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .w(px(COMMENT_WIDTH))
        .h_full()
        .px_3()
        .bg(theme.surface)
        .border_l_2()
        .border_r_1()
        .border_color(theme.border_subtle)
        .when(line == 0, |el| el.border_t_1().rounded_t_md())
        .when(last, |el| el.border_b_1().rounded_b_md())
        .text_xs();
    let card = if line == 0 {
        let id = comment.id;
        let (edit, delete) = (view.clone(), view.clone());
        card.test_selector(|| format!("diff-comment-{id}"))
            .child(
                Icon::new(IconName::MessageSquare)
                    .size(IconSize::XSmall)
                    .color(Color::Muted),
            )
            .child(Badge::new("Pending").color(Color::Warning))
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child(comment.label.clone()),
            )
            .child(
                IconButton::new(
                    gpui::ElementId::Name(format!("diff-comment-edit-{id}").into()),
                    IconName::FilePen,
                )
                .tooltip("Edit")
                .size(ButtonSize::Compact)
                .on_click(move |_, window, cx| {
                    edit.update(cx, |view, cx| view.edit_comment(id, window, cx));
                }),
            )
            .child(
                IconButton::new(
                    gpui::ElementId::Name(format!("diff-comment-delete-{id}").into()),
                    IconName::Trash,
                )
                .tooltip("Delete")
                .size(ButtonSize::Compact)
                .on_click(move |_, _, cx| {
                    delete.update(cx, |view, cx| view.delete_comment(id, cx));
                }),
            )
    } else {
        card.text_color(theme.text)
            .child(comment.lines[line - 1].clone())
    };
    base.pl(px(CODE_LEFT)).child(card).into_any_element()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{
        Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext,
    };

    use super::super::tests::{commit_on_feature, tip};
    use crate::github::{
        DiffLineRef, DiffSide, FakeGithub, PullRequest, ReviewComment, ReviewEvent, SubmittedReview,
    };
    use crate::state::{AppState, SectionKind};
    use crate::test_support::{
        VisualTestContextExt as _, actor, detail, git_checkout, open_docket, project, pull_request,
    };

    /// A checkout whose `feature` changes `a.txt`, and a pull request of
    /// it by `author`.
    fn reviewing(author: &str) -> (tempfile::TempDir, Arc<FakeGithub>, PullRequest) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_checkout(root, &[("a.txt", "one\ntwo\nthree\n")]);
        commit_on_feature(
            root,
            &[("a.txt", "one\nTWO\nthree\nfour\n")],
            "2026-01-02T00:00:00Z",
        );
        let mut pr = pull_request("acme/widgets", 7, "Shout");
        pr.base_ref_oid = tip(root, "main");
        pr.head_ref_oid = tip(root, "feature");
        pr.author = Some(actor(author));
        let github = Arc::new(FakeGithub::new());
        github.set_search(
            SectionKind::ReviewRequested.search_query(),
            vec![pr.clone()],
        );
        github.set_detail(pr.repository.parse().unwrap(), detail(&pr));
        (dir, github, pr)
    }

    /// Selects from the end of one line row to the end of another.
    fn select(cx: &mut VisualTestContext, from: &str, to: &str) {
        let from = cx.bounds_of(from).unwrap().center();
        let to = cx.bounds_of(to).unwrap().center();
        cx.simulate_click(from, Modifiers::none());
        cx.simulate_click(to, Modifiers::shift());
        cx.run_until_parked();
    }

    /// Selects a line row by triple-clicking it.
    fn select_line(cx: &mut VisualTestContext, row: &str) {
        let at = cx.bounds_of(row).unwrap().center();
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::none(),
            click_count: 3,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::none(),
            click_count: 3,
        });
        cx.run_until_parked();
    }

    fn comments(pr: &PullRequest, cx: &mut VisualTestContext) -> Vec<ReviewComment> {
        cx.read(|cx| {
            AppState::global(cx)
                .read(cx)
                .pending_review(&pr.id)
                .map(|r| r.comments.iter().map(|c| c.comment.clone()).collect())
                .unwrap_or_default()
        })
    }

    fn right(line: u32) -> DiffLineRef {
        DiffLineRef {
            side: DiffSide::Right,
            line,
        }
    }

    #[gpui::test]
    fn comments_on_lines_and_approves(cx: &mut TestAppContext) {
        let (dir, github, pr) = reviewing("hubot");
        let projects = vec![project(1, dir.path(), "acme/widgets")];
        let (_tab, cx) = open_docket(cx, github.clone(), projects);
        cx.click_on(&pr.id);
        cx.click_on("pr-tab-files");
        assert!(cx.is_drawn("diff-review-approve"));
        assert!(!cx.is_drawn("diff-comment"), "nothing's selected");

        // Selecting lines offers to comment on them.
        select(cx, "diff-line-0-R2", "diff-line-0-R4");
        cx.click_on("diff-comment");
        assert!(cx.is_drawn("diff-composer"));
        assert!(!cx.is_drawn("diff-comment"));
        cx.simulate_input("Why shout?");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("Quietly.");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert!(!cx.is_drawn("diff-composer"));
        let comment = ReviewComment {
            path: "a.txt".into(),
            line: right(4),
            start: Some(right(2)),
            body: "Why shout?\nQuietly.".into(),
        };
        assert_eq!(comments(&pr, cx), std::slice::from_ref(&comment));
        assert!(cx.is_drawn("diff-comment-1"));
        assert!(cx.is_drawn("diff-review-pending"));

        // It can be edited where it shows.
        cx.click_on("diff-comment-edit-1");
        assert!(cx.is_drawn("diff-composer"));
        assert!(!cx.is_drawn("diff-comment-1"));
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("Why shout here?");
        cx.click_on("diff-composer-save");
        let edited = ReviewComment {
            body: "Why shout here?".into(),
            ..comment
        };
        assert_eq!(comments(&pr, cx), std::slice::from_ref(&edited));

        // And a single line commented on, then deleted.
        select_line(cx, "diff-line-0-L2");
        cx.click_on("diff-comment");
        cx.simulate_input("Gone");
        cx.click_on("diff-composer-save");
        assert_eq!(comments(&pr, cx).len(), 2);
        assert_eq!(comments(&pr, cx)[1].start, None);
        cx.click_on("diff-comment-delete-2");
        assert_eq!(comments(&pr, cx), std::slice::from_ref(&edited));

        // Approving submits the review with its comments.
        cx.click_on("diff-review-approve");
        assert!(cx.is_drawn("diff-review-panel"));
        cx.simulate_input("Nice.");
        cx.click_on("diff-review-submit");
        assert_eq!(
            github.reviews(),
            [SubmittedReview {
                pull_request_id: pr.id.clone(),
                commit_oid: Some(pr.head_ref_oid.clone()),
                event: ReviewEvent::Approve,
                body: "Nice.".into(),
                comments: vec![edited],
            }]
        );
        assert!(!cx.is_drawn("diff-review-panel"));
        assert!(cx.is_drawn("diff-review-submitted"));
        assert!(!cx.is_drawn("diff-comment-1"));
        assert!(comments(&pr, cx).is_empty());
    }

    #[gpui::test]
    fn your_own_pull_requests_only_take_comments(cx: &mut TestAppContext) {
        // The fake's viewer.
        let (dir, github, pr) = reviewing("octocat");
        let projects = vec![project(1, dir.path(), "acme/widgets")];
        let (tab, cx) = open_docket(cx, github.clone(), projects);
        cx.click_on(&pr.id);
        cx.click_on("pr-tab-files");
        cx.click_on("diff-review-approve");
        assert!(!cx.is_drawn("diff-review-panel"));

        // Escape closes an empty comment box, and nothing else.
        select_line(cx, "diff-line-0-R2");
        cx.click_on("diff-comment");
        assert!(cx.is_drawn("diff-composer"));
        cx.simulate_keystrokes("escape");
        assert!(!cx.is_drawn("diff-composer"));
        assert!(tab.read_with(cx, |tab, _| tab.detail.is_some()));

        // Commenting with no comments needs a summary.
        cx.click_on("diff-review-comment");
        cx.click_on("diff-review-submit");
        assert!(github.reviews().is_empty());

        // A review GitHub refuses stays, saying why.
        github.fail_reviews(Some("Unprocessable"));
        cx.simulate_input("Thoughts");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert!(github.reviews().is_empty());
        assert!(cx.is_drawn("diff-review-panel"));
        let error = cx.read(|cx| {
            AppState::global(cx)
                .read(cx)
                .pending_review(&pr.id)
                .and_then(|r| r.error.clone())
        });
        assert!(error.is_some_and(|e| e.contains("Unprocessable")));

        github.fail_reviews(None);
        cx.click_on("diff-review-submit");
        let reviews = github.reviews();
        assert_eq!(reviews.len(), 1);
        assert_eq!(
            (reviews[0].event, reviews[0].body.as_str()),
            (ReviewEvent::Comment, "Thoughts")
        );
        assert!(!cx.is_drawn("diff-review-panel"));
    }
}
