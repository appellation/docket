//! Persists how the user signed in, in the OS keychain. Blocking I/O; run on
//! the background executor.

use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::{AccessToken, CredentialSource};
use crate::config::{APP_DIR_NAME, KEYCHAIN_SERVICE};

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
    pub fn load() -> Result<Option<Credential>> {
        Ok(Backend::current()?.get()?.map(decode))
    }

    pub fn save(credential: &Credential) -> Result<()> {
        Backend::current()?.set(&encode(credential)?)
    }

    pub fn clear() -> Result<()> {
        Backend::current()?.delete()
    }
}

/// Where the encoded sign-in is kept.
enum Backend {
    Keychain,
    /// A plaintext file, used by debug builds. macOS grants keychain access
    /// by code signature, and each `cargo build` re-signs the binary ad hoc,
    /// so the keychain would prompt again after every rebuild. Set
    /// `DOCKET_USE_KEYCHAIN` to use the keychain anyway.
    File(PathBuf),
    /// Tests: nothing is read or kept, so they never touch the user's
    /// sign-in.
    Nowhere,
}

impl Backend {
    fn current() -> Result<Self> {
        if cfg!(test) {
            Ok(Self::Nowhere)
        } else if cfg!(debug_assertions) && std::env::var_os("DOCKET_USE_KEYCHAIN").is_none() {
            let dir = dirs::config_dir().context("no config directory on this platform")?;
            Ok(Self::File(
                dir.join(APP_DIR_NAME).join("development_credentials"),
            ))
        } else {
            Ok(Self::Keychain)
        }
    }

    fn get(&self) -> Result<Option<String>> {
        match self {
            Self::Keychain => match keychain_entry()?.get_password() {
                Ok(secret) => Ok(Some(secret)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(err) => Err(err.into()),
            },
            Self::File(path) => match fs::read_to_string(path) {
                Ok(secret) => Ok(Some(secret)),
                Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
                Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
            },
            Self::Nowhere => Ok(None),
        }
    }

    fn set(&self, secret: &str) -> Result<()> {
        match self {
            Self::Keychain => Ok(keychain_entry()?.set_password(secret)?),
            Self::File(path) => write_private(path, secret),
            Self::Nowhere => Ok(()),
        }
    }

    fn delete(&self) -> Result<()> {
        match self {
            Self::Keychain => match keychain_entry()?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(err) => Err(err.into()),
            },
            Self::File(path) => match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
                Err(err) => Err(err).with_context(|| format!("removing {}", path.display())),
            },
            Self::Nowhere => Ok(()),
        }
    }
}

fn keychain_entry() -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(KEYCHAIN_SERVICE, ACCOUNT)?)
}

/// Writes atomically (temp file + rename), readable only by the user.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().expect("credentials path has a parent");
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(&tmp)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
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
