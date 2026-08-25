//! Request resolution and the response data model shared by all executors.

use crate::handler::{run_response_handler_with_globals, GlobalChange, HeaderChange};
use crate::parser::{Handler, Redirect, Request};
use crate::rng::Rng;
use crate::vars::{substitute, VarContext};
use serde_json::Value;
use std::collections::BTreeMap;

/// A request with all `{{variables}}` substituted and ready to send.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRequest {
    pub name: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub follow_redirects: bool,
    pub timeout_secs: Option<u64>,
    pub connection_timeout_secs: Option<u64>,
    pub pre_request_script: Option<Handler>,
    pub handler: Option<Handler>,
    pub redirect_to: Option<Redirect>,
}

/// An HTTP response.
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseData {
    pub status: u16,
    pub protocol: String,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub redirects: Vec<String>,
    pub body: Vec<u8>,
    pub elapsed_ms: u64,
}

impl ResponseData {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| key.to_lowercase() == lower)
            .map(|(_, value)| value.as_str())
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// Per-run mutable state: global variables set by response handlers.
#[derive(Debug, Default, Clone)]
pub struct RunState {
    pub globals: BTreeMap<String, Value>,
    pub logs: Vec<String>,
    pub handler_errors: Vec<String>,
    pub global_headers: BTreeMap<String, String>,
}

/// Substitutes all variables in a parsed request and returns the sendable form.
pub fn resolve_request(
    request: &Request,
    file_vars: &BTreeMap<String, Value>,
    env_vars: &BTreeMap<String, Value>,
    globals: &BTreeMap<String, Value>,
    global_headers: &BTreeMap<String, String>,
    system_env: &dyn Fn(&str) -> Option<String>,
    rng: &mut Rng,
) -> Result<ResolvedRequest, String> {
    let request_vars: BTreeMap<String, Value> = BTreeMap::new();
    let ctx = VarContext {
        env: env_vars,
        global: globals,
        file: file_vars,
        request: &request_vars,
        system_env,
    };
    let url = substitute(&request.url, &ctx, rng)?;
    let mut headers: Vec<(String, String)> = global_headers
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    for header in &request.headers {
        let value = substitute(&header.value, &ctx, rng)?;
        if let Some(existing) = headers
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case(&header.name))
        {
            *existing = (header.name.clone(), value);
        } else {
            headers.push((header.name.clone(), value));
        }
    }
    let body = request
        .body
        .as_deref()
        .map(|body| substitute(body, &ctx, rng))
        .transpose()?;
    Ok(ResolvedRequest {
        name: request.display_name.clone(),
        method: request.method.clone(),
        url,
        headers,
        body,
        follow_redirects: !request.no_redirect,
        timeout_secs: request.timeout_secs,
        connection_timeout_secs: request.connection_timeout_secs,
        pre_request_script: request.pre_request_script.clone(),
        handler: request.handler.clone(),
        redirect_to: request.redirect_to.clone(),
    })
}

/// Runs the response handler (when present) and folds its effects into the
/// run state: global variables, log lines and errors.
pub fn process_response(resolved: &ResolvedRequest, response: &ResponseData, state: &mut RunState) {
    let Some(Handler::Inline(script)) = resolved.handler.as_ref() else {
        return;
    };
    process_handler(
        script,
        response.status,
        &response.headers,
        &response.body,
        state,
    );
}

/// Runs an inline pre-request script and folds its effects into the run state.
pub fn process_pre_request(request: &Request, state: &mut RunState) {
    let Some(Handler::Inline(script)) = request.pre_request_script.as_ref() else {
        return;
    };
    process_handler(script, 0, &[], &[], state);
}

fn process_handler(
    script: &str,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
    state: &mut RunState,
) {
    let result = run_response_handler_with_globals(script, status, headers, body, &state.globals);
    for change in result.globals {
        match change {
            GlobalChange::Set(name, value) => {
                state.globals.insert(name, value);
            }
            GlobalChange::Clear(name) => {
                state.globals.remove(&name);
            }
            GlobalChange::ClearAll => state.globals.clear(),
        }
    }
    for change in result.headers {
        match change {
            HeaderChange::Set(name, value) => {
                state.global_headers.insert(name, value);
            }
            HeaderChange::Clear(name) => {
                state
                    .global_headers
                    .retain(|existing, _| !existing.eq_ignore_ascii_case(&name));
            }
        }
    }
    state.logs.extend(result.logs);
    state.handler_errors.extend(result.errors);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn resolves_urls_headers_and_bodies() {
        let doc = parse(
            "### login\nPOST https://{{host}}/login\nAuthorization: Bearer {{token}}\n\n{\"user\": \"{{user}}\"}\n",
        );
        let file_vars = BTreeMap::from([
            ("host".to_string(), Value::String("example.com".to_string())),
            ("user".to_string(), Value::String("admin".to_string())),
        ]);
        let globals = BTreeMap::from([("token".to_string(), Value::String("t0".to_string()))]);
        let mut rng = Rng::new();
        let resolved = resolve_request(
            &doc.requests[0],
            &file_vars,
            &BTreeMap::new(),
            &globals,
            &BTreeMap::new(),
            &no_env,
            &mut rng,
        )
        .unwrap();
        assert_eq!(resolved.url, "https://example.com/login");
        assert_eq!(resolved.headers[0].1, "Bearer t0");
        assert_eq!(resolved.body.as_deref(), Some("{\"user\": \"admin\"}"));
        assert!(resolved.follow_redirects);
    }

    #[test]
    fn process_response_updates_globals_from_handler() {
        let doc = parse(
            "### login\nPOST https://example.com/login\n\n{\"token\": \"abc\"}\n\n> {%\nclient.global.set(\"token\", response.body.json().token)\n%}\n",
        );
        let mut rng = Rng::new();
        let resolved = resolve_request(
            &doc.requests[0],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &no_env,
            &mut rng,
        )
        .unwrap();
        let response = ResponseData {
            status: 200,
            protocol: "HTTP/1.1".to_string(),
            status_text: "OK".to_string(),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            redirects: Vec::new(),
            body: b"{\"token\": \"abc\"}".to_vec(),
            elapsed_ms: 5,
        };
        let mut state = RunState::default();
        process_response(&resolved, &response, &mut state);
        assert_eq!(
            state.globals.get("token").and_then(Value::as_str),
            Some("abc")
        );
    }

    #[test]
    fn pre_request_scripts_apply_globals_and_headers() {
        let doc = parse(
            "< {%\nclient.global.set(\"token\", \"abc\")\nclient.global.headers.set(\"Authorization\", client.global.get(\"token\"))\n%}\nGET https://example.com\n",
        );
        let mut state = RunState::default();
        process_pre_request(&doc.requests[0], &mut state);
        let mut rng = Rng::new();
        let resolved = resolve_request(
            &doc.requests[0],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &state.globals,
            &state.global_headers,
            &no_env,
            &mut rng,
        )
        .unwrap();
        assert_eq!(
            state.globals.get("token"),
            Some(&Value::String("abc".to_string()))
        );
        assert_eq!(
            resolved.headers,
            vec![("Authorization".to_string(), "abc".to_string())]
        );
    }
}
