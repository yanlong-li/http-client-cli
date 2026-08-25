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
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum GlobalChange {
    Set(String, Value),
    Clear(String),
    ClearAll,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HeaderChange {
    Set(String, String),
    Clear(String),
}

#[derive(Debug, Default, Clone)]
pub struct HandlerResult {
    pub globals: Vec<GlobalChange>,
    pub headers: Vec<HeaderChange>,
    pub logs: Vec<String>,
    pub errors: Vec<String>,
    pub exited: bool,
}

/// Runs the response-handler subset against a response.
pub fn run_response_handler(
    script: &str,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> HandlerResult {
    run_response_handler_with_globals(script, status, headers, body, &BTreeMap::new())
}

/// Runs a handler with the globals that existed before this script began.
pub fn run_response_handler_with_globals(
    script: &str,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
    globals: &BTreeMap<String, Value>,
) -> HandlerResult {
    let mut result = HandlerResult::default();
    let mut current_globals = globals.clone();
    let lines: Vec<&str> = script.lines().collect();
    let mut line_index = 0;
    while line_index < lines.len() {
        if result.exited {
            break;
        }
        let mut statement = lines[line_index].trim().to_string();
        if statement.starts_with("client.test") {
            while !is_complete_statement(&statement) && line_index + 1 < lines.len() {
                line_index += 1;
                statement.push('\n');
                statement.push_str(lines[line_index].trim());
            }
        }
        let line = statement.trim_end_matches(';').trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            line_index += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix("client.global.set") {
            match parse_call_args(rest) {
                Some((name_arg, expr)) => match parse_string_literal(&name_arg) {
                    Some(name) => {
                        match eval_with_globals(&expr, status, headers, body, &current_globals) {
                            Ok(value) => {
                                current_globals.insert(name.clone(), value.clone());
                                result.globals.push(GlobalChange::Set(name, value));
                            }
                            Err(error) => result.errors.push(format!("{error} in: {line}")),
                        }
                    }
                    None => result.errors.push(format!(
                        "first argument of client.global.set must be a string literal: {line}"
                    )),
                },
                None => result
                    .errors
                    .push(format!("could not parse arguments of: {line}")),
            }
        } else if let Some(rest) = line.strip_prefix("client.global.clearAll") {
            if rest.trim() == "()" {
                current_globals.clear();
                result.globals.push(GlobalChange::ClearAll);
            } else {
                result
                    .errors
                    .push(format!("could not parse arguments of: {line}"));
            }
        } else if let Some(rest) = line.strip_prefix("client.global.clear") {
            match parse_call_args(rest).and_then(|(name, _)| parse_string_literal(&name)) {
                Some(name) => {
                    current_globals.remove(&name);
                    result.globals.push(GlobalChange::Clear(name));
                }
                None => result
                    .errors
                    .push(format!("could not parse arguments of: {line}")),
            }
        } else if let Some(rest) = line.strip_prefix("client.global.headers.set") {
            match parse_call_args(rest) {
                Some((name, value)) => match (
                    parse_string_literal(&name),
                    eval_with_globals(&value, status, headers, body, &current_globals),
                ) {
                    (Some(name), Ok(value)) if !value.is_null() => result
                        .headers
                        .push(HeaderChange::Set(name, value_to_log_string(&value))),
                    (Some(name), Ok(_)) => result.headers.push(HeaderChange::Clear(name)),
                    (_, Err(error)) => result.errors.push(format!("{error} in: {line}")),
                    _ => result
                        .errors
                        .push(format!("header name must be a string literal: {line}")),
                },
                None => result
                    .errors
                    .push(format!("could not parse arguments of: {line}")),
            }
        } else if line == "client.exit()" {
            result.exited = true;
        } else if let Some(rest) = line.strip_prefix("client.test") {
            match parse_call_args(rest) {
                Some((name, test_body)) => match parse_string_literal(&name) {
                    Some(name) => {
                        let function_body = extract_function_body(&test_body);
                        if let Some((condition, message)) = extract_assertion(&test_body) {
                            match eval_condition(
                                &condition,
                                status,
                                headers,
                                body,
                                &current_globals,
                            ) {
                                Ok(true) => {}
                                Ok(false) => result.errors.push(format!(
                                    "test `{name}` failed{}",
                                    message
                                        .map(|message| format!(": {message}"))
                                        .unwrap_or_default()
                                )),
                                Err(error) => {
                                    result.errors.push(format!("test `{name}` failed: {error}"))
                                }
                            }
                        }
                        if let Some(function_body) = function_body {
                            let statements_without_assertions = function_body
                                .lines()
                                .filter(|statement| {
                                    !statement.trim_start().starts_with("client.assert")
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            let nested = run_response_handler_with_globals(
                                &statements_without_assertions,
                                status,
                                headers,
                                body,
                                &current_globals,
                            );
                            apply_global_changes(&nested.globals, &mut current_globals);
                            result.globals.extend(nested.globals);
                            result.headers.extend(nested.headers);
                            result.logs.extend(nested.logs);
                            result.errors.extend(nested.errors);
                            if nested.exited {
                                result.exited = true;
                            }
                        }
                    }
                    None => result.errors.push(format!(
                        "first argument of client.test must be a string literal: {line}"
                    )),
                },
                None => result.errors.push(format!("could not parse test: {line}")),
            }
        } else if let Some(rest) = line.strip_prefix("client.assert") {
            match parse_call_args(rest) {
                Some((condition, message)) => {
                    match eval_condition(&condition, status, headers, body, &current_globals) {
                        Ok(true) => {}
                        Ok(false) => result.errors.push(
                            parse_string_literal(&message)
                                .unwrap_or_else(|| "assertion failed".to_string()),
                        ),
                        Err(error) => result.errors.push(error),
                    }
                }
                None => result
                    .errors
                    .push(format!("could not parse assertion: {line}")),
            }
        } else if let Some(rest) = line.strip_prefix("client.log") {
            match parse_call_args(rest) {
                Some((expr, _)) => {
                    match eval_with_globals(&expr, status, headers, body, &current_globals) {
                        Ok(value) => result.logs.push(value_to_log_string(&value)),
                        Err(error) => result.errors.push(format!("{error} in: {line}")),
                    }
                }
                None => result
                    .errors
                    .push(format!("could not parse arguments of: {line}")),
            }
        }
        // client.test / client.assert / arbitrary JavaScript are not available
        // without a JavaScript engine and remain intentionally ignored.
        line_index += 1;
    }
    result
}

fn apply_global_changes(changes: &[GlobalChange], globals: &mut BTreeMap<String, Value>) {
    for change in changes {
        match change {
            GlobalChange::Set(name, value) => {
                globals.insert(name.clone(), value.clone());
            }
            GlobalChange::Clear(name) => {
                globals.remove(name);
            }
            GlobalChange::ClearAll => globals.clear(),
        }
    }
}

fn extract_function_body(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (start < end).then(|| &text[start + 1..end])
}

fn is_complete_statement(text: &str) -> bool {
    let mut parens = 0i32;
    let mut braces = 0i32;
    let mut quote = None;
    let mut escaped = false;
    for character in text.chars() {
        if let Some(current_quote) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == current_quote {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' => parens += 1,
            ')' => parens -= 1,
            '{' => braces += 1,
            '}' => braces -= 1,
            _ => {}
        }
    }
    quote.is_none() && parens == 0 && braces == 0
}

fn extract_assertion(body: &str) -> Option<(String, Option<String>)> {
    let start = body.find("client.assert")?;
    let rest = body[start + "client.assert".len()..].trim();
    let end = rest.rfind(')')?;
    let (condition, message) = parse_call_args(&rest[..=end])?;
    Some((condition, parse_string_literal(&message)))
}

fn eval_condition(
    condition: &str,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
    globals: &BTreeMap<String, Value>,
) -> Result<bool, String> {
    for operator in ["===", "!==", "==", "!="] {
        if let Some((left, right)) = condition.split_once(operator) {
            let equal = eval_with_globals(left, status, headers, body, globals)?
                == eval_with_globals(right, status, headers, body, globals)?;
            return Ok(if operator == "!==" || operator == "!=" {
                !equal
            } else {
                equal
            });
        }
    }
    match eval_with_globals(condition, status, headers, body, globals)? {
        Value::Bool(value) => Ok(value),
        _ => Err("assertion condition must evaluate to a boolean".to_string()),
    }
}

fn eval_with_globals(
    expr: &str,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
    globals: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    if let Some(rest) = expr.trim().strip_prefix("client.global.get") {
        let name = parse_call_args(rest)
            .and_then(|(name, _)| parse_string_literal(&name))
            .ok_or("client.global.get requires a string literal")?;
        return Ok(globals.get(&name).cloned().unwrap_or(Value::Null));
    }
    if expr.trim() == "client.global.isEmpty()" {
        return Ok(Value::Bool(globals.is_empty()));
    }
    eval(expr, status, headers, body)
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
    Num(String),
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
                serde_json::from_str::<serde_json::Number>(&number)
                    .map_err(|_| format!("invalid number: {number}"))?;
                tokens.push(Token::Num(number));
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
        Some(Token::Num(text)) => {
            *tokens = &tokens[1..];
            let value =
                serde_json::from_str(&text).map_err(|_| format!("invalid number: {text}"))?;
            Ok(Value::Number(value))
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
            let index = index
                .parse::<usize>()
                .map_err(|_| "array index must be a non-negative integer".to_string())?;
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
            vec![GlobalChange::Set(
                "token".to_string(),
                Value::String("abc123".to_string())
            )]
        );
        assert!(result.errors.is_empty());
    }

    #[test]
    fn preserves_integer_literals_as_json_integers() {
        let result = run_response_handler(r#"client.global.set("test", 1);"#, 200, &[], b"");
        assert_eq!(
            result.globals,
            vec![GlobalChange::Set("test".to_string(), Value::from(1))]
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
            vec![GlobalChange::Set(
                "second".to_string(),
                Value::String("Lin".to_string())
            )]
        );
    }

    #[test]
    fn ignores_unsupported_statements() {
        let script = "client.test(\"ok\", function() { client.assert(response.status === 200) })";
        let result = run_response_handler(script, 200, &[], b"");
        assert!(result.globals.is_empty());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn manages_globals_headers_and_tests() {
        let globals = BTreeMap::from([("old".to_string(), Value::from(1))]);
        let script = r#"
client.global.set("token", "abc")
client.global.set("copied", client.global.get("token"))
client.global.clear("old")
client.global.headers.set("Authorization", client.global.get("copied"))
client.test("created", function() { client.assert(response.status === 201, "expected created") })
"#;
        let result = run_response_handler_with_globals(script, 201, &[], b"", &globals);
        assert_eq!(
            result.globals,
            vec![
                GlobalChange::Set("token".to_string(), Value::String("abc".to_string())),
                GlobalChange::Set("copied".to_string(), Value::String("abc".to_string())),
                GlobalChange::Clear("old".to_string()),
            ]
        );
        assert_eq!(
            result.headers,
            vec![HeaderChange::Set(
                "Authorization".to_string(),
                "abc".to_string()
            )]
        );
        assert!(result.errors.is_empty());
    }

    #[test]
    fn supports_multiline_tests() {
        let script = r#"
client.test("created", function() {
    client.assert(response.status === 201, "expected created")
})
"#;
        let result = run_response_handler(script, 201, &[], b"");
        assert!(result.errors.is_empty());
    }

    #[test]
    fn completes_a_multiline_test_with_an_unsupported_statement() {
        let script = r#"
client.global.set("createdUrl", response.body.json().url);
client.log(response.status);

client.test("测试",function(){
  client.assets(1===2,"test");
  client.global.set("test",response.body.json().origin);
  client.global.set("status",response.status);
  client.global.set("bool",true);
});
"#;
        let result = run_response_handler(
            script,
            200,
            &[],
            br#"{"url":"https://example.com","origin":"127.0.0.1"}"#,
        );
        assert!(result.errors.is_empty());
        assert_eq!(
            result.globals,
            vec![
                GlobalChange::Set(
                    "createdUrl".to_string(),
                    Value::String("https://example.com".to_string())
                ),
                GlobalChange::Set("test".to_string(), Value::String("127.0.0.1".to_string())),
                GlobalChange::Set("status".to_string(), Value::from(200)),
                GlobalChange::Set("bool".to_string(), Value::Bool(true)),
            ]
        );
    }

    #[test]
    fn skips_blank_and_comment_lines_without_looping() {
        let result = run_response_handler(
            "client.log(response.status)\n\n// separator\n# another separator\nclient.log(response.status)",
            200,
            &[],
            b"",
        );
        assert_eq!(result.logs, vec!["200".to_string(), "200".to_string()]);
    }

    #[test]
    fn reports_a_failed_test_once() {
        let result = run_response_handler(
            "client.test(\"status\", function() {\nclient.assert(response.status === 201, \"expected created\")\n})",
            200,
            &[],
            b"",
        );
        assert_eq!(
            result.errors,
            vec!["test `status` failed: expected created"]
        );
    }
}
