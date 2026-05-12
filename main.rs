//! REPL entry point — async via Tokio for the agentic layer,
//! rustyline for line editing, persistent history, and tab completion.

mod agent;
mod builtins;
mod executor;
mod jobs;
mod parser;
mod tokenizer;

use std::env;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};

// ANSI helpers — only for agentic output, not normal shell output.
const YELLOW: &str = "\x1b[33m";
const CYAN: &str   = "\x1b[36m";
const GREEN: &str  = "\x1b[32m";
const BOLD: &str   = "\x1b[1m";
const RESET: &str  = "\x1b[0m";

// ---------------------------------------------------------------------------
// Rustyline helper
// ---------------------------------------------------------------------------

struct AuraHelper {
    completer: FilenameCompleter,
}

impl AuraHelper {
    fn new() -> Self {
        Self { completer: FilenameCompleter::new() }
    }
}

impl Completer for AuraHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        self.completer.complete(line, pos, ctx)
    }
}

impl Hinter for AuraHelper {
    type Hint = String;
    fn hint(&self, _line: &str, _pos: usize, _ctx: &Context<'_>) -> Option<String> {
        None
    }
}

impl Highlighter for AuraHelper {}
impl Validator for AuraHelper {}
impl Helper for AuraHelper {}

// ---------------------------------------------------------------------------
// Editor type alias
// ---------------------------------------------------------------------------

type AuraEditor = Editor<AuraHelper, rustyline::history::FileHistory>;

// ---------------------------------------------------------------------------
// Shell helpers
// ---------------------------------------------------------------------------

fn history_path() -> Option<PathBuf> {
    env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".aura_history"))
}

fn make_prompt() -> String {
    let cwd = env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let home = env::var("HOME").unwrap_or_default();
    let display = if !home.is_empty() && cwd.starts_with(&home) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd
    };
    format!("{} ❯ ", display)
}

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

fn print_autopsy(explanation: &str) {
    eprintln!(
        "{}{}✦ Autopsy:{} {}{}{}",
        YELLOW, BOLD, RESET, YELLOW, explanation, RESET
    );
}

// ---------------------------------------------------------------------------
// Async agentic helpers
// ---------------------------------------------------------------------------

async fn run_autopsy(command: &str, stderr: &str) {
    eprint!("{}{}✦ Autopsy: analyzing...{}", YELLOW, BOLD, RESET);
    let _ = std::io::stderr().flush();

    match agent::analyze_error(command, stderr).await {
        Ok(explanation) => {
            eprint!("\r{:<60}\r", "");
            print_autopsy(&explanation);
        }
        Err(e) => {
            eprint!("\r{:<60}\r", "");
            eprintln!("{}aura: autopsy unavailable: {}{}", YELLOW, e, RESET);
        }
    }
}

async fn handle_nl_query(query: &str) -> bool {
    eprint!("{}⟳  translating...{}", CYAN, RESET);
    let _ = std::io::stderr().flush();

    match agent::translate_intent(query).await {
        Err(e) => {
            eprint!("\r{:<60}\r", "");
            eprintln!("{}aura: {}{}", YELLOW, e, RESET);
            false
        }
        Ok(cmd) => {
            eprint!("\r{:<60}\r", "");
            eprintln!("{}{}→  {}{}", CYAN, BOLD, RESET, cmd);
            eprint!("{}[Y/n]: {}", CYAN, RESET);
            let _ = std::io::stderr().flush();

            let mut answer = String::new();
            let _ = std::io::stdin().lock().read_line(&mut answer);
            let answer = answer.trim().to_lowercase();

            if answer.is_empty() || answer == "y" {
                let (_, autopsy) = execute_str(&cmd);
                if let Some((command, stderr)) = autopsy {
                    run_autopsy(&command, &stderr).await;
                }
                true
            } else {
                false
            }
        }
    }
}

/// Multi-turn chat: send one message, print reply, loop for follow-ups.
async fn handle_chat_query(query: &str, chat: &mut agent::ChatAgent) {
    let result = send_chat(query, chat).await;
    if !result {
        return;
    }
    // Enter follow-up loop
    loop {
        eprint!("{}chat> {}", GREEN, RESET);
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = line.trim().to_string();
        if line.is_empty() || line == "exit" || line == "quit" {
            break;
        }
        if !send_chat(&line, chat).await {
            break;
        }
    }
}

/// Send one message to chat agent and print reply. Returns false on error.
async fn send_chat(msg: &str, chat: &mut agent::ChatAgent) -> bool {
    eprint!("{}⟳  thinking...{}", GREEN, RESET);
    let _ = std::io::stderr().flush();
    match chat.chat(msg).await {
        Ok(reply) => {
            eprint!("\r{:<60}\r", "");
            println!("{}{}{}", GREEN, reply, RESET);
            true
        }
        Err(e) => {
            eprint!("\r{:<60}\r", "");
            eprintln!("{}aura: chat error: {}{}", YELLOW, e, RESET);
            false
        }
    }
}

