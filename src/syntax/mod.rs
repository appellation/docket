//! Syntax: tree-sitter languages and highlighting.
//!
//! Highlighting produces a [`SyntaxSnapshot`] per file: the parsed tree and
//! a flat list of highlight spans. Spans name a [`HighlightId`] rather than a
//! color, so the UI resolves them against whichever theme is active. The
//! tree is kept for features that need structure (revealing the enclosing
//! function, hunk-header context) without parsing again.

mod highlight;
mod languages;

use std::ops::Range;

pub use highlight::highlight;
pub use languages::{Language, LanguageRegistry};

/// Highlight names the theme styles, in [`HighlightId`] order. A query's
/// capture maps to the longest dotted prefix it starts with, so
/// `@function.method.call` styles as `function.method`.
pub const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "boolean",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "namespace",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.escape",
    "string.special",
    "tag",
    "text.literal",
    "text.reference",
    "text.title",
    "text.uri",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// Index into [`HIGHLIGHT_NAMES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HighlightId(pub u16);

impl HighlightId {
    /// Maps a query capture name to the most specific highlight name it
    /// falls under, if any.
    pub fn for_capture(capture: &str) -> Option<Self> {
        let mut name = capture;
        loop {
            if let Some(ix) = HIGHLIGHT_NAMES.iter().position(|n| *n == name) {
                return Some(Self(ix as u16));
            }
            name = name.rsplit_once('.')?.0;
        }
    }

    #[cfg(test)]
    pub fn name(self) -> &'static str {
        HIGHLIGHT_NAMES[self.0 as usize]
    }
}

/// A file's parse tree and highlights.
pub struct SyntaxSnapshot {
    /// Not read yet: kept for structural features (revealing the enclosing
    /// function, hunk-header context) so they needn't parse again.
    #[allow(dead_code)]
    pub tree: tree_sitter::Tree,
    /// Sorted, non-overlapping byte ranges into the file's text.
    pub spans: Vec<(Range<u32>, HighlightId)>,
}

impl SyntaxSnapshot {
    /// Spans that intersect `range`, clipped to it and made relative to its
    /// start, for rendering one line.
    pub fn spans_in(
        &self,
        range: Range<usize>,
    ) -> impl Iterator<Item = (Range<usize>, HighlightId)> {
        let first = self
            .spans
            .partition_point(|(span, _)| (span.end as usize) <= range.start);
        self.spans[first..]
            .iter()
            .take_while(move |(span, _)| (span.start as usize) < range.end)
            .map(move |(span, id)| {
                let start = (span.start as usize).max(range.start) - range.start;
                let end = (span.end as usize).min(range.end) - range.start;
                (start..end, *id)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_map_to_longest_prefix() {
        let name = |capture| HighlightId::for_capture(capture).map(HighlightId::name);
        assert_eq!(name("function.method"), Some("function.method"));
        assert_eq!(name("function.method.call"), Some("function.method"));
        assert_eq!(name("comment.documentation"), Some("comment"));
        assert_eq!(name("string.special.key"), Some("string.special"));
        assert_eq!(name("none"), None);
        assert_eq!(name("spell"), None);
    }
}
