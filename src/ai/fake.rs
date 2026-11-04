//! A stand-in for Anthropic in tests: a canned answer to every analysis,
//! with no network. Connect with `AnthropicClient::fake`.

use std::sync::Mutex;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

pub struct FakeAnthropic {
    data: Mutex<FakeData>,
}

struct FakeData {
    /// What every request answers: the model's JSON, or an error.
    answer: Result<Value, String>,
    /// The prompts asked, oldest first.
    prompts: Vec<String>,
}

impl FakeAnthropic {
    /// Answers with no groups, so every hunk lands in "Other changes".
    pub fn new() -> Self {
        Self {
            data: Mutex::new(FakeData {
                answer: Ok(json!({ "groups": [] })),
                prompts: Vec::new(),
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
        self.data.lock().unwrap().prompts.len()
    }

    pub fn last_prompt(&self) -> Option<String> {
        self.data.lock().unwrap().prompts.last().cloned()
    }

    pub fn verify(&self) -> Result<()> {
        Ok(())
    }

    pub fn complete_json(&self, prompt: &str) -> Result<Value> {
        let mut data = self.data.lock().unwrap();
        data.prompts.push(prompt.to_owned());
        data.answer.clone().map_err(|message| anyhow!(message))
    }
}

impl std::fmt::Debug for FakeAnthropic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeAnthropic")
    }
}
