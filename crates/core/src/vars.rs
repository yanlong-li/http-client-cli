//! `{{variable}}` substitution with documented compatibility precedence and the
//! built-in dynamic variables.

use crate::rng::{iso8601_timestamp, unix_timestamp, Rng};
use serde_json::Value;
use std::collections::BTreeMap;

/// Variable scopes for one substitution. Lookups follow the compatibility order:
/// environment > global > in-place (file) > per-request.
#[derive(Clone, Copy)]
pub struct VarContext<'a> {
    pub env: &'a BTreeMap<String, Value>,
    pub global: &'a BTreeMap<String, Value>,
    pub file: &'a BTreeMap<String, Value>,
    pub request: &'a BTreeMap<String, Value>,
    pub system_env: &'a dyn Fn(&str) -> Option<String>,
}

/// Substitutes all `{{...}}` placeholders. Substituted values may themselves
/// contain placeholders, which are resolved iteratively (bounded depth).
pub fn substitute(input: &str, ctx: &VarContext, rng: &mut Rng) -> Result<String, String> {
    let mut current = input.to_string();
    for _ in 0..10 {
        let next = substitute_once(&current, ctx, rng)?;
        if next == current {
            return Ok(current);
        }
        current = next;
    }
    Ok(current)
}

fn substitute_once(input: &str, ctx: &VarContext, rng: &mut Rng) -> Result<String, String> {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            output.push_str(rest);
            return Ok(output);
        };
        output.push_str(&rest[..start]);
        let name = after[..end].trim();
        output.push_str(&resolve(name, ctx, rng)?);
        rest = &after[end + 2..];
    }
    output.push_str(rest);
    Ok(output)
}

fn resolve(name: &str, ctx: &VarContext, rng: &mut Rng) -> Result<String, String> {
    if name.starts_with('$') {
        return resolve_dynamic(name, ctx, rng);
    }
    for scope in [ctx.env, ctx.global, ctx.file, ctx.request] {
        if let Some(value) = scope.get(name) {
            return Ok(value_to_string(value));
        }
        if name.contains('.') || name.contains('[') {
            if let Some(value) = resolve_path(scope, name) {
                return Ok(value_to_string(value));
            }
        }
    }
    Err(format!("unresolved variable: {{{{{name}}}}}"))
}

fn resolve_dynamic(name: &str, ctx: &VarContext, rng: &mut Rng) -> Result<String, String> {
    match name {
        "$uuid" | "$random.uuid" => Ok(rng.uuid_v4()),
        "$timestamp" => Ok(unix_timestamp().to_string()),
        "$isoTimestamp" => Ok(iso8601_timestamp()),
        "$randomInt" => Ok(rng.int_range(0, 1000).to_string()),
        "$random.email" => Ok(rng.email()),
        _ => {
            if let Some(var) = name.strip_prefix("$env.") {
                return (ctx.system_env)(var)
                    .ok_or_else(|| format!("unresolved environment variable: {var}"));
            }
            if let Some(var) = name.strip_prefix("$processEnv.") {
                return (ctx.system_env)(var)
                    .ok_or_else(|| format!("unresolved environment variable: {var}"));
            }
            if let Some(args) = name.strip_prefix("$random.integer") {
                let (lo, hi) = parse_int_args(args, 0, 1000);
                return Ok(rng.int_range(lo, hi).to_string());
            }
            if let Some(args) = name.strip_prefix("$random.float") {
                let (lo, hi) = parse_int_args(args, 0, 1000);
                let value = lo as f64 + rng.next_u64() as f64 / u64::MAX as f64 * (hi - lo) as f64;
                return Ok(format!("{value:.4}"));
            }
            if let Some(args) = name.strip_prefix("$random.alphabetic") {
                return Ok(rng.alphabetic(parse_len_arg(args)?));
            }
            if let Some(args) = name.strip_prefix("$random.alphanumeric") {
                return Ok(rng.alphanumeric(parse_len_arg(args)?));
            }
            if let Some(args) = name.strip_prefix("$random.hexadecimal") {
                return Ok(rng.hexadecimal(parse_len_arg(args)?));
            }
            Err(format!("unknown dynamic variable: {{{{{name}}}}}"))
        }
    }
}

fn parse_int_args(args: &str, default_lo: i64, default_hi: i64) -> (i64, i64) {
    let args = args.trim().trim_start_matches('(').trim_end_matches(')');
    let parts: Vec<&str> = args.split(',').map(|p| p.trim()).collect();
    match (parts.first(), parts.get(1)) {
        (Some(lo), Some(hi)) => (
            lo.parse().unwrap_or(default_lo),
            hi.parse().unwrap_or(default_hi),
        ),
        (Some(lo), None) if !lo.is_empty() => (lo.parse().unwrap_or(default_lo), default_hi),
        _ => (default_lo, default_hi),
    }
}

