//! Claude, run through Claude Code: grouping a diff's changes into logical
//! ones and summarizing each (`analysis.rs`).
//!
//! Like GitHub, Claude is reached with the user's own sign-in, with no
//! backend: Docket runs the user's `claude` (`claude_code.rs`), which
//! reads the code around a diff in a worktree at its head.

mod analysis;
pub mod cache;
mod claude_code;
#[cfg(test)]
mod fake;
mod store;

pub use analysis::{
    AnalysisKey, AnalysisRequest, AnalysisSource, ChangeGroup, DiffAnalysis, HunkRef,
};
pub use claude_code::ClaudeCode;
#[cfg(test)]
pub use fake::FakeClaude;
pub use store::{ClaudeStore, SavedConnection};
