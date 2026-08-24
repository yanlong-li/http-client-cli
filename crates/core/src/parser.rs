//! Line-based parser for `.http` request files.

/// A parsed `.http` document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpFile {
    /// All requests, in file order.
    pub requests: Vec<Request>,
    /// File-scoped in-place variables (`@name = value`), hoisted so every
    /// request in the file can reference them.
    pub variables: Vec<(String, String)>,
}

/// A single HTTP request block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Zero-based position of the request within the file.
    pub index: usize,
    /// Explicit name (`### name`, `# @name name` or `# @name = name`), if any.
    pub name: Option<String>,
    /// Explicit name or `#N` fallback (1-based position).
    pub display_name: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<Header>,
    pub body: Option<String>,
    /// Zero-based line index of the `METHOD URL` line.
    pub request_line: usize,
    /// Zero-based line index of the first line of the request's section
    /// (the `###` separator or the first leading comment).
    pub start_line: usize,
    /// Zero-based line index of the last line of the request (inclusive).
    pub end_line: usize,
    /// Whether the `@no-redirect` tag was present.
    pub no_redirect: bool,
    /// Timeout in seconds from the `@timeout` tag.
    pub timeout_secs: Option<u64>,
    /// Connection timeout in seconds from the `@connection-timeout` tag.
    pub connection_timeout_secs: Option<u64>,
    /// Response handler (`> {% ... %}` or `> script.js`).
    pub handler: Option<Handler>,
    /// Response redirect (`>>` or `>>! path`).
    pub redirect_to: Option<Redirect>,
    /// Pre-request script (`< {% ... %}`). Parsed but not executed.
    pub pre_request_script: Option<Handler>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handler {
    Inline(String),
    File(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    /// `>>!` overwrites; `>>` creates a new file with a numeric suffix.
    pub overwrite: bool,
    pub path: String,
}

#[derive(Debug, Default, Clone)]
struct SectionState {
    name: Option<String>,
    start_line: Option<usize>,
    no_redirect: bool,
    timeout_secs: Option<u64>,
    connection_timeout_secs: Option<u64>,
    pre_request_script: Option<Handler>,
}

#[derive(Debug, Clone)]
struct RequestBuilder {
    method: String,
    url: String,
    headers: Vec<Header>,
    request_line: usize,
    start_line: usize,
    handler: Option<Handler>,
    redirect_to: Option<Redirect>,
}

/// Parses the text of a `.http` file.
pub fn parse(input: &str) -> HttpFile {
    let lines: Vec<&str> = input.lines().collect();
    let mut requests: Vec<Request> = Vec::new();
    let mut variables: Vec<(String, String)> = Vec::new();

    let mut section = SectionState::default();
    let mut current: Option<RequestBuilder> = None;
    let mut body_lines: Vec<String> = Vec::new();
    let mut in_body = false;
    let mut section_has_content = false;

    let finalize = |section: &SectionState,
                    current: Option<RequestBuilder>,
                    body_lines: &mut Vec<String>,
                    end_line: usize,
                    requests: &mut Vec<Request>| {
        let Some(builder) = current else { return };
        while body_lines.last().is_some_and(|l| l.trim().is_empty()) {
            body_lines.pop();
        }
        while body_lines.first().is_some_and(|l| l.trim().is_empty()) {
            body_lines.remove(0);
        }
        let body = if body_lines.is_empty() {
            None
        } else {
            Some(body_lines.join("\n"))
        };
        let index = requests.len();
        let name = section.name.clone();
        let display_name = name.clone().unwrap_or_else(|| format!("#{}", index + 1));
        requests.push(Request {
            index,
            name,
            display_name,
            method: builder.method,
            url: builder.url,
            headers: builder.headers,
            body,
            request_line: builder.request_line,
            start_line: builder.start_line,
            end_line,
            no_redirect: section.no_redirect,
            timeout_secs: section.timeout_secs,
            connection_timeout_secs: section.connection_timeout_secs,
            handler: builder.handler,
            redirect_to: builder.redirect_to,
            pre_request_script: section.pre_request_script.clone(),
        });
        body_lines.clear();
    };

    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        let indented = line.starts_with(' ') || line.starts_with('\t');

        // `###` request separator (three or more `#` at line start).
        let hash_len = trimmed.chars().take_while(|c| *c == '#').count();
        if hash_len >= 3 {
            let request_end = if requests.is_empty() && current.is_none() && !section_has_content {
                0
            } else if current.is_some() {
                index.saturating_sub(1)
            } else {
                section.start_line.unwrap_or(index).saturating_sub(1)
            };
            finalize(
                &section,
                current.take(),
                &mut body_lines,
                request_end,
                &mut requests,
            );
            in_body = false;
            section_has_content = true;
            let separator_name = trimmed[hash_len..].trim();
            section = SectionState {
                name: (!separator_name.is_empty()).then(|| separator_name.to_string()),
                start_line: Some(index),
                ..SectionState::default()
            };
            index += 1;
            continue;
        }

        // Blank lines.
        if trimmed.is_empty() {
            if current.is_some() {
                if in_body {
                    body_lines.push(String::new());
                } else {
                    in_body = true;
                }
            }
            index += 1;
            continue;
        }

        // Comment lines (`//` or `#`): may carry a request name or tags.
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            if !section_has_content {
                section.start_line.get_or_insert(index);
            }
            let content = trimmed.trim_start_matches(['/', '#']).trim();
            if let Some(rest) = content.strip_prefix("@name") {
                let rest = rest.trim().trim_start_matches('=').trim();
                if !rest.is_empty() {
                    section.name = Some(rest.to_string());
                }
            } else if content == "@no-redirect" {
                section.no_redirect = true;
            } else if let Some(rest) = content.strip_prefix("@timeout") {
                section.timeout_secs = parse_duration(rest.trim());
            } else if let Some(rest) = content.strip_prefix("@connection-timeout") {
                section.connection_timeout_secs = parse_duration(rest.trim());
            }
            section_has_content = true;
            index += 1;
            continue;
        }

        // In-place variable declaration (`@name = value`), only before the
        // request line.
        if current.is_none() {
            if let Some((name, value)) = parse_variable_declaration(trimmed) {
                if !section_has_content {
                    section.start_line.get_or_insert(index);
                }
                variables.push((name, value));
                section_has_content = true;
                index += 1;
                continue;
            }

            // Pre-request script (`< {% ... %}` or `< script.js`).
            if let Some(stripped) = trimmed.strip_prefix('<') {
                let rest = stripped.trim();
                if rest.starts_with("{%") {
                    let (handler, consumed) = collect_inline_script(&lines, index, rest);
                    section.pre_request_script = Some(handler);
                    section_has_content = true;
                    index += consumed;
                    continue;
                }
                if !rest.is_empty() {
                    section.pre_request_script = Some(Handler::File(rest.to_string()));
                    section_has_content = true;
                    index += 1;
                    continue;
                }
            }

            // Request line (optionally indented continuation handled below).
            if indented {
                section_has_content = true;
                index += 1;
                continue;
            }
            let (method, url) = parse_request_line(trimmed);
            if !section_has_content {
                section.start_line.get_or_insert(index);
            }
            section_has_content = true;
            current = Some(RequestBuilder {
                method,
                url,
                headers: Vec::new(),
                request_line: index,
                start_line: section.start_line.unwrap_or(index),
                handler: None,
                redirect_to: None,
            });
            index += 1;

            // URL continuation lines (indented, non-blank) directly after the
            // request line.
            while index < lines.len() {
                let continuation = lines[index];
                let cont_trimmed = continuation.trim();
                if cont_trimmed.is_empty()
                    || !(continuation.starts_with(' ') || continuation.starts_with('\t'))
                {
                    break;
                }
                current.as_mut().unwrap().url.push_str(cont_trimmed);
                index += 1;
            }
            continue;
        }

        // Response redirect (`>>! path` or `>> path`).
        if let Some(redirect) = parse_redirect(trimmed) {
            current.as_mut().unwrap().redirect_to = Some(redirect);
            index += 1;
            continue;
        }

        // Response handler (`> {% ... %}` or `> script.js`).
        if trimmed.starts_with('>') && !trimmed.starts_with(">>") {
            let rest = trimmed[1..].trim();
            if rest.starts_with("{%") {
                let (handler, consumed) = collect_inline_script(&lines, index, rest);
                current.as_mut().unwrap().handler = Some(handler);
                index += consumed;
                continue;
            }
            if !rest.is_empty() {
                current.as_mut().unwrap().handler = Some(Handler::File(rest.to_string()));
                index += 1;
                continue;
            }
        }

        // Headers until the first blank line, then the body.
        if !in_body {
            if let Some(header) = parse_header(trimmed) {
                current.as_mut().unwrap().headers.push(header);
                index += 1;
                continue;
            }
            in_body = true;
        }
        body_lines.push(line.to_string());
        index += 1;
    }

    let end_line = lines.len().saturating_sub(1);
    finalize(
        &section,
        current.take(),
        &mut body_lines,
        end_line,
        &mut requests,
    );

    HttpFile {
        requests,
        variables,
    }
}

