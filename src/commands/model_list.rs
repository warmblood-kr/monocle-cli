use std::io::Write;

use serde::Deserialize;

use crate::auth::get_access_token;
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

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

pub fn model_list_command(client: &Client, creds: &Credentials) -> Result<()> {
    let session = get_access_token(client, creds);
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

    let models = resp.json::<ModelsResponse>()?.data;

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
