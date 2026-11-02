//! GitHub authentication.
//!
//! The app has no backend, so it uses the OAuth 2.0 device authorization
//! grant: the user enters a short code on github.com and we poll for the
//! token. The resulting token is persisted in the OS keychain.

mod device_flow;
mod token_store;

pub use device_flow::{DeviceCode, DeviceFlow, PollOutcome};
pub use token_store::TokenStore;

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
