//! A minimal LSP client: JSON-RPC over a child process's stdio.
//!
//! A reader thread routes responses to pending requests, answers the few
//! requests servers make of clients, and turns progress notifications into
//! [`ServerEvent`]s. Requests are futures, so they can be awaited on any
//! executor. Sending one writes to the server's stdin, which can block if
//! the server stops reading, so send from the background executor.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures::channel::{mpsc, oneshot};
use serde_json::{Value, json};

use super::servers::{Resolved, ServerSpec, search_path};
use super::symbols::Outline;

/// Lines of stderr kept to explain a server that exits.
const STDERR_LINES: usize = 20;

/// How LSP positions count characters within a line, negotiated at
/// startup. UTF-16 is the protocol's default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

impl PositionEncoding {
    /// `offset`, a byte offset into `line`, as an LSP character.
    pub fn character(self, line: &str, offset: usize) -> u32 {
        let prefix = &line[..offset.min(line.len())];
        match self {
            Self::Utf8 => prefix.len() as u32,
            Self::Utf16 => prefix.encode_utf16().count() as u32,
        }
    }

    /// An LSP character on `line` as a byte offset, clamped to the line.
    pub fn offset(self, line: &str, character: u32) -> usize {
        let character = character as usize;
        match self {
            Self::Utf8 => {
                let mut offset = character.min(line.len());
                while !line.is_char_boundary(offset) {
                    offset -= 1;
                }
                offset
            }
            Self::Utf16 => {
                let mut units = 0;
                for (offset, c) in line.char_indices() {
                    if units >= character {
                        return offset;
                    }
                    units += c.len_utf16();
                }
                line.len()
            }
        }
    }
}

/// A zero-based LSP position, in the server's [`PositionEncoding`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// Where something is defined, from a definition request.
#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    pub path: PathBuf,
    /// What the server points at: usually the definition's name.
    pub range: (Position, Position),
    /// The whole definition, doc comments and body included, when the
    /// server says (as a `LocationLink`); otherwise `range`.
    pub extent: (Position, Position),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hover {
    /// The hover's contents as Markdown.
    pub markdown: String,
    /// What the hover describes, if the server said.
    pub range: Option<(Position, Position)>,
}

/// Whether the server is busy (indexing, loading the workspace), from its
/// progress notifications.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerActivity {
    pub busy: bool,
    /// What it's doing, e.g. "Indexing".
    pub message: Option<String>,
}

pub enum ServerEvent {
    Activity(ServerActivity),
    /// The process exited; carries the end of its stderr.
    Exited(String),
}

type PendingRequests = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

/// Sends messages to a server and matches up its responses.
struct Connection {
    stdin: Arc<Mutex<ChildStdin>>,
    pending: PendingRequests,
    next_id: AtomicI64,
}

impl Connection {
    fn request(
        &self,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value>> + Send + use<> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let sent = write_message(&self.stdin, &message);
        if sent.is_err() {
            self.pending.lock().unwrap().remove(&id);
        }
        let method = method.to_owned();
        async move {
            sent.with_context(|| format!("couldn't send {method}"))?;
            match rx.await {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(message)) => Err(anyhow!("{method} failed: {message}")),
                Err(_) => Err(anyhow!("the language server exited")),
            }
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        write_message(&self.stdin, &message).with_context(|| format!("couldn't send {method}"))
    }
}

pub struct LanguageServer {
    pub spec: &'static ServerSpec,
    connection: Connection,
    child: Option<Child>,
    pub encoding: PositionEncoding,
    hover_provider: bool,
    document_symbol_provider: bool,
    definition_provider: bool,
    /// Documents sent with `didOpen`: the version last sent, and a hash of
    /// its text. In a worktree the text never changes, but a browsed
    /// checkout's files can between refreshes.
    opened: Mutex<HashMap<PathBuf, (i64, u64)>>,
}

