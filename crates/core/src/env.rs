//! Environment file loading for HTTP request clients.
//!
//! Environments live in `http-client.env.json` (public) and
//! `http-client.private.env.json` (private), discovered from the request
//! file's directory up to the project root. Private values override public
//! ones; files closer to the request file override files further up.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const PUBLIC_ENV_FILE: &str = "http-client.env.json";
pub const PRIVATE_ENV_FILE: &str = "http-client.private.env.json";

/// A named environment with its merged variable set.
#[derive(Debug, Clone, PartialEq)]
pub struct Environment {
    pub name: String,
    pub vars: BTreeMap<String, Value>,
}

/// Loads all environments visible to `file_dir`, walking directories from
/// `root` down to `file_dir` (workspace-relative, `/`-separated, "" is root).
pub fn load_environments(
    reader: &dyn Fn(&str) -> Option<String>,
    file_dir: &str,
    root: &str,
) -> (Vec<Environment>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut public: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut private: BTreeMap<String, Map<String, Value>> = BTreeMap::new();

    for dir in directory_chain(root, file_dir) {
        merge_env_file(reader, &dir, PUBLIC_ENV_FILE, &mut public, &mut warnings);
        merge_env_file(reader, &dir, PRIVATE_ENV_FILE, &mut private, &mut warnings);
    }

    let mut names: Vec<String> = public.keys().cloned().collect();
    for name in private.keys() {
        if !names.iter().any(|n| n == name) {
            names.push(name.clone());
        }
    }
    names.sort();

    let environments = names
        .into_iter()
        .map(|name| {
            let mut vars = BTreeMap::new();
            if let Some(map) = public.get(&name) {
                vars.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            if let Some(map) = private.get(&name) {
                vars.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            Environment { name, vars }
        })
        .collect();
    (environments, warnings)
}

fn merge_env_file(
    reader: &dyn Fn(&str) -> Option<String>,
    dir: &str,
    file_name: &str,
    target: &mut BTreeMap<String, Map<String, Value>>,
    warnings: &mut Vec<String>,
) {
    let path = join_path(dir, file_name);
    let Some(text) = reader(&path) else {
        return;
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(environments)) => {
            for (env_name, vars) in environments {
                match vars {
                    Value::Object(map) => {
                        target.entry(env_name).or_default().extend(map);
                    }
                    _ => warnings.push(format!(
                        "{path}: environment `{env_name}` must be a JSON object"
                    )),
                }
            }
        }
        _ => warnings.push(format!("{path}: not a valid environment file")),
    }
}

fn join_path(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        file.to_string()
    } else {
        format!("{dir}/{file}")
    }
}

/// Directories from `root` down to `file_dir` (inclusive), far to near.
fn directory_chain(root: &str, file_dir: &str) -> Vec<String> {
    let mut dirs = vec![root.to_string()];
    if file_dir.is_empty() || file_dir == root {
        return dirs;
    }
    let rel = if root.is_empty() {
        file_dir.to_string()
    } else {
        file_dir
            .strip_prefix(&format!("{root}/"))
            .unwrap_or(file_dir)
            .to_string()
    };
    let mut current = root.to_string();
    for part in rel.split('/') {
        if part.is_empty() {
            continue;
        }
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(part);
        dirs.push(current.clone());
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader<'a>(files: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |path| {
            files
                .iter()
                .find(|(name, _)| *name == path)
                .map(|(_, text)| text.to_string())
        }
    }

    #[test]
    fn merges_public_and_private_with_private_winning() {
        let files = [
            (
                "http-client.env.json",
                r#"{"dev": {"host": "example.com", "token": "public"}, "prod": {"host": "prod.example.com"}}"#,
            ),
            (
                "http-client.private.env.json",
                r#"{"dev": {"token": "secret"}}"#,
            ),
        ];
        let (envs, warnings) = load_environments(&reader(&files), "", "");
        assert!(warnings.is_empty());
        assert_eq!(envs.len(), 2);
        let dev = envs.iter().find(|e| e.name == "dev").unwrap();
        assert_eq!(
            dev.vars.get("host").and_then(Value::as_str),
            Some("example.com")
        );
        assert_eq!(
            dev.vars.get("token").and_then(Value::as_str),
            Some("secret")
        );
    }

    #[test]
    fn nearer_files_override_farther_ones() {
        let files = [
            (
                "http-client.env.json",
                r#"{"dev": {"host": "root", "port": 80}}"#,
            ),
            ("api/http-client.env.json", r#"{"dev": {"host": "near"}}"#),
        ];
        let (envs, _) = load_environments(&reader(&files), "api", "");
        let dev = envs.iter().find(|e| e.name == "dev").unwrap();
        assert_eq!(dev.vars.get("host").and_then(Value::as_str), Some("near"));
        assert_eq!(dev.vars.get("port").and_then(Value::as_i64), Some(80));
    }

    #[test]
    fn warns_about_invalid_files() {
        let files = [("http-client.env.json", "not json")];
        let (_, warnings) = load_environments(&reader(&files), "", "");
        assert_eq!(warnings.len(), 1);
    }
}
