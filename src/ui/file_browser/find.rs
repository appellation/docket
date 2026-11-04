//! Find in the open file: a bar above its code, its matches marked in the
//! lines, and one of them current.
//!
//! ⌘F opens the bar (with a one-line selection as the query), and Enter
//! and ⌘G move to the next match, Shift-Enter and ⌘⇧G to the previous,
//! wrapping at the ends. The query matches as the tree's text search does
//! (`projects::search`): literally, ignoring case unless it has a capital,
//! or as a regex. Matches are found on the background executor, again
//! whenever the query or the file's text changes, so the bar stays open as
//! files are opened; the first is the first at or after where the view
//! is. Escape closes the bar, and the marks with it.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, Focusable as _, KeyBinding, MouseButton, SharedString, Task,
    Window, actions, div, prelude::*, px,
};

use super::FileBrowserView;
use crate::Cancel;
use crate::diff::TextSnapshot;
use crate::projects::files::LocalFile;
use crate::projects::search::{ContentQuery, find_in_text};
use crate::state::FileLoad;
use crate::ui::code::{ROW_HEIGHT, TextPoint};
use crate::ui::components::{ButtonSize, IconButton, IconName, TestSelector as _};
use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::ui::theme::Theme;

actions!(
    file_browser,
    [
        /// Opens the find bar over the open file.
        Find,
        /// Moves to the next match.
        FindNext,
        /// Moves to the previous match.
        FindPrevious
    ]
);

const FIND_CONTEXT: &str = "FileFind";
/// Matches marked in a file, at most.
pub(super) const FIND_LIMIT: usize = 10_000;

pub(super) fn bind_keys(context: &'static str, cx: &mut App) {
    let context = Some(context);
    cx.bind_keys([
        KeyBinding::new("cmd-f", Find, context),
        KeyBinding::new("cmd-g", FindNext, context),
        KeyBinding::new("cmd-shift-g", FindPrevious, context),
    ]);
    // Enter is the field's own, which moves to the next match. The
    // arrows would otherwise move through the tree.
    let find = Some(FIND_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("shift-enter", FindPrevious, find),
        KeyBinding::new("down", FindNext, find),
        KeyBinding::new("up", FindPrevious, find),
    ]);
}

/// The find bar and its matches in the open file.
pub(super) struct FindState {
    pub open: bool,
    pub input: Entity<TextInput>,
    pub regex: bool,
    pub case_sensitive: bool,
    /// Each match's row and source range, in order.
    pub matches: Arc<Vec<(usize, Range<usize>)>>,
    /// The match moved to, by index into `matches`.
    pub current: Option<usize>,
    /// Why the query couldn't be searched for (a regex that doesn't parse).
    pub error: Option<SharedString>,
    /// The text and query `matches` are for, to find them again when
    /// either changes.
    found_for: Option<(Arc<TextSnapshot>, ContentQuery)>,
    /// Scroll to the current match once it's found: the query changed,
    /// rather than the file's text.
    reveal: bool,
    _task: Option<Task<()>>,
}

impl FindState {
    pub fn new(cx: &mut Context<FileBrowserView>) -> Self {
        let input = cx.new(|cx| TextInput::new("Find in file", cx));
        cx.subscribe(
            &input,
            |this: &mut FileBrowserView, _, event, cx| match event {
                TextInputEvent::Changed => {
                    this.find.reveal = true;
                    this.find.current = None;
                    cx.notify();
                }
                TextInputEvent::Submit => this.step_find(1, cx),
            },
        )
        .detach();
        Self {
            open: false,
            input,
            regex: false,
            case_sensitive: false,
            matches: Arc::default(),
            current: None,
            error: None,
            found_for: None,
            reveal: false,
            _task: None,
        }
    }

