//! Tools the agent can call — early-Claude-Code-style: filesystem (read/write/
//! edit) plus a cross-platform shell (`bash` on macOS/Linux, `powershell` on
//! Windows). The shell tool's NAME and description are platform-specific so the
//! model writes syntax for the right shell.
//!
//! Safety/sandboxing (SDD G4) is the loop's responsibility via the `Approver`
//! seam (see `loop`); tools here just execute. Paths resolve relative to the
//! tool context's working directory.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::agent::providers::ToolDef;
use crate::agent::runner::Cancel;

/// Filesystem backend for the read/write/edit tools. The default ([`LocalFs`])
/// is plain local disk I/O; under ACP, when the client advertises fs
/// capabilities, an editor-mediated backend is injected instead so unsaved
/// buffers and change-tracking flow through the client (see `acp::AcpClientFs`).
/// Errors are already-human-readable messages the tool drops into
/// [`ToolOutcome::error`].
pub trait FsBackend: Send + Sync {
    fn read(&self, path: &Path) -> std::result::Result<String, String>;
    fn write(&self, path: &Path, content: &str) -> std::result::Result<(), String>;
}

/// Default backend: read/write straight to local disk (today's behavior).
pub struct LocalFs;

impl FsBackend for LocalFs {
    fn read(&self, path: &Path) -> std::result::Result<String, String> {
        std::fs::read_to_string(path).map_err(|e| e.to_string())
    }

    fn write(&self, path: &Path, content: &str) -> std::result::Result<(), String> {
        // Create parent directories as needed (moved here from the write_file tool).
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
        }
        std::fs::write(path, content).map_err(|e| e.to_string())
    }
}

/// Execution context shared by all tools (the working directory the agent acts in,
/// plus the turn's [`Cancel`] flag so long-running tools can be interrupted mid-run).
pub struct ToolContext {
    pub workdir: PathBuf,
    /// Cancellation flag for the current turn. Defaults to a fresh, never-set
    /// `Cancel` (= "not cancellable") so the many `ToolContext::new(cwd)` call
    /// sites and tests keep working; the real callers attach the turn's flag via
    /// [`ToolContext::with_cancel`].
    pub cancel: Cancel,
    /// Filesystem backend for the read/write/edit tools. Defaults to [`LocalFs`]
    /// (local disk) so every existing caller/test is unaffected; the ACP surface
    /// swaps in a client-mediated backend via [`ToolContext::with_fs`].
    pub fs: Arc<dyn FsBackend>,
}

