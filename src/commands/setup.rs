use std::path::Path;

use serde_json::{json, Map, Value};

use crate::colors as c;
use crate::credentials::Credentials;
use crate::error::Result;

/// Configure Claude Code (`~/.claude/settings.json`) to authenticate via Monocle.
pub fn setup_command(creds: &Credentials, home: &Path) -> Result<()> {
    let creds = match creds.read() {
        Some(c) => c,
        None => {
            eprintln!("Not logged in. Run `monocle login --tenant <domain>` first.");
            std::process::exit(1);
        }
    };

    let claude_dir = home.join(".claude");
    let settings_path = claude_dir.join("settings.json");

    // Read existing settings (treat any failure as an empty object).
    let mut settings: Map<String, Value> = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    settings.insert("apiKeyHelper".to_string(), json!("monocle token"));

    // Resolve router URL.
    let router_url = match &creds.router_url {
        Some(url) => url.clone(),
        None => {
            let is_local = creds.tenant_domain.starts_with("localhost")
                || creds.tenant_domain.starts_with("127.0.0.1");
            let protocol = if is_local { "http" } else { "https" };
            eprintln!("Warning: router_url not found. Using tenant domain as fallback.");
            eprintln!("Run `monocle login --tenant <domain>` to update credentials.");
            format!("{protocol}://{}", creds.tenant_domain)
        }
    };

    // settings.env.ANTHROPIC_BASE_URL = router_url
    let env_obj = settings
        .entry("env".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !env_obj.is_object() {
        *env_obj = Value::Object(Map::new());
    }
    env_obj
        .as_object_mut()
        .unwrap()
        .insert("ANTHROPIC_BASE_URL".to_string(), json!(router_url));

    std::fs::create_dir_all(&claude_dir)?;
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&Value::Object(settings))?,
    )?;

    eprintln!(
        "{} Claude Code configured to use Monocle authentication",
        c::green("✓")
    );
    eprintln!("  {}       monocle token", c::dim("apiKeyHelper:"));
    eprintln!("  {} {router_url}", c::dim("ANTHROPIC_BASE_URL:"));

    // Warn about conflicting env vars.
    let conflicting: Vec<&str> = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]
        .into_iter()
        .filter(|k| std::env::var(k).map(|v| !v.is_empty()).unwrap_or(false))
        .collect();
    if !conflicting.is_empty() {
        eprintln!(
            "\n{} {} environment variable is set.",
            c::yellow("⚠"),
            c::yellow(&conflicting.join(", "))
        );
        eprintln!(
            "  Use {} to launch Claude Code — it clears conflicting env vars automatically.",
            c::bold("monocle claude")
        );
    }

    eprintln!(
        "\n{} {}",
        c::red(&c::dim("To disconnect Claude Code from Monocle, run:")),
        c::red(&c::bold("monocle unset"))
    );
    Ok(())
}
