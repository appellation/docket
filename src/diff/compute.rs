//! Line alignment between two versions of a file, and which parts of it a
//! diff shows.
//!
//! The alignment covers the whole file; [`Excerpt`]s pick the visible
//! ranges. Keeping them separate means expanding context (or revealing a
//! whole function) only edits the excerpts.

use std::ops::Range;
use std::time::{Duration, Instant};

use similar::{Algorithm, DiffOp};

use super::TextSnapshot;

/// Lines of unchanged context shown around each change, as in `git diff`.
pub const CONTEXT_LINES: u32 = 3;

/// Gives up on an optimal diff after this long and settles for a coarser
/// one, so a pathological file can't stall the load.
const DIFF_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Equal,
    Delete,
    Insert,
}

/// A run of aligned lines. `Delete` has an empty `new` range at the point
/// of deletion, and `Insert` an empty `old` range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineOp {
    pub kind: OpKind,
    pub old: Range<u32>,
    pub new: Range<u32>,
}

/// A visible window of the file, the same lines on both sides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Excerpt {
    pub old: Range<u32>,
    pub new: Range<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// One line of a unified diff. Line numbers are 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

/// Aligns every line of `old` and `new`.
pub fn align(old: &TextSnapshot, new: &TextSnapshot) -> Vec<LineOp> {
    let old_lines = old.lines_with_endings();
    let new_lines = new.lines_with_endings();
    let deadline = Instant::now() + DIFF_TIMEOUT;
    let ops = similar::capture_diff_slices_deadline(
        Algorithm::Myers,
        &old_lines,
        &new_lines,
        Some(deadline),
    );

    let range = |start: usize, len: usize| start as u32..(start + len) as u32;
    let mut aligned = Vec::with_capacity(ops.len());
    for op in ops {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => aligned.push(LineOp {
                kind: OpKind::Equal,
                old: range(old_index, len),
                new: range(new_index, len),
            }),
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => aligned.push(LineOp {
                kind: OpKind::Delete,
                old: range(old_index, old_len),
                new: range(new_index, 0),
            }),
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => aligned.push(LineOp {
                kind: OpKind::Insert,
                old: range(old_index, 0),
                new: range(new_index, new_len),
            }),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                aligned.push(LineOp {
                    kind: OpKind::Delete,
                    old: range(old_index, old_len),
                    new: range(new_index, 0),
                });
                aligned.push(LineOp {
                    kind: OpKind::Insert,
                    old: range(old_index + old_len, 0),
                    new: range(new_index, new_len),
                });
            }
        }
    }
    aligned
}

/// Each change with `context` lines around it, merging windows that overlap
/// or touch.
pub fn excerpts(ops: &[LineOp], context: u32) -> Vec<Excerpt> {
    let equal_len = |ix: Option<usize>| {
        ix.and_then(|ix| ops.get(ix))
            .filter(|op| op.kind == OpKind::Equal)
            .map_or(0, |op| op.old.len() as u32)
    };

    let mut excerpts: Vec<Excerpt> = Vec::new();
    for (ix, op) in ops.iter().enumerate() {
        if op.kind == OpKind::Equal {
            continue;
        }
        let lead = context.min(equal_len(ix.checked_sub(1)));
        let trail = context.min(equal_len(Some(ix + 1)));
        let excerpt = Excerpt {
            old: op.old.start - lead..op.old.end + trail,
            new: op.new.start - lead..op.new.end + trail,
        };
        match excerpts.last_mut() {
            Some(last) if excerpt.old.start <= last.old.end => {
                last.old.end = last.old.end.max(excerpt.old.end);
                last.new.end = last.new.end.max(excerpt.new.end);
            }
            _ => excerpts.push(excerpt),
        }
    }
    excerpts
}

