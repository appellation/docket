//! Code intelligence from language servers, for reading diffs.
//!
//! Servers run against Docket's own worktree of the project (see
//! `worktree.rs`), checked out at the PR's head, so a line on the diff's new
//! side is the same line on disk. Each server is a child process speaking
//! JSON-RPC over stdio (`client.rs`); which servers Docket knows and where it
//! looks for them is in `servers.rs`. A file's symbols, which give the
//! scopes a diff excerpt sits in, are modeled in `symbols.rs`. Blocking I/O runs on threads the client
//! owns or on the background executor, never the UI thread.

mod client;
mod servers;
mod symbols;
pub mod worktree;

pub use client::{LanguageServer, Position, ServerActivity, ServerEvent};
pub use servers::{Missing, Resolved, ServerSpec, language_id, server_for};
pub use symbols::{Outline, SymbolKind};
