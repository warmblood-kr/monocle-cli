//! Tools the agent can call — early-Claude-Code-style: filesystem (read/write/
//! edit) plus a cross-platform shell (`bash` on macOS/Linux, `powershell` on
//! Windows). The shell tool's NAME and description are platform-specific so the
//! model writes syntax for the right shell.
//!
//! Safety/sandboxing (SDD G4) is the loop's responsibility via the `Approver`
//! seam (see `loop`); tools here just execute. Paths resolve relative to the
//! tool context's working directory.

use std::path::PathBuf;
use std::process::Command;

use serde_json::{json, Value};

use crate::agent::providers::ToolDef;

/// Execution context shared by all tools (the working directory the agent acts in).
pub struct ToolContext {
    pub workdir: PathBuf,
}

impl ToolContext {
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }

    /// Resolve a (possibly relative) path against the working directory.
    fn resolve(&self, p: &str) -> PathBuf {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            self.workdir.join(path)
        }
    }
}

/// The result of running a tool. `is_error` lets the loop/model see failures
/// without aborting the whole turn.
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

pub trait Tool {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema for the tool's arguments.
    fn parameters(&self) -> Value;
    fn run(&self, ctx: &ToolContext, args: &Value) -> ToolOutcome;

    /// Whether the tool mutates state / has side effects (gates approval).
    /// Read-only tools override this to `false`.
    fn is_side_effecting(&self) -> bool {
        true
    }

    fn def(&self) -> ToolDef {
        ToolDef::function(self.name(), self.description(), self.parameters())
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> std::result::Result<&'a str, ToolOutcome> {
    args[key]
        .as_str()
        .ok_or_else(|| ToolOutcome::error(format!("missing or non-string argument: {key}")))
}

// ── read_file ──────────────────────────────────────────────────────────────

pub struct ReadFile;
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a UTF-8 text file and return its contents. `path` may be absolute or relative to the working directory."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "File path to read" } },
            "required": ["path"]
        })
    }
    fn is_side_effecting(&self) -> bool {
        false
    }
    fn run(&self, ctx: &ToolContext, args: &Value) -> ToolOutcome {
        let path = match str_arg(args, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        match std::fs::read_to_string(ctx.resolve(path)) {
            // Cap large reads so one file can't blow the model's context window.
            Ok(content) => {
                let total = content.chars().count();
                if total > MAX_READ_CHARS {
                    let head: String = content.chars().take(MAX_READ_CHARS).collect();
                    ToolOutcome::ok(format!(
                        "{head}\n\n[truncated: showing first {MAX_READ_CHARS} of {total} characters]"
                    ))
                } else {
                    ToolOutcome::ok(content)
                }
            }
            Err(e) => ToolOutcome::error(format!("read_file failed for {path}: {e}")),
        }
    }
}

/// Cap on `read_file` output (characters) to protect the context window.
const MAX_READ_CHARS: usize = 50_000;

// ── write_file ─────────────────────────────────────────────────────────────

pub struct WriteFile;
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write (creating or overwriting) a UTF-8 text file. Creates parent directories as needed."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to write" },
                "content": { "type": "string", "description": "Full file contents" }
            },
            "required": ["path", "content"]
        })
    }
    fn run(&self, ctx: &ToolContext, args: &Value) -> ToolOutcome {
        let path = match str_arg(args, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let content = match str_arg(args, "content") {
            Ok(c) => c,
            Err(e) => return e,
        };
        let full = ctx.resolve(path);
        if let Some(parent) = full.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolOutcome::error(format!(
                    "write_file failed to create {}: {e}",
                    parent.display()
                ));
            }
        }
        match std::fs::write(&full, content) {
            Ok(()) => ToolOutcome::ok(format!("Wrote {} bytes to {path}", content.len())),
            Err(e) => ToolOutcome::error(format!("write_file failed for {path}: {e}")),
        }
    }
}

// ── edit_file ──────────────────────────────────────────────────────────────

pub struct EditFile;
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn description(&self) -> &str {
        "Replace an exact substring in a text file. `old_string` must occur exactly once (include enough surrounding context to make it unique)."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to edit" },
                "old_string": { "type": "string", "description": "Exact text to replace (must be unique in the file)" },
                "new_string": { "type": "string", "description": "Replacement text" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn run(&self, ctx: &ToolContext, args: &Value) -> ToolOutcome {
        let path = match str_arg(args, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let old = match str_arg(args, "old_string") {
            Ok(s) => s,
            Err(e) => return e,
        };
        let new = match str_arg(args, "new_string") {
            Ok(s) => s,
            Err(e) => return e,
        };
        let full = ctx.resolve(path);
        let original = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(e) => return ToolOutcome::error(format!("edit_file failed to read {path}: {e}")),
        };
        let count = original.matches(old).count();
        if count == 0 {
            return ToolOutcome::error(format!("edit_file: `old_string` not found in {path}"));
        }
        if count > 1 {
            return ToolOutcome::error(format!(
                "edit_file: `old_string` occurs {count} times in {path}; add surrounding context to make it unique"
            ));
        }
        let updated = original.replacen(old, new, 1);
        match std::fs::write(&full, updated) {
            Ok(()) => ToolOutcome::ok(format!("Edited {path}")),
            Err(e) => ToolOutcome::error(format!("edit_file failed to write {path}: {e}")),
        }
    }
}

// ── shell (bash on unix / powershell on windows) ────────────────────────────

pub struct Shell;
impl Tool for Shell {
    fn name(&self) -> &str {
        #[cfg(windows)]
        {
            "powershell"
        }
        #[cfg(not(windows))]
        {
            "bash"
        }
    }
    fn description(&self) -> &str {
        #[cfg(windows)]
        {
            "Run a PowerShell command on Windows in the working directory. Returns combined stdout/stderr and the exit code."
        }
        #[cfg(not(windows))]
        {
            "Run a bash command on macOS/Linux in the working directory. Returns combined stdout/stderr and the exit code."
        }
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "command": { "type": "string", "description": "The shell command to run" } },
            "required": ["command"]
        })
    }
    fn run(&self, ctx: &ToolContext, args: &Value) -> ToolOutcome {
        let command = match str_arg(args, "command") {
            Ok(c) => c,
            Err(e) => return e,
        };

        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", command]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new("bash");
            c.arg("-c").arg(command);
            c
        };
        cmd.current_dir(&ctx.workdir);

        match cmd.output() {
            Ok(out) => {
                let code = out.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut content = String::new();
                if !stdout.is_empty() {
                    content.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !content.is_empty() && !content.ends_with('\n') {
                        content.push('\n');
                    }
                    content.push_str(&stderr);
                }
                content.push_str(&format!("\n[exit code: {code}]"));
                ToolOutcome {
                    content,
                    is_error: !out.status.success(),
                }
            }
            Err(e) => ToolOutcome::error(format!("failed to run shell: {e}")),
        }
    }
}

// ── registry ────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The default early-Claude-Code-style toolset: read/write/edit + shell.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        r.add(Box::new(ReadFile));
        r.add(Box::new(WriteFile));
        r.add(Box::new(EditFile));
        r.add(Box::new(Shell));
        r
    }

    pub fn add(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn defs(&self) -> Vec<ToolDef> {
        self.tools.iter().map(|t| t.def()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|b| b.as_ref())
    }

    pub fn run(&self, ctx: &ToolContext, name: &str, args: &Value) -> ToolOutcome {
        match self.get(name) {
            Some(tool) => tool.run(ctx, args),
            None => ToolOutcome::error(format!("unknown tool: {name}")),
        }
    }
}
