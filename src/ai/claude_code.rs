//! Running Claude Code (`claude -p`) headlessly: asking it about code in a
//! directory, with read-only tools, and getting back JSON that matches a
//! schema. Blocking; run on the background executor.
//!
//! Claude Code signs in on its own (`claude auth login`, or whatever the
//! user set it up with), so Docket keeps no credential: it only finds
//! `claude` and checks that it's signed in. Each run is locked down, since
//! the code it reads is under review and may be anyone's:
//!
//! - `--restricted` drops the tools that run commands, ignores the
//!   directory's (and the user's) settings files, so a repository's hooks
//!   and permission rules don't apply, and keeps file tools inside the
//!   directory.
//! - `--tools Read,Grep,Glob` leaves only those, `--permission-mode
//!   dontAsk` denies anything not already allowed rather than prompting,
//!   and `--strict-mcp-config` loads no MCP servers.
//! - `--no-session-persistence` keeps runs out of the user's session list.
//!
//! In tests it answers from a `FakeClaude` instead, and never runs.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::Value;

#[cfg(test)]
use super::FakeClaude;
use crate::config::ANALYSIS_MODEL;

/// Generous: Claude reads around the diff before answering, and a large
/// diff is a long prompt.
const TIMEOUT: Duration = Duration::from_secs(600);

/// How often a run checks whether it's finished or been cancelled.
const POLL: Duration = Duration::from_millis(100);

/// Where `claude` usually lives, besides `PATH`. Apps launched from
/// Finder get a minimal `PATH` that excludes Homebrew and `~/.local/bin`
/// (the native installer's).
const FALLBACK_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];
const HOME_FALLBACK_DIRS: &[&str] = &[".local/bin", ".claude/local", ".npm-global/bin"];

const INSTALL_HINT: &str = "Claude Code isn't installed. Install it from \
    claude.com/claude-code, sign in by running `claude`, then try again.";

#[derive(Clone)]
pub struct ClaudeCode {
    #[cfg(test)]
    fake: Option<std::sync::Arc<FakeClaude>>,
}

impl std::fmt::Debug for ClaudeCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClaudeCode")
    }
}

impl ClaudeCode {
    pub fn new() -> Self {
        Self {
            #[cfg(test)]
            fake: None,
        }
    }

    /// Claude Code that answers from `fake`.
    #[cfg(test)]
    pub fn fake(fake: std::sync::Arc<FakeClaude>) -> Self {
        Self { fake: Some(fake) }
    }

    /// Checks that `claude` is installed and signed in.
    pub fn verify(&self) -> Result<()> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.verify();
        }
        let output = command()?
            .args(["auth", "status"])
            .stdin(Stdio::null())
            .output()
            .context("couldn't run Claude Code")?;
        // Prints JSON either way; exits non-zero when signed out.
        let status: Value = serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "couldn't read Claude Code's sign-in status ({})",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?;
        if status["loggedIn"] != Value::Bool(true) {
            bail!("Claude Code isn't signed in. Run `claude auth login`, then try again.");
        }
        Ok(())
    }

    /// Asks the analysis model about the code in `dir`, which it may read
    /// with its file tools, for JSON that matches `schema`. Stops early,
    /// with an error, once `cancel` is set.
    pub fn complete_json(
        &self,
        dir: &Path,
        system: &str,
        prompt: &str,
        schema: &Value,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake.complete_json(dir, prompt);
        }
        let mut command = command()?;
        command
            .current_dir(dir)
            .args(["--print", "--model", ANALYSIS_MODEL])
            // Grouping hunks needs some reasoning, not deep reasoning.
            .args(["--effort", "low"])
            .args(["--restricted", "--tools", "Read,Grep,Glob"])
            .args([
                "--permission-mode",
                "dontAsk",
                "--permission-prompts",
                "none",
            ])
            .args(["--strict-mcp-config", "--disable-slash-commands"])
            .arg("--no-session-persistence")
            .args(["--output-format", "json"])
            .arg("--json-schema")
            .arg(serde_json::to_string(schema)?)
            .arg("--system-prompt")
            .arg(system);
        let output = run(command, prompt.as_bytes(), cancel)?;
        result_json(&output.stdout).map_err(|err| match output.stderr.trim() {
            "" => err,
            stderr => err.context(stderr.to_owned()),
        })
    }
}

