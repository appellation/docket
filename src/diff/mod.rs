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
mod viewed;

use std::ops::Range;
use std::sync::Arc;

use crate::syntax::SyntaxSnapshot;

pub use compute::{DiffLine, Excerpt, LineKind, LineOp, OpKind, lines_in, merge_windows, widen};
pub use text::TextSnapshot;
pub use viewed::{ViewKey, Viewed, ViewedStore};

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
    /// What the diff shows of it, to tell when it changes after it's
    /// marked as viewed.
    pub view_key: ViewKey,
}

impl FileDiff {
    /// The path to show: the new one, unless the file was deleted.
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or_default()
    }

    /// The directory `path` is in, or `""` at the top level.
    pub fn directory(&self) -> &str {
        self.path().rsplit_once('/').map_or("", |(dir, _)| dir)
    }

    /// `path`'s last component.
    pub fn name(&self) -> &str {
        let path = self.path();
        path.rsplit_once('/').map_or(path, |(_, name)| name)
    }
}

/// Orders files as the changed-files tree lists them, which is how the
/// file browser lists a tree: in each directory, its subdirectories (by
/// name) and then its files, so `src/ui/view.rs` precedes `src/a.rs`, and
/// top-level files come last. Paths are compared a component at a time, so
/// `src` precedes `src-gen`. Each directory's files, and everything under
/// it, are then contiguous.
pub fn sort_files(files: &mut [FileDiff]) {
    files.sort_by(|a, b| {
        let (a_path, b_path) = (a.path(), b.path());
        let mut a_parts = a_path.split('/').peekable();
        let mut b_parts = b_path.split('/').peekable();
        loop {
            match (a_parts.next(), b_parts.next()) {
                (Some(a_part), Some(b_part)) => {
                    // A part with more after it is a directory.
                    let a_dir = a_parts.peek().is_some();
                    let b_dir = b_parts.peek().is_some();
                    let order = b_dir.cmp(&a_dir).then_with(|| a_part.cmp(b_part));
                    if order.is_ne() {
                        return order;
                    }
                }
                (a_part, b_part) => return a_part.is_some().cmp(&b_part.is_some()),
            }
        }
    });
}

/// `@@ -a,b +c,d @@`, numbered as Git does: 1-based, and an empty range is
/// numbered by the line before it.
pub fn hunk_header(excerpt: &Excerpt) -> String {
    let range = |r: &Range<u32>| match r.len() {
        0 => format!("{},0", r.start),
        1 => format!("{}", r.start + 1),
        len => format!("{},{len}", r.start + 1),
    };
    format!("@@ -{} +{} @@", range(&excerpt.old), range(&excerpt.new))
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
    fn numbers_hunk_headers_like_git() {
        let header = |old: Range<u32>, new: Range<u32>| hunk_header(&Excerpt { old, new });
        assert_eq!(header(1..8, 1..9), "@@ -2,7 +2,8 @@");
        assert_eq!(header(0..0, 0..2), "@@ -0,0 +1,2 @@");
        assert_eq!(header(4..5, 4..5), "@@ -5 +5 @@");
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

    #[test]
    fn sorts_files_as_a_tree() {
        let file = |path: &str| FileDiff {
            old_path: Some(path.to_owned()),
            new_path: Some(path.to_owned()),
            status: FileStatus::Modified,
            additions: 0,
            deletions: 0,
            content: FileContent::Binary,
            view_key: ViewKey::default(),
        };
        // Git's order: bytewise by path.
        let mut files: Vec<_> = [
            "README.md",
            "src-gen/out.rs",
            "src/a.rs",
            "src/ui/view.rs",
            "src/z.rs",
        ]
        .map(file)
        .into();
        sort_files(&mut files);
        let paths: Vec<_> = files.iter().map(FileDiff::path).collect();
        assert_eq!(
            paths,
            [
                "src/ui/view.rs",
                "src/a.rs",
                "src/z.rs",
                "src-gen/out.rs",
                "README.md"
            ]
        );
    }
}
