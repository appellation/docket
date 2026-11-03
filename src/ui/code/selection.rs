//! Text selection across a code view's rows, and the mapping between a
//! line's source text and what's displayed.
//!
//! Positions are byte offsets into a row's source line, before tabs expand,
//! so copying gives the file's own text. In the diff a selection can span
//! files; rows without code (headers, notices) contribute nothing when
//! copied.

use std::ops::Range;
use std::rc::Rc;

use gpui::{HighlightStyle, Hsla, SharedString};

/// Columns a tab expands to.
pub const TAB_WIDTH: usize = 4;

/// Columns `line` takes up, with tabs expanded.
pub fn display_width(line: &str) -> usize {
    line.chars()
        .map(|c| if c == '\t' { TAB_WIDTH } else { 1 })
        .sum()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPoint {
    pub row: usize,
    /// Byte offset into the row's source line.
    pub offset: usize,
}

/// What a click selects, and what dragging after it extends by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
    Character,
    Word,
    Line,
}

impl Granularity {
    pub fn for_click_count(count: usize) -> Self {
        match count {
            0 | 1 => Self::Character,
            2 => Self::Word,
            _ => Self::Line,
        }
    }
}

/// A start and end, ordered.
pub type Span = (TextPoint, TextPoint);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// What was clicked: a point, or the word or line under it.
    anchor: Span,
    /// The same, where the mouse is now.
    head: Span,
    pub granularity: Granularity,
}

impl Selection {
    pub fn new(at: Span, granularity: Granularity) -> Self {
        Self {
            anchor: at,
            head: at,
            granularity,
        }
    }

    pub fn extend_to(&mut self, head: Span) {
        self.head = head;
    }

    /// The selected text's start and end.
    pub fn range(&self) -> Span {
        if self.head.0 < self.anchor.0 {
            (self.head.0, self.anchor.1)
        } else {
            (self.anchor.0, self.head.1)
        }
    }

    pub fn is_empty(&self) -> bool {
        let (start, end) = self.range();
        start == end
    }

    /// The selected part of `row`, a line `len` bytes long, and whether the
    /// selection carries on past its end.
    pub fn in_row(&self, row: usize, len: usize) -> Option<(Range<usize>, bool)> {
        let (start, end) = self.range();
        if row < start.row || row > end.row {
            return None;
        }
        let from = if row == start.row { start.offset } else { 0 }.min(len);
        let to = if row == end.row { end.offset } else { len }.min(len);
        let continues = row < end.row;
        (from < to || continues).then_some((from..to, continues))
    }
}

/// The run of word characters, whitespace, or punctuation around `offset`,
/// for double-click selection. At a boundary it prefers the run after.
pub fn word_range(text: &str, offset: usize) -> Range<usize> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Space,
        Punctuation,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Punctuation
        }
    };
    let offset = offset.min(text.len());
    let Some(c) = text[offset..]
        .chars()
        .next()
        .or_else(|| text[..offset].chars().next_back())
    else {
        return offset..offset;
    };
    let target = class(c);
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| class(*c) == target)
        .last()
        .map_or(offset, |(ix, _)| ix);
    let end = text[offset..]
        .char_indices()
        .find(|(_, c)| class(*c) != target)
        .map_or(text.len(), |(ix, _)| offset + ix);
    start..end
}

/// Maps byte offsets between a line's source text and its display text,
/// where tabs are expanded to spaces.
#[derive(Clone, Default)]
pub struct TabMap {
    /// Display offset of each source offset (and the end); `None` when the
    /// line has no tabs and they're the same.
    display_offsets: Option<Rc<[usize]>>,
}

impl TabMap {
    pub fn to_display(&self, source: usize) -> usize {
        match &self.display_offsets {
            Some(offsets) => offsets[source.min(offsets.len() - 1)],
            None => source,
        }
    }

    /// The start of the source character `display` falls in.
    pub fn source_at(&self, display: usize) -> usize {
        match &self.display_offsets {
            Some(offsets) => offsets.partition_point(|&d| d <= display).saturating_sub(1),
            None => display,
        }
    }

    /// The source offset nearest `display`; inside an expanded tab, the
    /// side of the tab it's closer to.
    pub fn to_source(&self, display: usize) -> usize {
        let Some(offsets) = &self.display_offsets else {
            return display;
        };
        let ix = offsets.partition_point(|&d| d <= display).saturating_sub(1);
        match offsets.get(ix + 1) {
            Some(&next) if display - offsets[ix] > (next - offsets[ix]) / 2 => ix + 1,
            _ => ix,
        }
    }
}

