use std::path::Path;

use serde_json::{Map, Value};

use crate::colors as c;
use crate::error::Result;

fn removed_message() {
    eprintln!(
        "{} Monocle configuration removed. {}",
        c::green("✓"),
        c::dim("Claude Code will use Anthropic directly.")
    );
}

/// Remove Monocle configuration from Claude Code, preserving other settings.
pub fn unset_command(home: &Path) -> Result<()> {
    let settings_path = home.join(".claude").join("settings.json");

    if !settings_path.exists() {
        removed_message();
        return Ok(());
    }

    let mut settings: Map<String, Value> = match std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .and_then(|v| v.as_object().cloned())
    {
        Some(map) => map,
        None => {
            removed_message();
            return Ok(());
        }
    };

    settings.remove("apiKeyHelper");

    if let Some(env_val) = settings.get_mut("env") {
        if let Some(env_obj) = env_val.as_object_mut() {
            env_obj.remove("ANTHROPIC_BASE_URL");
            if env_obj.is_empty() {
                settings.remove("env");
            }
        }
    }

    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&Value::Object(settings))?,
    )?;
    removed_message();
    Ok(())
}
