//! Remembers that the user chose to group changes with Claude Code, in the
//! OS keychain beside the GitHub sign-in. Claude Code keeps its own
//! sign-in, so there's no secret here, only the choice. Blocking I/O; run
//! on the background executor.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::auth::SecretStore;

/// The entry earlier versions kept an Anthropic API key or the Anthropic
/// CLI's login in, reused so connecting overwrites a saved key.
const CLAUDE: SecretStore =
    SecretStore::new("anthropic-credential", "development_anthropic_credentials");

/// What was saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavedConnection {
    ClaudeCode,
    /// An API key or the Anthropic CLI's login, from before analysis ran
    /// in Claude Code: the user chose to group changes, but with
    /// something Docket no longer uses.
    Legacy,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Stored {
    ClaudeCode,
    ApiKey {},
    Cli,
}

pub struct ClaudeStore;

impl ClaudeStore {
    pub fn load() -> Result<Option<SavedConnection>> {
        let Some(secret) = CLAUDE.get()? else {
            return Ok(None);
        };
        Ok(Some(decode(&secret)?))
    }

    pub fn save() -> Result<()> {
        CLAUDE.set(&serde_json::to_string(&Stored::ClaudeCode)?)
    }

    pub fn clear() -> Result<()> {
        CLAUDE.delete()
    }
}

fn decode(secret: &str) -> Result<SavedConnection> {
    Ok(match serde_json::from_str(secret)? {
        Stored::ClaudeCode => SavedConnection::ClaudeCode,
        Stored::ApiKey {} | Stored::Cli => SavedConnection::Legacy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_this_and_earlier_versions_saved() {
        let saved = serde_json::to_string(&Stored::ClaudeCode).unwrap();
        assert_eq!(decode(&saved).unwrap(), SavedConnection::ClaudeCode);
        let key = r#"{"kind":"api_key","key":"sk-ant-secret"}"#;
        assert_eq!(decode(key).unwrap(), SavedConnection::Legacy);
        assert_eq!(
            decode(r#"{"kind":"cli"}"#).unwrap(),
            SavedConnection::Legacy
        );
    }
}
