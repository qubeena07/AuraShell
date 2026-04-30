//! Parser: turn a Vec<Token> into a Script.
//!
//! Grammar (informal):
//!   script   := pipeline (connector pipeline)*
//!   connector := ';' | '&&'
//!   pipeline := command ('|' command)* ['&']
//!   command  := WORD (WORD | redirection)*
//!   redir    := '<' WORD | '>' WORD | '>>' WORD | '2>' WORD | '2>>' WORD | '2>&1'

use crate::tokenizer::Token;

#[derive(Debug, Default, Clone)]
pub struct Command {
    pub argv: Vec<String>,
    pub infile: Option<String>,
    pub outfile: Option<String>,
    pub append: bool,
    /// 2> file
    pub errfile: Option<String>,
    /// 2>> file (append to errfile)
    pub err_append: bool,
    /// 2>&1 — dup stderr onto stdout after any stdout redirection is applied
    pub err_to_out: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Pipeline {
    pub commands: Vec<Command>,
    pub background: bool,
}

/// How two adjacent pipelines are joined.
#[derive(Debug, Clone, PartialEq)]
pub enum Connector {
    /// `;`  — always run the next pipeline
    Semi,
    /// `&&` — run next only if previous exit status == 0
    And,
}

/// A full parsed line: one or more pipelines separated by `;` or `&&`.
#[derive(Debug, Default)]
pub struct Script {
    /// pipelines[i] is connected to pipelines[i+1] by connectors[i].
    pub pipelines: Vec<Pipeline>,
    pub connectors: Vec<Connector>,
}

pub fn parse(tokens: Vec<Token>) -> Result<Option<Script>, String> {
    let mut script = Script::default();
    let mut remaining = tokens.as_slice();

    loop {
        let (pipeline, rest, connector) = parse_pipeline(remaining)?;
        if !pipeline.commands.is_empty() {
            script.pipelines.push(pipeline);
            if let Some(conn) = connector {
                script.connectors.push(conn);
            }
        }
        remaining = rest;
        if remaining.is_empty() {
            break;
        }
    }

    if script.pipelines.is_empty() {
        Ok(None)
    } else {
        Ok(Some(script))
    }
}

/// Parse one pipeline from the front of `tokens`.
/// Returns `(pipeline, remaining_tokens, connector_that_ended_this_pipeline)`.
fn parse_pipeline<'a>(
    tokens: &'a [Token],
) -> Result<(Pipeline, &'a [Token], Option<Connector>), String> {
    let mut pipeline = Pipeline::default();
    let mut cur = Command::default();
    let mut i = 0;

    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(s) => {
                cur.argv.push(s.clone());
                i += 1;
            }

            Token::RedirIn => {
                i += 1;
                match tokens.get(i) {
                    Some(Token::Word(s)) => { cur.infile = Some(s.clone()); i += 1; }
                    _ => return Err("syntax error: expected filename after '<'".into()),
                }
            }

            Token::RedirOut => {
                i += 1;
                match tokens.get(i) {
                    Some(Token::Word(s)) => {
                        cur.outfile = Some(s.clone());
                        cur.append = false;
                        i += 1;
                    }
                    _ => return Err("syntax error: expected filename after '>'".into()),
                }
            }

            Token::RedirAppend => {
                i += 1;
                match tokens.get(i) {
                    Some(Token::Word(s)) => {
                        cur.outfile = Some(s.clone());
                        cur.append = true;
                        i += 1;
                    }
                    _ => return Err("syntax error: expected filename after '>>'".into()),
                }
            }

            Token::Redir2Out => {
                i += 1;
                match tokens.get(i) {
                    Some(Token::Word(s)) => {
                        cur.errfile = Some(s.clone());
                        cur.err_append = false;
                        cur.err_to_out = false;
                        i += 1;
                    }
                    _ => return Err("syntax error: expected filename after '2>'".into()),
                }
            }

            Token::Redir2Append => {
                i += 1;
                match tokens.get(i) {
                    Some(Token::Word(s)) => {
                        cur.errfile = Some(s.clone());
                        cur.err_append = true;
                        cur.err_to_out = false;
                        i += 1;
                    }
                    _ => return Err("syntax error: expected filename after '2>>'".into()),
                }
            }

            Token::Redir2ToOut => {
                cur.err_to_out = true;
                cur.errfile = None;
                i += 1;
            }

            Token::Pipe => {
                if cur.argv.is_empty() {
                    return Err("syntax error near '|'".into());
                }
                pipeline.commands.push(std::mem::take(&mut cur));
                i += 1;
            }

            Token::Amp => {
                pipeline.background = true;
                i += 1;
                // '&' ends the pipeline; anything after is a new statement
                // (e.g. `sleep 5 & echo hi`). Treat as Semi-connected.
                push_last_command(&mut pipeline, cur);
                return Ok((pipeline, &tokens[i..], Some(Connector::Semi)));
            }

            Token::Semi => {
                i += 1;
                push_last_command(&mut pipeline, cur);
                return Ok((pipeline, &tokens[i..], Some(Connector::Semi)));
            }

            Token::AndAnd => {
                i += 1;
                push_last_command(&mut pipeline, cur);
                return Ok((pipeline, &tokens[i..], Some(Connector::And)));
            }
        }
    }

    // Reached end of token stream.
    if !cur.argv.is_empty() || cur.infile.is_some() || cur.outfile.is_some()
        || cur.errfile.is_some() || cur.err_to_out
    {
        if cur.argv.is_empty() {
            return Err("syntax error: redirection without command".into());
        }
        pipeline.commands.push(cur);
    }

    Ok((pipeline, &[], None))
}

fn push_last_command(pipeline: &mut Pipeline, cmd: Command) {
    if !cmd.argv.is_empty() {
        pipeline.commands.push(cmd);
    }
}
