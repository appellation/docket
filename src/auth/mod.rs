//! GitHub authentication.
//!
//! The app has no backend, so by default it uses the OAuth 2.0 device
//! authorization grant: the user enters a short code on github.com and we
//! poll for the token.
//!
//! Orgs can block OAuth apps they haven't approved, and an OAuth token is
//! always tied to the app that requested it. So users in those orgs can sign
//! in as themselves instead, with a personal access token or the GitHub
//! CLI's existing login. The sign-in is persisted in the OS keychain.

mod device_flow;
pub mod github_cli;
mod token_store;

pub use device_flow::{DeviceCode, DeviceFlow, PollOutcome};
pub use token_store::{Credential, SecretStore, TokenStore};

/// How the user signed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    /// Docket's OAuth App, via device flow.
    OAuth,
    PersonalAccessToken,
    GithubCli,
}

impl CredentialSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::OAuth => "Docket",
            Self::PersonalAccessToken => "personal access token",
            Self::GithubCli => "GitHub CLI",
        }
    }
}

/// A GitHub OAuth access token. `Debug` is redacted so it never ends up in logs.
#[derive(Clone)]
pub struct AccessToken(String);

impl AccessToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}
