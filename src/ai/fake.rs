//! A stand-in for Claude Code in tests: a canned answer to every analysis,
//! with nothing run. Connect with `ClaudeCode::fake`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

pub struct FakeClaude {
    data: Mutex<FakeData>,
}

struct FakeData {
    /// What every request answers: the model's JSON, or an error.
    answer: Result<Value, String>,
    /// What was asked, oldest first.
    requests: Vec<Request>,
}

struct Request {
    prompt: String,
    /// Where it ran, which Claude could read.
    dir: PathBuf,
    /// Whether it could read history too.
    trusted: bool,
}

impl FakeClaude {
    /// Answers with no groups, so every hunk lands in "Other changes".
    pub fn new() -> Self {
        Self {
            data: Mutex::new(FakeData {
                answer: Ok(json!({ "groups": [] })),
                requests: Vec::new(),
            }),
        }
    }

    /// Sets the JSON the model answers with, from the next request on.
    pub fn set_answer(&self, answer: Value) {
        self.data.lock().unwrap().answer = Ok(answer);
    }

    /// Fails every request from the next on.
    pub fn set_error(&self, message: &str) {
        self.data.lock().unwrap().answer = Err(message.to_owned());
    }

    /// How many analyses were asked for.
    pub fn requests(&self) -> usize {
        self.data.lock().unwrap().requests.len()
    }

    pub fn last_prompt(&self) -> Option<String> {
        let data = self.data.lock().unwrap();
        data.requests.last().map(|request| request.prompt.clone())
    }

    /// Where the last analysis ran, which Claude could read.
    pub fn last_dir(&self) -> Option<PathBuf> {
        let data = self.data.lock().unwrap();
        data.requests.last().map(|request| request.dir.clone())
    }

    /// Whether the last analysis was trusted with history.
    pub fn last_trusted(&self) -> Option<bool> {
        let data = self.data.lock().unwrap();
        data.requests.last().map(|request| request.trusted)
    }

    pub fn verify(&self) -> Result<()> {
        Ok(())
    }

    pub fn complete_json(&self, dir: &Path, prompt: &str, trusted: bool) -> Result<Value> {
        let mut data = self.data.lock().unwrap();
        data.requests.push(Request {
            prompt: prompt.to_owned(),
            dir: dir.to_owned(),
            trusted,
        });
        data.answer.clone().map_err(|message| anyhow!(message))
    }
}

impl std::fmt::Debug for FakeClaude {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeClaude")
    }
}