/// Expands tabs to spaces (GPUI doesn't lay them out as tab stops),
/// remapping highlight ranges to match.
pub fn expand_tabs(
    text: &str,
    spans: Vec<(Range<usize>, HighlightStyle)>,
) -> (SharedString, Vec<(Range<usize>, HighlightStyle)>, TabMap) {
    if !text.contains('\t') {
        return (
            SharedString::from(text.to_owned()),
            spans,
            TabMap::default(),
        );
    }
    let mut expanded = String::with_capacity(text.len() + TAB_WIDTH * 2);
    let mut offsets = Vec::with_capacity(text.len() + 1);
    let mut column = 0;
    for c in text.chars() {
        let start = expanded.len();
        if c == '\t' {
            let width = TAB_WIDTH - column % TAB_WIDTH;
            expanded.extend(std::iter::repeat_n(' ', width));
            column += width;
        } else {
            expanded.push(c);
            column += 1;
        }
        offsets.extend(std::iter::repeat_n(start, c.len_utf8()));
    }
    offsets.push(expanded.len());
    let spans = spans
        .into_iter()
        .map(|(range, style)| (offsets[range.start]..offsets[range.end], style))
        .collect();
    let map = TabMap {
        display_offsets: Some(offsets.into()),
    };
    (expanded.into(), spans, map)
}

/// Gives `range` of a `len`-byte line a background, splitting the sorted,
/// non-overlapping `spans` around it so they stay that way.
pub fn with_background(
    spans: Vec<(Range<usize>, HighlightStyle)>,
    range: Range<usize>,
    color: Hsla,
    len: usize,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if range.is_empty() {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len() + 3);
    let mut push = |part: Range<usize>, style: HighlightStyle| {
        let pieces = [
            (part.start..part.end.min(range.start), false),
            (part.start.max(range.start)..part.end.min(range.end), true),
            (part.start.max(range.end)..part.end, false),
        ];
        for (piece, selected) in pieces {
            if piece.is_empty() {
                continue;
            }
            let mut style = style;
            if selected {
                style.background_color = Some(color);
            } else if style == HighlightStyle::default() {
                continue;
            }
            out.push((piece, style));
        }
    };
    let mut at = 0;
    for (span, style) in spans {
        push(at..span.start, HighlightStyle::default());
        at = span.end;
        push(span, style);
    }
    push(at..len, HighlightStyle::default());
    out
}

#[cfg(test)]
mod tests {
    use gpui::hsla;

    use super::*;

    fn point(row: usize, offset: usize) -> TextPoint {
        TextPoint { row, offset }
    }

    #[test]
    fn selections_order_their_ends() {
        let mut selection = Selection::new((point(3, 4), point(3, 4)), Granularity::Character);
        selection.extend_to((point(1, 2), point(1, 2)));
        assert_eq!(selection.range(), (point(1, 2), point(3, 4)));
        assert_eq!(selection.in_row(1, 10), Some((2..10, true)));
        assert_eq!(selection.in_row(2, 0), Some((0..0, true)));
        assert_eq!(selection.in_row(3, 10), Some((0..4, false)));
        assert_eq!(selection.in_row(4, 10), None);
    }

    #[test]
    fn word_selections_keep_the_clicked_word() {
        // Double-click "bar" in row 2, then drag back into row 1.
        let mut selection = Selection::new((point(2, 4), point(2, 7)), Granularity::Word);
        selection.extend_to((point(1, 0), point(1, 3)));
        assert_eq!(selection.range(), (point(1, 0), point(2, 7)));
    }

    #[test]
    fn finds_words() {
        let text = "let foo_bar = a::b;";
        assert_eq!(word_range(text, 5), 4..11);
        assert_eq!(word_range(text, 4), 4..11);
        assert_eq!(word_range(text, 3), 3..4);
        assert_eq!(word_range(text, 15), 15..17);
        assert_eq!(word_range(text, text.len()), 18..19);
        assert_eq!(word_range("", 0), 0..0);
    }

    #[test]
    fn expands_tabs_to_tab_stops() {
        let style = HighlightStyle::default();
        let (text, spans, map) = expand_tabs("a\tbc\td", vec![(2..4, style)]);
        assert_eq!(text.as_ref(), "a   bc  d");
        assert_eq!(spans[0].0, 4..6);
        assert_eq!(map.to_display(2), 4);
        assert_eq!(map.to_source(4), 2);
        // Inside the first tab, which spans display columns 1..4.
        assert_eq!(map.to_source(2), 1);
        assert_eq!(map.to_source(3), 2);
        assert_eq!(map.source_at(3), 1);
    }

    #[test]
    fn splits_highlights_around_a_background() {
        let bold = HighlightStyle {
            font_weight: Some(gpui::FontWeight::BOLD),
            ..Default::default()
        };
        let bg = hsla(0., 0., 0., 1.);
        let spans = with_background(vec![(2..6, bold)], 4..8, bg, 10);
        let ranges: Vec<_> = spans
            .iter()
            .map(|(r, s)| {
                (
                    r.clone(),
                    s.font_weight.is_some(),
                    s.background_color.is_some(),
                )
            })
            .collect();
        assert_eq!(
            ranges,
            [(2..4, true, false), (4..6, true, true), (6..8, false, true)]
        );
    }
}
