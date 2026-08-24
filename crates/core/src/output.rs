//! Response formatting for terminals.

use crate::execute::{ResolvedRequest, ResponseData};

const MAX_BODY_BYTES: usize = 256 * 1024;

/// Formats a response as plain text for the CLI / task terminal.
pub fn format_response_plain(
    request: &ResolvedRequest,
    response: &ResponseData,
    logs: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("── {} ", request.name));
    out.push_str(&"─".repeat(60usize.saturating_sub(request.name.len() + 4)));
    out.push('\n');
    out.push_str(&format!("{} {}\n", request.method, request.url));
    out.push('\n');
    if !request.headers.is_empty() {
        out.push_str("Request headers:\n");
        for (name, value) in &request.headers {
            out.push_str(&format!("  {name}: {value}\n"));
        }
        out.push('\n');
    }
    if !response.redirects.is_empty() {
        out.push_str("Redirects:\n");
        for redirect in &response.redirects {
            out.push_str(&format!("  -> {redirect}\n"));
        }
        out.push('\n');
    }
    let status_line = if response.status_text.is_empty() {
        response.status.to_string()
    } else {
        format!("{} {}", response.status, response.status_text)
    };
    out.push_str(&format!("{} {}\n", response.protocol, status_line));
    out.push_str(&format!(
        "Response code: {}; Time: {} ms; Content length: {} bytes\n",
        response.status,
        response.elapsed_ms,
        response.body.len()
    ));
    out.push('\n');
    if !response.headers.is_empty() {
        out.push_str("Response headers:\n");
        for (name, value) in &response.headers {
            out.push_str(&format!("  {name}: {value}\n"));
        }
        out.push('\n');
    }
    let body = display_body(response, false);
    if !body.is_empty() {
        out.push_str("Body:\n");
        for line in body.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    } else {
        out.push_str("Body: (empty)\n");
    }
    if !logs.is_empty() {
        out.push('\n');
        out.push_str("Logs:\n");
        for log in logs {
            out.push_str(&format!("  {log}\n"));
        }
    }
    out
}

/// Formats a response as Markdown.
pub fn format_response_markdown(
    request: &ResolvedRequest,
    response: Option<&ResponseData>,
    logs: &[String],
) -> String {
    let mut out = String::new();
    match response {
        Some(response) => {
            if response.status == 0 {
                out.push_str(&format!(
                    "### ⚠️ completed (status unavailable) · {} ms\n\n",
                    response.elapsed_ms
                ));
            } else {
                let icon = if (200..300).contains(&response.status) {
                    "✅"
                } else {
                    "⚠️"
                };
                out.push_str(&format!(
                    "### {icon} {} · {} ms\n\n",
                    response.status, response.elapsed_ms
                ));
            }
        }
        None => out.push_str("### ⚠️ completed (status unavailable)\n\n"),
    }
    out.push_str(&format!("**{}** `{}`\n\n", request.method, request.url));
    if !request.headers.is_empty() {
        out.push_str("**Request Headers**\n\n");
        out.push_str("```\n");
        for (name, value) in &request.headers {
            out.push_str(&format!("{name}: {value}\n"));
        }
        out.push_str("```\n\n");
    }
    if let Some(response) = response {
        if !response.headers.is_empty() {
            out.push_str("**Response Headers**\n\n");
            out.push_str("```\n");
            for (name, value) in &response.headers {
                out.push_str(&format!("{name}: {value}\n"));
            }
            out.push_str("```\n\n");
        }
        let (body, language) = display_body_markdown(response);
        if !body.is_empty() {
            out.push_str(&format!("**Body**\n\n```{language}\n{body}\n```\n"));
        } else {
            out.push_str("**Body**\n\n(empty)\n");
        }
    }
    if !logs.is_empty() {
        out.push_str("\n**Logs**\n\n");
        for log in logs {
            out.push_str(&format!("- {log}\n"));
        }
    }
    out
}

fn display_body(response: &ResponseData, _markdown: bool) -> String {
    if response.body.is_empty() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&response.body);
    let text = if is_json(response) {
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string()),
            Err(_) => text.to_string(),
        }
    } else {
        text.to_string()
    };
    if response.body.len() > MAX_BODY_BYTES {
        let mut truncated = text.chars().take(MAX_BODY_BYTES).collect::<String>();
        truncated.push_str("\n… (body truncated)");
        truncated
    } else {
        text
    }
    .trim_end()
    .to_string()
    .replace('\r', "")
}

fn display_body_markdown(response: &ResponseData) -> (String, &'static str) {
    let body = display_body(response, true);
    let language = if is_json(response) {
        "json"
    } else if response
        .header("content-type")
        .is_some_and(|v| v.contains("html"))
    {
        "html"
    } else if response
        .header("content-type")
        .is_some_and(|v| v.contains("xml"))
    {
        "xml"
    } else {
        ""
    };
    (body, language)
}

fn is_json(response: &ResponseData) -> bool {
    response
        .header("content-type")
        .is_some_and(|value| value.contains("json"))
}
