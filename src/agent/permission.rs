//! Per-session / persisted tool-permission decisions for the interactive agent.
//!
//! The agent loop gates side-effecting tools behind an `Approver` (runner.rs).
//! On its own the interactive approver has no memory, so the user re-answers the
//! same prompt every time. This module gives it one: a decision can be remembered
//! for **this session** (in-memory) or **always** — persisted to a working-dir
//! `.monocle/settings.json` so future sessions don't re-prompt.
//!
//! Granularity is **per tool name**, except the shell tool (`bash`/`powershell`)
//! which is **per command string**, so allowing `npm test` never green-lights
//! every shell command. Persisted rules live under `allowedTools`:
//!
//! ```json
//! { "allowedTools": ["write_file", "bash(npm test)", "bash(cargo *)"] }
//! ```
//!
//! A `bash(...)` pattern matches a command exactly, or as a prefix when it ends
//! with `*` (`bash(cargo *)` allows `cargo build`, `cargo test`, …). Users can
//! hand-edit the file to add such wildcards.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// The command string for a shell tool call, if present. Its presence is what
/// makes a call *command-scoped* (only the shell tool takes a `command` arg).
fn command_arg(args: &Value) -> Option<&str> {
    args.get("command").and_then(Value::as_str)
}

/// The rule string to remember for a given tool call: the tool name, or
/// `name(command)` for a shell call.
pub fn rule_for(tool_name: &str, args: &Value) -> String {
    match command_arg(args) {
        Some(cmd) => format!("{tool_name}({})", cmd.trim()),
        None => tool_name.to_string(),
    }
}

/// Whether a stored `rule` authorizes this tool call. Plain rules match by tool
/// name; `name(pattern)` rules also require the command to match `pattern`
/// (exact, or prefix when `pattern` ends with `*`).
pub fn rule_matches(rule: &str, tool_name: &str, args: &Value) -> bool {
    match rule.strip_suffix(')').and_then(|r| r.split_once('(')) {
        Some((name, pat)) => {
            name == tool_name
                && command_arg(args).is_some_and(|cmd| command_matches(pat, cmd.trim()))
        }
        None => rule == tool_name,
    }
}

fn command_matches(pat: &str, cmd: &str) -> bool {
    match pat.strip_suffix('*') {
        Some(prefix) => cmd.starts_with(prefix),
        None => cmd == pat,
    }
}

/// A session's remembered tool-permission decisions: the in-memory allow-set
/// (session + rules loaded from disk) plus where "always" decisions persist.
pub struct PermissionStore {
    rules: Vec<String>,
    settings_path: PathBuf,
}

impl PermissionStore {
    /// Load allow-rules from `<workdir>/.monocle/settings.json`. A missing or
    /// invalid file yields an empty allow-set (nothing is pre-authorized).
    pub fn load(workdir: &Path) -> Self {
        let settings_path = workdir.join(".monocle").join("settings.json");
        let rules = read_allowed_tools(&settings_path);
        Self {
            rules,
            settings_path,
        }
    }

    /// Whether this call is already authorized by a remembered rule.
    pub fn is_allowed(&self, tool_name: &str, args: &Value) -> bool {
        self.rules.iter().any(|r| rule_matches(r, tool_name, args))
    }

    /// Remember this call for the rest of the session only (not persisted).
    pub fn allow_session(&mut self, tool_name: &str, args: &Value) {
        self.remember(rule_for(tool_name, args));
    }

    /// Remember this call for the session AND persist it to
    /// `.monocle/settings.json` so future sessions skip the prompt.
    pub fn allow_always(&mut self, tool_name: &str, args: &Value) -> std::io::Result<()> {
        let rule = rule_for(tool_name, args);
        self.remember(rule.clone());
        persist_allowed_tool(&self.settings_path, &rule)
    }

    fn remember(&mut self, rule: String) {
        if !self.rules.contains(&rule) {
            self.rules.push(rule);
        }
    }
}

/// Read the `allowedTools` string array from a settings file (missing/invalid → empty).
fn read_allowed_tools(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .and_then(|v| v.get("allowedTools").and_then(Value::as_array).cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Append `rule` to `allowedTools`, preserving any other keys the user hand-added
/// (read-modify-write, matching `commands::setup`'s treatment of settings files).
fn persist_allowed_tool(path: &Path, rule: &str) -> std::io::Result<()> {
    let mut settings: Map<String, Value> = std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    let list = settings
        .entry("allowedTools".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !list.is_array() {
        *list = Value::Array(Vec::new());
    }
    let arr = list.as_array_mut().unwrap();
    if !arr.iter().any(|x| x.as_str() == Some(rule)) {
        arr.push(Value::String(rule.to_string()));
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body =
        serde_json::to_string_pretty(&Value::Object(settings)).map_err(std::io::Error::other)?;
    std::fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_tool_rule_matches_by_name() {
        assert!(rule_matches(
            "write_file",
            "write_file",
            &json!({ "path": "a" })
        ));
        assert!(!rule_matches("write_file", "edit_file", &json!({})));
    }

    #[test]
    fn shell_rule_is_command_scoped() {
        let rule = rule_for("bash", &json!({ "command": "npm test" }));
        assert_eq!(rule, "bash(npm test)");
        assert!(rule_matches(
            &rule,
            "bash",
            &json!({ "command": "npm test" })
        ));
        assert!(!rule_matches(
            &rule,
            "bash",
            &json!({ "command": "rm -rf /" })
        ));
    }

    #[test]
    fn shell_wildcard_prefix_matches() {
        assert!(rule_matches(
            "bash(cargo *)",
            "bash",
            &json!({ "command": "cargo build" })
        ));
        assert!(!rule_matches(
            "bash(cargo *)",
            "bash",
            &json!({ "command": "npm run" })
        ));
    }

    #[test]
    fn store_persists_and_reloads_always_rules() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = PermissionStore::load(dir.path());
        assert!(!store.is_allowed("write_file", &json!({})));
        store.allow_always("write_file", &json!({})).unwrap();
        assert!(store.is_allowed("write_file", &json!({})));
        // A fresh load sees the persisted rule.
        assert!(PermissionStore::load(dir.path()).is_allowed("write_file", &json!({})));
    }

    #[test]
    fn session_rules_are_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = PermissionStore::load(dir.path());
        store.allow_session("bash", &json!({ "command": "ls" }));
        assert!(store.is_allowed("bash", &json!({ "command": "ls" })));
        // Session-only decisions do not survive into a fresh load.
        assert!(!PermissionStore::load(dir.path()).is_allowed("bash", &json!({ "command": "ls" })));
    }

    #[test]
    fn persist_preserves_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".monocle").join("settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{ "model": "claude-sonnet-4-6" }"#).unwrap();
        PermissionStore::load(dir.path())
            .allow_always("edit_file", &json!({}))
            .unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["model"], "claude-sonnet-4-6");
        assert_eq!(v["allowedTools"][0], "edit_file");
    }
}
