//! `http-client` — run `.http` requests from the command line.
//!
//! This binary is the execution engine behind editor runnables and CI.

use http_client_core::env::load_environments;
use http_client_core::execute::{
    process_response, resolve_request, ResolvedRequest, ResponseData, RunState,
};
use http_client_core::output::format_response_plain;
use http_client_core::parser::parse;
use http_client_core::rng::Rng;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "\
http-client — run .http requests

USAGE:
  http-client run <file> [options]   Run one request (or all with --all)
  http-client list <file>            List requests in a file
  http-client env <file> [name]      List environments, or persist a selection

OPTIONS (run):
  --name <name>    Run the request with this name (`#N` also works)
  --index <n>      Run the request at this zero-based index
  --line <n>       Run the request containing this one-based line
  --all            Run every request in the file, in order
  --env <name>     Environment to use (see http-client.env.json)
  --no-env         Ignore the persisted environment selection
  --root <path>    Project root used for env discovery (default: git root)
  --curl <path>    Path to the curl binary (default: `curl` from PATH)
  --fail-on-http-error    Exit non-zero when a response status is >= 400

Environment selection order: --env > $HTTP_CLIENT_ENV > .http-client-env file
at the project root. If none is set, the request runs without environment
variables. Variables resolve as: environment > global > file > per-request
(compatible precedence).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("list") => list(&args[1..]),
        Some("env") => env_command(&args[1..]),
        Some("--help") | Some("-h") | Some("help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

struct RunOptions {
    file: PathBuf,
    name: Option<String>,
    index: Option<usize>,
    line: Option<usize>,
    all: bool,
    env_name: Option<String>,
    no_env: bool,
    root: Option<PathBuf>,
    curl: String,
    fail_on_http_error: bool,
}

fn run(args: &[String]) -> ExitCode {
    let options = match parse_run_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let content = match std::fs::read_to_string(&options.file) {
        Ok(content) => content,
        Err(error) => {
            eprintln!("error: cannot read {}: {error}", options.file.display());
            return ExitCode::FAILURE;
        }
    };
    let doc = parse(&content);
    if doc.requests.is_empty() {
        eprintln!("error: no requests found in {}", options.file.display());
        return ExitCode::FAILURE;
    }

    let root = match project_root(&options.file, options.root.as_deref()) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let file_dir_rel = relative_dir(&root, &options.file);
    let reader = |rel: &str| read_workspace_file(&root, rel);
    let (environments, warnings) = load_environments(&reader, &file_dir_rel, "");
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }

    let env_name = match select_environment(&options, &root, &environments) {
        Ok(name) => name,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let env_vars = env_name
        .as_ref()
        .and_then(|name| environments.iter().find(|e| &e.name == name))
        .map(|env| env.vars.clone())
        .unwrap_or_default();

    let selected: Vec<usize> = match select_requests(&doc.requests, &options) {
        Ok(selected) => selected,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("available requests:");
            for request in &doc.requests {
                eprintln!(
                    "  #{}  {}  {} {}",
                    request.index, request.display_name, request.method, request.url
                );
            }
            return ExitCode::FAILURE;
        }
    };

    let file_vars: BTreeMap<String, Value> = doc
        .variables
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect();
    let mut state = RunState::default();
    let mut rng = Rng::new();
    let system_env = |name: &str| std::env::var(name).ok();
    let mut failed = false;

    for index in selected {
        let request = &doc.requests[index];
        let resolved = match resolve_request(
            request,
            &file_vars,
            &env_vars,
            &state.globals,
            &system_env,
            &mut rng,
        ) {
            Ok(resolved) => resolved,
            Err(error) => {
                if error.starts_with("unresolved variable")
                    && env_name.is_none()
                    && !environments.is_empty()
                {
                    eprintln!(
                        "error resolving `{}`: {error}\n  no environment selected; use `http-client env <file> <name>`",
                        request.display_name
                    );
                } else {
                    eprintln!("error resolving `{}`: {error}", request.display_name);
                }
                failed = true;
                break;
            }
        };
        match execute_with_curl(&options.curl, &resolved) {
            Ok(response) => {
                process_response(&resolved, &response, &mut state);
                if let Err(error) = write_redirect(&resolved, &response, &options.file) {
                    eprintln!("warning: could not redirect response: {error}");
                }
                let saved_path = match save_response_file(&resolved, &response, &root) {
                    Ok(path) => Some(path),
                    Err(error) => {
                        eprintln!("warning: could not save response: {error}");
                        None
                    }
                };
                let mut formatted = format_response_plain(&resolved, &response, &state.logs);
                if let Some(path) = saved_path {
                    formatted.push_str(&format!("\nResponse saved to {}\n", path.display()));
                }
                print!("{formatted}");
                for error in &state.handler_errors {
                    eprintln!("handler error: {error}");
                }
                state.logs.clear();
                state.handler_errors.clear();
                if options.fail_on_http_error && response.status >= 400 {
                    failed = true;
                }
            }
            Err(error) => {
                eprintln!("error sending `{}`: {error}", resolved.name);
                failed = true;
                break;
            }
        }
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn list(args: &[String]) -> ExitCode {
    let Some(file) = args.first() else {
        eprintln!("error: missing file argument\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let content = match std::fs::read_to_string(file) {
        Ok(content) => content,
        Err(error) => {
            eprintln!("error: cannot read {file}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let doc = parse(&content);
    for request in &doc.requests {
        println!(
            "#{:<3} {:<24} {:<8} {}",
            request.index, request.display_name, request.method, request.url
        );
    }
    ExitCode::SUCCESS
}

fn env_command(args: &[String]) -> ExitCode {
    let Some(file) = args.first() else {
        eprintln!("error: missing file argument\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let file = PathBuf::from(file);
    let Ok(root) = project_root(&file, None) else {
        eprintln!("error: cannot determine project root");
        return ExitCode::FAILURE;
    };
    let file_dir_rel = relative_dir(&root, &file);
    let reader = |rel: &str| read_workspace_file(&root, rel);
    let (environments, warnings) = load_environments(&reader, &file_dir_rel, "");
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }

    let interactive = args.iter().skip(1).any(|arg| arg == "--select");
    let requested_name = args
        .iter()
        .skip(1)
        .find(|arg| arg.as_str() != "--select")
        .cloned()
        .or_else(|| {
            if !interactive {
                return None;
            }
            if environments.is_empty() {
                return Some(String::new());
            }
            println!("Available environments:");
            for (index, environment) in environments.iter().enumerate() {
                println!("  {}. {}", index + 1, environment.name);
            }
            print!("Select environment (number or name): ");
            if io::stdout().flush().is_err() {
                return Some(String::new());
            }
            let mut answer = String::new();
            if io::stdin().read_line(&mut answer).is_err() {
                return Some(String::new());
            }
            let answer = answer.trim();
            if let Ok(index) = answer.parse::<usize>() {
                return environments
                    .get(index.saturating_sub(1))
                    .map(|environment| environment.name.clone())
                    .or_else(|| Some(answer.to_string()));
            }
            Some(answer.to_string())
        });

    match requested_name.as_deref() {
        None => {
            if environments.is_empty() {
                println!(
                    "no environments found (looked for http-client.env.json files up to {})",
                    root.display()
                );
            }
            for env in &environments {
                let keys: Vec<&str> = env.vars.keys().map(String::as_str).collect();
                println!("{}: {}", env.name, keys.join(", "));
            }
            ExitCode::SUCCESS
        }
        Some(name) if !name.is_empty() => {
            if !environments.iter().any(|env| env.name == name) {
                eprintln!("error: environment `{name}` not found");
                return ExitCode::FAILURE;
            }
            let marker = root.join(".http-client-env");
            if let Err(error) = std::fs::write(&marker, name) {
                eprintln!("error: cannot write {}: {error}", marker.display());
                return ExitCode::FAILURE;
            }
            println!("selected environment `{name}` ({})", marker.display());
            ExitCode::SUCCESS
        }
        Some(_) => ExitCode::from(2),
    }
}

fn parse_run_args(args: &[String]) -> Result<RunOptions, String> {
    let mut file: Option<PathBuf> = None;
    let mut name = None;
    let mut index = None;
    let mut line = None;
    let mut all = false;
    let mut env_name = None;
    let mut no_env = false;
    let mut root: Option<PathBuf> = None;
    let mut curl = std::env::var("CURL").unwrap_or_else(|_| "curl".to_string());
    let mut fail_on_http_error = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let value = |name: &str| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--name" => {
                name = Some(value("--name")?);
                i += 1;
            }
            "--index" => {
                index = Some(
                    value("--index")?
                        .parse()
                        .map_err(|_| "--index expects a number".to_string())?,
                );
                i += 1;
            }
            "--line" => {
                let parsed = value("--line")?
                    .parse::<usize>()
                    .map_err(|_| "--line expects a number".to_string())?;
                if parsed == 0 {
                    return Err("--line expects a one-based line number".to_string());
                }
                line = Some(parsed);
                i += 1;
            }
            "--env" => {
                env_name = Some(value("--env")?);
                i += 1;
            }
            "--root" => {
                root = Some(PathBuf::from(value("--root")?));
                i += 1;
            }
            "--curl" => {
                curl = value("--curl")?;
                i += 1;
            }
            "--all" => all = true,
            "--no-env" => no_env = true,
            "--fail-on-http-error" => fail_on_http_error = true,
            other if other.starts_with('-') => return Err(format!("unknown option: {other}")),
            other if file.is_none() => file = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument: {other}")),
        }
        i += 1;
    }
    let file = file.ok_or("missing file argument")?;
    Ok(RunOptions {
        file,
        name,
        index,
        line,
        all,
        env_name,
        no_env,
        root,
        curl,
        fail_on_http_error,
    })
}

fn select_requests(
    requests: &[http_client_core::parser::Request],
    options: &RunOptions,
) -> Result<Vec<usize>, String> {
    if options.all {
        return Ok((0..requests.len()).collect());
    }
    if let Some(name) = &options.name {
        if let Some(request) = requests
            .iter()
            .find(|request| &request.display_name == name)
        {
            return Ok(vec![request.index]);
        }
        if let Some(index) = name
            .strip_prefix('#')
            .and_then(|index| index.parse::<usize>().ok())
            .filter(|index| *index > 0)
            .map(|index| index - 1)
        {
            if let Some(request) = requests.get(index) {
                return Ok(vec![request.index]);
            }
        }
        return Err(format!("no request named `{name}`"));
    }
    if let Some(index) = options.index {
        return requests
            .get(index)
            .map(|request| vec![request.index])
            .ok_or_else(|| format!("no request at index {index}"));
    }
    if let Some(line) = options.line {
        let line = line.checked_sub(1).ok_or("line numbers start at 1")?;
        if let Some(request) = requests
            .iter()
            .find(|request| request.request_line == line)
            .or_else(|| {
                requests
                    .iter()
                    .find(|request| line >= request.start_line && line <= request.end_line)
            })
        {
            return Ok(vec![request.index]);
        }
        return Err(format!("no request at line {line}"));
    }
    if requests.len() == 1 {
        return Ok(vec![0]);
    }
    Err("specify --name, --index, --line or --all".to_string())
}

fn project_root(file: &Path, override_root: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(root) = override_root {
        let root = root
            .canonicalize()
            .map_err(|error| format!("invalid --root: {error}"))?;
        return Ok(root);
    }
    let mut dir = file
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "file has no parent directory".to_string())?;
    loop {
        if dir.join(".git").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Ok(file.parent().unwrap_or(Path::new(".")).to_path_buf());
        }
    }
}

fn relative_dir(root: &Path, file: &Path) -> String {
    let parent = file.parent().unwrap_or(root);
    match parent.strip_prefix(root) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => String::new(),
    }
}

fn read_workspace_file(root: &Path, rel: &str) -> Option<String> {
    let path = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    std::fs::read_to_string(path).ok()
}

fn select_environment(
    options: &RunOptions,
    root: &Path,
    environments: &[http_client_core::env::Environment],
) -> Result<Option<String>, String> {
    if options.no_env {
        return Ok(None);
    }
    let name = if let Some(name) = &options.env_name {
        Some(name.clone())
    } else if let Ok(name) = std::env::var("HTTP_CLIENT_ENV") {
        Some(name)
    } else if let Ok(name) = std::fs::read_to_string(root.join(".http-client-env")) {
        let name = name.trim().to_string();
        (!name.is_empty()).then_some(name)
    } else {
        None
    };
    if let Some(name) = &name {
        if !environments.iter().any(|env| &env.name == name) {
            return Err(format!(
                "environment `{name}` not found (available: {})",
                environments
                    .iter()
                    .map(|env| env.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(name)
}

fn execute_with_curl(curl: &str, request: &ResolvedRequest) -> Result<ResponseData, String> {
    let temp = temp_dir();
    let token = format!(
        "http-client-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let header_file = temp.join(format!("{token}.headers"));
    let body_file = temp.join(format!("{token}.body"));
    let request_body_file = temp.join(format!("{token}.request-body"));

    let mut args: Vec<String> = vec!["-sS".into(), "-X".into(), request.method.clone()];
    for (name, value) in &request.headers {
        args.push("-H".into());
        args.push(format!("{name}: {value}"));
    }
    if let Some(body) = &request.body {
        std::fs::write(&request_body_file, body)
            .map_err(|error| format!("cannot write request body: {error}"))?;
        args.push("--data-binary".into());
        args.push(format!("@{}", request_body_file.display()));
    }
    if request.follow_redirects {
        args.push("-L".into());
    }
    args.push("--compressed".into());
    if let Some(timeout) = request.timeout_secs {
        args.push("--max-time".into());
        args.push(timeout.to_string());
    }
    if let Some(timeout) = request.connection_timeout_secs {
        args.push("--connect-timeout".into());
        args.push(timeout.to_string());
    }
    args.push("-D".into());
    args.push(header_file.display().to_string());
    args.push("-o".into());
    args.push(body_file.display().to_string());
    args.push("-w".into());
    args.push("%{http_code} %{time_total}".into());
    args.push(request.url.clone());

    let output = Command::new(curl)
        .args(&args)
        .output()
        .map_err(|error| format!("cannot start curl (`{curl}`): {error}"))?;
    let _ = std::fs::remove_file(&request_body_file);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let _ = std::fs::remove_file(&header_file);
        let _ = std::fs::remove_file(&body_file);
        return Err(if stderr.is_empty() { stdout } else { stderr });
    }

    let write_out = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let mut parts = write_out.split_whitespace();
    let status: u16 = parts.next().and_then(|code| code.parse().ok()).unwrap_or(0);
    let elapsed_ms = parts
        .next()
        .and_then(|time| time.parse::<f64>().ok())
        .map(|time| (time * 1000.0).round() as u64)
        .unwrap_or(0);
    let trace = parse_header_file(&header_file, &request.url);
    let body = std::fs::read(&body_file).unwrap_or_default();
    let _ = std::fs::remove_file(&header_file);
    let _ = std::fs::remove_file(&body_file);

    if status == 0 {
        return Err("curl failed (no HTTP status received)".to_string());
    }
    Ok(ResponseData {
        status,
        protocol: trace.protocol,
        status_text: trace.status_text,
        headers: trace.headers,
        redirects: trace.redirects,
        body,
        elapsed_ms,
    })
}

struct HeaderTrace {
    protocol: String,
    status_text: String,
    headers: Vec<(String, String)>,
    redirects: Vec<String>,
}

fn parse_header_file(path: &Path, request_url: &str) -> HeaderTrace {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HeaderTrace {
            protocol: "HTTP".to_string(),
            status_text: String::new(),
            headers: Vec::new(),
            redirects: Vec::new(),
        };
    };
    let blocks: Vec<&str> = text
        .split("\r\n\r\n")
        .filter(|block| !block.trim().is_empty())
        .collect();
    let mut redirects = Vec::new();
    for block in blocks.iter().take(blocks.len().saturating_sub(1)) {
        if let Some(location) = block.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("location").then(|| value.trim())
        }) {
            redirects.push(resolve_url(request_url, location));
        }
    }
    let last_block = blocks.last().copied().unwrap_or(&text);
    let mut lines = last_block.lines();
    let (protocol, status_text) = lines
        .next()
        .map(parse_status_line)
        .unwrap_or_else(|| ("HTTP".to_string(), String::new()));
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();
    HeaderTrace {
        protocol,
        status_text,
        headers,
        redirects,
    }
}

fn parse_status_line(line: &str) -> (String, String) {
    let mut parts = line.splitn(3, ' ');
    let protocol = parts.next().unwrap_or("HTTP").to_string();
    let _status = parts.next();
    let text = parts.next().unwrap_or("").trim().to_string();
    (protocol, text)
}

fn resolve_url(base: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }
    let Some(scheme_end) = base.find("://") else {
        return location.to_string();
    };
    let authority_start = scheme_end + 3;
    let authority_end = base[authority_start..]
        .find('/')
        .map(|index| authority_start + index)
        .unwrap_or(base.len());
    let origin = &base[..authority_end];
    if location.starts_with('/') {
        format!("{origin}{location}")
    } else {
        let directory = base
            .rfind('/')
            .map(|index| &base[..index + 1])
            .unwrap_or(base);
        format!("{directory}{location}")
    }
}

fn write_redirect(
    request: &ResolvedRequest,
    response: &ResponseData,
    source: &Path,
) -> Result<(), String> {
    let Some(redirect) = &request.redirect_to else {
        return Ok(());
    };
    let base_dir = source.parent().ok_or("source file has no directory")?;
    let mut path = base_dir.join(&redirect.path);
    if !redirect.overwrite {
        let mut counter = 0;
        while path.exists() {
            counter += 1;
            let file_name = format!(
                "{}-{counter}.{}",
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("response"),
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("txt")
            );
            path = path.with_file_name(file_name);
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create directory: {error}"))?;
    }
    std::fs::write(&path, &response.body)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn save_response_file(
    request: &ResolvedRequest,
    response: &ResponseData,
    root: &Path,
) -> Result<PathBuf, String> {
    let directory = root.join(".http-client").join("httpRequests");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let file_name = format!("{timestamp}.json");
    let path = directory.join(file_name);
    let value = serde_json::json!({
        "request": {
            "method": request.method,
            "url": request.url,
            "headers": request.headers,
        },
        "response": {
            "protocol": response.protocol,
            "status": response.status,
            "statusText": response.status_text,
            "headers": response.headers,
            "redirects": response.redirects,
            "body": String::from_utf8_lossy(&response.body),
            "elapsedMs": response.elapsed_ms,
        }
    });
    let text = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    std::fs::write(&path, text)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(path)
}

fn temp_dir() -> PathBuf {
    std::env::temp_dir()
}