/// Parses `METHOD URL [HTTP-version]` or the short `URL` GET form.
fn parse_request_line(line: &str) -> (String, String) {
    if let Some((first, rest)) = line.split_once(char::is_whitespace) {
        let rest = rest.trim();
        if first.len() >= 2 && first.chars().all(|c| c.is_ascii_uppercase()) && !rest.is_empty() {
            let url = strip_http_version(rest);
            return (first.to_string(), url.to_string());
        }
    }
    let url = strip_http_version(line);
    ("GET".to_string(), url.to_string())
}

/// Strips a trailing `HTTP/1.1`, `HTTP/2` or `HTTP/2 (Prior Knowledge)` token.
fn strip_http_version(line: &str) -> &str {
    let mut line = line;
    if let Some(base) = line.strip_suffix(" (Prior Knowledge)") {
        line = base;
    }
    if let Some(pos) = line.rfind(" HTTP/") {
        if is_http_version_token(&line[pos + 1..]) {
            return line[..pos].trim_end();
        }
    }
    line
}

fn is_http_version_token(token: &str) -> bool {
    let Some(rest) = token.strip_prefix("HTTP/") else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Parses a `Name: value` header line.
fn parse_header(line: &str) -> Option<Header> {
    let (name, value) = line.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || name.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some(Header {
        name: name.to_string(),
        value: value.trim().to_string(),
    })
}

/// Parses `@name = value`.
fn parse_variable_declaration(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix('@')?;
    let (name, value) = rest.split_once('=')?;
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return None;
    }
    Some((name.to_string(), value.trim().to_string()))
}

