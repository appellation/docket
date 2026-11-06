//! Text field, adapted from GPUI's `examples/input.rs`: a single line, or,
//! `multiline`, a box of wrapped lines.
//!
//! Supports IME composition, selection, and clipboard. Double-clicking
//! selects a word and triple-clicking all of it, and dragging on from
//! either extends the selection a word (or all) at a time. `masked` fields
//! render bullets and refuse copy/cut, for secrets like access tokens.
//!
//! A multiline field shows a fixed number of lines, scrolling to keep the
//! cursor in view. Enter starts a new line there, and command-enter
//! submits.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window,
    WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use super::theme::Theme;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Submit,
        Newline,
        Up,
        Down,
        SelectUp,
        SelectDown
    ]
);

const KEY_CONTEXT: &str = "TextInput";
const MULTILINE_KEY_CONTEXT: &str = "TextArea";
const MASK: char = '•';

pub enum TextInputEvent {
    /// Enter was pressed.
    Submit,
    /// The text changed, by typing or otherwise.
    Changed,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    masked: bool,
    /// How many lines a multiline field shows; `None` for a single line.
    multiline: Option<usize>,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    /// A multiline field's lines as last laid out.
    last_lines: Vec<LaidOutLine>,
    last_line_height: Pixels,
    /// How far a multiline field is scrolled down.
    scroll_top: Pixels,
    is_selecting: bool,
    /// What a drag selects by: what the press that started it did.
    granularity: Granularity,
    /// What that press selected, which a drag by words keeps.
    drag_origin: Range<usize>,
}

/// One line of a multiline field, wrapped.
struct LaidOutLine {
    /// Where it starts in the content.
    start: usize,
    line: WrappedLine,
    /// How far below the first it starts, before scrolling.
    top: Pixels,
}

/// Where `offset` is laid out among a multiline field's `lines`, from
/// the top-left of the first.
fn position_in(lines: &[LaidOutLine], height: Pixels, offset: usize) -> Option<Point<Pixels>> {
    let line = lines.iter().rev().find(|l| l.start <= offset)?;
    let local = (offset - line.start).min(line.line.len());
    let at = line.line.position_for_index(local, height)?;
    Some(point(at.x, at.y + line.top))
}

/// The offset nearest `position` among a multiline field's `lines`,
/// from the top-left of the first.
fn offset_in(lines: &[LaidOutLine], height: Pixels, position: Point<Pixels>) -> usize {
    let Some(line) = lines
        .iter()
        .rev()
        .find(|l| l.top <= position.y)
        .or(lines.first())
    else {
        return 0;
    };
    let rows = line.line.size(height).height;
    let local = point(
        position.x.max(px(0.)),
        (position.y - line.top).clamp(px(0.), rows - px(1.)),
    );
    let index = match line.line.closest_index_for_position(local, height) {
        Ok(ix) | Err(ix) => ix,
    };
    line.start + index.min(line.line.len())
}

