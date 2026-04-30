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
const BOLD: &str   = "\x1b[1m";
const RESET: &str  = "\x1b[0m";

// ---------------------------------------------------------------------------
// Rustyline helper
// ---------------------------------------------------------------------------

/// Wires rustyline's completion, hinting, highlighting, and validation traits
/// into a single struct. Only `Completer` has real logic; the rest are
/// no-ops that satisfy the `Helper` bound.
struct AuraHelper {
    completer: FilenameCompleter,
}

impl AuraHelper {
    fn new() -> Self {
        Self { completer: FilenameCompleter::new() }
    }
}

/// Delegate completion to `FilenameCompleter`.
///
/// We find the start of the current word (the token being typed) by scanning
/// backwards from the cursor for whitespace. This lets completion work
/// correctly regardless of how many words precede it on the line, e.g.:
///   `cat src/ma<TAB>`  →  `cat src/main.rs`
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

/// No-op hinter — return `None` so no ghost-text hints are shown.
impl Hinter for AuraHelper {
    type Hint = String;
    fn hint(&self, _line: &str, _pos: usize, _ctx: &Context<'_>) -> Option<String> {
        None
    }
}

/// No-op highlighter — return the line unchanged.
impl Highlighter for AuraHelper {}

/// No-op validator — every line is valid from rustyline's perspective
/// (our parser handles real syntax errors).
impl Validator for AuraHelper {}

/// Marker trait that combines all four.
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
    // U+276F HEAVY RIGHT-POINTING ANGLE QUOTATION MARK — classic zsh-style prompt.
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

// current_thread: we block on readline and API calls — no concurrent tasks.
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
                run_line(&line).await;
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