impl LanguageServer {
    /// Starts `spec`'s server as `resolved` says, in `root`, and waits for
    /// it to initialize. Progress and exit are reported on `events`.
    pub async fn start(
        spec: &'static ServerSpec,
        resolved: Resolved,
        root: PathBuf,
        events: mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<Self> {
        // Killed if initializing fails or times out (dropping this future).
        let mut child = KillOnDrop(Some(
            Command::new(&resolved.binary)
                .args(&resolved.args)
                .envs(resolved.env.iter().map(|(key, value)| (key, value)))
                .current_dir(&root)
                .env("PATH", search_path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("couldn't run {}", resolved.binary.display()))?,
        ));
        let child_mut = child.0.as_mut().expect("just spawned");
        let stdin = Arc::new(Mutex::new(child_mut.stdin.take().expect("stdin is piped")));
        let stdout = child_mut.stdout.take().expect("stdout is piped");
        let stderr = child_mut.stderr.take().expect("stderr is piped");

        let pending = PendingRequests::default();
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_thread = thread::Builder::new()
            .name(format!("{} stderr", spec.name))
            .spawn({
                let tail = stderr_tail.clone();
                move || {
                    for line in BufReader::new(stderr).lines() {
                        let Ok(line) = line else { break };
                        let mut tail = tail.lock().unwrap();
                        if tail.len() == STDERR_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    }
                }
            })?;
        thread::Builder::new()
            .name(format!("{} stdout", spec.name))
            .spawn({
                let stdin = stdin.clone();
                let pending = pending.clone();
                let configuration = resolved.configuration.clone();
                move || {
                    read_messages(stdout, &stdin, &pending, &configuration, &events);
                    // Fail whatever is still waiting.
                    pending.lock().unwrap().clear();
                    stderr_thread.join().ok();
                    let tail = Vec::from(std::mem::take(&mut *stderr_tail.lock().unwrap()));
                    events
                        .unbounded_send(ServerEvent::Exited(tail.join("\n")))
                        .ok();
                }
            })?;

        let connection = Connection {
            stdin,
            pending,
            next_id: AtomicI64::new(1),
        };
        let uri = file_uri(&root);
        let name = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let init = connection
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "clientInfo": {"name": "Docket", "version": env!("CARGO_PKG_VERSION")},
                    "rootUri": uri,
                    "rootPath": root,
                    "workspaceFolders": [{"uri": uri, "name": name}],
                    "initializationOptions": resolved.initialization_options,
                    "capabilities": {
                        "general": {"positionEncodings": ["utf-8", "utf-16"]},
                        "textDocument": {
                            "hover": {"contentFormat": ["markdown", "plaintext"]},
                            "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                            "definition": {"linkSupport": true},
                            "synchronization": {},
                        },
                        "window": {"workDoneProgress": true},
                        "workspace": {"workspaceFolders": true, "configuration": true},
                        // rust-analyzer's richer busy signal.
                        "experimental": {"serverStatusNotification": true},
                    },
                }),
            )
            .await?;
        connection.notify("initialized", json!({}))?;

        let capabilities = &init["capabilities"];
        let provides = |capability: &str| {
            !matches!(capabilities[capability], Value::Null | Value::Bool(false))
        };
        Ok(Self {
            spec,
            connection,
            child: child.0.take(),
            encoding: match capabilities["positionEncoding"].as_str() {
                Some("utf-8") => PositionEncoding::Utf8,
                _ => PositionEncoding::Utf16,
            },
            hover_provider: provides("hoverProvider"),
            document_symbol_provider: provides("documentSymbolProvider"),
            definition_provider: provides("definitionProvider"),
            opened: Mutex::new(HashMap::new()),
        })
    }

    /// Tells the server about a document before asking about it. Later
    /// calls for the same path send nothing, unless the text changed.
    pub fn open(&self, path: &Path, language_id: &str, text: &str) -> Result<()> {
        let hash = {
            let mut hasher = std::hash::DefaultHasher::new();
            std::hash::Hash::hash(text, &mut hasher);
            std::hash::Hasher::finish(&hasher)
        };
        let mut opened = self.opened.lock().unwrap();
        if let Some((version, sent)) = opened.get_mut(path) {
            if *sent == hash {
                return Ok(());
            }
            *version += 1;
            *sent = hash;
            // The whole text, which servers accept whatever sync they
            // asked for.
            return self.connection.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": file_uri(path), "version": *version},
                    "contentChanges": [{"text": text}],
                }),
            );
        }
        opened.insert(path.to_owned(), (1, hash));
        self.connection.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": file_uri(path),
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        )
    }

    /// Hover information at `position` in an opened document.
    pub fn hover(
        &self,
        path: &Path,
        position: Position,
    ) -> impl Future<Output = Result<Option<Hover>>> + Send + use<> {
        let request = self.hover_provider.then(|| {
            self.connection.request(
                "textDocument/hover",
                json!({
                    "textDocument": {"uri": file_uri(path)},
                    "position": {"line": position.line, "character": position.character},
                }),
            )
        });
        async move {
            match request {
                Some(request) => Ok(parse_hover(&request.await?)),
                None => Ok(None),
            }
        }
    }

    /// The symbols in an opened document, nested.
    pub fn outline(&self, path: &Path) -> impl Future<Output = Result<Outline>> + Send + use<> {
        let request = self.document_symbol_provider.then(|| {
            self.connection.request(
                "textDocument/documentSymbol",
                json!({"textDocument": {"uri": file_uri(path)}}),
            )
        });
        let name = self.spec.name;
        async move {
            let request = request.ok_or_else(|| anyhow!("{name} doesn't list symbols"))?;
            Ok(Outline::from_response(request.await?)?)
        }
    }

    /// Where the symbol at `position` in an opened document is defined.
    /// Usually one place; none if the server doesn't know.
    pub fn definition(
        &self,
        path: &Path,
        position: Position,
    ) -> impl Future<Output = Result<Vec<Location>>> + Send + use<> {
        let request = self.definition_provider.then(|| {
            self.connection.request(
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": file_uri(path)},
                    "position": {"line": position.line, "character": position.character},
                }),
            )
        });
        async move {
            match request {
                Some(request) => Ok(parse_locations(&request.await?)),
                None => Ok(Vec::new()),
            }
        }
    }
}

