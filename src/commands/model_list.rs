use std::io::Write;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use rustyline::completion::Pair;
use serde::Deserialize;

use crate::auth::{get_access_token, try_access_token, AuthSession};
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::origin::auth_headers;

#[derive(Deserialize)]
struct ModelInfo {
    id: String,
    name: Option<String>,
    owned_by: Option<String>,
    context_window: Option<f64>,
    modality: Option<String>,
}

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelInfo>,
}

pub(crate) fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// Fetch the full model listing from the router (`GET /v1/models`) given an
/// already-resolved session, with no printing — the pure data path shared by
/// `monocle models` and the `agent` REPL's tab-completion (which needs the id
/// list, not a rendered table). Takes the session rather than resolving it
/// itself so callers can choose exiting (`get_access_token`) vs. graceful
/// (`try_access_token`) auth resolution.
fn fetch_models_with_session(client: &Client, session: &AuthSession) -> Result<Vec<ModelInfo>> {
    let bearer = format!("Bearer {}", session.token);

    let resp = client.get(
        &format!("{}{}", session.router_url, endpoints::MODELS),
        &auth_headers(&bearer),
    )?;

    if !resp.ok() {
        return Err(AppError::new(format!(
            "API error {}: {}",
            resp.status,
            resp.text()
        )));
    }

    Ok(resp.json::<ModelsResponse>()?.data)
}

/// Fetch just the model ids — what both `chat` and `agent`'s `/model`
/// completers need. Uses the non-exiting [`try_access_token`] (not
/// [`get_access_token`]) so a missing login or a network hiccup returns an
/// `Err` instead of killing the process; callers treat that as "no
/// candidates" and start the REPL anyway rather than propagating.
pub fn fetch_model_ids(client: &Client, creds: &Credentials) -> Result<Vec<String>> {
    let session = try_access_token(client, creds)?;
    Ok(fetch_models_with_session(client, &session)?
        .into_iter()
        .map(|m| m.id)
        .collect())
}

/// A parsed `/model` REPL invocation, shared by `chat`'s and `agent`'s REPL
/// dispatch.
#[derive(Debug, PartialEq, Eq)]
pub enum ModelCommand {
    /// Not a `/model` invocation at all.
    NotModel,
    /// Bare `/model` — show the current model.
    Show,
    /// `/model <id>` — switch to `id`.
    Switch(String),
}

/// Pure parser for the `/model` command, shared by `chat`'s and `agent`'s
/// REPL dispatch.
pub fn parse_model_command(input: &str) -> ModelCommand {
    let trimmed = input.trim();
    if trimmed == "/model" {
        return ModelCommand::Show;
    }
    match trimmed.strip_prefix("/model ") {
        None => ModelCommand::NotModel,
        Some(rest) => {
            let rest = rest.trim();
            if rest.is_empty() {
                ModelCommand::Show
            } else {
                ModelCommand::Switch(rest.to_string())
            }
        }
    }
}

/// Handle a `/model` line if `trimmed` is one: prints the switch/show
/// message and updates `*model` in place. Returns `true` if this input was
/// a `/model` invocation (caller should treat it as consumed and not fall
/// through to normal dispatch), `false` otherwise. Shared by `chat`'s (both
/// the completions and `--responses` REPLs) and `agent`'s REPL dispatch so
/// the "handle `/model` on Enter" logic can't drift between the three call
/// sites.
pub fn handle_model_command(trimmed: &str, model: &mut String) -> bool {
    match parse_model_command(trimmed) {
        ModelCommand::NotModel => false,
        ModelCommand::Show => {
            eprintln!("{}", crate::colors::dim(&format!("model: {model}")));
            true
        }
        ModelCommand::Switch(id) => {
            eprintln!("{}", crate::colors::dim(&format!("model → {id}")));
            *model = id;
            true
        }
    }
}

/// Fuzzy-match `query` against cached model ids for `/model` tab-completion,
/// shared by `chat`'s and `agent`'s REPL helpers. An empty query returns the
/// full list unranked (the "show me what's available" dropdown); otherwise
/// ids are scored with `fuzzy-matcher`'s skim algorithm (subsequence match,
/// case-smart), kept only if they match at all, and sorted best match first.
pub fn fuzzy_model_candidates(models: &[String], query: &str) -> Vec<Pair> {
    let to_pair = |id: &String| Pair {
        display: id.clone(),
        replacement: id.clone(),
    };
    if query.is_empty() {
        return models.iter().map(to_pair).collect();
    }
    // `ignore_case` (not the default smart-case) so typing in any case still
    // narrows the (lowercase-by-convention) model ids — smart-case would make
    // an all-caps query like "CLAUDE" match nothing.
    let matcher = SkimMatcherV2::default().ignore_case();
    let mut scored: Vec<(i64, &String)> = models
        .iter()
        .filter_map(|id| matcher.fuzzy_match(id, query).map(|score| (score, id)))
        .collect();
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, id)| to_pair(id)).collect()
}

