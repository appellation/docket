//! Static configuration resolved at startup.

use anyhow::{Context as _, Result};

/// OAuth scopes requested during sign-in.
///
/// `repo` is required to read and review PRs in private repositories;
/// `read:org` lets us resolve team review requests.
pub const OAUTH_SCOPES: &[&str] = &["repo", "read:org"];

pub const GITHUB_WEB_URL: &str = "https://github.com";
pub const GITHUB_API_URL: &str = "https://api.github.com";

/// Service name used for OS keychain entries.
pub const KEYCHAIN_SERVICE: &str = "dev.gh-reviews";

#[derive(Clone, Debug)]
pub struct Config {
    /// Client ID of the GitHub OAuth App. Device flow only needs the client
    /// ID (no secret), which is what lets this app run without a backend.
    pub client_id: String,
}

impl Config {
    /// Reads the OAuth client ID from `GH_REVIEWS_CLIENT_ID` at runtime,
    /// falling back to a value baked in at compile time.
    pub fn load() -> Result<Self> {
        let client_id = std::env::var("GH_REVIEWS_CLIENT_ID")
            .ok()
            .or_else(|| option_env!("GH_REVIEWS_CLIENT_ID").map(str::to_owned))
            .filter(|id| !id.is_empty())
            .context(
                "GH_REVIEWS_CLIENT_ID is not set; create a GitHub OAuth App with \
                 device flow enabled and export its client ID",
            )?;
        Ok(Self { client_id })
    }
}
