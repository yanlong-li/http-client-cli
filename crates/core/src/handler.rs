//! A pragmatic compatibility subset of the response-handler script API.
//!
//! Supported statements (one per line):
//!
//! * `client.global.set("name", <expr>)`
//! * `client.log(<expr>)`
//!
//! Supported expressions: `response.status`, `response.body`,
//! `response.body.json()`, `response.headers.valueOf("Name")`,
//! `response.headers.valuesOf("Name")`, string/number/boolean/null literals
//! and property / index access chains (`.prop`, `[0]`, `['key']`).
//!
//! Anything else (tests, control flow, ...) is ignored. This is enough for
//! the common chain-login-then-use-token workflows without embedding a full
//! JavaScript engine.

use serde_json::Value;

#[derive(Debug, Default, Clone)]
pub struct HandlerResult {
    pub globals: Vec<(String, Value)>,
    pub logs: Vec<String>,
    pub errors: Vec<String>,
}

/// Runs the response-handler subset against a response.
pub fn run_response_handler(
    script: &str,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> HandlerResult {
    let mut result = HandlerResult::default();
    for raw_line in script.lines() {
        let line = raw_line.trim().trim_end_matches(';').trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("client.global.set") {
            match parse_call_args(rest) {
                Some((name_arg, expr)) => match parse_string_literal(&name_arg) {
                    Some(name) => match eval(&expr, status, headers, body) {
                        Ok(value) => result.globals.push((name, value)),
                        Err(error) => result.errors.push(format!("{error} in: {line}")),
                    },
                    None => result.errors.push(format!(
                        "first argument of client.global.set must be a string literal: {line}"
                    )),
                },
                None => result
                    .errors
                    .push(format!("could not parse arguments of: {line}")),
            }
        } else if let Some(rest) = line.strip_prefix("client.log") {
            match parse_call_args(rest) {
                Some((expr, _)) => match eval(&expr, status, headers, body) {
                    Ok(value) => result.logs.push(value_to_log_string(&value)),
                    Err(error) => result.errors.push(format!("{error} in: {line}")),
                },
                None => result
                    .errors
                    .push(format!("could not parse arguments of: {line}")),
            }
        }
        // client.test / client.assert / anything else: silently ignored.
    }
    result
}

/// Splits `(arg1, arg2)` into its two top-level argument strings.
fn parse_call_args(rest: &str) -> Option<(String, String)> {
    let rest = rest.trim();
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (index, c) in inner.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    return Some((
                        inner[..index].trim().to_string(),
                        inner[index + 1..].trim().to_string(),
                    ));
                }
                _ => {}
            },
        }
    }
    Some((inner.trim().to_string(), String::new()))
}

fn parse_string_literal(text: &str) -> Option<String> {
    let text = text.trim();
    let inner = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            text.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })?;
    Some(inner.to_string())
}

fn value_to_log_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Str(String),
    Num(f64),
    Punct(char),
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '.' | '[' | ']' | '(' | ')' => {
                tokens.push(Token::Punct(c));
                chars.next();
            }
            '"' | '\'' => {
                let quote = c;
                chars.next();
                let mut value = String::new();
                let mut closed = false;
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        if let Some(escaped) = chars.next() {
                            value.push(escaped);
                        }
                    } else if c == quote {
                        closed = true;
                        break;
                    } else {
                        value.push(c);
                    }
                }
                if !closed {
                    return Err("unterminated string".to_string());
                }
                tokens.push(Token::Str(value));
            }
            c if c.is_ascii_digit() || c == '-' => {
                let mut number = String::new();
                number.push(c);
                chars.next();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() || c == '.' {
                        number.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let value: f64 = number
                    .parse()
                    .map_err(|_| format!("invalid number: {number}"))?;
                tokens.push(Token::Num(value));
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut ident = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' {
                        ident.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Ident(ident));
            }
            other => return Err(format!("unexpected character: {other}")),
        }
    }
    Ok(tokens)
}