/// What a press selects, by how many clicks it took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Granularity {
    #[default]
    Character,
    Word,
    All,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    pub fn bind_keys(cx: &mut App) {
        for context in [KEY_CONTEXT, MULTILINE_KEY_CONTEXT] {
            let context = Some(context);
            cx.bind_keys([
                KeyBinding::new("backspace", Backspace, context),
                KeyBinding::new("delete", Delete, context),
                KeyBinding::new("left", Left, context),
                KeyBinding::new("right", Right, context),
                KeyBinding::new("shift-left", SelectLeft, context),
                KeyBinding::new("shift-right", SelectRight, context),
                KeyBinding::new("cmd-a", SelectAll, context),
                KeyBinding::new("cmd-v", Paste, context),
                KeyBinding::new("cmd-c", Copy, context),
                KeyBinding::new("cmd-x", Cut, context),
                KeyBinding::new("home", Home, context),
                KeyBinding::new("end", End, context),
                KeyBinding::new("cmd-left", Home, context),
                KeyBinding::new("cmd-right", End, context),
            ]);
        }
        let single = Some(KEY_CONTEXT);
        let multi = Some(MULTILINE_KEY_CONTEXT);
        cx.bind_keys([
            KeyBinding::new("enter", Submit, single),
            KeyBinding::new("enter", Newline, multi),
            KeyBinding::new("shift-enter", Newline, multi),
            KeyBinding::new("cmd-enter", Submit, multi),
            KeyBinding::new("up", Up, multi),
            KeyBinding::new("down", Down, multi),
            KeyBinding::new("shift-up", SelectUp, multi),
            KeyBinding::new("shift-down", SelectDown, multi),
        ]);
    }

    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::default(),
            placeholder: placeholder.into(),
            masked: false,
            multiline: None,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            last_lines: Vec::new(),
            last_line_height: px(0.),
            scroll_top: px(0.),
            is_selecting: false,
            granularity: Granularity::Character,
            drag_origin: 0..0,
        }
    }

    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    /// Makes it a box of wrapped lines, `rows` of them tall.
    pub fn multiline(mut self, rows: usize) -> Self {
        self.multiline = Some(rows.max(1));
        self
    }

    /// Whether it's empty, or only whitespace.
    pub fn is_blank(&self) -> bool {
        self.content.trim().is_empty()
    }

    /// Clears the field and returns what it held.
    pub fn take(&mut self, cx: &mut Context<Self>) -> String {
        let content = std::mem::take(&mut self.content);
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_layout = None;
        self.last_lines.clear();
        self.scroll_top = px(0.);
        if !content.is_empty() {
            cx.emit(TextInputEvent::Changed);
        }
        cx.notify();
        content.to_string()
    }

    /// What the field holds.
    pub fn text(&self) -> &str {
        &self.content
    }

    /// Replaces what the field holds, selecting all of it, so typing
    /// replaces it.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.content == text {
            return;
        }
        self.content = text.to_owned().into();
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    /// Selects all of the text, so typing replaces it.
    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    /// The start of the line the cursor is on; in a single line, of it all.
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let start = match self.multiline {
            Some(_) => self.content[..cursor].rfind('\n').map_or(0, |ix| ix + 1),
            None => 0,
        };
        self.move_to(start, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let end = match self.multiline {
            Some(_) => self.content[cursor..]
                .find('\n')
                .map_or(self.content.len(), |ix| cursor + ix),
            None => self.content.len(),
        };
        self.move_to(end, cx);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.vertical_offset(-1);
        self.move_to(offset, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.vertical_offset(1);
        self.move_to(offset, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.vertical_offset(-1);
        self.select_to(offset, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.vertical_offset(1);
        self.select_to(offset, cx);
    }

    /// The offset a line above (`-1`) or below (`1`) the cursor, as laid
    /// out; past the first or last line, the start or end.
    fn vertical_offset(&self, direction: i32) -> usize {
        // What's laid out is the placeholder.
        if self.content.is_empty() {
            return 0;
        }
        let cursor = self.cursor_offset();
        let Some(at) = self.position_for_offset(cursor) else {
            return if direction < 0 { 0 } else { self.content.len() };
        };
        let height = self.last_line_height;
        let y = at.y + height * direction as f32 + height / 2.;
        let total = self
            .last_lines
            .last()
            .map_or(px(0.), |l| l.top + l.line.size(height).height);
        if y < px(0.) {
            0
        } else if y >= total {
            self.content.len()
        } else {
            self.offset_for_position(point(at.x, y))
        }
    }

    fn position_for_offset(&self, offset: usize) -> Option<Point<Pixels>> {
        position_in(&self.last_lines, self.last_line_height, offset)
    }

    fn offset_for_position(&self, position: Point<Pixels>) -> usize {
        offset_in(&self.last_lines, self.last_line_height, position)
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Submit);
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        self.granularity = match event.click_count {
            0 | 1 => Granularity::Character,
            2 => Granularity::Word,
            _ => Granularity::All,
        };
        match self.granularity {
            Granularity::Character if event.modifiers.shift => {
                self.select_to(self.index_for_mouse_position(event.position), cx);
            }
            Granularity::Character => {
                self.move_to(self.index_for_mouse_position(event.position), cx)
            }
            Granularity::Word => {
                let word = self.word_under(event.position);
                self.drag_origin = word.clone();
                self.select_range(word, false, cx);
            }
            Granularity::All => {
                self.drag_origin = 0..self.content.len();
                self.select_range(0..self.content.len(), false, cx);
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.is_selecting {
            return;
        }
        match self.granularity {
            Granularity::Character => {
                self.select_to(self.index_for_mouse_position(event.position), cx)
            }
            // From the word first selected to the one under the pointer,
            // whichever side it's on.
            Granularity::Word => {
                let word = self.word_under(event.position);
                let origin = self.drag_origin.clone();
                if word.start < origin.start {
                    self.select_range(word.start..origin.end, true, cx);
                } else {
                    self.select_range(origin.start..word.end.max(origin.end), false, cx);
                }
            }
            Granularity::All => {}
        }
    }

    /// The word under the pointer: its run of letters and digits, or of
    /// spaces, or a single punctuation mark. In a masked field, all of it,
    /// since its bullets show no words.
    fn word_under(&self, position: Point<Pixels>) -> Range<usize> {
        if self.masked {
            return 0..self.content.len();
        }
        word_at(&self.content, self.char_index_for_mouse_position(position))
    }

    /// The character under the pointer, rather than the nearest boundary
    /// between two: clicking a word's last letter is clicking that word.
    fn char_index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.multiline.is_some() {
            return self.index_for_mouse_position(position);
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        match line.index_for_x(position.x - bounds.left()) {
            Some(index) => self.content_offset(index),
            None => self.index_for_mouse_position(position),
        }
    }

    /// Selects `range`, with the cursor at its start if `reversed`.
    fn select_range(&mut self, range: Range<usize>, reversed: bool, cx: &mut Context<Self>) {
        self.selected_range = range;
        self.selection_reversed = reversed;
        cx.notify();
    }

    #[cfg(test)]
    pub fn selected_text(&self) -> &str {
        &self.content[self.selected_range.clone()]
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = match self.multiline {
                Some(_) => text.replace("\r\n", "\n"),
                None => text.replace('\n', " "),
            };
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.masked && !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.masked && !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        if self.multiline.is_some() {
            let Some(bounds) = self.last_bounds else {
                return 0;
            };
            let local = position - bounds.origin;
            return self.offset_for_position(point(local.x, local.y + self.scroll_top));
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        self.content_offset(line.closest_index_for_x(position.x - bounds.left()))
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    /// Masked fields show one bullet per character, so content offsets and
    /// offsets in the shaped (displayed) line differ.
    fn display_offset(&self, offset: usize) -> usize {
        if self.masked {
            masked_display_offset(&self.content, offset)
        } else {
            offset
        }
    }

    fn content_offset(&self, offset: usize) -> usize {
        if self.masked {
            masked_content_offset(&self.content, offset)
        } else {
            offset
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

/// The word in `text` at byte offset `offset`, by Unicode's word
/// boundaries: a run of letters, digits, and joiners like `_` and `'`; a
/// run of spaces; or one punctuation mark. Past the end, the last one.
fn word_at(text: &str, offset: usize) -> Range<usize> {
    let mut last = 0..0;
    for (start, word) in text.split_word_bound_indices() {
        let range = start..start + word.len();
        if range.contains(&offset) {
            return range;
        }
        last = range;
    }
    last
}

/// Byte offset in the bullets shown for `content` of byte offset `offset`.
fn masked_display_offset(content: &str, offset: usize) -> usize {
    content[..offset].chars().count() * MASK.len_utf8()
}

/// Inverse of [`masked_display_offset`].
fn masked_content_offset(content: &str, display_offset: usize) -> usize {
    content
        .char_indices()
        .nth(display_offset / MASK.len_utf8())
        .map_or(content.len(), |(ix, _)| ix)
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        if !(range.is_empty() && new_text.is_empty()) {
            cx.emit(TextInputEvent::Changed);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());

        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        if self.multiline.is_some() {
            let origin = point(bounds.left(), bounds.top() - self.scroll_top);
            let start = self.position_for_offset(range.start)?;
            let end = self.position_for_offset(range.end)?;
            return Some(Bounds::from_corners(
                origin + start,
                origin + point(end.x, end.y + self.last_line_height),
            ));
        }
        let last_layout = self.last_layout.as_ref()?;
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(self.display_offset(range.start)),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(self.display_offset(range.end)),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        if self.multiline.is_some() {
            let offset =
                self.offset_for_position(gpui::point(line_point.x, line_point.y + self.scroll_top));
            return Some(self.offset_to_utf16(offset));
        }
        let last_layout = self.last_layout.as_ref()?;
        let display_index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(self.content_offset(display_index)))
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx);
        let focused = self.focus_handle.is_focused(window);
        div()
            .flex()
            .key_context(match self.multiline {
                Some(_) => MULTILINE_KEY_CONTEXT,
                None => KEY_CONTEXT,
            })
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .when(self.multiline.is_some(), |el| {
                el.on_action(cx.listener(Self::newline))
                    .on_action(cx.listener(Self::up))
                    .on_action(cx.listener(Self::down))
                    .on_action(cx.listener(Self::select_up))
                    .on_action(cx.listener(Self::select_down))
            })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(if focused {
                theme.border_focused
            } else {
                theme.border
            })
            .bg(theme.elevated)
            .overflow_hidden()
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    /// A multiline field's lines, and how far it's scrolled.
    lines: Vec<LaidOutLine>,
    scroll_top: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The runs to shape `display_text` with: `run`, with the text being
/// composed underlined.
fn text_runs(input: &TextInput, display_text: &str, run: TextRun) -> Vec<TextRun> {
    match input.marked_range.as_ref() {
        Some(marked) if !input.content.is_empty() => {
            let (start, end) = (
                input.display_offset(marked.start),
                input.display_offset(marked.end),
            );
            vec![
                TextRun {
                    len: start,
                    ..run.clone()
                },
                TextRun {
                    len: end - start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        }
        _ => vec![run],
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let rows = self.input.read(cx).multiline.unwrap_or(1);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * rows as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = Theme::get(cx);
        let input = self.input.read(cx);
        let style = window.text_style();

        let (display_text, text_color): (SharedString, _) = if input.content.is_empty() {
            (input.placeholder.clone(), theme.text_subtle)
        } else if input.masked {
            (
                MASK.to_string()
                    .repeat(input.content.chars().count())
                    .into(),
                style.color,
            )
        } else {
            (input.content.clone(), style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = text_runs(input, &display_text, run);
        let font_size = style.font_size.to_pixels(window.rem_size());
        if input.multiline.is_some() {
            return prepaint_lines(input, display_text, &runs, font_size, bounds, window, cx);
        }
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let selected = input.display_offset(input.selected_range.start)
            ..input.display_offset(input.selected_range.end);
        let (selections, cursor) = if selected.is_empty() {
            let cursor_x = line.x_for_index(input.display_offset(input.cursor_offset()));
            (
                Vec::new(),
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_x, bounds.top()),
                        size(px(1.5), bounds.bottom() - bounds.top()),
                    ),
                    theme.accent,
                )),
            )
        } else {
            (
                vec![fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected.end),
                            bounds.bottom(),
                        ),
                    ),
                    theme.accent.opacity(0.35),
                )],
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selections,
            lines: Vec::new(),
            scroll_top: px(0.),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let line_height = window.line_height();
        let line = prepaint.line.take();
        let lines = std::mem::take(&mut prepaint.lines);
        let scroll_top = prepaint.scroll_top;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for selection in prepaint.selections.drain(..) {
                window.paint_quad(selection);
            }
            match &line {
                Some(line) => {
                    line.paint(bounds.origin, line_height, window, cx).ok();
                }
                None => {
                    for laid in &lines {
                        let origin = point(bounds.left(), bounds.top() - scroll_top + laid.top);
                        laid.line
                            .paint(origin, line_height, TextAlign::Left, None, window, cx)
                            .ok();
                    }
                }
            }
            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });

        self.input.update(cx, |input, _cx| {
            input.last_layout = line;
            input.last_lines = lines;
            input.last_line_height = line_height;
            input.scroll_top = scroll_top;
            input.last_bounds = Some(bounds);
        });
    }
}

/// Lays out a multiline field: its text wrapped to `bounds`, scrolled to
/// keep the cursor in view.
fn prepaint_lines(
    input: &TextInput,
    display_text: SharedString,
    runs: &[TextRun],
    font_size: Pixels,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &App,
) -> PrepaintState {
    let theme = Theme::get(cx);
    let height = window.line_height();
    let wrapped = window
        .text_system()
        .shape_text(display_text, font_size, runs, Some(bounds.size.width), None)
        .unwrap_or_default();
    let mut lines = Vec::with_capacity(wrapped.len());
    let (mut start, mut top) = (0, px(0.));
    for line in wrapped {
        let len = line.len();
        let rows = line.size(height).height;
        lines.push(LaidOutLine { start, line, top });
        start += len + 1;
        top += rows;
    }
    // The placeholder's layout isn't the content's.
    let at = |offset: usize| {
        if input.content.is_empty() {
            Some(point(px(0.), px(0.)))
        } else {
            position_in(&lines, height, offset)
        }
    };

    let cursor = at(input.cursor_offset()).unwrap_or_default();
    let visible = bounds.size.height;
    let mut scroll_top = input.scroll_top;
    if cursor.y < scroll_top {
        scroll_top = cursor.y;
    } else if cursor.y + height > scroll_top + visible {
        scroll_top = cursor.y + height - visible;
    }
    scroll_top = scroll_top.clamp(px(0.), (top - visible).max(px(0.)));

    let origin = point(bounds.left(), bounds.top() - scroll_top);
    let range = &input.selected_range;
    let (selections, cursor) = if range.is_empty() {
        let quad = fill(
            Bounds::new(origin + cursor, size(px(1.5), height)),
            theme.accent,
        );
        (Vec::new(), Some(quad))
    } else {
        let color = theme.accent.opacity(0.35);
        let (from, to) = (
            at(range.start).unwrap_or_default(),
            at(range.end).unwrap_or_default(),
        );
        let right = bounds.size.width;
        let quad = |left: Pixels, top: Pixels, right: Pixels, bottom: Pixels| {
            fill(
                Bounds::from_corners(origin + point(left, top), origin + point(right, bottom)),
                color,
            )
        };
        let quads = if from.y == to.y {
            vec![quad(from.x, from.y, to.x, from.y + height)]
        } else {
            vec![
                quad(from.x, from.y, right, from.y + height),
                quad(px(0.), from.y + height, right, to.y),
                quad(px(0.), to.y, to.x, to.y + height),
            ]
        };
        (quads, None)
    };
    PrepaintState {
        line: None,
        cursor,
        selections,
        lines,
        scroll_top,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_word_at_an_offset() {
        let text = "models/applica  find_in_text it's ünï";
        let word = |offset| &text[word_at(text, offset)];
        assert_eq!(word(0), "models");
        assert_eq!(word(5), "models");
        assert_eq!(word(6), "/");
        assert_eq!(word(7), "applica");
        assert_eq!(word(14), "  ");
        assert_eq!(word(17), "find_in_text");
        assert_eq!(word(30), "it's");
        assert_eq!(word(text.len() - 1), "ünï");
        assert_eq!(word(text.len()), "ünï", "past the end");
        assert_eq!(word_at("", 0), 0..0);
    }

    #[test]
    fn maps_offsets_between_content_and_bullets() {
        let content = "ab€c"; // `€` is 3 bytes, like the bullet.
        let char_starts = [0, 1, 2, 5, 6];
        for (chars_before, &offset) in char_starts.iter().enumerate() {
            let display = masked_display_offset(content, offset);
            assert_eq!(display, chars_before * MASK.len_utf8());
            assert_eq!(masked_content_offset(content, display), offset);
        }
    }
}
