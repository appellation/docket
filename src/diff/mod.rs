//! Pull request diffs: what changed in each file, with both sides' full
//! text.
//!
//! This is the model the diff view renders and later features build on:
//! files keep their whole old and new text, the line alignment covers the
//! whole file, and [`Excerpt`]s choose what's visible. Diffs are loaded from
//! the project's local checkout (see `git.rs`).

mod compute;
pub mod git;
mod text;

use std::sync::Arc;

use crate::syntax::SyntaxSnapshot;

pub use compute::{DiffLine, Excerpt, LineKind, LineOp, OpKind, lines_in};
pub use text::TextSnapshot;

/// Changes from the merge base of a PR's base and head to its head, as
/// GitHub shows them.
pub struct PullRequestDiff {
    pub head_oid: String,
    pub files: Vec<FileDiff>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    /// `similarity` is Git's percentage estimate.
    Renamed {
        similarity: u8,
    },
    Copied {
        similarity: u8,
    },
    /// Changed between a file, symlink, or submodule.
    TypeChanged,
}

pub struct FileDiff {
    /// `None` when the file was added.
    pub old_path: Option<String>,
    /// `None` when the file was deleted.
    pub new_path: Option<String>,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
    pub content: FileContent,
}

impl FileDiff {
    /// The path to show: the new one, unless the file was deleted.
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or_default()
    }
}

pub enum FileContent {
    Text(TextContent),
    Binary,
    Submodule,
    TooLarge { bytes: u64 },
}

pub struct TextContent {
    /// `None` when that side doesn't exist (an added or deleted file).
    pub old: Option<Arc<TextSnapshot>>,
    pub new: Option<Arc<TextSnapshot>>,
    pub ops: Vec<LineOp>,
    pub excerpts: Vec<Excerpt>,
    /// Filled in once background highlighting finishes; until then the
    /// lines render as plain text.
    pub old_syntax: Option<Arc<SyntaxSnapshot>>,
    pub new_syntax: Option<Arc<SyntaxSnapshot>>,
}

impl TextContent {
    pub fn new(old: Option<TextSnapshot>, new: Option<TextSnapshot>) -> Self {
        let empty = TextSnapshot::empty();
        let ops = compute::align(
            old.as_ref().unwrap_or(&empty),
            new.as_ref().unwrap_or(&empty),
        );
        let excerpts = compute::excerpts(&ops, compute::CONTEXT_LINES);
        Self {
            old: old.map(Arc::new),
            new: new.map(Arc::new),
            ops,
            excerpts,
            old_syntax: None,
            new_syntax: None,
        }
    }

    /// Where on the new side to look for `excerpt`'s scopes: the line of
    /// its first change, and its first visible line. `None` for a deleted
    /// file.
    pub fn scope_anchor(&self, excerpt: &Excerpt) -> Option<(u32, u32)> {
        self.new.as_ref()?;
        let visible = &excerpt.new;
        let change = self
            .ops
            .iter()
            .find(|op| op.kind != OpKind::Equal && op.old.start >= excerpt.old.start)
            .map_or(visible.start, |op| op.new.start);
        // A deletion at the end of the excerpt starts just past it.
        let line = change.min(visible.end.saturating_sub(1)).max(visible.start);
        Some((line, visible.start))
    }

    pub fn stats(&self) -> (u32, u32) {
        compute::stats(&self.ops)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(old: Option<&str>, new: Option<&str>) -> TextContent {
        let snapshot = |s: &str| TextSnapshot::new(s.as_bytes().to_vec(), "a.rs");
        TextContent::new(old.map(snapshot), new.map(snapshot))
    }

    fn numbered(lines: u32) -> String {
        (1..=lines).map(|n| format!("{n}\n")).collect()
    }

    #[test]
    fn anchors_scopes_at_the_first_change() {
        let old = numbered(20);
        let text = content(Some(&old), Some(&old.replace("10\n", "ten\n")));
        // Context starts three lines above the change on line 9.
        assert_eq!(text.scope_anchor(&text.excerpts[0]), Some((9, 6)));

        // A deletion at the very end anchors on the last visible line.
        let text = content(Some(&old), Some(&numbered(19)));
        assert_eq!(text.scope_anchor(&text.excerpts[0]), Some((18, 16)));

        // A deleted file has no new side to ask about.
        let text = content(Some(&old), None);
        assert_eq!(text.scope_anchor(&text.excerpts[0]), None);
    }
}
