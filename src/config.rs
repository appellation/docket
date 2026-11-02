//! Static configuration resolved at startup.

/// Client ID of the project's GitHub OAuth App. OAuth client IDs are public,
/// so it's safe to ship in the binary.
const DEFAULT_CLIENT_ID: &str = "Ov23li19k9ydc09DIehC";

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
    /// Uses `GH_REVIEWS_CLIENT_ID` if set (at runtime, then compile time),
    /// otherwise the project's default OAuth App.
    pub fn load() -> Self {
        let client_id = std::env::var("GH_REVIEWS_CLIENT_ID")
            .ok()
            .or_else(|| option_env!("GH_REVIEWS_CLIENT_ID").map(str::to_owned))
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_owned());
        Self { client_id }
    }
}
