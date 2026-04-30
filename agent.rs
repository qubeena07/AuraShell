//! Agentic layer: natural-language → command translation and error autopsy.
//!
//! Calls the Google Gemini API using GEMINI_API_KEY environment variable.
//! One system prompt, one user turn, one text response. No history kept.

use reqwest::Client;
use serde_json::{json, Value};

const MODEL: &str = "gemini-2.5-flash";
const MAX_TOKENS: u32 = 256;

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

/// POST to Gemini generateContent endpoint and return the model's text reply.
async fn call(system: &str, user: &str) -> Result<String, AgentError> {
    let key = api_key()?;

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        MODEL, key
    );

    let body = json!({
        "system_instruction": {
            "parts": [{ "text": system }]
        },
        "contents": [{
            "role": "user",
            "parts": [{ "text": user }]
        }],
        "generationConfig": {
            "maxOutputTokens": MAX_TOKENS,
            "temperature": 0.2
        }
    });

    let resp = client()
        .post(&url)
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

    json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| AgentError::Parse(format!("unexpected shape: {}", json)))
}

/// Translate a natural-language description into a single shell command.
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