/// Parses `>>` or `>>!` response redirects.
fn parse_redirect(line: &str) -> Option<Redirect> {
    let rest = line.strip_prefix(">>")?;
    let (overwrite, rest) = match rest.strip_prefix('!') {
        Some(r) => (true, r),
        None => (false, rest),
    };
    let path = rest.trim();
    if path.is_empty() {
        return None;
    }
    Some(Redirect {
        overwrite,
        path: path.to_string(),
    })
}

/// Parses `# @timeout 600`, `# @timeout 600 ms`, `// @timeout 2 m` etc.
/// Bare numbers are seconds.
fn parse_duration(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let value: f64 = number.parse().ok()?;
    let millis = match unit.trim() {
        "ms" => value,
        "s" | "" => value * 1000.0,
        "m" => value * 60.0 * 1000.0,
        _ => return None,
    };
    Some((millis / 1000.0).ceil() as u64)
}

/// Collects an inline `{% ... %}` script, returning the script body and how
/// many lines were consumed.
fn collect_inline_script(lines: &[&str], start: usize, first: &str) -> (Handler, usize) {
    let mut script = String::new();
    let push = |script: &mut String, text: &str| {
        if !text.trim().is_empty() {
            if !script.is_empty() {
                script.push('\n');
            }
            script.push_str(text.trim());
        }
    };

    let inner = first
        .strip_prefix("{%")
        .map(|rest| rest.strip_suffix("%}").unwrap_or(rest))
        .unwrap_or(first);
    push(&mut script, inner);

    if first.trim_end().ends_with("%}") {
        return (Handler::Inline(script), 1);
    }

    let mut index = start + 1;
    while index < lines.len() {
        let line = lines[index];
        if let Some(before) = line.strip_suffix("%}") {
            push(&mut script, before);
            return (Handler::Inline(script), index - start + 1);
        }
        if !script.is_empty() {
            script.push('\n');
        }
        script.push_str(line);
        index += 1;
    }
    (Handler::Inline(script), lines.len() - start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_request() {
        let doc = parse("GET https://example.com/api HTTP/1.1\nAccept: application/json\n");
        assert_eq!(doc.requests.len(), 1);
        let req = &doc.requests[0];
        assert_eq!(req.method, "GET");
        assert_eq!(req.url, "https://example.com/api");
        assert_eq!(req.headers.len(), 1);
        assert_eq!(req.headers[0].name, "Accept");
        assert_eq!(req.body, None);
        assert_eq!(req.display_name, "#1");
    }

    #[test]
    fn parses_multiple_requests_with_names_and_bodies() {
        let input = "\
### login
POST {{host}}/login HTTP/1.1
Content-Type: application/json

{
  \"user\": \"admin\"
}

### fetch things
GET {{host}}/things

###
DELETE {{host}}/things/1
";
        let doc = parse(input);
        assert_eq!(doc.requests.len(), 3);
        assert_eq!(doc.requests[0].name.as_deref(), Some("login"));
        assert_eq!(doc.requests[0].method, "POST");
        assert_eq!(
            doc.requests[0].body.as_deref(),
            Some("{\n  \"user\": \"admin\"\n}")
        );
        assert_eq!(doc.requests[1].name.as_deref(), Some("fetch things"));
        assert_eq!(doc.requests[1].display_name, "fetch things");
        assert_eq!(doc.requests[2].name, None);
        assert_eq!(doc.requests[2].display_name, "#3");
    }

    #[test]
    fn parses_short_get_form_and_url_continuation() {
        let input = "\
https://example.com/api
    /things
    ?id=1
    &flag=true
";
        let doc = parse(input);
        assert_eq!(doc.requests.len(), 1);
        assert_eq!(doc.requests[0].method, "GET");
        assert_eq!(
            doc.requests[0].url,
            "https://example.com/api/things?id=1&flag=true"
        );
    }

    #[test]
    fn parses_variables_tags_and_handlers() {
        let input = "\
@host = https://example.com
@token = abc

### named via comment
// @no-redirect
# @timeout 30
GET {{host}}/status/301

> {%
client.global.set(\"token\", response.body.json().token)
%}
>>! out/response.json
";
        let doc = parse(input);
        assert_eq!(
            doc.variables,
            vec![
                ("host".to_string(), "https://example.com".to_string()),
                ("token".to_string(), "abc".to_string())
            ]
        );
        let req = &doc.requests[0];
        assert_eq!(req.name.as_deref(), Some("named via comment"));
        assert!(req.no_redirect);
        assert_eq!(req.timeout_secs, Some(30));
        assert!(matches!(
            req.handler,
            Some(Handler::Inline(ref script)) if script.contains("client.global.set")
        ));
        assert_eq!(
            req.redirect_to,
            Some(Redirect {
                overwrite: true,
                path: "out/response.json".to_string()
            })
        );
    }

    #[test]
    fn parses_duration_units() {
        assert_eq!(parse_duration("600"), Some(600));
        assert_eq!(parse_duration("600ms"), Some(1));
        assert_eq!(parse_duration("100 ms"), Some(1));
        assert_eq!(parse_duration("2 m"), Some(120));
    }

    #[test]
    fn strips_http_versions() {
        assert_eq!(
            strip_http_version("https://example.com HTTP/1.1"),
            "https://example.com"
        );
        assert_eq!(
            strip_http_version("https://example.com HTTP/2"),
            "https://example.com"
        );
        assert_eq!(
            strip_http_version("https://example.com HTTP/2 (Prior Knowledge)"),
            "https://example.com"
        );
    }
}
