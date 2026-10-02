use std::io::Write;

use crate::auth::ensure_fresh_token;
use crate::credentials::Credentials;
use crate::net::Client;

/// Output the access token to stdout (and ONLY that) for `apiKeyHelper`.
pub fn token_command(client: &Client, creds: &Credentials) {
    let token = match ensure_fresh_token(client, creds) {
        Ok(session) => session.token,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // Exactly the token, no trailing newline.
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(token.as_bytes());
    let _ = stdout.flush();
}
