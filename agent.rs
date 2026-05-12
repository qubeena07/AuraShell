//! Agentic layer: NL→command translation, error autopsy, multi-turn chat,
//! and task planning. Calls the Google Gemini API via GEMINI_API_KEY.

use reqwest::Client;
use serde_json::{json, Value};

const MODEL: &str = "gemini-2.5-flash";
const MAX_TOKENS: u32 = 256;
const CHAT_MAX_TOKENS: u32 = 512;
const PLAN_MAX_TOKENS: u32 = 512;

fn client() -> Client {
    Client::new()
}

fn api_key() -> Result<String, AgentError> {
    std::env::var("GEMINI_API_KEY").map_err(|_| AgentError::NoApiKey)
}



#[derive(Debug)]
pub enum AgentError {
    NoApiKey,
    Http(String),
    Parse(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::NoApiKey => write!(
                f,
                "GEMINI_API_KEY not set — run: export GEMINI_API_KEY=your-key"
            ),
            AgentError::Http(e) => write!(f, "API request failed: {}", e),
            AgentError::Parse(e) => write!(f, "API response parse error: {}", e),
        }
    }
}

/// POST to Gemini with a single user turn.
async fn call(system: &str, user: &str, max_tokens: u32) -> Result<String, AgentError> {
    let key = api_key()?;
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        MODEL, key
    );
    let body = json!({
        "system_instruction": { "parts": [{ "text": system }] },
        "contents": [{ "role": "user", "parts": [{ "text": user }] }],
        "generationConfig": { "maxOutputTokens": max_tokens, "temperature": 0.2 }
    });
    let resp = client()
        .post(&url)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| AgentError::Http(e.to_string()))?;
    let status = resp.status();
    let json: Value = resp.json().await.map_err(|e| AgentError::Parse(e.to_string()))?;
    if !status.is_success() {
        let msg = json["error"]["message"].as_str().unwrap_or("unknown error").to_string();
        return Err(AgentError::Http(format!("HTTP {}: {}", status, msg)));
    }
    json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| AgentError::Parse(format!("unexpected shape: {}", json)))
}

/// POST to Gemini with a full multi-turn conversation history.
async fn call_with_history(
    system: &str,
    history: &[(String, String)],
    user_msg: &str,
    max_tokens: u32,
) -> Result<String, AgentError> {
    let key = api_key()?;
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        MODEL, key
    );
    let mut contents: Vec<Value> = Vec::new();
    for (u, m) in history {
        contents.push(json!({"role": "user",  "parts": [{"text": u}]}));
        contents.push(json!({"role": "model", "parts": [{"text": m}]}));
    }
    contents.push(json!({"role": "user", "parts": [{"text": user_msg}]}));
    let body = json!({
        "system_instruction": { "parts": [{ "text": system }] },
        "contents": contents,
        "generationConfig": { "maxOutputTokens": max_tokens, "temperature": 0.7 }
    });
    let resp = client()
        .post(&url)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| AgentError::Http(e.to_string()))?;
    let status = resp.status();
    let json: Value = resp.json().await.map_err(|e| AgentError::Parse(e.to_string()))?;
    if !status.is_success() {
        let msg = json["error"]["message"].as_str().unwrap_or("unknown error").to_string();
        return Err(AgentError::Http(format!("HTTP {}: {}", status, msg)));
    }
    json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| AgentError::Parse(format!("unexpected shape: {}", json)))
}

// ---------------------------------------------------------------------------
// Stateless helpers
// ---------------------------------------------------------------------------

/// Translate a natural-language description into a single shell command.
pub async fn translate_intent(input: &str) -> Result<String, AgentError> {
    let system = "\
You are a Unix shell command translator. \
The user describes what they want to do in plain English. \
Reply with ONLY the exact shell command that accomplishes it. \
No explanation, no markdown, no backticks, no newlines. \
If the request is ambiguous, emit the safest, most common interpretation.";
    call(system, input, MAX_TOKENS).await
}

/// Explain why a command failed, given its stderr output.
pub async fn analyze_error(command: &str, stderr: &str) -> Result<String, AgentError> {
    let system = "\
You are a Unix shell error analyst. \
Given a failed command and its stderr, identify the root cause and the fix \
in 1-2 concise sentences. Be direct and specific. \
No markdown, no bullet points, plain text only.";
    let user = format!("Command: {}\nStderr: {}", command, stderr.trim());
    call(system, &user, MAX_TOKENS).await
}

/// Break a goal into an ordered list of shell commands.
pub async fn plan_task(goal: &str) -> Result<Vec<String>, AgentError> {
    let system = "\
You are a Unix task planner. Given a goal, output ONLY a numbered list of \
shell commands to accomplish it, one per line. \
Format each line exactly as: '1. command', '2. command', etc. \
No explanation, no markdown, no headers, no blank lines between steps. \
Each command must be a valid shell command the user can run directly.";
    let raw = call(system, goal, PLAN_MAX_TOKENS).await?;
    let steps: Vec<String> = raw
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if let Some(dot_pos) = line.find(". ") {
                let prefix = &line[..dot_pos];
                if prefix.chars().all(|c| c.is_ascii_digit()) && !prefix.is_empty() {
                    let cmd = line[dot_pos + 2..].trim().to_string();
                    if !cmd.is_empty() {
                        return Some(cmd);
                    }
                }
            }
            None
        })
        .collect();
    if steps.is_empty() {
        return Err(AgentError::Parse("no steps returned by planner".to_string()));
    }
    Ok(steps)
}

// ---------------------------------------------------------------------------
// Multi-turn chat agent
// ---------------------------------------------------------------------------

/// Stateful multi-turn chat agent. History lives for the shell session only.
pub struct ChatAgent {
    history: Vec<(String, String)>,
}

impl ChatAgent {
    pub fn new() -> Self {
        Self { history: Vec::new() }
    }

    pub async fn chat(&mut self, input: &str) -> Result<String, AgentError> {
        let system = "\
You are a helpful Unix/Linux shell assistant. \
Answer questions about shell commands, scripts, system administration, \
and programming. Be concise. Plain text; show code in fenced blocks only \
when it adds clarity. You remember the conversation history.";
        let reply = call_with_history(system, &self.history, input, CHAT_MAX_TOKENS).await?;
        self.history.push((input.to_string(), reply.clone()));
        Ok(reply)
    }

    pub fn reset(&mut self) {
        self.history.clear();
    }

    pub fn print_history(&self) {
        if self.history.is_empty() {
            println!("(no chat history)");
            return;
        }
        for (i, (u, m)) in self.history.iter().enumerate() {
            println!("[{}] you:  {}", i + 1, u);
            println!("[{}] aura: {}", i + 1, m);
            println!();
        }
    }
}
