//! The HTTP client GPUI uses to load remote images, such as avatars. GPUI
//! ships without one, so `img(url)` fails until the app provides it.
//!
//! Like the rest of the app's network calls this uses blocking `ureq`, here
//! on a thread per request so GPUI's async image loader can await it.

use std::any::type_name;

use anyhow::Result;
use futures::channel::oneshot;
use futures::future::BoxFuture;
use futures::{AsyncReadExt as _, FutureExt as _};
use gpui::http_client::http::{HeaderValue, Request, Response};
use gpui::http_client::{AsyncBody, HttpClient, RedirectPolicy, Url};

use crate::config::USER_AGENT;

/// Large enough for any avatar; ureq's default is 10 MB.
const MAX_BODY_BYTES: u64 = 20 * 1024 * 1024;
/// ureq's default limit, for `RedirectPolicy::FollowAll`.
const MAX_REDIRECTS: u32 = 10;

pub struct UreqHttpClient {
    agent: ureq::Agent,
    user_agent: HeaderValue,
}

impl UreqHttpClient {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            // Callers inspect the status themselves.
            .http_status_as_error(false)
            // Return the redirect response when not following it.
            .max_redirects_will_error(false)
            .build()
            .into();
        Self {
            agent,
            user_agent: HeaderValue::from_static(USER_AGENT),
        }
    }
}

impl HttpClient for UreqHttpClient {
    fn type_name(&self) -> &'static str {
        type_name::<Self>()
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(&self, request: Request<AsyncBody>) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
        let agent = self.agent.clone();
        async move {
            let (parts, mut body) = request.into_parts();
            let mut bytes = Vec::new();
            body.read_to_end(&mut bytes).await?;
            let max_redirects = match parts.extensions.get::<RedirectPolicy>() {
                Some(RedirectPolicy::FollowAll) => MAX_REDIRECTS,
                Some(RedirectPolicy::FollowLimit(limit)) => *limit,
                Some(RedirectPolicy::NoFollow) | None => 0,
            };
            let request = Request::from_parts(parts, bytes);

            let (tx, rx) = oneshot::channel();
            std::thread::spawn(move || {
                let result = (|| {
                    let request = agent
                        .configure_request(request)
                        .max_redirects(max_redirects)
                        .build();
                    let (parts, body) = agent.run(request)?.into_parts();
                    let bytes = body
                        .into_with_config()
                        .limit(MAX_BODY_BYTES)
                        .read_to_vec()?;
                    Ok(Response::from_parts(parts, AsyncBody::from(bytes)))
                })();
                tx.send(result).ok();
            });
            rx.await?
        }
        .boxed()
    }
}
