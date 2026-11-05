//! Blocking client for Anthropic's Messages API. Run calls on GPUI's
//! background executor.
//!
//! In tests it answers from a `FakeAnthropic` instead, and never reaches
//! Anthropic.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

#[cfg(test)]
use super::FakeAnthropic;
use super::{AnthropicCredential, AnthropicSource, cli};
use crate::config::{
    ANALYSIS_MODEL, ANTHROPIC_API_URL, ANTHROPIC_OAUTH_BETA, ANTHROPIC_VERSION, USER_AGENT,
};

/// Generous: a large diff is a long prompt, and the model thinks first.
const TIMEOUT: Duration = Duration::from_secs(180);

/// Room for thinking and a grouping of a few hundred hunks. Kept under
/// what a request that doesn't stream can ask for without timing out.
const MAX_TOKENS: u32 = 16_000;

#[derive(Clone)]
pub struct AnthropicClient {
    credential: AnthropicCredential,
    #[cfg(test)]
    fake: Option<std::sync::Arc<FakeAnthropic>>,
}

impl std::fmt::Debug for AnthropicClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AnthropicClient({:?})", self.source())
    }
}

/// An error response from the API.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            401 => write!(
                f,
                "Anthropic didn't accept the credential ({})",
                self.message
            ),
            403 => write!(f, "Anthropic refused the request ({})", self.message),
            429 => write!(f, "Anthropic is rate limiting requests; try again shortly"),
            529 => write!(f, "Anthropic's API is overloaded; try again shortly"),
            status => write!(f, "Anthropic returned {status}: {}", self.message),
        }
    }
}

impl std::error::Error for ApiError {}

impl AnthropicClient {
    pub fn new(credential: AnthropicCredential) -> Self {
        Self {
            credential,
            #[cfg(test)]
            fake: None,
        }
    }

    /// A client that answers from `fake`.
    #[cfg(test)]
    pub fn fake(fake: std::sync::Arc<FakeAnthropic>) -> Self {
        Self {
            credential: AnthropicCredential::Cli,
            fake: Some(fake),
        }
    }

    pub fn source(&self) -> AnthropicSource {
        self.credential.source()
    }

    /// Checks that the credential works and can use the analysis model.
    pub fn verify(&self) -> Result<()> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.verify();
        }
        let request = self.authorize(ureq::get(format!(
            "{ANTHROPIC_API_URL}/v1/models/{ANALYSIS_MODEL}"
        )))?;
        read(request.call()?)?;
        Ok(())
    }

    /// Asks the analysis model for JSON that matches `schema`, with
    /// structured outputs, and returns it parsed.
    pub fn complete_json(&self, system: &str, prompt: &str, schema: &Value) -> Result<Value> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.complete_json(prompt);
        }
        let body = json!({
            "model": ANALYSIS_MODEL,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "messages": [{ "role": "user", "content": prompt }],
            "output_config": {
                // Grouping hunks needs some reasoning, not deep reasoning.
                "effort": "low",
                "format": { "type": "json_schema", "schema": schema },
            },
        });
        let request = self.authorize(ureq::post(format!("{ANTHROPIC_API_URL}/v1/messages")))?;
        let response = read(request.send_json(body)?)?;
        message_json(&response)
    }

    /// Adds the headers every request needs, and the credential: an API key,
    /// or a fresh OAuth token from the Anthropic CLI.
    fn authorize<B>(&self, request: ureq::RequestBuilder<B>) -> Result<ureq::RequestBuilder<B>> {
        if cfg!(test) {
            bail!("tests can't reach Anthropic; connect with `AnthropicClient::fake`");
        }
        let request = request
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build()
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("User-Agent", USER_AGENT);
        Ok(match &self.credential {
            AnthropicCredential::ApiKey(key) => request.header("x-api-key", key.secret()),
            AnthropicCredential::Cli => {
                let token = cli::token()?;
                request
                    .header("Authorization", &format!("Bearer {}", token.secret()))
                    .header("anthropic-beta", ANTHROPIC_OAUTH_BETA)
            }
        })
    }
}

/// A response's JSON body, or the API's error.
fn read(mut response: ureq::http::Response<ureq::Body>) -> Result<Value> {
    let status = response.status().as_u16();
    let body: Value = response
        .body_mut()
        .read_json()
        .with_context(|| format!("decoding Anthropic's response ({status})"))?;
    if !(200..300).contains(&status) {
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or("no details")
            .to_owned();
        return Err(ApiError { status, message }.into());
    }
    Ok(body)
}

/// The JSON a Messages response's text holds. Thinking blocks come first,
/// so text is found by type rather than position.
fn message_json(response: &Value) -> Result<Value> {
    match response["stop_reason"].as_str() {
        Some("refusal") => bail!("Claude declined to analyze this diff"),
        Some("max_tokens") => bail!("the analysis ran out of room; the diff has too many changes"),
        _ => {}
    }
    let text: String = response["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect();
    serde_json::from_str(&text).context("the analysis wasn't the JSON asked for")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_json_from_text_after_thinking() {
        let response = json!({
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "", "signature": "x" },
                { "type": "text", "text": "{\"groups\": []}" },
            ],
        });
        assert_eq!(message_json(&response).unwrap(), json!({ "groups": [] }));

        let refused = json!({ "stop_reason": "refusal", "content": [] });
        assert!(message_json(&refused).is_err());
    }
}