    fn query(&self, cx: &App) -> ContentQuery {
        ContentQuery {
            text: self.input.read(cx).text().to_owned(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
        }
    }

    /// The matches to mark, while the bar is open.
    pub fn marked(&self) -> (&[(usize, Range<usize>)], Option<usize>) {
        if self.open {
            (&self.matches, self.current)
        } else {
            (&[], None)
        }
    }
}

impl FileBrowserView {
    /// The open file's text, once it's read.
    fn open_text(&self, cx: &App) -> Option<Arc<TextSnapshot>> {
        let file = self.state.read(cx).browser.as_ref()?.file.as_ref()?;
        match &file.content {
            FileLoad::Ready(content) => match &**content {
                LocalFile::Text(text) => Some(text.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// ⌘F: opens the bar and focuses its field, its text selected. A
    /// selection within a line becomes the query.
    pub(super) fn open_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_text(cx).is_none() {
            return;
        }
        let selected = self.code.selection.as_ref().and_then(|selection| {
            let (start, end) = selection.range();
            if start.row != end.row || start.offset == end.offset {
                return None;
            }
            let text = self.open_text(cx)?;
            let line = text.line(u32::try_from(start.row).ok()?);
            line.get(start.offset..end.offset).map(str::to_owned)
        });
        self.find.open = true;
        let input = self.find.input.clone();
        input.update(cx, |input, cx| {
            if let Some(selected) = &selected {
                input.set_text(selected, cx);
            }
            input.select_all_text(cx);
        });
        window.focus(&input.read(cx).focus_handle(cx));
        self.find.reveal = true;
        cx.notify();
    }

    /// Escape: closes the bar, giving the keyboard back to the file.
    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.open = false;
        self.find._task = None;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Escape in the file: closes the bar if it's open, or else does what
    /// escape does around the view.
    pub(super) fn cancel_find(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.open {
            self.close_find(window, cx);
        } else {
            cx.propagate();
        }
    }

    pub(super) fn find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step_find(1, cx);
    }

    pub(super) fn find_previous(
        &mut self,
        _: &FindPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_find(-1, cx);
    }

    /// Moves `by` matches from the current one, wrapping at the ends, and
    /// scrolls to it.
    fn step_find(&mut self, by: isize, cx: &mut Context<Self>) {
        let len = self.find.matches.len();
        if !self.find.open || len == 0 {
            return;
        }
        let next = match self.find.current {
            Some(ix) => (ix as isize + by).rem_euclid(len as isize) as usize,
            None if by > 0 => 0,
            None => len - 1,
        };
        self.find.current = Some(next);
        self.reveal_match(next);
        cx.notify();
    }

    /// Scrolls to a match, unless it's already in view.
    fn reveal_match(&mut self, ix: usize) {
        let Some(&(row, _)) = self.find.matches.get(ix) else {
            return;
        };
        let (top, rows) = self.visible_rows();
        if rows == 0 || row < top || row >= top + rows {
            self.code
                .scroll
                .scroll_to_item(row, gpui::ScrollStrategy::Center);
        }
    }

    /// The first row in view, and how many fit.
    fn visible_rows(&self) -> (usize, usize) {
        let handle = self.code.scroll.0.borrow().base_handle.clone();
        let top = (-handle.offset().y / px(ROW_HEIGHT)).max(0.).floor() as usize;
        let rows = (handle.bounds().size.height / px(ROW_HEIGHT)).floor() as usize;
        (top, rows)
    }

    /// Finds the matches again if the query or the file's text changed
    /// since they were found. Before rendering the lines.
    pub(super) fn sync_find(&mut self, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        let Some(text) = self.open_text(cx) else {
            self.find.matches = Arc::default();
            self.find.current = None;
            self.find.found_for = None;
            return;
        };
        let query = self.find.query(cx);
        if let Some((found_text, found_query)) = &self.find.found_for
            && Arc::ptr_eq(found_text, &text)
            && *found_query == query
        {
            return;
        }
        self.find.found_for = Some((text.clone(), query.clone()));
        if query.text.is_empty() {
            self.find.matches = Arc::default();
            self.find.current = None;
            self.find.error = None;
            self.find._task = None;
            return;
        }
        // From the current match, or else the selection or the top of the
        // view.
        let from = match self.find.current.and_then(|ix| self.find.matches.get(ix)) {
            Some((row, range)) => TextPoint {
                row: *row,
                offset: range.start,
            },
            None => match &self.code.selection {
                Some(selection) => selection.range().0,
                None => TextPoint {
                    row: self.visible_rows().0,
                    offset: 0,
                },
            },
        };
        self.find._task = Some(cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { find_in_text(&text, &query, FIND_LIMIT) })
                .await;
            this.update(cx, |this, cx| {
                let find = &mut this.find;
                match found {
                    Ok(matches) => {
                        find.current = matches
                            .iter()
                            .position(|(row, range)| (*row, range.start) >= (from.row, from.offset))
                            .or((!matches.is_empty()).then_some(0));
                        find.matches = Arc::new(matches);
                        find.error = None;
                    }
                    Err(err) => {
                        find.matches = Arc::default();
                        find.current = None;
                        find.error = Some(format!("{err:#}").into());
                    }
                }
                if std::mem::take(&mut find.reveal)
                    && let Some(ix) = find.current
                {
                    this.reveal_match(ix);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The bar, above the open file's code.
    pub(super) fn render_find_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.find.open {
            return None;
        }
        let theme = Theme::get(cx);
        let find = &self.find;
        let has_query = !find.input.read(cx).text().is_empty();
        let count = match (&find.error, find.matches.len(), find.current) {
            (Some(_), ..) => "Invalid regex".to_owned(),
            _ if !has_query => String::new(),
            (None, 0, _) => "No matches".to_owned(),
            (None, len, Some(ix)) if len >= FIND_LIMIT => format!("{} of {len}+", ix + 1),
            (None, len, Some(ix)) => format!("{} of {len}", ix + 1),
            (None, len, None) => format!("{len} matches"),
        };
        let error = find.error.clone();
        let stepping = !find.matches.is_empty();
        Some(
            div()
                .key_context(FIND_CONTEXT)
                .on_action(cx.listener(|this, _: &Cancel, window, cx| this.close_find(window, cx)))
                // Keeps the keyboard in the field, rather than the view
                // taking it back.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border_subtle)
                .text_xs()
                .child(
                    div()
                        .test_selector(|| "browser-find-input".into())
                        .w(px(260.))
                        .flex_shrink()
                        .min_w(px(120.))
                        .child(find.input.clone()),
                )
                .child(
                    IconButton::new("find-case", IconName::CaseSensitive)
                        .size(ButtonSize::Compact)
                        .tooltip("Match case")
                        .selected(find.case_sensitive)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.find.case_sensitive = !this.find.case_sensitive;
                            this.find.reveal = true;
                            cx.notify();
                        })),
                )
                .child(
                    IconButton::new("find-regex", IconName::Regex)
                        .size(ButtonSize::Compact)
                        .tooltip("Use regular expression")
                        .selected(find.regex)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.find.regex = !this.find.regex;
                            this.find.reveal = true;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("find-count")
                        .test_selector(|| "find-count".into())
                        .flex_none()
                        .px_1()
                        .text_color(if error.is_some() {
                            theme.danger
                        } else {
                            theme.text_subtle
                        })
                        .child(count)
                        .when_some(error, |el, error| {
                            el.tooltip(move |_, cx| {
                                crate::ui::components::Tooltip::view(error.clone(), cx)
                            })
                        }),
                )
                .child(div().flex_1())
                .child(
                    IconButton::new("find-previous", IconName::ChevronUp)
                        .size(ButtonSize::Compact)
                        .tooltip("Previous match (⇧⏎)")
                        .disabled(!stepping)
                        .on_click(cx.listener(|this, _, _, cx| this.step_find(-1, cx))),
                )
                .child(
                    IconButton::new("find-next", IconName::ChevronDown)
                        .size(ButtonSize::Compact)
                        .tooltip("Next match (⏎)")
                        .disabled(!stepping)
                        .on_click(cx.listener(|this, _, _, cx| this.step_find(1, cx))),
                )
                .child(
                    IconButton::new("find-close", IconName::X)
                        .size(ButtonSize::Compact)
                        .tooltip("Close (Esc)")
                        .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
                )
                .into_any_element(),
        )
    }
}
