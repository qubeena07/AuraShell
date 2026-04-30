# AuraShell

A custom command-line shell written in Rust, combining low-level POSIX OS
mechanics (process management, pipes, signal handling) with an agentic LLM
layer for natural-language command execution and error analysis.

## Build & run

```sh
# Requires Rust 1.75+ and a Unix-like OS (Linux/macOS)
cargo build --release
./target/release/msh
```

Set your Anthropic API key to enable AI features:

```sh
export ANTHROPIC_API_KEY=sk-ant-...
./target/release/msh
```

Without the key the shell runs fully normally — AI features degrade gracefully
with a yellow warning instead of crashing.

## Features

### Core shell

| Feature | Example |
|---|---|
| External commands | `ls -la /etc` |
| Pipes (any depth) | `cat /etc/passwd \| grep root \| wc -l` |
| Sequential chaining | `echo hello ; echo world` |
| Conditional chaining | `mkdir foo && cd foo` |
| Redirection | `cmd > out`, `cmd >> out`, `cmd < in` |
| Stderr redirection | `cmd 2> err.log`, `cmd 2>> err.log` |
| Stderr merge | `cmd 2>&1 \| less` |
| Background jobs | `sleep 10 &` |
| Variable expansion | `$VAR`, `${VAR}`, `$?`, `$$` |
| Quoting | `'literal $HOME'`, `"expanded $HOME"` |
| Comments | `# anything after hash` |
| Ctrl-C kills foreground job, not shell | (signal handling) |

### Builtins

`cd [dir|-]` · `pwd` · `echo [-n]` · `export VAR=VAL` · `unset VAR` ·
`jobs` · `help` · `exit [N]`

### Agentic layer (requires `ANTHROPIC_API_KEY`)

**Natural-language intent** — prefix any input with `?`:

```
aura:~$ ? find all rust source files modified in the last week
→  find . -name "*.rs" -mtime -7
[Y/n]: y
./src/main.rs
./src/parser.rs
```

**Error Autopsy** — when a command exits non-zero, AuraShell captures its
stderr and asks the LLM for a concise diagnosis:

```
aura:~$ git pussh origin main
git: 'pussh' is not a git command. Did you mean 'push'?
✦ Autopsy: 'pussh' is a typo — run `git push origin main` instead.
```

## Architecture

```
AuraShell/
├── main.rs        — async REPL loop (tokio); ? prefix handler; autopsy display
├── tokenizer.rs   — lexer: words, pipes, redirections (including 2>, 2>&1), &&, ;
├── parser.rs      — tokens → Script { pipelines, connectors }
├── executor.rs    — fork/exec, pipe wiring, signal handling, stderr capture
├── builtins.rs    — cd, pwd, export, unset, echo, jobs, help, exit
├── jobs.rs        — background job table (Mutex<Vec<Job>>)
└── agent.rs       — Anthropic API client: translate_intent, analyze_error
```

**Pipeline:** `input line → tokenize → parse → execute_script → execute_pipeline`

State (last exit status, job table, shell pgid, SIGCHLD flag) lives in
module-private `static`s using `AtomicI32` / `AtomicBool` / `Mutex`.

### Stderr capture for Error Autopsy

For single-command foreground pipelines where the user did not explicitly
redirect stderr, `executor` opens a `pipe()` before `fork()`, gives the
write end to the child (which `dup2`s it to fd 2), closes the write end in
the parent after fork, calls `waitpid`, then drains the read end. This is
deadlock-safe because the child is the sole writer and has exited before the
parent reads.

### Async boundary

`executor.rs` stays synchronous (POSIX fork/exec). The `#[tokio::main]`
runtime lives in `main.rs`; after `execute_script` returns with autopsy data,
`run_line` awaits `agent::analyze_error`.

## Dependencies

| Crate | Purpose |
|---|---|
| `nix` | POSIX syscalls (fork, dup2, pipe, sigaction, tcsetpgrp, …) |
| `libc` | `_exit` in child after fork |
| `tokio` | Async runtime for HTTP calls |
| `reqwest` | HTTP client for Anthropic API |
| `serde` / `serde_json` | JSON serialization for API requests/responses |

## Known limitations

- No `Ctrl-Z` / `fg` / `bg` (SIGTSTP ignored, no stopped-job state)
- No globbing (`*.rs`)
- No `||` (or-chain) — only `;` and `&&`
- No here-docs (`<<EOF`), subshells (`(...)`), or command substitution (`$(...)`)
- No aliases or shell functions
- Stderr autopsy only fires for single-command pipelines (multi-stage pipelines skip it)
- `2>&1 > file` ordering not POSIX-exact — stdout applied before stderr unconditionally
