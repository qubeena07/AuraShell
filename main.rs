//! REPL entry point — async via Tokio to support the agentic layer.

mod agent;
mod builtins;
mod executor;
mod jobs;
mod parser;
mod tokenizer;

use std::env;
use std::io::{BufRead, Write};

// ANSI helpers — only used for agentic output, not normal shell output.
const YELLOW: &str = "\x1b[33m";
const CYAN: &str   = "\x1b[36m";
const BOLD: &str   = "\x1b[1m";
const RESET: &str  = "\x1b[0m";

fn make_prompt() -> String {
    let cwd = env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let home = env::var("HOME").unwrap_or_default();
    if !home.is_empty() && cwd.starts_with(&home) {
        format!("aura:~{}$ ", &cwd[home.len()..])
    } else {
        format!("aura:{}$ ", cwd)
    }
}

/// Execute a shell command string through the normal parser pipeline.
/// Returns the exit status.
fn execute_str(line: &str) -> (i32, Option<(String, String)>) {
    let tokens = tokenizer::tokenize(line);
    if tokens.is_empty() {
        return (0, None);
    }
    match parser::parse(tokens) {
        Ok(Some(script)) => executor::execute_script(&script),
        Ok(None) => (0, None),
        Err(e) => {
            eprintln!("aura: {}", e);
            (1, None)
        }
    }
}

/// Print the Error Autopsy result in yellow.
fn print_autopsy(explanation: &str) {
    eprintln!(
        "{}{}✦ Autopsy:{} {}{}{}",
        YELLOW, BOLD, RESET, YELLOW, explanation, RESET
    );
}

/// Handle a line that begins with `?` — natural-language intent translation.
/// Returns true if the user accepted and ran the suggested command.
async fn handle_nl_query(query: &str) -> bool {
    eprint!("{}⟳  translating...{}", CYAN, RESET);
    let _ = std::io::stderr().flush();

    match agent::translate_intent(query).await {
        Err(e) => {
            eprintln!("\raura: agent error: {}{:50}{}", YELLOW, " ", RESET); // clear spinner
            eprintln!("{}aura: {}{}", YELLOW, e, RESET);
            false
        }
        Ok(cmd) => {
            // Clear the spinner line and print the suggestion.
            eprintln!("\r{:<60}", ""); // overwrite spinner
            eprintln!("{}{}→  {}{}", CYAN, BOLD, RESET, cmd);
            eprint!("{}[Y/n]: {}", CYAN, RESET);
            let _ = std::io::stderr().flush();

            let mut answer = String::new();
            let _ = std::io::stdin().lock().read_line(&mut answer);
            let answer = answer.trim().to_lowercase();

            if answer.is_empty() || answer == "y" {
                let (status, autopsy) = execute_str(&cmd);
                if let Some((command, stderr)) = autopsy {
                    run_autopsy(&command, &stderr).await;
                }
                let _ = status;
                true
            } else {
                false
            }
        }
    }
}

/// Call analyze_error and print the result.
async fn run_autopsy(command: &str, stderr: &str) {
    eprint!("{}{}✦ Autopsy: analyzing...{}", YELLOW, BOLD, RESET);
    let _ = std::io::stderr().flush();

    match agent::analyze_error(command, stderr).await {
        Ok(explanation) => {
            eprint!("\r{:<60}\r", ""); // clear the "analyzing..." line
            print_autopsy(&explanation);
        }
        Err(e) => {
            eprint!("\r{:<60}\r", "");
            eprintln!("{}aura: autopsy unavailable: {}{}", YELLOW, e, RESET);
        }
    }
}

async fn run_line(line: &str) {
    if let Some(query) = line.strip_prefix('?') {
        let query = query.trim();
        if query.is_empty() {
            eprintln!("aura: ? requires a description, e.g.  ? list all rust files");
            return;
        }
        handle_nl_query(query).await;
        return;
    }

    let (_, autopsy) = execute_str(line);
    if let Some((command, stderr)) = autopsy {
        run_autopsy(&command, &stderr).await;
    }
}

#[tokio::main]
async fn main() {
    executor::init_shell();

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut buf = String::new();

    loop {
        executor::reap_jobs();
        let prompt = make_prompt();
        let _ = stdout.write_all(prompt.as_bytes());
        let _ = stdout.flush();

        buf.clear();
        match stdin.lock().read_line(&mut buf) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {
                let line = buf.trim_end_matches('\n');
                if line.is_empty() {
                    continue;
                }
                run_line(line).await;
            }
            Err(_) => break,
        }
    }

    std::process::exit(executor::last_exit_status());
}