impl Drop for LanguageServer {
    /// Asks the server to exit, then kills it if it hasn't within a couple
    /// of seconds. Runs on a thread, since writing to a stuck server blocks.
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let stdin = self.connection.stdin.clone();
        thread::spawn(move || {
            let message = |message: Value| write_message(&stdin, &message).ok();
            message(json!({"jsonrpc": "2.0", "id": 0, "method": "shutdown"}));
            message(json!({"jsonrpc": "2.0", "method": "exit"}));
            for _ in 0..20 {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            child.kill().ok();
            child.wait().ok();
        });
    }
}

/// A server process that hasn't finished starting.
struct KillOnDrop(Option<Child>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            child.kill().ok();
            child.wait().ok();
        }
    }
}

fn write_message(stdin: &Mutex<ChildStdin>, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    let mut stdin = stdin.lock().unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(&body)?;
    stdin.flush()
}

/// Reads one `Content-Length`-framed message, or `None` at the end of the
/// stream.
fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "message without Content-Length")
    })?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

/// The reader thread's loop: runs until the server closes stdout.
fn read_messages(
    stdout: impl Read,
    stdin: &Mutex<ChildStdin>,
    pending: &PendingRequests,
    configuration: &Value,
    events: &mpsc::UnboundedSender<ServerEvent>,
) {
    let mut reader = BufReader::new(stdout);
    let mut progress = Progress::default();
    loop {
        let message = match read_message(&mut reader) {
            Ok(Some(message)) => message,
            Ok(None) => return,
            Err(err) => {
                eprintln!("language server sent an unreadable message: {err}");
                return;
            }
        };
        match (message.get("id"), message["method"].as_str()) {
            // A response to one of our requests.
            (Some(id), None) => {
                let Some(sender) = id
                    .as_i64()
                    .and_then(|id| pending.lock().unwrap().remove(&id))
                else {
                    continue;
                };
                let result = match message.get("error") {
                    Some(error) => Err(error["message"].as_str().unwrap_or("error").to_owned()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                sender.send(result).ok();
            }
            // A request from the server.
            (Some(id), Some(method)) => {
                let response = match method {
                    "workspace/configuration" => {
                        let items = message["params"]["items"].as_array();
                        let result: Vec<Value> = items
                            .into_iter()
                            .flatten()
                            .map(|item| configuration_section(configuration, &item["section"]))
                            .collect();
                        json!({"jsonrpc": "2.0", "id": id, "result": result})
                    }
                    "window/workDoneProgress/create"
                    | "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/showMessageRequest"
                    | "workspace/workspaceFolders" => {
                        json!({"jsonrpc": "2.0", "id": id, "result": null})
                    }
                    _ => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": format!("{method} is unsupported")},
                    }),
                };
                write_message(stdin, &response).ok();
            }
            (None, Some(method)) => {
                if progress.notification(method, &message["params"]) {
                    events
                        .unbounded_send(ServerEvent::Activity(progress.activity()))
                        .ok();
                }
            }
            (None, None) => {}
        }
    }
}

