//! Persists how the user signed in, in the OS keychain.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{AccessToken, CredentialSource};
use crate::config::KEYCHAIN_SERVICE;

const ACCOUNT: &str = "github-oauth-token";

/// A saved sign-in. GitHub CLI sign-ins store no token: it's read from `gh`
/// on each launch, so `gh auth refresh` and `gh auth logout` carry over.
pub enum Credential {
    OAuth(AccessToken),
    PersonalAccessToken(AccessToken),
    GithubCli,
}

impl Credential {
    pub fn source(&self) -> CredentialSource {
        match self {
            Self::OAuth(_) => CredentialSource::OAuth,
            Self::PersonalAccessToken(_) => CredentialSource::PersonalAccessToken,
            Self::GithubCli => CredentialSource::GithubCli,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Stored {
    Oauth { token: String },
    PersonalAccessToken { token: String },
    GithubCli,
}

pub struct TokenStore;

impl TokenStore {
    fn entry() -> Result<keyring::Entry> {
        Ok(keyring::Entry::new(KEYCHAIN_SERVICE, ACCOUNT)?)
    }

    pub fn load() -> Result<Option<Credential>> {
        match Self::entry()?.get_password() {
            Ok(secret) => Ok(Some(decode(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub fn save(credential: &Credential) -> Result<()> {
        Self::entry()?.set_password(&encode(credential)?)?;
        Ok(())
    }

    pub fn clear() -> Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

fn decode(secret: String) -> Credential {
    match serde_json::from_str(&secret) {
        Ok(Stored::Oauth { token }) => Credential::OAuth(AccessToken::new(token)),
        Ok(Stored::PersonalAccessToken { token }) => {
            Credential::PersonalAccessToken(AccessToken::new(token))
        }
        Ok(Stored::GithubCli) => Credential::GithubCli,
        // Entries from before sign-in methods were tracked are a bare
        // OAuth token.
        Err(_) => Credential::OAuth(AccessToken::new(secret)),
    }
}

fn encode(credential: &Credential) -> Result<String> {
    let stored = match credential {
        Credential::OAuth(token) => Stored::Oauth {
            token: token.secret().to_owned(),
        },
        Credential::PersonalAccessToken(token) => Stored::PersonalAccessToken {
            token: token.secret().to_owned(),
        },
        Credential::GithubCli => Stored::GithubCli,
    };
    Ok(serde_json::to_string(&stored)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(credential: Credential) -> Credential {
        decode(encode(&credential).unwrap())
    }

    #[test]
    fn round_trips_each_sign_in_method() {
        let token = || AccessToken::new("secret");
        assert!(
            matches!(round_trip(Credential::OAuth(token())), Credential::OAuth(t) if t.secret() == "secret")
        );
        assert!(matches!(
            round_trip(Credential::PersonalAccessToken(token())),
            Credential::PersonalAccessToken(t) if t.secret() == "secret"
        ));
        assert!(matches!(
            round_trip(Credential::GithubCli),
            Credential::GithubCli
        ));
        // The GitHub CLI's token is never stored.
        assert!(!encode(&Credential::GithubCli).unwrap().contains("token"));
    }

    #[test]
    fn reads_bare_tokens_from_older_versions_as_oauth() {
        assert!(
            matches!(decode("gho_abc123".into()), Credential::OAuth(t) if t.secret() == "gho_abc123")
        );
    }
}
