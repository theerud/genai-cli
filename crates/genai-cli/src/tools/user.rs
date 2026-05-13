use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::gemini::types::FunctionDeclaration;

use super::LocalTool;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// On-disk schema for a user-defined tool. One file per tool under
/// `<config_dir>/tools/<name>.toml`. The filename stem is the tool name.
#[derive(Debug, Deserialize)]
struct UserToolSpec {
    description: String,
    command: Vec<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    confirmation: bool,
    #[serde(default)]
    args: HashMap<String, ArgSpec>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArgSpec {
    #[serde(rename = "type")]
    ty: ArgType,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    default: Option<Value>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ArgType {
    String,
    Integer,
    Number,
    Boolean,
}

impl ArgType {
    fn as_json_str(self) -> &'static str {
        match self {
            ArgType::String => "string",
            ArgType::Integer => "integer",
            ArgType::Number => "number",
            ArgType::Boolean => "boolean",
        }
    }
}

pub struct UserTool {
    name: String,
    description: String,
    args: Vec<(String, ArgSpec)>,
    command: Vec<String>,
    timeout_secs: u64,
    confirmation: bool,
    bin_dir: PathBuf,
}

impl LocalTool for UserTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn requires_confirmation(&self) -> bool {
        self.confirmation
    }

    fn declaration(&self) -> FunctionDeclaration {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for (name, spec) in &self.args {
            let mut p = json!({"type": spec.ty.as_json_str()});
            if let Some(d) = &spec.description {
                p["description"] = json!(d);
            }
            props.insert(name.clone(), p);
            if spec.required {
                required.push(name.clone());
            }
        }
        FunctionDeclaration {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: json!({
                "type": "object",
                "properties": props,
                "required": required,
            }),
        }
    }

    fn describe_call(&self, args: &Value) -> String {
        // Render the command as it would be invoked, with substitutions.
        let coerced = match coerce_args(&self.args, args) {
            Ok(v) => v,
            Err(_) => return format!("{}(?)", self.name),
        };
        let argv = substitute(&self.command, &coerced);
        shell_join(&argv)
    }

    fn run(&self, args: &Value) -> Result<Value> {
        let coerced = coerce_args(&self.args, args)
            .with_context(|| format!("validating args for tool '{}'", self.name))?;
        let argv = substitute(&self.command, &coerced);
        let (program, rest) = argv
            .split_first()
            .ok_or_else(|| anyhow!("tool '{}' has empty command", self.name))?;

        let mut cmd = std::process::Command::new(program);
        cmd.args(rest);
        prepend_path(&mut cmd, &self.bin_dir);
        let captured = super::process::run_with_caps(
            &mut cmd,
            std::time::Duration::from_secs(self.timeout_secs),
            MAX_OUTPUT_BYTES,
        )
        .with_context(|| format!("spawning '{}' for tool '{}'", program, self.name))?;

        Ok(json!({
            "command": argv,
            "exit_code": captured.exit_code,
            "timed_out": captured.timed_out,
            "stdout": captured.stdout,
            "stderr": captured.stderr,
        }))
    }
}

/// Load every `<dir>/*.toml` as a UserTool. `bin_dir` is the path that should
/// be prepended to `PATH` when any of these tools is executed.
pub fn load_dir(dir: &Path, bin_dir: PathBuf) -> Vec<UserTool> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        match load_one(&path, stem, &bin_dir) {
            Ok(t) => out.push(t),
            Err(e) => eprintln!("warning: skipping tool {}: {e:#}", path.display()),
        }
    }
    out
}

fn load_one(path: &Path, name: &str, bin_dir: &Path) -> Result<UserTool> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let spec: UserToolSpec =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    if spec.command.is_empty() {
        bail!("'command' must be a non-empty array");
    }
    let mut args: Vec<(String, ArgSpec)> = spec.args.into_iter().collect();
    args.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(UserTool {
        name: name.to_string(),
        description: spec.description,
        args,
        command: spec.command,
        timeout_secs: spec.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        confirmation: spec.confirmation,
        bin_dir: bin_dir.to_path_buf(),
    })
}

fn coerce_args(
    spec: &[(String, ArgSpec)],
    incoming: &Value,
) -> Result<HashMap<String, Value>> {
    let mut out = HashMap::new();
    let empty = serde_json::Map::new();
    let map = incoming.as_object().unwrap_or(&empty);
    for (name, s) in spec {
        let value = match map.get(name) {
            Some(v) => coerce_one(name, s.ty, v)?,
            None => match &s.default {
                Some(d) => coerce_one(name, s.ty, d)?,
                None => {
                    if s.required {
                        bail!("missing required argument '{name}'");
                    }
                    continue;
                }
            },
        };
        out.insert(name.clone(), value);
    }
    Ok(out)
}

