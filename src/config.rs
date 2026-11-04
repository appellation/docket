//! Static configuration resolved at startup.

/// Client ID of the project's GitHub OAuth App. OAuth client IDs are public,
/// so it's safe to ship in the binary.
const DEFAULT_CLIENT_ID: &str = "Ov23li19k9ydc09DIehC";

/// OAuth scopes requested during sign-in.
///
/// `repo` is required to read and review PRs in private repositories;
/// `read:org` lets us resolve team review requests.
pub const OAUTH_SCOPES: &[&str] = &["repo", "read:org"];

/// The fine-grained personal access token permissions covering what Docket
/// reads, named as GitHub's token settings show them. Each needs
/// "Read-only"; GitHub adds the required "Metadata" itself.
///
/// Fine-grained tokens can't read check runs at all (GitHub limits the
/// Checks API to GitHub Apps), so only commit statuses show for them.
pub const FINE_GRAINED_REPOSITORY_PERMISSIONS: &[&str] =
    &["Pull requests", "Contents", "Commit statuses", "Issues"];
/// For team review requests; only applies when the token's resource owner
/// is an organization.
pub const FINE_GRAINED_ORGANIZATION_PERMISSIONS: &[&str] = &["Members"];

pub const GITHUB_WEB_URL: &str = "https://github.com";
pub const GITHUB_API_URL: &str = "https://api.github.com";

pub const ANTHROPIC_API_URL: &str = "https://api.anthropic.com";
/// The Messages API version Docket's requests are written against.
pub const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Sent with OAuth access tokens (the Anthropic CLI's login) in place of
/// an API key.
pub const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";
/// Where to create an Anthropic API key.
pub const ANTHROPIC_KEYS_URL: &str = "https://platform.claude.com/settings/keys";
/// The model that groups and summarizes a diff's changes: fast and cheap
/// enough to run on every diff opened.
pub const ANALYSIS_MODEL: &str = "claude-haiku-5-5";
pub const ANALYSIS_MODEL_NAME: &str = "Claude Haiku 5.5";

pub const USER_AGENT: &str = concat!("docket/", env!("CARGO_PKG_VERSION"));

/// Service name used for OS keychain entries.
pub const KEYCHAIN_SERVICE: &str = "dev.docket";

/// Directory under the platform config dir (e.g. `~/Library/Application
/// Support` on macOS) where local state such as projects is stored.
pub const APP_DIR_NAME: &str = "dev.docket";

#[derive(Clone, Debug)]
pub struct Config {
    /// Client ID of the GitHub OAuth App. Device flow only needs the client
    /// ID (no secret), which is what lets this app run without a backend.
    pub client_id: String,
}

impl Config {
    /// Uses `DOCKET_CLIENT_ID` if set (at runtime, then compile time),
    /// otherwise the project's default OAuth App.
    pub fn load() -> Self {
        let client_id = std::env::var("DOCKET_CLIENT_ID")
            .ok()
            .or_else(|| option_env!("DOCKET_CLIENT_ID").map(str::to_owned))
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_owned());
        Self { client_id }
    }
}
