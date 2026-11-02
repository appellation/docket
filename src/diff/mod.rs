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

pub use compute::{DiffLine, Excerpt, LineKind, LineOp, lines_in};
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

    pub fn stats(&self) -> (u32, u32) {
        compute::stats(&self.ops)
    }
}