impl ToolContext {
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
            cancel: Cancel::new(),
            fs: Arc::new(LocalFs),
        }
    }

    /// Attach the turn's cancellation flag so cancellable tools (the shell) can be
    /// interrupted mid-run instead of only at the step boundary.
    pub fn with_cancel(mut self, cancel: Cancel) -> Self {
        self.cancel = cancel;
        self
    }

    /// Route the read/write/edit tools through a custom filesystem backend
    /// (e.g. an ACP client-mediated one) instead of the default local disk.
    pub fn with_fs(mut self, fs: Arc<dyn FsBackend>) -> Self {
        self.fs = fs;
        self
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

/// The result of running a tool, split into two channels (SDD §9a):
/// `llm` is fed back into the conversation (re-sent every turn → keep bounded);
/// `ui` is the human/client-facing rendering (falls back to `llm`).
/// `is_error` lets the loop/model see failures without aborting the whole turn.
pub struct ToolOutcome {
    pub llm: String,
    pub ui: Option<String>,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(llm: impl Into<String>) -> Self {
        Self {
            llm: llm.into(),
            ui: None,
            is_error: false,
        }
    }
    pub fn error(llm: impl Into<String>) -> Self {
        Self {
            llm: llm.into(),
            ui: None,
            is_error: true,
        }
    }
    /// Attach an explicit human/UI-facing rendering.
    pub fn with_ui(mut self, ui: impl Into<String>) -> Self {
        self.ui = Some(ui.into());
        self
    }
    /// The human-facing text (explicit `ui`, else the `llm` channel).
    pub fn ui_text(&self) -> &str {
        self.ui.as_deref().unwrap_or(&self.llm)
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

/// Cap on `bash`/`powershell` output fed to the model (re-sent every turn, §9a).
const MAX_SHELL_CHARS: usize = 20_000;

/// Truncate the middle of long output, keeping head + tail (errors often trail).
fn cap_output(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    let head_n = max * 3 / 5;
    let tail_n = max - head_n;
    let head: String = s.chars().take(head_n).collect();
    let tail: String = s.chars().skip(total - tail_n).collect();
    format!(
        "{head}\n[... {} characters omitted ...]\n{tail}",
        total - head_n - tail_n
    )
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
        match ctx.fs.read(&ctx.resolve(path)) {
            // Cap large reads so one file can't blow the model's context window.
            Ok(content) => {
                let total = content.chars().count();
                let (llm, truncated) = if total > MAX_READ_CHARS {
                    let head: String = content.chars().take(MAX_READ_CHARS).collect();
                    (
                        format!(
                            "{head}\n\n[truncated: showing first {MAX_READ_CHARS} of {total} characters]"
                        ),
                        ", truncated",
                    )
                } else {
                    (content, "")
                };
                ToolOutcome::ok(llm).with_ui(format!("read {path} — {total} chars{truncated}"))
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
        match ctx.fs.write(&ctx.resolve(path), content) {
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
        let original = match ctx.fs.read(&full) {
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
        match ctx.fs.write(&full, &updated) {
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

        // Honor a cancel that landed before we even spawned — don't start work.
        if ctx.cancel.is_cancelled() {
            return ToolOutcome::error("shell command cancelled before it started");
        }

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
        cmd.current_dir(&ctx.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutcome::error(format!("failed to run shell: {e}")),
        };

        // Drain stdout/stderr on their own threads so a chatty long-running command
        // can't deadlock the poll loop by filling a pipe buffer.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let drain = |pipe: Option<std::process::ChildStdout>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut p) = pipe {
                    let _ = p.read_to_end(&mut buf);
                }
                buf
            })
        };
        let drain_err = |pipe: Option<std::process::ChildStderr>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut p) = pipe {
                    let _ = p.read_to_end(&mut buf);
                }
                buf
            })
        };
        let out_handle = drain(stdout_pipe);
        let err_handle = drain_err(stderr_pipe);

        // Poll: exit → done; cancel → kill+reap; else nap and re-check.
        let mut cancelled = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if ctx.cancel.is_cancelled() {
                        let _ = child.kill();
                        let _ = child.wait();
                        cancelled = true;
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    let _ = child.wait();
                    return ToolOutcome::error(format!("failed to wait on shell: {e}"));
                }
            }
        };

        // Reader threads finish once the pipes close (on exit or kill).
        let stdout = out_handle.join().unwrap_or_default();
        let stderr = err_handle.join().unwrap_or_default();
        let stdout = String::from_utf8_lossy(&stdout);
        let stderr = String::from_utf8_lossy(&stderr);
        let mut combined = String::new();
        if !stdout.is_empty() {
            combined.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&stderr);
        }
        // Cap the LLM-facing output (re-sent every turn — §9a).
        let capped = cap_output(&combined, MAX_SHELL_CHARS);

        if cancelled {
            let partial = if capped.is_empty() {
                String::new()
            } else {
                format!("\n[partial output before cancel]\n{capped}")
            };
            return ToolOutcome::error(format!("shell command cancelled{partial}"))
                .with_ui("cancelled".to_string());
        }

        let code = status.and_then(|s| s.code()).unwrap_or(-1);
        let success = status.map(|s| s.success()).unwrap_or(false);
        let llm = format!("{capped}\n[exit code: {code}]");
        ToolOutcome {
            llm,
            ui: Some(format!("exit {code}")),
            is_error: !success,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localfs_write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        let fs = LocalFs;
        fs.write(&path, "hello disk").unwrap();
        assert_eq!(fs.read(&path).unwrap(), "hello disk");
    }

    #[test]
    fn localfs_write_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // Nested path whose parent directories do not yet exist.
        let path = dir.path().join("a").join("b").join("deep.txt");
        let fs = LocalFs;
        fs.write(&path, "made the dirs").unwrap();
        assert!(path.exists());
        assert_eq!(fs.read(&path).unwrap(), "made the dirs");
    }

    #[test]
    fn localfs_read_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.txt");
        assert!(LocalFs.read(&path).is_err());
    }
}
