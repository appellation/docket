//! Claude, through Anthropic's API: grouping a diff's changes into logical
//! ones and summarizing each (`analysis.rs`).
//!
//! Like GitHub, Anthropic is reached directly with the user's own
//! credential, with no backend: an API key, kept in the keychain, or the
//! Anthropic CLI's OAuth login (`cli.rs`), since Anthropic doesn't let
//! other apps run an OAuth flow of their own.

mod analysis;
pub mod cache;
mod cli;
mod client;
#[cfg(test)]
mod fake;
mod store;

pub use analysis::{AnalysisKey, AnalysisRequest, ChangeGroup, DiffAnalysis, HunkRef};
pub use client::{AnthropicClient, ApiError};
#[cfg(test)]
pub use fake::FakeAnthropic;
pub use store::{AnthropicCredential, AnthropicStore};

/// How Docket connects to Anthropic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnthropicSource {
    ApiKey,
    /// The Anthropic CLI's login (`ant auth login`).
    Cli,
}
