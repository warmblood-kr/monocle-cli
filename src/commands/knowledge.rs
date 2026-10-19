//! `monocle knowledge create|upload|list|delete` — manage the logged-in user's
//! jarvice knowledge collections. Data (ids, tables) goes to stdout; progress,
//! the profile banner and summaries go to stderr, so output pipes cleanly.

use std::io::Write;

use crate::auth::AuthSession;
use crate::commands::chat::resolve_jarvice;
use crate::commands::model_list::pad;
use crate::credentials::Credentials;
use crate::error::{AppError, Result};
use crate::jarvice_knowledge as kb;
use crate::net::Client;
use crate::profiles::Selection;

/// Auth + jarvice host, with the same stderr profile banner `chat list` prints.
fn connect(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
) -> Result<(String, String)> {
    let (_session, jarvice_url, bearer): (AuthSession, String, String) =
        resolve_jarvice(client, creds)?;
    eprintln!(
        "Profile: {} ({}) — tenant {}",
        selection.profile,
        selection.environment,
        creds
            .read()
            .map(|c| crate::util::plain(&c.tenant_domain))
            .unwrap_or_default()
    );
    Ok((jarvice_url, bearer))
}

/// Left-aligned table on stdout; the last column is left unpadded.
fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let widths: Vec<usize> = (0..headers.len())
        .map(|c| {
            rows.iter()
                .map(|r| r[c].chars().count())
                .chain([headers[c].len()])
                .max()
                .unwrap()
        })
        .collect();
    let line = |cells: &[String]| {
        let last = cells.len() - 1;
        let parts: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i == last {
                    c.clone()
                } else {
                    pad(c, widths[i])
                }
            })
            .collect();
        let _ = writeln!(std::io::stdout(), "{}", parts.join("  "));
    };
    line(&headers.iter().map(|h| h.to_string()).collect::<Vec<_>>());
    rows.iter().for_each(|r| line(r));
}

/// `monocle knowledge create NAME` — prints the new collection id to stdout.
pub fn knowledge_create_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    name: &str,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, selection)?;
    // Names are how every other command addresses a collection, so keep them unique.
    let existing = kb::list_knowledge(client, &url, &bearer, false)?;
    if let Some(dup) = existing.iter().find(|k| k.name == name) {
        return Err(AppError::new(format!(
            "Collection '{name}' already exists (id {}); delete it first or pick another name.",
            dup.id
        )));
    }
    let created = kb::create_knowledge(client, &url, &bearer, name)?;
    println!("{}", created.id);
    Ok(())
}

/// `monocle knowledge upload COLLECTION PATH...` — one line per stored file on
/// stdout (`file-id<TAB>name`); a per-file failure is reported and the rest go on.
pub fn knowledge_upload_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    collection: &str,
    paths: &[String],
) -> Result<()> {
    let files = kb::collect_files(paths)?;
    let (url, bearer) = connect(client, creds, selection)?;
    let all = kb::list_knowledge(client, &url, &bearer, true)?;
    let target = kb::find(&all, collection)?;

    let total = files.len();
    let (mut ok, mut skipped, mut failed) = (0, 0, 0);
    for (i, file) in files.iter().enumerate() {
        let name = file
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(crate::util::plain)
            .unwrap_or_else(|| "file".to_string());
        let tag = format!("[{}/{total}] {name}", i + 1);
        // Re-running an upload must not pile up duplicates of the same document.
        if target.files().iter().any(|f| f.meta.name == name) {
            eprintln!("{tag}: skipped (already in collection)");
            skipped += 1;
            continue;
        }
        let result = kb::upload_file(client, &url, &bearer, &name, file).and_then(|id| {
            kb::add_file(client, &url, &bearer, &target.id, &id).inspect_err(|_| {
                // Don't leave a stored-but-unattached file behind.
                let _ = kb::delete_file(client, &url, &bearer, &id);
            })?;
            Ok(id)
        });
        match result {
            Ok(id) => {
                eprintln!("{tag}: ok");
                println!("{id}\t{name}");
                ok += 1;
            }
            Err(e) => {
                eprintln!("{tag}: FAILED — {e}");
                failed += 1;
            }
        }
    }
    eprintln!("Uploaded {ok}, skipped {skipped}, failed {failed} (of {total}).");
    if failed > 0 {
        return Err(AppError::new(format!(
            "{failed} of {total} file(s) failed."
        )));
    }
    Ok(())
}

/// `monocle knowledge list [COLLECTION]` — all collections, or one's files.
pub fn knowledge_list_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    collection: Option<&str>,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, selection)?;
    let all = kb::list_knowledge(client, &url, &bearer, false)?;
    match collection {
        None if all.is_empty() => eprintln!("No collections found."),
        None => {
            let rows: Vec<Vec<String>> = all
                .iter()
                .map(|k| vec![k.id.clone(), k.name.clone(), k.files().len().to_string()])
                .collect();
            print_table(&["COLLECTION ID", "NAME", "FILES"], &rows);
        }
        Some(key) => {
            let one = kb::find(&all, key)?;
            if one.files().is_empty() {
                eprintln!("No files in '{}'.", one.name);
            } else {
                let rows: Vec<Vec<String>> = one
                    .files()
                    .iter()
                    .map(|f| vec![f.id.clone(), f.meta.name.clone()])
                    .collect();
                print_table(&["FILE ID", "NAME"], &rows);
            }
        }
    }
    Ok(())
}

/// `monocle knowledge delete COLLECTION` — the collection and its files.
pub fn knowledge_delete_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    collection: &str,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, selection)?;
    let all = kb::list_knowledge(client, &url, &bearer, true)?;
    let target = kb::find(&all, collection)?;
    let n = kb::delete_knowledge_and_files(client, &url, &bearer, target)?;
    eprintln!(
        "Deleted collection '{}' ({}) and {n} file(s).",
        target.name, target.id
    );
    Ok(())
}