/// The unified diff lines within `excerpt`: removals before additions
/// where a block was replaced, as Git shows them.
pub fn lines_in(ops: &[LineOp], excerpt: &Excerpt) -> Vec<DiffLine> {
    let clip = |range: &Range<u32>, window: &Range<u32>| {
        range.start.max(window.start)..range.end.min(window.end)
    };
    let mut lines = Vec::new();
    for op in ops {
        match op.kind {
            OpKind::Equal => {
                let offset = op.new.start as i64 - op.old.start as i64;
                for old in clip(&op.old, &excerpt.old) {
                    lines.push(DiffLine {
                        kind: LineKind::Context,
                        old_line: Some(old),
                        new_line: Some((old as i64 + offset) as u32),
                    });
                }
            }
            OpKind::Delete => {
                lines.extend(clip(&op.old, &excerpt.old).map(|old| DiffLine {
                    kind: LineKind::Removed,
                    old_line: Some(old),
                    new_line: None,
                }));
            }
            OpKind::Insert => {
                lines.extend(clip(&op.new, &excerpt.new).map(|new| DiffLine {
                    kind: LineKind::Added,
                    old_line: None,
                    new_line: Some(new),
                }));
            }
        }
    }
    lines
}

/// Added and removed line counts.
pub fn stats(ops: &[LineOp]) -> (u32, u32) {
    ops.iter()
        .fold((0, 0), |(added, removed), op| match op.kind {
            OpKind::Equal => (added, removed),
            OpKind::Delete => (added, removed + op.old.len() as u32),
            OpKind::Insert => (added + op.new.len() as u32, removed),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> TextSnapshot {
        TextSnapshot::new(s.as_bytes().to_vec(), "a.txt")
    }

    fn numbered(lines: u32) -> String {
        (1..=lines).map(|n| format!("{n}\n")).collect()
    }

    /// Renders the excerpts like a unified diff body, for readable asserts.
    fn render(old: &str, new: &str, context: u32) -> Vec<String> {
        let (old, new) = (text(old), text(new));
        let ops = align(&old, &new);
        excerpts(&ops, context)
            .iter()
            .flat_map(|excerpt| {
                let header = format!(
                    "@@ -{} +{} @@",
                    excerpt.old.start + 1,
                    excerpt.new.start + 1
                );
                std::iter::once(header).chain(lines_in(&ops, excerpt).into_iter().map(|line| {
                    match line.kind {
                        LineKind::Context => format!(" {}", new.line(line.new_line.unwrap())),
                        LineKind::Added => format!("+{}", new.line(line.new_line.unwrap())),
                        LineKind::Removed => format!("-{}", old.line(line.old_line.unwrap())),
                    }
                }))
            })
            .collect()
    }

    #[test]
    fn replaced_line_with_context() {
        let old = numbered(10);
        let new = old.replace("5\n", "five\n");
        assert_eq!(
            render(&old, &new, 3),
            [
                "@@ -2 +2 @@",
                " 2",
                " 3",
                " 4",
                "-5",
                "+five",
                " 6",
                " 7",
                " 8"
            ]
        );
    }

    #[test]
    fn added_and_deleted_files() {
        assert_eq!(render("", "a\nb\n", 3), ["@@ -1 +1 @@", "+a", "+b"]);
        assert_eq!(render("a\nb\n", "", 3), ["@@ -1 +1 @@", "-a", "-b"]);
        let (old, new) = (text("a\nb\n"), text("a\nb\n"));
        assert_eq!(stats(&align(&old, &new)), (0, 0));
        assert!(excerpts(&align(&old, &new), 3).is_empty());
    }

    #[test]
    fn missing_trailing_newline_is_a_change() {
        assert_eq!(
            render("a\nb", "a\nb\n", 3),
            ["@@ -1 +1 @@", " a", "-b", "+b"]
        );
    }

    #[test]
    fn nearby_changes_merge_and_distant_ones_split() {
        let old = numbered(20);
        // Six unchanged lines between: the contexts touch, so one excerpt.
        let near = old.replace("3\n", "x\n").replace("10\n", "y\n");
        assert_eq!(
            render(&old, &near, 3)
                .iter()
                .filter(|l| l.starts_with("@@"))
                .count(),
            1
        );
        // Seven between: two excerpts.
        let far = old.replace("3\n", "x\n").replace("11\n", "y\n");
        assert_eq!(
            render(&old, &far, 3)
                .iter()
                .filter(|l| l.starts_with("@@"))
                .count(),
            2
        );
    }

    #[test]
    fn counts_stats() {
        let (old, new) = (text("a\nb\nc\n"), text("a\nB\nc\nd\n"));
        assert_eq!(stats(&align(&old, &new)), (2, 1));
    }
}
