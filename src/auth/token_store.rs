//! Persists the access token in the OS keychain.

use anyhow::Result;

use super::AccessToken;
use crate::config::KEYCHAIN_SERVICE;

const ACCOUNT: &str = "github-oauth-token";

pub struct TokenStore;

impl TokenStore {
    fn entry() -> Result<keyring::Entry> {
        Ok(keyring::Entry::new(KEYCHAIN_SERVICE, ACCOUNT)?)
    }

    pub fn load() -> Result<Option<AccessToken>> {
        match Self::entry()?.get_password() {
            Ok(token) => Ok(Some(AccessToken::new(token))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub fn save(token: &AccessToken) -> Result<()> {
        Self::entry()?.set_password(token.secret())?;
        Ok(())
    }

    pub fn clear() -> Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}
