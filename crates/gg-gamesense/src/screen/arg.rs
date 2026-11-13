// SPDX-License-Identifier: GPL-3.0-or-later
//! A screen line's `arg`: a GoLisp expression over the event's data. GameSense runs real
//! GoLisp; this server works out only the forms the SDK's screen doc shows (section "Data
//! Accessors"), and says so for anything else:
//!
//! - `self`, the event's data: `value` and `context-frame` (the event's `frame`);
//! - `(key: e)`, the entry `key` of `e`, as in `(value: self)` and
//!   `(textvalue: (context-frame: self))`;
//! - `(/ a b)`, as in `(/ (numericalvalue: (context-frame: self)) 44)`; two whole numbers give
//!   a whole number;
//! - `(string-upcase e)`;
//! - numbers and `"strings"`, and the empty `arg` (`""`), which shows no value: the doc's static
//!   label is a line with `"arg": ""` and a `prefix`.

use serde_json::{json, Map, Value};

/// An expression of one of the forms above.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// The empty `arg`: no value.
    Nothing,
    /// `self`.
    This,
    /// `(key: e)`.
    Get(String, Box<Expr>),
    /// `(/ a b)`.
    Div(Box<Expr>, Box<Expr>),
    /// `(string-upcase e)`.
    Upcase(Box<Expr>),
    Int(i64),
    Float(f64),
    Str(String),
}

/// `self` for an event: its value and its frame.
pub fn this(value: Option<i64>, frame: &Map<String, Value>) -> Value {
    json!({ "value": value, "context-frame": frame })
}

/// The expression in `text`; `None` for a form this server can't work out.
pub fn parse(text: &str) -> Option<Expr> {
    let tokens = tokens(text)?;
    if tokens.is_empty() {
        return Some(Expr::Nothing);
    }
    let mut at = 0;
    let e = expr(&tokens, &mut at)?;
    (at == tokens.len()).then_some(e)
}

/// Its value for `this`; `None` when an entry is missing or the types don't fit.
pub fn eval(e: &Expr, this: &Value) -> Option<Value> {
    match e {
        Expr::Nothing => None,
        Expr::This => Some(this.clone()),
        Expr::Get(key, of) => eval(of, this)?.get(key).cloned(),
        Expr::Div(a, b) => {
            let (a, b) = (eval(a, this)?, eval(b, this)?);
            match (a.as_i64(), b.as_i64()) {
                (Some(_), Some(0)) => None,
                (Some(a), Some(b)) => Some(json!(a / b)),
                _ => {
                    let (a, b) = (a.as_f64()?, b.as_f64()?);
                    (b != 0.0).then(|| json!(a / b))
                }
            }
        }
        Expr::Upcase(of) => Some(Value::String(
            eval(of, this)?.as_str()?.to_uppercase(),
        )),
        Expr::Int(n) => Some(json!(n)),
        Expr::Float(f) => Some(json!(f)),
        Expr::Str(s) => Some(Value::String(s.clone())),
    }
}

/// The frame entries it reads, `k` of each `(k: (context-frame: self))`.
pub fn frame_keys(e: &Expr) -> Vec<String> {
    let mut keys = Vec::new();
    walk(e, &mut keys);
    keys
}

fn walk(e: &Expr, keys: &mut Vec<String>) {
    match e {
        Expr::Get(key, of) => {
            if let Expr::Get(frame, this) = of.as_ref() {
                if frame == "context-frame" && **this == Expr::This {
                    keys.push(key.clone());
                }
            }
            walk(of, keys);
        }
        Expr::Div(a, b) => {
            walk(a, keys);
            walk(b, keys);
        }
        Expr::Upcase(of) => walk(of, keys),
        _ => {}
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Open,
    Close,
    Atom(String),
    Str(String),
}

fn tokens(text: &str) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                out.push(Token::Open);
            }
            ')' => {
                chars.next();
                out.push(Token::Close);
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => s.push(chars.next()?),
                        c => s.push(c),
                    }
                }
                out.push(Token::Str(s));
            }
            _ => {
                let mut atom = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || c == '(' || c == ')' || c == '"' {
                        break;
                    }
                    atom.push(c);
                    chars.next();
                }
                out.push(Token::Atom(atom));
            }
        }
    }
    Some(out)
}

fn expr(tokens: &[Token], at: &mut usize) -> Option<Expr> {
    let token = tokens.get(*at)?;
    *at += 1;
    match token {
        Token::Str(s) => Some(Expr::Str(s.clone())),
        Token::Atom(a) if a == "self" => Some(Expr::This),
        Token::Atom(a) => a
            .parse::<i64>()
            .map(Expr::Int)
            .ok()
            .or_else(|| a.parse::<f64>().ok().filter(|f| f.is_finite()).map(Expr::Float)),
        Token::Close => None,
        Token::Open => {
            let Token::Atom(head) = tokens.get(*at)? else {
                return None;
            };
            *at += 1;
            let mut args = Vec::new();
            while *tokens.get(*at)? != Token::Close {
                args.push(expr(tokens, at)?);
            }
            *at += 1;
            let mut args = args.into_iter();
            let e = match (head.as_str(), args.len()) {
                ("/", 2) => Expr::Div(Box::new(args.next()?), Box::new(args.next()?)),
                ("string-upcase", 1) => Expr::Upcase(Box::new(args.next()?)),
                (key, 1) if key.len() > 1 && key.ends_with(':') => Expr::Get(
                    key.trim_end_matches(':').to_owned(),
                    Box::new(args.next()?),
                ),
                _ => return None,
            };
            Some(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The doc's example payload: value 56, frame `textvalue` and `numericalvalue`.
    fn doc_self() -> Value {
        let frame = json!({ "textvalue": "this is some text", "numericalvalue": 88 });
        this(Some(56), frame.as_object().unwrap())
    }

    fn run(text: &str) -> Option<Value> {
        eval(&parse(text).expect(text), &doc_self())
    }

    #[test]
    fn the_docs_accessor_examples_give_the_docs_values() {
        assert_eq!(run("(textvalue: (context-frame: self))"), Some(json!("this is some text")));
        assert_eq!(run("(numericalvalue: (context-frame: self))"), Some(json!(88)));
        assert_eq!(run("(/ (numericalvalue: (context-frame: self)) 44)"), Some(json!(2)));
        assert_eq!(
            run("(string-upcase (textvalue: (context-frame: self)))"),
            Some(json!("THIS IS SOME TEXT"))
        );
        assert_eq!(run("(value: self)"), Some(json!(56)));
    }

    #[test]
    fn the_empty_arg_has_no_value_and_missing_entries_have_none() {
        assert_eq!(parse(""), Some(Expr::Nothing));
        assert_eq!(run(""), None);
        assert_eq!(run("(nothere: (context-frame: self))"), None);
        assert_eq!(run("(/ (value: self) 0)"), None, "no division by zero");
        assert_eq!(run("(/ (value: self) 8.0)"), Some(json!(7.0)));
    }

    #[test]
    fn forms_the_doc_does_not_show_are_not_guessed() {
        for text in [
            "(+ (value: self) 1)",
            "(foo)",
            "unknown-symbol",
            "(value: self",
            "(value: self))",
            "(string-upcase a b)",
            "\"unterminated",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn the_frame_keys_an_arg_reads_are_found() {
        let e = parse("(/ (numericalvalue: (context-frame: self)) (value: self))").unwrap();
        assert_eq!(frame_keys(&e), vec!["numericalvalue".to_string()]);
        assert!(frame_keys(&parse("(value: self)").unwrap()).is_empty());
    }
}
