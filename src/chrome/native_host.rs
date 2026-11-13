//! Docket as its Chrome extension's native messaging host. The browser
//! starts Docket's executable with the extension's origin as an argument,
//! which `main` checks for before starting the app, and they talk over
//! stdio: each message is JSON, after its length as a native-endian `u32`.
//! The extension sends one message per process.
//!
//! - `{"type": "hello"}` answers `{"version": …}`, the extension's version
//!   in this Docket, after writing its files again where they've changed,
//!   so the extension can reload when Docket updated it.
//! - `{"type": "open", "url": …, "always": false}` opens a pull request's
//!   URL in Docket if [`OpenLinks`] says to (or `always`), and answers
//!   `{"opened": …}` once the app has (over [`socket`], launching it if
//!   it isn't running), with the app's `error` when it couldn't.
//!
//! Stdout is the browser's, so nothing else may print to it.

use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{EXTENSION_ID, Install, OpenLinks, socket, version};
use crate::github::PullRequestLink;
use crate::updater;

/// More than any message the extension sends.
const MAX_MESSAGE: u32 = 1 << 20;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Hello,
    Open {
        url: String,
        #[serde(default)]
        always: bool,
    },
}

/// The extension's origin, when the browser started Docket as its host.
pub fn origin_arg() -> Option<String> {
    std::env::args()
        .skip(1)
        .find(|arg| arg.starts_with("chrome-extension://"))
}

/// Answers the extension at `origin` until the browser closes stdin; the
/// process's exit code. Any other extension is refused.
pub fn run(origin: &str) -> i32 {
    if origin.trim_end_matches('/') != format!("chrome-extension://{EXTENSION_ID}") {
        eprintln!("docket: not Docket's extension: {origin}");
        return 1;
    }
    let result = serve(
        io::stdin().lock(),
        io::stdout().lock(),
        OpenLinks::load,
        open_in_docket,
    );
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("docket: {err:#}");
            1
        }
    }
}

fn serve(
    mut input: impl Read,
    mut output: impl Write,
    links: impl Fn() -> OpenLinks,
    mut open: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    while let Some(message) = read_message(&mut input)? {
        let reply = match serde_json::from_slice(&message) {
            Ok(request) => answer(request, &links, &mut open),
            Err(err) => json!({ "error": err.to_string() }),
        };
        write_message(&mut output, &reply)?;
    }
    Ok(())
}

fn answer(
    request: Request,
    links: &impl Fn() -> OpenLinks,
    open: &mut impl FnMut(&str) -> Result<()>,
) -> Value {
    match request {
        Request::Hello => {
            if let Some(Err(err)) = Install::current().map(|install| install.refresh()) {
                eprintln!("docket: updating the extension: {err:#}");
            }
            json!({ "version": version() })
        }
        Request::Open { url, always } => {
            let Some(link) = PullRequestLink::parse(&url) else {
                return json!({ "opened": false });
            };
            if !always && !links().opens(&link) {
                return json!({ "opened": false });
            }
            match open(&url) {
                Ok(()) => json!({ "opened": true }),
                Err(err) => json!({ "opened": false, "error": format!("{err:#}") }),
            }
        }
    }
}

/// The next message, or `None` once the browser closes stdin.
fn read_message(input: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut len = [0; 4];
    match input.read_exact(&mut len) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err).context("reading a message's length"),
    }
    let len = u32::from_ne_bytes(len);
    if len > MAX_MESSAGE {
        bail!("a message of {len} bytes is too long");
    }
    let mut message = vec![0; len as usize];
    input
        .read_exact(&mut message)
        .context("reading a message")?;
    Ok(Some(message))
}

fn write_message(output: &mut impl Write, message: &Value) -> Result<()> {
    let json = serde_json::to_vec(message)?;
    let len = u32::try_from(json.len()).context("reply too long")?;
    output.write_all(&len.to_ne_bytes())?;
    output.write_all(&json)?;
    output.flush()?;
    Ok(())
}

/// Has the running Docket open `url`, launching it first if it isn't.
fn open_in_docket(url: &str) -> Result<()> {
    let path = socket::path().context("Docket has no data directory")?;
    socket::open(&path, url, launch)
}

/// Launches this bundle, where the host runs from one, so it's the Docket
/// that installed it; or else this executable, as the app.
fn launch() -> Result<()> {
    let quiet = |command: &mut Command| {
        // Not to the browser's stdout.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    };
    match updater::running_bundle() {
        Some(bundle) => {
            let mut command = Command::new("open");
            command.arg("-a").arg(bundle);
            quiet(&mut command);
            let status = command.status().context("running open")?;
            if !status.success() {
                bail!("open exited with {status}");
            }
        }
        None => {
            let mut command = Command::new(std::env::current_exe()?);
            quiet(&mut command);
            // Out of the host's process group, which the browser may end
            // along with the host.
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
            command.spawn().context("starting Docket")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::github::RepoRef;

    fn framed(messages: &[Value]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for message in messages {
            write_message(&mut bytes, message).unwrap();
        }
        bytes
    }

    fn replies(mut bytes: &[u8]) -> Vec<Value> {
        let mut replies = Vec::new();
        while let Some(reply) = read_message(&mut bytes).unwrap() {
            replies.push(serde_json::from_slice(&reply).unwrap());
        }
        replies
    }

    #[test]
    fn opens_relevant_pull_requests() {
        let repo: RepoRef = "acme/widgets".parse().unwrap();
        let links = || OpenLinks::new([&repo], []);
        let opened = RefCell::new(Vec::new());
        let input = framed(&[
            json!({ "type": "hello" }),
            json!({ "type": "open", "url": "https://github.com/acme/widgets/pull/7/files" }),
            json!({ "type": "open", "url": "https://github.com/other/thing/pull/1" }),
            json!({ "type": "open", "url": "https://github.com/other/thing/pull/1", "always": true }),
            json!({ "type": "open", "url": "https://github.com/acme/widgets/issues/7", "always": true }),
            json!({ "type": "launch" }),
        ]);
        let mut output = Vec::new();
        serve(&input[..], &mut output, links, |url| {
            opened.borrow_mut().push(url.to_owned());
            Ok(())
        })
        .unwrap();

        let replies = replies(&output);
        assert_eq!(replies[0], json!({ "version": version() }));
        assert_eq!(replies[1], json!({ "opened": true }));
        assert_eq!(replies[2], json!({ "opened": false }), "not relevant");
        assert_eq!(replies[3], json!({ "opened": true }), "asked for");
        assert_eq!(replies[4], json!({ "opened": false }), "not a pull request");
        assert!(replies[5]["error"].is_string());
        assert_eq!(
            opened.into_inner(),
            [
                "https://github.com/acme/widgets/pull/7/files",
                "https://github.com/other/thing/pull/1",
            ]
        );
    }

    #[test]
    fn says_when_opening_fails() {
        let input = framed(&[
            json!({ "type": "open", "url": "https://github.com/a/b/pull/1", "always": true }),
        ]);
        let mut output = Vec::new();
        serve(&input[..], &mut output, OpenLinks::default, |_| {
            bail!("no Docket")
        })
        .unwrap();
        assert_eq!(
            replies(&output),
            [json!({ "opened": false, "error": "no Docket" })]
        );
    }

    #[test]
    fn refuses_other_extensions_and_long_messages() {
        assert_eq!(
            run("chrome-extension://abcdefghijklmnopabcdefghijklmnop/"),
            1
        );
        let mut input = &(MAX_MESSAGE + 1).to_ne_bytes()[..];
        assert!(read_message(&mut input).is_err());
    }
}