/// Evaluates a supported response-handler expression.
pub fn eval(
    expr: &str,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<Value, String> {
    let tokens = tokenize(expr)?;
    let mut tokens = tokens.as_slice();
    let mut value = parse_primary(&mut tokens, status, headers, body)?;
    loop {
        match tokens.first() {
            Some(Token::Punct('.')) => {
                tokens = &tokens[1..];
                let Token::Ident(name) =
                    tokens.first().ok_or("expected property after `.`")?.clone()
                else {
                    return Err("expected property name after `.`".to_string());
                };
                tokens = &tokens[1..];
                if tokens.first() == Some(&Token::Punct('(')) {
                    tokens = &tokens[1..];
                    let mut argument = None;
                    if tokens.first() != Some(&Token::Punct(')')) {
                        match tokens.first().cloned() {
                            Some(Token::Str(value)) => {
                                argument = Some(value);
                                tokens = &tokens[1..];
                            }
                            _ => {
                                return Err(format!(
                                    "method .{name}() takes a single string argument"
                                ))
                            }
                        }
                    }
                    if tokens.first() != Some(&Token::Punct(')')) {
                        return Err("expected `)`".to_string());
                    }
                    tokens = &tokens[1..];
                    value = apply_method(&value, &name, argument.as_deref(), headers, body)?;
                } else {
                    value = apply_property(&value, &name)?;
                }
            }
            Some(Token::Punct('[')) => {
                tokens = &tokens[1..];
                let index_expr = tokens.first().cloned().ok_or("expected index")?;
                tokens = &tokens[1..];
                if tokens.first() != Some(&Token::Punct(']')) {
                    return Err("expected `]`".to_string());
                }
                tokens = &tokens[1..];
                value = apply_index(&value, index_expr)?;
            }
            None => break,
            _ => return Err("unexpected trailing tokens".to_string()),
        }
    }
    Ok(value)
}

fn parse_primary(
    tokens: &mut &[Token],
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<Value, String> {
    match tokens.first().cloned() {
        Some(Token::Ident(ident)) if ident == "response" => {
            *tokens = &tokens[1..];
            Ok(response_value(status, headers, body))
        }
        Some(Token::Str(value)) => {
            *tokens = &tokens[1..];
            Ok(Value::String(value))
        }
        Some(Token::Num(value)) => {
            *tokens = &tokens[1..];
            Ok(serde_json::json!(value))
        }
        Some(Token::Ident(ident)) if ident == "true" => {
            *tokens = &tokens[1..];
            Ok(Value::Bool(true))
        }
        Some(Token::Ident(ident)) if ident == "false" => {
            *tokens = &tokens[1..];
            Ok(Value::Bool(false))
        }
        Some(Token::Ident(ident)) if ident == "null" => {
            *tokens = &tokens[1..];
            Ok(Value::Null)
        }
        _ => Err("expected `response` or a literal".to_string()),
    }
}

fn response_value(status: u16, headers: &[(String, String)], body: &[u8]) -> Value {
    let header_map: serde_json::Map<String, Value> = headers
        .iter()
        .map(|(name, value)| (name.to_lowercase(), Value::String(value.clone())))
        .collect();
    serde_json::json!({
        "status": status,
        "body": String::from_utf8_lossy(body).to_string(),
        "headers": Value::Object(header_map),
    })
}

fn apply_property(value: &Value, name: &str) -> Result<Value, String> {
    match value {
        Value::Object(map) => Ok(map.get(name).cloned().unwrap_or(Value::Null)),
        _ => Err(format!(
            "cannot access property `{name}` on {}",
            type_name(value)
        )),
    }
}

fn apply_index(value: &Value, index: Token) -> Result<Value, String> {
    match (value, index) {
        (Value::Array(items), Token::Num(index)) => {
            let index = index as usize;
            Ok(items.get(index).cloned().unwrap_or(Value::Null))
        }
        (Value::Object(map), Token::Str(key)) => Ok(map.get(&key).cloned().unwrap_or(Value::Null)),
        _ => Err("unsupported index expression".to_string()),
    }
}

fn apply_method(
    value: &Value,
    name: &str,
    argument: Option<&str>,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<Value, String> {
    match (value, name, argument) {
        (Value::String(_), "json", None) => {
            serde_json::from_slice(body).map_err(|_| "response body is not valid JSON".to_string())
        }
        (Value::Object(_), "valueOf", Some(header_name)) => {
            let lower = header_name.to_lowercase();
            Ok(headers
                .iter()
                .find(|(name, _)| name.to_lowercase() == lower)
                .map(|(_, value)| Value::String(value.clone()))
                .unwrap_or(Value::Null))
        }
        (Value::Object(_), "valuesOf", Some(header_name)) => {
            let lower = header_name.to_lowercase();
            Ok(Value::Array(
                headers
                    .iter()
                    .filter(|(name, _)| name.to_lowercase() == lower)
                    .map(|(_, value)| Value::String(value.clone()))
                    .collect(),
            ))
        }
        _ => Err(format!("unsupported method: .{name}()")),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn sets_global_from_json_body() {
        let script = r#"client.global.set("token", response.body.json().token)"#;
        let body = br#"{"token": "abc123"}"#;
        let result = run_response_handler(
            script,
            200,
            &headers(&[("Content-Type", "application/json")]),
            body,
        );
        assert_eq!(
            result.globals,
            vec![("token".to_string(), Value::String("abc123".to_string()))]
        );
        assert!(result.errors.is_empty());
    }

    #[test]
    fn logs_status_and_headers() {
        let script =
            "client.log(response.status)\nclient.log(response.headers.valueOf(\"Set-Cookie\"))";
        let result = run_response_handler(script, 201, &headers(&[("Set-Cookie", "a=1")]), b"");
        assert_eq!(result.logs, vec!["201".to_string(), "a=1".to_string()]);
    }

    #[test]
    fn supports_index_and_nested_access() {
        let body = br#"{"users": [{"name": "Ada"}, {"name": "Lin"}]}"#;
        let script = r#"client.global.set("second", response.body.json().users[1].name)"#;
        let result = run_response_handler(script, 200, &[], body);
        assert_eq!(
            result.globals,
            vec![("second".to_string(), Value::String("Lin".to_string()))]
        );
    }

    #[test]
    fn ignores_unsupported_statements() {
        let script = "client.test(\"ok\", function() { client.assert(response.status === 200) })";
        let result = run_response_handler(script, 200, &[], b"");
        assert!(result.globals.is_empty());
        assert!(result.errors.is_empty());
    }
}
