//! OAuth device flow: https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow
//!
//! These calls block; run them on GPUI's background executor.

use std::time::Duration;

use anyhow::{Result, bail};
use serde::Deserialize;

use super::AccessToken;
use crate::config::{Config, GITHUB_WEB_URL, OAUTH_SCOPES};

const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Code the user must enter at `verification_uri`.
#[derive(Clone, Debug, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Result of a single poll of the token endpoint.
#[derive(Debug)]
pub enum PollOutcome {
    Authorized(AccessToken),
    /// User hasn't finished yet; poll again after the given interval.
    Pending {
        interval: Duration,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TokenResponse {
    Success {
        access_token: String,
    },
    Error {
        error: String,
        interval: Option<u64>,
    },
}

#[derive(Clone)]
pub struct DeviceFlow {
    client_id: String,
}

impl DeviceFlow {
    pub fn new(config: &Config) -> Self {
        Self {
            client_id: config.client_id.clone(),
        }
    }

    /// Step 1: request a device + user code.
    pub fn request_code(&self) -> Result<DeviceCode> {
        let scope = OAUTH_SCOPES.join(" ");
        let code = ureq::post(format!("{GITHUB_WEB_URL}/login/device/code"))
            .header("Accept", "application/json")
            .send_form([("client_id", self.client_id.as_str()), ("scope", &scope)])?
            .body_mut()
            .read_json::<DeviceCode>()?;
        Ok(code)
    }

    /// Step 2: poll once for the access token.
    pub fn poll(&self, code: &DeviceCode) -> Result<PollOutcome> {
        let response = ureq::post(format!("{GITHUB_WEB_URL}/login/oauth/access_token"))
            .header("Accept", "application/json")
            .send_form([
                ("client_id", self.client_id.as_str()),
                ("device_code", &code.device_code),
                ("grant_type", DEVICE_GRANT_TYPE),
            ])?
            .body_mut()
            .read_json::<TokenResponse>()?;

        match response {
            TokenResponse::Success { access_token } => {
                Ok(PollOutcome::Authorized(AccessToken::new(access_token)))
            }
            TokenResponse::Error { error, interval } => match error.as_str() {
                "authorization_pending" => Ok(PollOutcome::Pending {
                    interval: Duration::from_secs(code.interval),
                }),
                "slow_down" => Ok(PollOutcome::Pending {
                    interval: Duration::from_secs(interval.unwrap_or(code.interval + 5)),
                }),
                other => bail!("device flow failed: {other}"),
            },
        }
    }
}
