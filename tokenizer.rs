//! Lexer: turn an input line into a Vec<Token>.
//!
//! Handles:
//!   - whitespace separation
//!   - metacharacters: |  ||  <  >  >>  &&  &  ;  2>  2>>  2>&1
//!   - single quotes  '...'  : literal, no expansion
//!   - double quotes  "..."  : $VAR expansion + backslash escapes for " \ $ `
//!   - backslash outside quotes: escapes next char
//!   - $VAR, ${VAR}, $?, $$ expansion
//!   - # starts a comment until end of line





use crate::executor::last_exit_status;
use std::env;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Word(String),
    Pipe,
    /// ||  — run next pipeline only if previous failed
    OrOr,
    RedirIn,
    RedirOut,
    RedirAppend,
    /// 2>file  — redirect stderr to file (truncate)
    Redir2Out,
    /// 2>>file — redirect stderr to file (append)
    Redir2Append,
    /// 2>&1    — merge stderr into stdout
    Redir2ToOut,
    /// &&      — run next pipeline only if previous succeeded
    AndAnd,
    Amp,
    Semi,
}

pub fn tokenize(line: &str) -> Vec<Token> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let n = chars.len();

    while i < n {
        // skip whitespace
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= n {
            break;
        }
        if chars[i] == '#' {
            break;
        }

        // metacharacters
        match chars[i] {
            '|' => {
                i += 1;
                if i < n && chars[i] == '|' {
                    tokens.push(Token::OrOr);
                    i += 1;
                } else {
                    tokens.push(Token::Pipe);
                }
                continue;
            }
            '<' => { tokens.push(Token::RedirIn); i += 1; continue; }
            '>' => {
                i += 1;
                if i < n && chars[i] == '>' {
                    tokens.push(Token::RedirAppend);
                    i += 1;
                } else {
                    tokens.push(Token::RedirOut);
                }
                continue;
            }
            // 2>  2>>  2>&1  — must come before the word-building fallthrough
            '2' if i + 1 < n && chars[i + 1] == '>' => {
                i += 2; // consume '2' and '>'
                if i + 1 < n && chars[i] == '&' && chars[i + 1] == '1' {
                    tokens.push(Token::Redir2ToOut);
                    i += 2;
                } else if i < n && chars[i] == '>' {
                    tokens.push(Token::Redir2Append);
                    i += 1;
                } else {
                    tokens.push(Token::Redir2Out);
                }
                continue;
            }
            '&' => {
                i += 1;
                if i < n && chars[i] == '&' {
                    tokens.push(Token::AndAnd);
                    i += 1;
                } else {
                    tokens.push(Token::Amp);
                }
                continue;
            }
            ';' => { tokens.push(Token::Semi); i += 1; continue; }
            _ => {}
        }

        // word
        let mut buf = String::new();
        while i < n {
            let c = chars[i];
            if c.is_whitespace() || matches!(c, '|' | '<' | '>' | '&' | ';' | '#') {
                break;
            }
            if c == '2' && i + 1 < n && chars[i + 1] == '>' && !buf.is_empty() {
                break;
            }
            match c {
                '\'' => {
                    i += 1;
                    while i < n && chars[i] != '\'' {
                        buf.push(chars[i]);
                        i += 1;
                    }
                    if i < n {
                        i += 1; // consume closing '
                    }
                }
                '"' => {
                    i += 1;
                    while i < n && chars[i] != '"' {
                        if chars[i] == '\\'
                            && i + 1 < n
                            && matches!(chars[i + 1], '"' | '\\' | '$' | '`')
                        {
                            i += 1;
                            buf.push(chars[i]);
                            i += 1;
                        } else if chars[i] == '$' {
                            expand_var(&chars, &mut i, &mut buf);
                        } else {
                            buf.push(chars[i]);
                            i += 1;
                        }
                    }
                    if i < n {
                        i += 1; // consume closing "
                    }
                }
                '\\' if i + 1 < n => {
                    i += 1;
                    buf.push(chars[i]);
                    i += 1;
                }
                '$' => expand_var(&chars, &mut i, &mut buf),
                _ => {
                    buf.push(c);
                    i += 1;
                }
            }
        }
        tokens.push(Token::Word(buf));
    }
    tokens
}

fn expand_var(chars: &[char], i: &mut usize, buf: &mut String) {
    *i += 1; // skip $
    if *i >= chars.len() {
        buf.push('$');
        return;
    }
    let c = chars[*i];
    if c == '?' {
        *i += 1;
        buf.push_str(&last_exit_status().to_string());
        return;
    }
    if c == '$' {
        *i += 1;
        buf.push_str(&std::process::id().to_string());
        return;
    }
    if c == '{' {
        *i += 1;
        let mut name = String::new();
        while *i < chars.len() && chars[*i] != '}' {
            name.push(chars[*i]);
            *i += 1;
        }
        if *i < chars.len() {
            *i += 1; // consume }
        }
        if let Ok(v) = env::var(&name) {
            buf.push_str(&v);
        }
        return;
    }
    if !c.is_alphabetic() && c != '_' {
        buf.push('$');
        return;
    }
    let mut name = String::new();
    while *i < chars.len() && (chars[*i].is_alphanumeric() || chars[*i] == '_') {
        name.push(chars[*i]);
        *i += 1;
    }
    if let Ok(v) = env::var(&name) {
        buf.push_str(&v);
    }
}