/// Tracks `$/progress` tasks and rust-analyzer's `experimental/serverStatus`.
#[derive(Default)]
struct Progress {
    /// Open work-done progress, by token, with its title.
    tasks: Vec<(String, String)>,
    /// From `serverStatus`: whether rust-analyzer has finished loading.
    quiescent: Option<bool>,
}

impl Progress {
    /// Applies a notification; returns whether the activity changed.
    fn notification(&mut self, method: &str, params: &Value) -> bool {
        let before = self.activity();
        match method {
            "$/progress" => {
                let token = params["token"].to_string();
                let value = &params["value"];
                match value["kind"].as_str() {
                    Some("begin") => {
                        let title = value["title"].as_str().unwrap_or_default().to_owned();
                        self.tasks.push((token, title));
                    }
                    Some("end") => self.tasks.retain(|(t, _)| *t != token),
                    _ => {}
                }
            }
            "experimental/serverStatus" => self.quiescent = params["quiescent"].as_bool(),
            _ => {}
        }
        self.activity() != before
    }

    fn activity(&self) -> ServerActivity {
        ServerActivity {
            busy: self.quiescent == Some(false) || !self.tasks.is_empty(),
            message: self
                .tasks
                .last()
                .map(|(_, title)| title.clone())
                .filter(|title| !title.is_empty()),
        }
    }
}

/// A `workspace/configuration` item: the dotted `section` of the server's
/// settings (`python.analysis`), the whole of them if there's no section,
/// or null if they don't have it.
fn configuration_section(configuration: &Value, section: &Value) -> Value {
    let Some(section) = section.as_str() else {
        return configuration.clone();
    };
    section
        .split('.')
        .try_fold(configuration, |value, key| value.get(key))
        .cloned()
        .unwrap_or(Value::Null)
}

fn parse_hover(result: &Value) -> Option<Hover> {
    let markdown = hover_markdown(result.get("contents")?).trim().to_owned();
    if markdown.is_empty() {
        return None;
    }
    Some(Hover {
        markdown,
        range: parse_range(&result["range"]),
    })
}

fn parse_position(value: &Value) -> Option<Position> {
    Some(Position {
        line: value["line"].as_u64()? as u32,
        character: value["character"].as_u64()? as u32,
    })
}

fn parse_range(value: &Value) -> Option<(Position, Position)> {
    parse_position(&value["start"]).zip(parse_position(&value["end"]))
}

