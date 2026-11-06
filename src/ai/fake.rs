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
    /// The prompts asked, oldest first, and the directory each was asked
    /// in.
    requests: Vec<(String, PathBuf)>,
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
        data.requests.last().map(|(prompt, _)| prompt.clone())
    }

    /// Where the last analysis ran, which Claude could read.
    pub fn last_dir(&self) -> Option<PathBuf> {
        let data = self.data.lock().unwrap();
        data.requests.last().map(|(_, dir)| dir.clone())
    }

    pub fn verify(&self) -> Result<()> {
        Ok(())
    }

    pub fn complete_json(&self, dir: &Path, prompt: &str) -> Result<Value> {
        let mut data = self.data.lock().unwrap();
        data.requests.push((prompt.to_owned(), dir.to_owned()));
        data.answer.clone().map_err(|message| anyhow!(message))
    }
}

impl std::fmt::Debug for FakeClaude {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeClaude")
    }
}
