//! Agentic layer: natural-language → command translation and error autopsy.
//!
//! Both functions call the Anthropic Messages API using the ANTHROPIC_API_KEY
//! environment variable. They are intentionally thin: one system prompt, one
//! user turn, one text response. No conversation history is kept.
//!
//! Model: claude-haiku-4-5 — lowest latency, appropriate for a shell tool.

use reqwest::Client;
use serde_json::{json, Value};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-haiku-4-5-20251001";
// Keep responses short; a command or a 2-sentence explanation needs < 256 tokens.
const MAX_TOKENS: u32 = 256;

/// Shared HTTP client — constructed once per call site (cheap with reqwest).
fn client() -> Client {
    Client::new()
}

fn api_key() -> Result<String, AgentError> {
    std::env::var("ANTHROPIC_API_KEY").map_err(|_| AgentError::NoApiKey)
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
                "ANTHROPIC_API_KEY not set — export it to enable AI features"
            ),
            AgentError::Http(e) => write!(f, "API request failed: {}", e),
            AgentError::Parse(e) => write!(f, "API response parse error: {}", e),
        }
    }
}

/// POST one user message to the Anthropic Messages API and return the
/// assistant's text reply, trimmed of leading/trailing whitespace.
async fn call(system: &str, user: &str) -> Result<String, AgentError> {
    let key = api_key()?;

    let body = json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "messages": [{ "role": "user", "content": user }]
    });

    let resp = client()
        .post(API_URL)
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| AgentError::Http(e.to_string()))?;

    let status = resp.status();
    let json: Value = resp
        .json()
        .await
        .map_err(|e| AgentError::Parse(e.to_string()))?;

    if !status.is_success() {
        let msg = json["error"]["message"]
            .as_str()
            .unwrap_or("unknown error")
            .to_string();
        return Err(AgentError::Http(format!("HTTP {}: {}", status, msg)));
    }

    json["content"][0]["text"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| AgentError::Parse(format!("unexpected shape: {}", json)))
}

/// Translate a natural-language description into a single shell command.
///
/// The model is instructed to return ONLY the command — no markdown, no
/// explanation — so the caller can pass the result directly to the parser.
pub async fn translate_intent(input: &str) -> Result<String, AgentError> {
    let system = "\
You are a Unix shell command translator. \
The user describes what they want to do in plain English. \
Reply with ONLY the exact shell command that accomplishes it. \
No explanation, no markdown, no backticks, no newlines. \
If the request is ambiguous, emit the safest, most common interpretation.";

    call(system, input).await
}

/// Explain why a command failed, given its stderr output.
///
/// Returns a 1–2 sentence plain-text explanation suitable for printing
/// directly in a terminal. No markdown formatting.
pub async fn analyze_error(command: &str, stderr: &str) -> Result<String, AgentError> {
    let system = "\
You are a Unix shell error analyst. \
Given a failed command and its stderr, identify the root cause and the fix \
in 1-2 concise sentences. \
Be direct and specific. \
No markdown, no bullet points, plain text only.";

    let user = format!("Command: {}\nStderr: {}", command, stderr.trim());
    call(system, &user).await
}
