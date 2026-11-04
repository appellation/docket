//! Persists how Docket connects to Anthropic, in the OS keychain beside the
//! GitHub sign-in. Blocking I/O; run on the background executor.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::AnthropicSource;
use crate::auth::{AccessToken, SecretStore};

const ANTHROPIC: SecretStore =
    SecretStore::new("anthropic-credential", "development_anthropic_credentials");

/// A saved connection. The Anthropic CLI's stores no token: Docket asks
/// `ant` for a fresh one on each request, so its refreshes and `ant auth
/// logout` carry over.
#[derive(Clone)]
pub enum AnthropicCredential {
    ApiKey(AccessToken),
    Cli,
}

impl AnthropicCredential {
    pub fn source(&self) -> AnthropicSource {
        match self {
            Self::ApiKey(_) => AnthropicSource::ApiKey,
            Self::Cli => AnthropicSource::Cli,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Stored {
    ApiKey { key: String },
    Cli,
}

pub struct AnthropicStore;

impl AnthropicStore {
    pub fn load() -> Result<Option<AnthropicCredential>> {
        let Some(secret) = ANTHROPIC.get()? else {
            return Ok(None);
        };
        Ok(Some(decode(&secret)?))
    }

    pub fn save(credential: &AnthropicCredential) -> Result<()> {
        ANTHROPIC.set(&encode(credential)?)
    }

    pub fn clear() -> Result<()> {
        ANTHROPIC.delete()
    }
}

fn decode(secret: &str) -> Result<AnthropicCredential> {
    Ok(match serde_json::from_str(secret)? {
        Stored::ApiKey { key } => AnthropicCredential::ApiKey(AccessToken::new(key)),
        Stored::Cli => AnthropicCredential::Cli,
    })
}

fn encode(credential: &AnthropicCredential) -> Result<String> {
    let stored = match credential {
        AnthropicCredential::ApiKey(key) => Stored::ApiKey {
            key: key.secret().to_owned(),
        },
        AnthropicCredential::Cli => Stored::Cli,
    };
    Ok(serde_json::to_string(&stored)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_each_connection() {
        let key = AnthropicCredential::ApiKey(AccessToken::new("sk-ant-secret"));
        let decoded = decode(&encode(&key).unwrap()).unwrap();
        assert!(matches!(decoded, AnthropicCredential::ApiKey(k) if k.secret() == "sk-ant-secret"));

        let cli = encode(&AnthropicCredential::Cli).unwrap();
        assert!(matches!(decode(&cli).unwrap(), AnthropicCredential::Cli));
        // The CLI's token is never stored.
        assert!(!cli.contains("key"));
    }
}