/// Task planner: get steps from Gemini, confirm each, execute in order.
async fn handle_plan_query(goal: &str) {
    eprint!("{}⟳  planning...{}", CYAN, RESET);
    let _ = std::io::stderr().flush();

    let steps = match agent::plan_task(goal).await {
        Ok(s) => {
            eprint!("\r{:<60}\r", "");
            s
        }
        Err(e) => {
            eprint!("\r{:<60}\r", "");
            eprintln!("{}aura: planner error: {}{}", YELLOW, e, RESET);
            return;
        }
    };

    let total = steps.len();
    eprintln!("{}{}Plan ({} step{}):{}", CYAN, BOLD, total, if total == 1 { "" } else { "s" }, RESET);
    for (i, step) in steps.iter().enumerate() {
        eprintln!("  {}{}. {}{}", CYAN, i + 1, RESET, step);
    }
    eprintln!();

    let mut aborted = false;
    for (i, step) in steps.iter().enumerate() {
        eprint!(
            "{}Step {}/{}: {}{} {}[Y/n/skip/abort]: {}",
            CYAN, i + 1, total, RESET, step, CYAN, RESET
        );
        let _ = std::io::stderr().flush();

        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        let answer = answer.trim().to_lowercase();

        match answer.as_str() {
            "abort" | "a" => {
                eprintln!("{}✗ Aborted at step {}/{}{}", YELLOW, i + 1, total, RESET);
                aborted = true;
                break;
            }
            "skip" | "s" => {
                eprintln!("{}  skipped{}", YELLOW, RESET);
                continue;
            }
            "" | "y" => {
                let (status, autopsy) = execute_str(step);
                if let Some((command, stderr)) = autopsy {
                    run_autopsy(&command, &stderr).await;
                }
                if status != 0 && i + 1 < total {
                    eprint!(
                        "{}Step {} failed (exit {}). Continue? [Y/n]: {}",
                        YELLOW, i + 1, status, RESET
                    );
                    let _ = std::io::stderr().flush();
                    let mut cont = String::new();
                    let _ = std::io::stdin().lock().read_line(&mut cont);
                    let cont = cont.trim().to_lowercase();
                    if cont == "n" || cont == "no" {
                        eprintln!("{}✗ Aborted at step {}/{}{}", YELLOW, i + 1, total, RESET);
                        aborted = true;
                        break;
                    }
                }
            }
            _ => {
                eprintln!("{}  skipped (unrecognised input){}", YELLOW, RESET);
                continue;
            }
        }
    }

    if !aborted {
        eprintln!("{}✓ Plan complete{}", GREEN, RESET);
    }
}

async fn run_line(line: &str, chat: &mut agent::ChatAgent) {
    // ?? — multi-turn chat (check before single ? to avoid prefix clash)
    if let Some(query) = line.strip_prefix("??") {
        let query = query.trim();
        if query == "reset" {
            chat.reset();
            println!("{}chat history cleared{}", CYAN, RESET);
            return;
        }
        if query == "history" {
            chat.print_history();
            return;
        }
        if query.is_empty() {
            eprintln!("aura: ?? requires a message, e.g.  ?? how do I find large files");
            return;
        }
        handle_chat_query(query, chat).await;
        return;
    }

    // ?! — task planner
    if let Some(goal) = line.strip_prefix("?!") {
        let goal = goal.trim();
        if goal.is_empty() {
            eprintln!("aura: ?! requires a goal, e.g.  ?! create a rust hello world project");
            return;
        }
        handle_plan_query(goal).await;
        return;
    }

    // ? — single-shot NL→command translation
    if let Some(query) = line.strip_prefix('?') {
        let query = query.trim();
        if query.is_empty() {
            eprintln!("aura: ? requires a description, e.g.  ? list all rust files");
            return;
        }
        handle_nl_query(query).await;
        return;
    }

    let (code, autopsy) = execute_str(line);

    // Command-not-found suggestion (exit 127)
    if code == 127 {
        let cmd_name = line.split_whitespace().next().unwrap_or(line);
        eprint!("{}⟳  looking up suggestion...{}", YELLOW, RESET);
        let _ = std::io::stderr().flush();
        let stderr_hint = format!("{}: command not found", cmd_name);
        match agent::analyze_error(cmd_name, &stderr_hint).await {
            Ok(suggestion) => {
                eprint!("\r{:<60}\r", "");
                print_autopsy(&suggestion);
            }
            Err(_) => {
                eprint!("\r{:<60}\r", "");
            }
        }
        return;
    }

    if let Some((command, stderr)) = autopsy {
        run_autopsy(&command, &stderr).await;
    }
}

// ---------------------------------------------------------------------------
// REPL
// ---------------------------------------------------------------------------

fn save_history(rl: &mut AuraEditor) {
    if let Some(path) = history_path() {
        if let Err(e) = rl.save_history(&path) {
            eprintln!("aura: could not save history: {}", e);
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    executor::init_shell();

    let mut rl = match Editor::with_history(
        rustyline::Config::default(),
        rustyline::history::FileHistory::new(),
    ) {
        Ok(mut e) => {
            e.set_helper(Some(AuraHelper::new()));
            e
        }
        Err(e) => {
            eprintln!("aura: failed to initialise line editor: {}", e);
            std::process::exit(1);
        }
    };

    if let Some(path) = history_path() {
        let _ = rl.load_history(&path);
    }

    let mut chat = agent::ChatAgent::new();

    loop {
        executor::reap_jobs();

        if let Some(code) = executor::pending_exit() {
            save_history(&mut rl);
            std::process::exit(code);
        }

        let prompt = make_prompt();

        match rl.readline(&prompt) {
            Ok(line) => {
                let line = line.trim_end_matches('\n').to_string();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(&line);
                run_line(&line, &mut chat).await;
            }

            Err(ReadlineError::Interrupted) => {
                eprintln!();
                continue;
            }

            Err(ReadlineError::Eof) => {
                println!();
                break;
            }

            Err(e) => {
                eprintln!("aura: readline error: {}", e);
                break;
            }
        }
    }

    save_history(&mut rl);
    std::process::exit(executor::last_exit_status());
}