pub fn model_list_command(client: &Client, creds: &Credentials) -> Result<()> {
    let session = get_access_token(client, creds);
    let models = fetch_models_with_session(client, &session)?;

    if models.is_empty() {
        eprintln!("No models available.");
        return Ok(());
    }

    let id_width = models
        .iter()
        .map(|m| m.id.chars().count())
        .chain([8])
        .max()
        .unwrap();
    let name_width = models
        .iter()
        .map(|m| m.name.as_deref().unwrap_or("").chars().count())
        .chain([4])
        .max()
        .unwrap();
    let modality_width = models
        .iter()
        .map(|m| m.modality.as_deref().unwrap_or("").chars().count())
        .chain([8])
        .max()
        .unwrap();

    let mut out = std::io::stdout();
    writeln!(
        out,
        "{}  {}  {}  {}  CONTEXT",
        pad("MODEL ID", id_width),
        pad("NAME", name_width),
        pad("MODALITY", modality_width),
        pad("OWNER", 10)
    )?;
    writeln!(
        out,
        "{}  {}  {}  {}  {}",
        "─".repeat(id_width),
        "─".repeat(name_width),
        "─".repeat(modality_width),
        "─".repeat(10),
        "─".repeat(7)
    )?;

    for m in &models {
        let ctx = match m.context_window {
            Some(cw) if cw != 0.0 => format!("{}k", (cw / 1000.0).round() as i64),
            _ => "-".to_string(),
        };
        writeln!(
            out,
            "{}  {}  {}  {}  {}",
            pad(&m.id, id_width),
            pad(m.name.as_deref().unwrap_or(""), name_width),
            pad(m.modality.as_deref().unwrap_or("-"), modality_width),
            pad(m.owned_by.as_deref().unwrap_or(""), 10),
            ctx
        )?;
    }

    eprintln!("\n{} model(s) available.", models.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_command_variants() {
        // Not a /model command.
        assert_eq!(parse_model_command("hello"), ModelCommand::NotModel);
        assert_eq!(parse_model_command("/models"), ModelCommand::NotModel);
        assert_eq!(parse_model_command("/help"), ModelCommand::NotModel);
        // Bare /model (with or without surrounding / trailing space) shows current.
        assert_eq!(parse_model_command("/model"), ModelCommand::Show);
        assert_eq!(parse_model_command("  /model  "), ModelCommand::Show);
        assert_eq!(parse_model_command("/model   "), ModelCommand::Show);
        // /model <id> switches.
        assert_eq!(
            parse_model_command("/model gpt-4o"),
            ModelCommand::Switch("gpt-4o".to_string())
        );
        assert_eq!(
            parse_model_command("/model  claude-x  "),
            ModelCommand::Switch("claude-x".to_string())
        );
    }

    #[test]
    fn handle_model_command_switches_shows_and_passes_through() {
        let mut model = "old-model".to_string();

        // Not a /model line — unconsumed, model untouched.
        assert!(!handle_model_command("hello", &mut model));
        assert_eq!(model, "old-model");

        // Bare /model — consumed, just shows (no mutation).
        assert!(handle_model_command("/model", &mut model));
        assert_eq!(model, "old-model");

        // /model <id> — consumed, switches.
        assert!(handle_model_command("/model new-model", &mut model));
        assert_eq!(model, "new-model");
    }

    fn model_ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fuzzy_model_candidates_empty_query_returns_full_list_unranked() {
        let models = model_ids(&["claude-sonnet-4-6", "gpt-4o", "claude-opus-4"]);
        let got: Vec<String> = fuzzy_model_candidates(&models, "")
            .into_iter()
            .map(|p| p.replacement)
            .collect();
        assert_eq!(got, models);
    }

    #[test]
    fn fuzzy_model_candidates_narrows_by_subsequence() {
        let models = model_ids(&["claude-sonnet-4-6", "gpt-4o", "claude-opus-4"]);
        let got: Vec<String> = fuzzy_model_candidates(&models, "cla")
            .into_iter()
            .map(|p| p.replacement)
            .collect();
        assert_eq!(got, vec!["claude-sonnet-4-6", "claude-opus-4"]);
    }

    #[test]
    fn fuzzy_model_candidates_ranks_tighter_match_first() {
        // "gpt4o" as a subsequence appears in both, but a contiguous match in
        // "gpt-4o" should outrank the more scattered match in "gpt-4-omni".
        let models = model_ids(&["gpt-4-omni", "gpt-4o"]);
        let got: Vec<String> = fuzzy_model_candidates(&models, "gpt4o")
            .into_iter()
            .map(|p| p.replacement)
            .collect();
        assert_eq!(got.first().map(String::as_str), Some("gpt-4o"));
    }

    #[test]
    fn fuzzy_model_candidates_excludes_non_matches() {
        let models = model_ids(&["claude-sonnet-4-6", "gpt-4o"]);
        let got = fuzzy_model_candidates(&models, "zzz");
        assert!(got.is_empty());
    }

    #[test]
    fn fuzzy_model_candidates_case_insensitive() {
        let models = model_ids(&["claude-sonnet-4-6"]);
        let got = fuzzy_model_candidates(&models, "CLAUDE");
        assert_eq!(got.len(), 1);
    }
}