/// `claude`, with what it inherits from Docket cleaned up: launched from a
/// Claude Code session (in development), it would think it's nested.
fn command() -> Result<Command> {
    if cfg!(test) {
        bail!("tests can't run Claude Code; connect with `ClaudeCode::fake`");
    }
    let claude = find_claude().ok_or_else(|| anyhow!(INSTALL_HINT))?;
    let mut command = Command::new(claude);
    command
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT");
    Ok(command)
}

fn find_claude() -> Option<PathBuf> {
    let path_dirs = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let home_dirs: Vec<PathBuf> = dirs::home_dir()
        .map(|home| {
            HOME_FALLBACK_DIRS
                .iter()
                .map(|dir| home.join(dir))
                .collect()
        })
        .unwrap_or_default();
    path_dirs
        .into_iter()
        .chain(FALLBACK_DIRS.iter().map(PathBuf::from))
        .chain(home_dirs)
        .map(|dir| dir.join("claude"))
        .find(|claude| claude.is_file())
}

struct Output {
    stdout: Vec<u8>,
    stderr: String,
}

/// Runs `command` with `input` on its stdin, until it exits, `cancel` is
/// set, or `TIMEOUT` passes. Its pipes are read and written on threads of
/// their own, so a long prompt or answer can't fill one and stall it.
fn run(mut command: Command, input: &[u8], cancel: &AtomicBool) -> Result<Output> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("couldn't run Claude Code")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let input = input.to_vec();
    // A write fails once it exits, which the exit says more about.
    std::thread::spawn(move || stdin.write_all(&input));
    let read = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        })
    };
    let stdout = read(Box::new(child.stdout.take().context("no stdout")?));
    let stderr = read(Box::new(child.stderr.take().context("no stderr")?));

    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            child.kill().ok();
            child.wait().ok();
            bail!("the analysis was cancelled");
        }
        if started.elapsed() > TIMEOUT {
            child.kill().ok();
            child.wait().ok();
            bail!(
                "Claude Code took longer than {} minutes",
                TIMEOUT.as_secs() / 60
            );
        }
        std::thread::sleep(POLL);
    }
    let join = |thread: std::thread::JoinHandle<std::io::Result<Vec<u8>>>| {
        thread
            .join()
            .map_err(|_| anyhow!("reading Claude Code's output panicked"))?
            .context("reading Claude Code's output")
    };
    Ok(Output {
        stdout: join(stdout)?,
        stderr: String::from_utf8_lossy(&join(stderr)?).trim().to_owned(),
    })
}

/// The structured output of `claude -p --output-format json`, or why
/// there isn't one.
fn result_json(stdout: &[u8]) -> Result<Value> {
    let result: Value =
        serde_json::from_slice(stdout).context("Claude Code's answer wasn't the JSON asked for")?;
    if result["is_error"] == Value::Bool(true) || result["subtype"] != "success" {
        let message = result["result"]
            .as_str()
            .filter(|message| !message.trim().is_empty())
            .or(result["subtype"].as_str())
            .unwrap_or("no details");
        bail!("Claude Code stopped: {}", message.trim());
    }
    match &result["structured_output"] {
        Value::Null => {
            let text = result["result"].as_str().unwrap_or_default();
            serde_json::from_str(text).context("the analysis wasn't the JSON asked for")
        }
        output => Ok(output.clone()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_the_structured_output() {
        let stdout = json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "{\"groups\": []}",
            "structured_output": { "groups": [] },
        });
        let read = |value: &Value| result_json(value.to_string().as_bytes());
        assert_eq!(read(&stdout).unwrap(), json!({ "groups": [] }));

        let mut text_only = stdout.clone();
        text_only["structured_output"] = Value::Null;
        assert_eq!(read(&text_only).unwrap(), json!({ "groups": [] }));

        let signed_out = json!({
            "type": "result",
            "subtype": "success",
            "is_error": true,
            "result": "Not logged in · Please run /login",
        });
        let err = read(&signed_out).unwrap_err().to_string();
        assert!(err.contains("Not logged in"), "{err}");

        let ran_out = json!({ "type": "result", "subtype": "error_max_turns", "is_error": false });
        let err = read(&ran_out).unwrap_err().to_string();
        assert!(err.contains("error_max_turns"), "{err}");
    }

    #[test]
    fn stops_when_cancelled() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let cancel = AtomicBool::new(true);
        let started = Instant::now();
        assert!(run(command, b"", &cancel).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
