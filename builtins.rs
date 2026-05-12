//! Builtins that should run in the shell process so their state changes
//! persist (cd, export, exit). They're also runnable inside a forked
//! pipeline child, where their state changes obviously die with the child.

use crate::executor::last_exit_status;
use crate::jobs;
use std::env;
use std::fs;

pub fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "cd" | "pwd" | "exit" | "export" | "unset" | "jobs" | "echo" | "help" | "source" | "."
    )
}

pub fn run_builtin(args: &[String]) -> i32 {
    if args.is_empty() {
        return 1;
    }
    match args[0].as_str() {
        "cd" => builtin_cd(args),
        "pwd" => builtin_pwd(),
        "exit" => builtin_exit(args),
        "export" => builtin_export(args),
        "unset" => builtin_unset(args),
        "jobs" => {
            jobs::list_jobs();
            0
        }
        "echo" => builtin_echo(args),
        "help" => builtin_help(),
        "source" | "." => builtin_source(args),
        _ => 1,
    }
}

fn builtin_cd(args: &[String]) -> i32 {
    let target: String = if args.len() < 2 {
        match env::var("HOME") {
            Ok(h) => h,
            Err(_) => {
                eprintln!("cd: HOME not set");
                return 1;
            }
        }
    } else if args[1] == "-" {
        match env::var("OLDPWD") {
            Ok(p) => {
                println!("{}", p);
                p
            }
            Err(_) => {
                eprintln!("cd: OLDPWD not set");
                return 1;
            }
        }
    } else {
        args[1].clone()
    };

    let old = env::current_dir().ok();
    if let Err(e) = env::set_current_dir(&target) {
        eprintln!("cd: {}: {}", target, e);
        return 1;
    }
    if let Some(o) = old {
        env::set_var("OLDPWD", o);
    }
    if let Ok(c) = env::current_dir() {
        env::set_var("PWD", c);
    }
    0
}

fn builtin_pwd() -> i32 {
    match env::current_dir() {
        Ok(p) => {
            println!("{}", p.display());
            0
        }
        Err(e) => {
            eprintln!("pwd: {}", e);
            1
        }
    }
}

fn builtin_exit(args: &[String]) -> i32 {
    let code = if args.len() >= 2 {
        args[1].parse().unwrap_or(0)
    } else {
        last_exit_status()
    };
    // Signal the main loop to exit cleanly (save history, flush, etc.)
    // instead of hard-exiting here and bypassing cleanup.
    crate::executor::request_exit(code);
    code
}

fn builtin_export(args: &[String]) -> i32 {
    if args.len() < 2 {
        for (k, v) in env::vars() {
            println!("{}={}", k, v);
        }
        return 0;
    }
    for a in &args[1..] {
        if let Some(eq) = a.find('=') {
            let (k, rest) = a.split_at(eq);
            env::set_var(k, &rest[1..]);
        } else if env::var(a).is_err() {
            env::set_var(a, "");
        }
    }
    0
}

fn builtin_unset(args: &[String]) -> i32 {
    for a in &args[1..] {
        env::remove_var(a);
    }
    0
}

fn builtin_echo(args: &[String]) -> i32 {
    let mut newline = true;
    let mut start = 1;
    if args.len() > 1 && args[1] == "-n" {
        newline = false;
        start = 2;
    }
    let parts: Vec<&str> = args[start..].iter().map(|s| s.as_str()).collect();
    print!("{}", parts.join(" "));
    if newline {
        println!();
    }
    0
}

fn builtin_source(args: &[String]) -> i32 {
    if args.len() < 2 {
        eprintln!("source: usage: source <file>");
        return 1;
    }
    let path = &args[1];
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("source: {}: {}", path, e);
            return 1;
        }
    };

    let mut loaded = 0usize;
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        // skip blanks, comments, and `export` prefix
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        match line.find('=') {
            None => {
                eprintln!("source: {}:{}: no '=' in {:?}, skipping", path, lineno + 1, line);
            }
            Some(eq) => {
                let key = line[..eq].trim();
                let val = line[eq + 1..].trim().trim_matches('"').trim_matches('\'');
                if key.is_empty() {
                    eprintln!("source: {}:{}: empty key, skipping", path, lineno + 1);
                    continue;
                }
                env::set_var(key, val);
                loaded += 1;
            }
        }
    }

    println!(
        "source: loaded {} variable{} from {}",
        loaded,
        if loaded == 1 { "" } else { "s" },
        path
    );
    0
}

fn builtin_help() -> i32 {
    println!("AuraShell — an AI-powered Unix shell");
    println!();
    println!("Builtins:");
    println!("  cd [dir|-]      change directory ('-' = previous)");
    println!("  pwd             print working directory");
    println!("  echo [-n] ARGS  print arguments");
    println!("  export VAR=VAL  set environment variable");
    println!("  unset VAR       remove environment variable");
    println!("  jobs            list background jobs");
    println!("  source <file>   load KEY=VALUE pairs from file into environment");
    println!("  help            show this help");
    println!("  exit [N]        exit the shell");
    println!();
    println!("Features:");
    println!("  pipes:        cmd1 | cmd2 | cmd3");
    println!("  redirection:  cmd > out, cmd >> out, cmd < in");
    println!("  chaining:     cmd1 && cmd2   (run if success)");
    println!("                cmd1 || cmd2   (run if failure)");
    println!("  background:   long_running &");
    println!("  glob:         ls *.rs, cat src/*.toml");
    println!("  expansion:    echo $HOME, echo \"${{USER}}!\"");
    println!("  Ctrl-C kills the foreground job, not the shell.");
    println!();
    println!("AI agents (requires GEMINI_API_KEY):");
    println!("  ? <desc>        translate English to a shell command");
    println!("  ?? <question>   multi-turn chat with memory (follow-ups work!)");
    println!("  ?? reset        clear chat history");
    println!("  ?? history      show chat history");
    println!("  ?! <goal>       plan a multi-step task and execute step by step");
    println!("  (auto)          error autopsy when a command fails");
    println!("  (auto)          command-not-found suggestion (exit 127)");
    0
}
