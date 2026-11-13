//! How the extension's host reaches the running app: a Unix socket in
//! Docket's data directory. The host connects, writes a request as one line
//! of JSON, and reads the app's answer, one line back, once the app has
//! opened the pull request or knows it can't. So the extension hears
//! whether Docket really opened it, not only that something was sent to
//! Docket, and leaves the page in the browser when it didn't.
//!
//! The app listens from launch ([`listen`]). When nothing answers, the host
//! launches Docket and waits for it to ([`open`]). The socket opens only
//! pull requests, which any app can already ask of Docket with a
//! `docket://` link.

use std::fs;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use futures::channel::mpsc;
use serde::{Deserialize, Serialize};

use crate::config::APP_DIR_NAME;

/// How long a launched Docket has to start listening.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the host tries the socket while Docket launches.
const LAUNCH_POLL: Duration = Duration::from_millis(100);
/// How long the app has to answer: restoring its sign-in on a launch, then
/// asking GitHub for the pull request.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the app waits for the host's request once it connects.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// What the host asks of the app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// A pull request's URL.
    pub url: String,
    /// Whether the host launched Docket for it, so the window Docket opened
    /// as it launched can show it rather than a new tab.
    #[serde(default)]
    pub launched: bool,
}

/// The app's answer.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Reply {
    opened: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// A request that reached the app, with where its answer goes: `Ok` once
/// the pull request is open, or why it isn't.
pub struct OpenRequest {
    pub request: Request,
    pub reply: std_mpsc::Sender<Result<(), String>>,
}

/// Where the app listens: in its data directory, so a debug build and a
/// bundle share it. Never in tests, which use a temp dir.
pub fn path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(dirs::data_dir()?.join(APP_DIR_NAME).join("extension.sock"))
}

/// Listens at `path`, replacing whatever was there (a socket left by a
/// Docket that quit, or one still running: the newest Docket answers), and
/// sends each request to the app on the returned channel. Each connection
/// is served on its own thread.
pub fn listen(path: &Path) -> Result<mpsc::UnboundedReceiver<OpenRequest>> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("removing {}", path.display())),
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("listening at {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).ok();
    let (requests, received) = mpsc::unbounded();
    thread::Builder::new()
        .name("extension-socket".into())
        .spawn(move || {
            for stream in listener.incoming() {
                if requests.is_closed() {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let sender = requests.clone();
                thread::spawn(move || {
                    if let Err(err) = serve(stream, &sender) {
                        eprintln!("docket: answering the Chrome extension: {err:#}");
                    }
                });
            }
        })
        .context("starting the extension's socket")?;
    Ok(received)
}

fn serve(stream: UnixStream, requests: &mpsc::UnboundedSender<OpenRequest>) -> Result<()> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line).context("reading the request")?;
    let (reply, answer) = std_mpsc::channel();
    let result = match requests.unbounded_send(OpenRequest { request, reply }) {
        Ok(()) => answer
            .recv_timeout(REPLY_TIMEOUT)
            .unwrap_or_else(|_| Err("Docket didn't open it in time".into())),
        Err(_) => Err("Docket is quitting".into()),
    };
    let reply = match result {
        Ok(()) => Reply {
            opened: true,
            error: None,
        },
        Err(error) => Reply {
            opened: false,
            error: Some(error),
        },
    };
    let mut stream = &stream;
    serde_json::to_writer(&mut stream, &reply)?;
    stream.write_all(b"\n")?;
    Ok(())
}

/// Has the app listening at `path` open `url`, launching it with `launch`
/// if nothing's listening. `Ok` once the app says it's open.
pub fn open(path: &Path, url: &str, launch: impl FnOnce() -> Result<()>) -> Result<()> {
    let (stream, launched) = match UnixStream::connect(path) {
        Ok(stream) => (stream, false),
        Err(_) => {
            launch().context("launching Docket")?;
            (wait_for(path)?, true)
        }
    };
    ask(
        stream,
        &Request {
            url: url.to_owned(),
            launched,
        },
    )
}

/// Connects once a launching Docket listens.
fn wait_for(path: &Path) -> Result<UnixStream> {
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(_) if Instant::now() < deadline => thread::sleep(LAUNCH_POLL),
            Err(_) => bail!("Docket didn't start"),
        }
    }
}

fn ask(stream: UnixStream, request: &Request) -> Result<()> {
    stream.set_read_timeout(Some(REPLY_TIMEOUT + REQUEST_TIMEOUT))?;
    let mut writer = &stream;
    serde_json::to_writer(&mut writer, request)?;
    writer.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .context("waiting for Docket")?;
    if line.is_empty() {
        bail!("Docket closed the connection");
    }
    let reply: Reply = serde_json::from_str(&line).context("reading Docket's answer")?;
    match (reply.opened, reply.error) {
        (true, _) => Ok(()),
        (false, error) => Err(anyhow!(
            error.unwrap_or_else(|| "Docket didn't open it".into())
        )),
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;

    use super::*;

    /// Answers each request at `path` with `answer`, recording it.
    fn app(
        path: &Path,
        answer: impl Fn(&Request) -> Result<(), String> + Send + 'static,
    ) -> std_mpsc::Receiver<Request> {
        let mut requests = listen(path).unwrap();
        let (seen, received) = std_mpsc::channel();
        thread::spawn(move || {
            while let Some(OpenRequest { request, reply }) =
                futures::executor::block_on(requests.next())
            {
                reply.send(answer(&request)).ok();
                seen.send(request).ok();
            }
        });
        received
    }

    const URL: &str = "https://github.com/acme/widgets/pull/7";

    #[test]
    fn asks_the_running_app() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let seen = app(&path, |_| Ok(()));
        open(&path, URL, || panic!("it's running")).unwrap();
        assert_eq!(
            seen.recv().unwrap(),
            Request {
                url: URL.into(),
                launched: false
            }
        );
    }

    #[test]
    fn says_why_the_app_didnt_open_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        app(&path, |_| Err("acme/widgets#7 not found".into()));
        let err = open(&path, URL, || panic!("it's running")).unwrap_err();
        assert_eq!(err.to_string(), "acme/widgets#7 not found");
    }

    #[test]
    fn launches_the_app_when_nothing_answers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        // Something left at the path, as by a Docket that quit. (Not a
        // dropped listener: another test's child process can briefly
        // inherit it between fork and exec, and accept the connection.)
        fs::write(&path, "").unwrap();
        let (launched, seen) = std_mpsc::channel();
        let launch_path = path.clone();
        open(&path, URL, move || {
            // Starts listening a moment later, as a launching app does.
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(250));
                launched.send(app(&launch_path, |_| Ok(()))).ok();
            });
            Ok(())
        })
        .unwrap();
        let request = seen.recv().unwrap().recv().unwrap();
        assert!(request.launched);

        let err = open(&dir.path().join("none.sock"), URL, || bail!("no Docket")).unwrap_err();
        assert_eq!(format!("{err:#}"), "launching Docket: no Docket");
    }
}
