use std::process::Command;

use serde_json::json;

use crate::auth::router_url_for;
use crate::credentials::Credentials;
use crate::origin::{MONOCLE_ORIGIN, ORIGIN_HEADER_NAME};

/// Launch Claude Code through Monocle: inline settings, conflicting env vars
/// stripped, base URL injected, usage attributed to the "cli" surface.
pub fn claude_command(creds: &Credentials, args: &[String]) {
    let creds = match creds.read() {
        Some(c) => c,
        None => {
            eprintln!("Not logged in. Run `monocle login --tenant <domain>` first.");
            std::process::exit(1);
        }
    };

    let router_url = router_url_for(&creds);

    // Inline settings scoped to this child only — avoids mutating
    // ~/.claude/settings.json. `apiKeyHelper` keeps tokens fresh across long
    // sessions; `model` defaults to the 1M-context Sonnet to avoid surprise Opus
    // costs (a user `--model` flag still wins, since the CLI flag outranks the
    // settings field).
    let inline_settings = json!({
        "apiKeyHelper": "monocle token",
        "model": "sonnet[1m]",
    })
    .to_string();

    // Merge the origin header into ANTHROPIC_CUSTOM_HEADERS (newline-separated
    // `Name: Value` lines) so we don't clobber any the user already set.
    let origin_line = format!("{ORIGIN_HEADER_NAME}: {MONOCLE_ORIGIN}");
    let custom_headers = match std::env::var("ANTHROPIC_CUSTOM_HEADERS") {
        Ok(existing) if !existing.is_empty() => format!("{existing}\n{origin_line}"),
        _ => origin_line,
    };

    let mut cmd = Command::new("claude");
    cmd.arg("--settings")
        .arg(&inline_settings)
        .args(args)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env("ANTHROPIC_BASE_URL", &router_url)
        .env("ANTHROPIC_CUSTOM_HEADERS", &custom_headers);

    run(cmd);
}

#[cfg(unix)]
fn run(mut cmd: Command) {
    use std::os::unix::process::CommandExt;
    // exec replaces this process — stdio, TTY, signals, and exit code all pass
    // through transparently. Only returns on failure to launch.
    let err = cmd.exec();
    report_spawn_error(err);
}

#[cfg(not(unix))]
fn run(mut cmd: Command) {
    match cmd.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(0)),
        Err(err) => report_spawn_error(err),
    }
}

fn report_spawn_error(err: std::io::Error) -> ! {
    if err.kind() == std::io::ErrorKind::NotFound {
        eprintln!("Error: `claude` command not found. Is Claude Code installed?");
    } else {
        eprintln!("Error: {err}");
    }
    std::process::exit(1);
}
