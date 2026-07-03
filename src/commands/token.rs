use std::io::Write;

use crate::credentials::Credentials;
use crate::net::Client;
use crate::refresh::refresh_access_token;
use crate::util::{now_ms, parse_iso_ms};

const EXPIRY_BUFFER_MS: i64 = 5 * 60 * 1000; // 5 minutes

/// Output the access token to stdout (and ONLY that) for `apiKeyHelper`.
pub fn token_command(client: &Client, creds: &Credentials) {
    let stored = match creds.read() {
        Some(c) => c,
        None => {
            eprintln!("Not logged in. Run `monocle login --tenant <domain>` first.");
            std::process::exit(1);
        }
    };

    let expired = parse_iso_ms(&stored.access_token_expires_at)
        .map(|exp| now_ms() + EXPIRY_BUFFER_MS > exp)
        .unwrap_or(false);

    let token = if expired {
        match refresh_access_token(client, &stored, creds) {
            Ok(refreshed) => refreshed.access_token,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    } else {
        stored.access_token
    };

    // Exactly the token, no trailing newline.
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(token.as_bytes());
    let _ = stdout.flush();
}
