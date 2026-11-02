//! Parses a file and flattens its highlight captures into spans. CPU-bound,
//! so run on the background executor.

use std::ops::Range;

use tree_sitter::{Parser, QueryCursor, StreamingIterator as _};

use super::{HighlightId, Language, SyntaxSnapshot};

/// Larger files render as plain text; highlighting them would hold a
/// background thread for too long to be worth it.
const MAX_BYTES: usize = 1024 * 1024;
const MAX_LINES: usize = 20_000;

/// Highlights `text`, or returns `None` if it's too large or the language's
/// query doesn't compile.
pub fn highlight(text: &str, language: &Language) -> Option<SyntaxSnapshot> {
    if text.len() > MAX_BYTES || text.lines().count() > MAX_LINES {
        return None;
    }
    let compiled = language.compiled()?;
    let mut parser = Parser::new();
    parser.set_language(&compiled.grammar).ok()?;
    let tree = parser.parse(text, None)?;

    let mut captures = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.captures(&compiled.query, tree.root_node(), text.as_bytes());
    while let Some((m, ix)) = matches.next() {
        let capture = m.captures()[*ix];
        if let Some(id) = compiled.capture_map[capture.index as usize] {
            let range = capture.node.byte_range();
            captures.push(Capture {
                range: range.start as u32..range.end as u32,
                pattern: m.pattern_index,
                id,
            });
        }
    }
    Some(SyntaxSnapshot {
        tree,
        spans: flatten(captures),
    })
}

struct Capture {
    range: Range<u32>,
    pattern: usize,
    id: HighlightId,
}

/// Resolves nested captures into sorted, non-overlapping spans: the
/// innermost capture wins, and among captures of the same range the latest
/// pattern wins. Grammars' queries are written that way: a catch-all like
/// `(identifier) @variable` first, refinements after it.
fn flatten(mut captures: Vec<Capture>) -> Vec<(Range<u32>, HighlightId)> {
    captures.sort_by(|a, b| {
        (a.range.start, b.range.end, b.pattern).cmp(&(b.range.start, a.range.end, a.pattern))
    });

    let mut flattener = Flattener::default();
    for mut capture in captures {
        flattener.advance(capture.range.start);
        if let Some(top) = flattener.stack.last() {
            if top.range == capture.range {
                continue;
            }
            capture.range.end = capture.range.end.min(top.range.end);
        }
        flattener.stack.push(capture);
    }
    flattener.advance(u32::MAX);
    flattener.spans
}

#[derive(Default)]
struct Flattener {
    spans: Vec<(Range<u32>, HighlightId)>,
    /// Open captures, outermost first. Syntax nodes nest, so each capture
    /// lies within the one below it.
    stack: Vec<Capture>,
    /// Everything before this has been emitted.
    pos: u32,
}

impl Flattener {
    /// Emits spans up to `target`, closing captures that end before it.
    fn advance(&mut self, target: u32) {
        while let Some(top) = self.stack.last() {
            let id = top.id;
            if top.range.end > target {
                self.emit(self.pos..target, id);
                self.pos = target;
                return;
            }
            let end = top.range.end;
            self.emit(self.pos..end, id);
            self.pos = self.pos.max(end);
            self.stack.pop();
        }
        self.pos = self.pos.max(target);
    }

    fn emit(&mut self, range: Range<u32>, id: HighlightId) {
        if range.is_empty() {
            return;
        }
        match self.spans.last_mut() {
            Some((last, last_id)) if *last_id == id && last.end == range.start => {
                last.end = range.end;
            }
            _ => self.spans.push((range, id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::LanguageRegistry;

    fn id(name: &str) -> HighlightId {
        HighlightId::for_capture(name).unwrap()
    }

    fn capture(range: Range<u32>, pattern: usize, name: &str) -> Capture {
        Capture {
            range,
            pattern,
            id: id(name),
        }
    }

    /// Highlights `text` as `path`'s language, then names the highlight at
    /// each needle's first occurrence.
    fn names_at<'a>(
        path: &str,
        text: &str,
        needles: &[&'a str],
    ) -> Vec<(&'a str, Option<&'static str>)> {
        let language = LanguageRegistry::for_path(path).unwrap();
        let snapshot = highlight(text, language).unwrap();
        assert!(
            snapshot
                .spans
                .windows(2)
                .all(|w| w[0].0.end <= w[1].0.start)
        );
        needles
            .iter()
            .map(|needle| {
                let start = text.find(needle).unwrap() as u32;
                let name = snapshot
                    .spans
                    .iter()
                    .find(|(range, _)| range.contains(&start))
                    .map(|(_, id)| id.name());
                (*needle, name)
            })
            .collect()
    }

    #[test]
    fn innermost_capture_wins() {
        let spans = flatten(vec![
            capture(0..10, 0, "string"),
            capture(3..5, 1, "escape"),
        ]);
        assert_eq!(
            spans,
            [
                (0..3, id("string")),
                (3..5, id("escape")),
                (5..10, id("string"))
            ]
        );
    }

    #[test]
    fn latest_pattern_wins_on_identical_ranges() {
        let spans = flatten(vec![
            capture(0..4, 2, "variable"),
            capture(0..4, 7, "constant"),
            capture(6..8, 0, "number"),
        ]);
        assert_eq!(spans, [(0..4, id("constant")), (6..8, id("number"))]);
    }

    #[test]
    fn highlights_rust() {
        let text = "// hi\nfn main() { let s = \"x\"; }\n";
        assert_eq!(
            names_at("main.rs", text, &["// hi", "fn", "main", "\"x\""]),
            [
                ("// hi", Some("comment")),
                ("fn", Some("keyword")),
                ("main", Some("function")),
                ("\"x\"", Some("string")),
            ]
        );
    }

    #[test]
    fn refinements_override_catch_all_patterns() {
        // Python's query starts with `(identifier) @variable`.
        let text = "TAGS = load(x)\n";
        assert_eq!(
            names_at("a.py", text, &["TAGS", "load", "x"]),
            [
                ("TAGS", Some("constant")),
                ("load", Some("function")),
                ("x", Some("variable")),
            ]
        );
    }
}