fn coerce_one(name: &str, ty: ArgType, v: &Value) -> Result<Value> {
    let coerced = match (ty, v) {
        (ArgType::String, Value::String(_)) => v.clone(),
        (ArgType::String, other) => Value::String(value_to_plain_string(other)),
        (ArgType::Integer, Value::Number(n)) if n.is_i64() => v.clone(),
        (ArgType::Integer, Value::Number(n)) => Value::Number(
            serde_json::Number::from(
                n.as_f64()
                    .ok_or_else(|| anyhow!("arg '{name}': not a finite number"))?
                    as i64,
            ),
        ),
        (ArgType::Integer, Value::String(s)) => Value::Number(
            s.trim()
                .parse::<i64>()
                .map(serde_json::Number::from)
                .map_err(|_| anyhow!("arg '{name}': '{s}' is not an integer"))?,
        ),
        (ArgType::Number, Value::Number(_)) => v.clone(),
        (ArgType::Number, Value::String(s)) => {
            let f = s
                .trim()
                .parse::<f64>()
                .map_err(|_| anyhow!("arg '{name}': '{s}' is not a number"))?;
            Value::Number(
                serde_json::Number::from_f64(f)
                    .ok_or_else(|| anyhow!("arg '{name}': not a finite number"))?,
            )
        }
        (ArgType::Boolean, Value::Bool(_)) => v.clone(),
        (ArgType::Boolean, Value::String(s)) => match s.to_ascii_lowercase().as_str() {
            "true" | "yes" | "y" | "1" => Value::Bool(true),
            "false" | "no" | "n" | "0" => Value::Bool(false),
            _ => bail!("arg '{name}': '{s}' is not a boolean"),
        },
        (ty, other) => bail!(
            "arg '{name}': expected {}, got {other}",
            ty.as_json_str()
        ),
    };
    Ok(coerced)
}

fn value_to_plain_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}

fn substitute(command: &[String], args: &HashMap<String, Value>) -> Vec<String> {
    command.iter().map(|s| substitute_one(s, args)).collect()
}

fn substitute_one(s: &str, args: &HashMap<String, Value>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str("{{");
            rest = after;
            continue;
        };
        let name = after[..end].trim();
        let replacement = args
            .get(name)
            .map(value_to_plain_string)
            .unwrap_or_default();
        out.push_str(&replacement);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

fn prepend_path(cmd: &mut std::process::Command, bin_dir: &Path) {
    if !bin_dir.is_dir() {
        return;
    }
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut combined = std::ffi::OsString::from(bin_dir.as_os_str());
    if !existing.is_empty() {
        combined.push(":");
        combined.push(&existing);
    }
    cmd.env("PATH", combined);
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty() || a.chars().any(|c| c.is_whitespace() || c == '\'' || c == '"') {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_replaces_named_placeholders() {
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("/tmp".to_string()));
        args.insert("n".to_string(), json!(5));
        let cmd = vec!["echo".into(), "{{path}}".into(), "-n".into(), "{{n}}".into()];
        let out = substitute(&cmd, &args);
        assert_eq!(out, vec!["echo", "/tmp", "-n", "5"]);
    }

    #[test]
    fn substitute_drops_unknown_placeholders() {
        let args = HashMap::new();
        let cmd = vec!["x{{missing}}y".to_string()];
        assert_eq!(substitute(&cmd, &args), vec!["xy"]);
    }

    #[test]
    fn coerce_integer_from_string() {
        let spec = vec![(
            "n".to_string(),
            ArgSpec {
                ty: ArgType::Integer,
                description: None,
                required: true,
                default: None,
            },
        )];
        let v = coerce_args(&spec, &json!({"n": "20"})).unwrap();
        assert_eq!(v.get("n"), Some(&json!(20)));
    }

    #[test]
    fn coerce_uses_default_when_missing() {
        let spec = vec![(
            "n".to_string(),
            ArgSpec {
                ty: ArgType::Integer,
                description: None,
                required: false,
                default: Some(json!(7)),
            },
        )];
        let v = coerce_args(&spec, &json!({})).unwrap();
        assert_eq!(v.get("n"), Some(&json!(7)));
    }

    #[test]
    fn coerce_required_missing_errors() {
        let spec = vec![(
            "p".to_string(),
            ArgSpec {
                ty: ArgType::String,
                description: None,
                required: true,
                default: None,
            },
        )];
        assert!(coerce_args(&spec, &json!({})).is_err());
    }
}