/// A definition answer is a `Location`, a list of them, or a list of
/// `LocationLink`s, which also give the whole definition's range.
fn parse_locations(result: &Value) -> Vec<Location> {
    let items = match result {
        Value::Array(items) => items.as_slice(),
        Value::Null => &[],
        single => std::slice::from_ref(single),
    };
    items
        .iter()
        .filter_map(|item| {
            let (uri, range, extent) = match item.get("targetUri") {
                Some(uri) => (uri, &item["targetSelectionRange"], &item["targetRange"]),
                None => (&item["uri"], &item["range"], &item["range"]),
            };
            let range = parse_range(range)?;
            Some(Location {
                path: path_from_uri(uri.as_str()?)?,
                range,
                extent: parse_range(extent).unwrap_or(range),
            })
        })
        .collect()
}

/// Hover contents are `MarkupContent`, a `MarkedString` (Markdown, or code
/// in a language), or a list of `MarkedString`s.
fn hover_markdown(contents: &Value) -> String {
    match contents {
        Value::String(markdown) => markdown.clone(),
        Value::Array(items) => items
            .iter()
            .map(hover_markdown)
            .filter(|item| !item.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(object) => {
            let value = object.get("value").and_then(Value::as_str).unwrap_or("");
            match object.get("language").and_then(Value::as_str) {
                Some(language) => format!("```{language}\n{value}\n```"),
                None => value.to_owned(),
            }
        }
        _ => String::new(),
    }
}

/// A `file://` URI for an absolute path.
pub fn file_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !path.starts_with('/') {
        uri.push('/');
    }
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                uri.push(byte as char)
            }
            _ => write!(uri, "%{byte:02X}").unwrap(),
        }
    }
    uri
}