fn parse_len_arg(args: &str) -> Result<usize, String> {
    let args = args.trim().trim_start_matches('(').trim_end_matches(')');
    let value: usize = args
        .parse()
        .map_err(|_| format!("invalid length argument: {args}"))?;
    if value == 0 {
        return Err("length must be greater than 0".to_string());
    }
    Ok(value)
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Resolves a dotted (and bracketed) path such as `client.host.url`,
/// `users[0].name` or `client.['host.url']` inside a variable scope.
fn resolve_path<'a>(scope: &'a BTreeMap<String, Value>, path: &str) -> Option<&'a Value> {
    let segments = parse_path_segments(path)?;
    let first_key = match segments.first()? {
        PathSegment::Key(key) => *key,
        PathSegment::Index(_) => return None,
    };
    let mut current: &Value = scope.get(first_key)?;
    for segment in segments.iter().skip(1) {
        current = match (current, segment) {
            (Value::Object(map), PathSegment::Key(key)) => map.get(*key)?,
            (Value::Array(items), PathSegment::Index(index)) => items.get(*index)?,
            _ => return None,
        };
    }
    Some(current)
}

enum PathSegment<'a> {
    Key(&'a str),
    Index(usize),
}

fn parse_path_segments(path: &str) -> Option<Vec<PathSegment<'_>>> {
    let mut segments = Vec::new();
    let chars = path.char_indices().peekable();
    let mut start = 0usize;
    let mut in_bracket = false;

    for (i, c) in chars {
        match c {
            '.' if !in_bracket => {
                if i > start {
                    segments.push(PathSegment::Key(&path[start..i]));
                }
                start = i + 1;
            }
            '[' => {
                if i > start && !in_bracket {
                    segments.push(PathSegment::Key(&path[start..i]));
                }
                in_bracket = true;
                start = i + 1;
            }
            ']' => {
                let inner = &path[start..i];
                let segment = if let Ok(index) = inner.parse::<usize>() {
                    PathSegment::Index(index)
                } else {
                    PathSegment::Key(inner.trim_matches(|c| c == '\'' || c == '"').trim())
                };
                segments.push(segment);
                in_bracket = false;
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < path.len() {
        segments.push(PathSegment::Key(&path[start..]));
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leak(map: BTreeMap<String, Value>) -> &'static BTreeMap<String, Value> {
        Box::leak(Box::new(map))
    }

    fn test_context() -> VarContext<'static> {
        let env = BTreeMap::from([
            ("host".to_string(), Value::String("env-host".to_string())),
            (
                "client".to_string(),
                serde_json::json!({"host": {"url": "https://env.example.com"}}),
            ),
        ]);
        let global = BTreeMap::from([(
            "token".to_string(),
            Value::String("global-token".to_string()),
        )]);
        let file = BTreeMap::from([("host".to_string(), Value::String("file-host".to_string()))]);
        let request = BTreeMap::from([("user".to_string(), Value::String("req-user".to_string()))]);
        VarContext {
            env: leak(env),
            global: leak(global),
            file: leak(file),
            request: leak(request),
            system_env: Box::leak(Box::new(|_: &str| None)),
        }
    }

    #[test]
    fn resolves_in_documented_priority_order() {
        let ctx = test_context();
        let mut rng = Rng::new();
        assert_eq!(
            substitute("https://{{host}}/api", &ctx, &mut rng).unwrap(),
            "https://env-host/api"
        );
        assert_eq!(
            substitute("Bearer {{token}}", &ctx, &mut rng).unwrap(),
            "Bearer global-token"
        );
        assert_eq!(substitute("{{user}}", &ctx, &mut rng).unwrap(), "req-user");
    }

    #[test]
    fn resolves_dotted_paths() {
        let ctx = test_context();
        let mut rng = Rng::new();
        assert_eq!(
            substitute("{{client.host.url}}/api", &ctx, &mut rng).unwrap(),
            "https://env.example.com/api"
        );
    }

    #[test]
    fn resolves_dynamic_variables() {
        let ctx = test_context();
        let mut rng = Rng::new();
        let uuid = substitute("{{$uuid}}", &ctx, &mut rng).unwrap();
        assert_eq!(uuid.len(), 36);
        let timestamp: u64 = substitute("{{$timestamp}}", &ctx, &mut rng)
            .unwrap()
            .parse()
            .unwrap();
        assert!(timestamp > 1_500_000_000);
        let int: i64 = substitute("{{$random.integer(10, 20)}}", &ctx, &mut rng)
            .unwrap()
            .parse()
            .unwrap();
        assert!((10..20).contains(&int));
    }

    #[test]
    fn resolves_system_env() {
        let ctx = VarContext {
            env: &BTreeMap::new(),
            global: &BTreeMap::new(),
            file: &BTreeMap::new(),
            request: &BTreeMap::new(),
            system_env: &|name: &str| (name == "USER").then(|| "admin".to_string()),
        };
        let mut rng = Rng::new();
        assert_eq!(
            substitute("{{$env.USER}}", &ctx, &mut rng).unwrap(),
            "admin"
        );
    }

    #[test]
    fn reports_unresolved_variables() {
        let ctx = test_context();
        let mut rng = Rng::new();
        assert!(substitute("{{missing}}", &ctx, &mut rng).is_err());
    }

    #[test]
    fn substitutes_recursively() {
        let file = BTreeMap::from([
            ("a".to_string(), Value::String("{{b}}".to_string())),
            ("b".to_string(), Value::String("done".to_string())),
        ]);
        let ctx = VarContext {
            env: &BTreeMap::new(),
            global: &BTreeMap::new(),
            file: leak(file),
            request: &BTreeMap::new(),
            system_env: &|_: &str| None,
        };
        let mut rng = Rng::new();
        assert_eq!(substitute("{{a}}", &ctx, &mut rng).unwrap(), "done");
    }
}