/// The path a `file://` URI names, or `None` for any other scheme.
pub fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let mut bytes = Vec::with_capacity(rest.len());
    let mut input = rest.bytes();
    while let Some(byte) = input.next() {
        if byte != b'%' {
            bytes.push(byte);
            continue;
        }
        let hex = [input.next()?, input.next()?];
        bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
    }
    let path = String::from_utf8(bytes).ok()?;
    // `file:///C:/...` on Windows.
    let path = match path.as_bytes() {
        [b'/', drive, b':', ..] if cfg!(windows) && drive.is_ascii_alphabetic() => &path[1..],
        _ => &path,
    };
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_framed_messages() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let stream = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{body}",
            body.len()
        );
        let mut reader = io::Cursor::new(stream.into_bytes());
        let message = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(message["id"], 1);
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    #[test]
    fn converts_positions_between_encodings() {
        let line = "let é = \"😀\"; x";
        let x = line.find('x').unwrap();
        assert_eq!(PositionEncoding::Utf8.character(line, x), x as u32);
        // `é` is one UTF-16 unit and `😀` is two.
        assert_eq!(PositionEncoding::Utf16.character(line, x), 14);
        assert_eq!(PositionEncoding::Utf16.offset(line, 14), x);
        assert_eq!(PositionEncoding::Utf8.offset(line, 5), 4);
        assert_eq!(PositionEncoding::Utf16.offset(line, 99), line.len());
    }

    #[test]
    fn flattens_hover_contents_to_markdown() {
        let markup = json!({"contents": {"kind": "markdown", "value": "```rust\nfn a()\n```"}});
        assert_eq!(
            parse_hover(&markup).unwrap().markdown,
            "```rust\nfn a()\n```"
        );

        let marked = json!({
            "contents": [{"language": "go", "value": "func A()"}, "", "Docs."],
            "range": {"start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 5}},
        });
        let hover = parse_hover(&marked).unwrap();
        assert_eq!(hover.markdown, "```go\nfunc A()\n```\n\nDocs.");
        assert_eq!(hover.range.unwrap().1.character, 5);

        assert_eq!(parse_hover(&json!({"contents": ""})), None);
        assert_eq!(parse_hover(&Value::Null), None);
    }

    #[test]
    fn answers_configuration_by_section() {
        let configuration = json!({"python": {"pythonPath": "/p/python"}});
        let section = |s: Value| configuration_section(&configuration, &s);
        assert_eq!(section(json!("python.pythonPath")), json!("/p/python"));
        assert_eq!(section(json!("python.analysis")), Value::Null);
        assert_eq!(section(Value::Null), configuration);
    }

    #[test]
    fn tracks_server_activity() {
        let mut progress = Progress::default();
        let begin = json!({"token": "t", "value": {"kind": "begin", "title": "Indexing"}});
        assert!(progress.notification("$/progress", &begin));
        assert_eq!(progress.activity().message.as_deref(), Some("Indexing"));
        let end = json!({"token": "t", "value": {"kind": "end"}});
        assert!(progress.notification("$/progress", &end));
        assert!(!progress.activity().busy);
        assert!(progress.notification("experimental/serverStatus", &json!({"quiescent": false})));
        assert!(progress.activity().busy);
    }

    /// Resolves and runs a real server, and hovers a symbol until it
    /// answers. By default, rust-analyzer on this crate:
    /// `cargo test -- --ignored hovers_with_a_real_server`. To check
    /// another project, set `DOCKET_HOVER_CHECK=<root>|<file>|<symbol>`
    /// (the file relative to the root), and `DOCKET_HOVER_EXPECT` to wait
    /// for a hover containing something other than the symbol.
    #[test]
    #[ignore = "needs an installed language server, and indexes a project"]
    fn hovers_with_a_real_server() {
        let check = std::env::var("DOCKET_HOVER_CHECK").unwrap_or_else(|_| {
            format!(
                "{}|src/lsp/servers.rs|server_for",
                env!("CARGO_MANIFEST_DIR")
            )
        });
        let [root, file, symbol] = check.split('|').collect::<Vec<_>>()[..] else {
            panic!("DOCKET_HOVER_CHECK is <root>|<file>|<symbol>");
        };
        let root = PathBuf::from(root);
        let language = crate::syntax::LanguageRegistry::for_path(file).unwrap();
        let spec = super::super::server_for(language).unwrap();
        let resolved = spec.resolve(&root).expect("the server is installed");
        eprintln!(
            "running {} ({}) with {}",
            resolved.binary.display(),
            resolved.source,
            resolved.initialization_options
        );
        let expect = std::env::var("DOCKET_HOVER_EXPECT").unwrap_or_else(|_| symbol.to_owned());
        let path = root.join(file);
        let text = std::fs::read_to_string(&path).unwrap();
        let (line, column) = text
            .lines()
            .enumerate()
            .find_map(|(ix, line)| Some((ix, line.find(symbol)? + 1)))
            .unwrap();

        let (events, _rx) = mpsc::unbounded();
        let server =
            futures::executor::block_on(LanguageServer::start(spec, resolved, root, events))
                .unwrap();
        let language_id = super::super::language_id(file, language);
        server.open(&path, language_id, &text).unwrap();
        let position = Position {
            line: line as u32,
            character: server
                .encoding
                .character(text.lines().nth(line).unwrap(), column),
        };
        // Empty, or partial, until the project has loaded.
        let start = std::time::Instant::now();
        let mut last = None;
        for _ in 0..300 {
            if let Ok(Some(hover)) = futures::executor::block_on(server.hover(&path, position)) {
                if last.as_ref() != Some(&hover.markdown) {
                    eprintln!("{:?}: {}", start.elapsed(), hover.markdown);
                }
                if hover.markdown.contains(&expect) {
                    return;
                }
                last = Some(hover.markdown);
            }
            thread::sleep(Duration::from_secs(1));
        }
        panic!("no hover containing {expect:?} within five minutes");
    }

    /// Runs rust-analyzer on this crate and asks where a call in one file
    /// is defined in another, until it's loaded enough to say:
    /// `cargo test -- --ignored finds_definitions_with_a_real_server`.
    #[test]
    #[ignore = "needs rust-analyzer, and indexes this crate"]
    fn finds_definitions_with_a_real_server() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let file = "src/state/code_intel.rs";
        let call = "server_for(language)";
        let language = crate::syntax::LanguageRegistry::for_path(file).unwrap();
        let spec = super::super::server_for(language).unwrap();
        let resolved = spec.resolve(&root).expect("rust-analyzer is installed");
        let path = root.join(file);
        let text = std::fs::read_to_string(&path).unwrap();
        let (line, column) = text
            .lines()
            .enumerate()
            .find_map(|(ix, line)| Some((ix, line.find(call)?)))
            .unwrap();
        let defined_in = root.join("src/lsp/servers.rs");
        let defined_at = std::fs::read_to_string(&defined_in)
            .unwrap()
            .lines()
            .position(|line| line.starts_with("pub fn server_for("))
            .unwrap() as u32;

        let (events, _rx) = mpsc::unbounded();
        let server =
            futures::executor::block_on(LanguageServer::start(spec, resolved, root, events))
                .unwrap();
        server
            .open(&path, super::super::language_id(file, language), &text)
            .unwrap();
        let position = Position {
            line: line as u32,
            character: server
                .encoding
                .character(text.lines().nth(line).unwrap(), column + 1),
        };
        for _ in 0..300 {
            let found = futures::executor::block_on(server.definition(&path, position));
            if let Ok([location, ..]) = found.as_deref() {
                eprintln!("{location:?}");
                assert_eq!(location.path, defined_in);
                assert_eq!(location.range.0.line, defined_at);
                assert!(location.extent.1.line > location.extent.0.line);
                return;
            }
            thread::sleep(Duration::from_secs(1));
        }
        panic!("no definition within five minutes");
    }

    #[test]
    fn encodes_file_uris() {
        assert_eq!(
            file_uri(Path::new("/Users/a b/src/lib.rs")),
            "file:///Users/a%20b/src/lib.rs"
        );
    }

    #[test]
    fn decodes_file_uris() {
        let path = Path::new("/Users/a b/ünï/[id].ts");
        assert_eq!(path_from_uri(&file_uri(path)).as_deref(), Some(path));
        assert_eq!(
            path_from_uri("file://localhost/tmp/a%2Bb.rs").as_deref(),
            Some(Path::new("/tmp/a+b.rs"))
        );
        assert_eq!(path_from_uri("untitled:Untitled-1"), None);
        assert_eq!(path_from_uri("file:///bad%2"), None);
    }

    #[test]
    fn parses_definition_locations_and_links() {
        let range = |line: u32, start: u32, end: u32| json!({"start": {"line": line, "character": start}, "end": {"line": line, "character": end}});
        let location = json!({"uri": "file:///src/a.rs", "range": range(3, 4, 7)});
        let parsed = parse_locations(&location);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path, Path::new("/src/a.rs"));
        assert_eq!(parsed[0].range.0.character, 4);
        assert_eq!(parsed[0].extent, parsed[0].range);

        let links = json!([{
            "targetUri": "file:///src/b.rs",
            "targetRange": {"start": {"line": 1, "character": 0}, "end": {"line": 9, "character": 1}},
            "targetSelectionRange": range(2, 7, 10),
        }]);
        let parsed = parse_locations(&links);
        assert_eq!(parsed[0].path, Path::new("/src/b.rs"));
        assert_eq!(parsed[0].range.0.line, 2);
        assert_eq!((parsed[0].extent.0.line, parsed[0].extent.1.line), (1, 9));

        assert!(parse_locations(&Value::Null).is_empty());
        assert!(parse_locations(&json!([{"uri": "file:///a"}])).is_empty());
    }
}
