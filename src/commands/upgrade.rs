//! `monocle upgrade [--check]` — self-update from GitHub Releases.
//!
//! Downloads the prebuilt binary for the current platform and swaps it in place
//! over the running executable. The platform→asset mapping is the same contract
//! as `install.sh`. All network I/O goes through the `net.rs` facade — the
//! GitHub API call via `client.get`, and the (potentially large) asset download
//! via `client.get_download` (a generous-timeout path that won't inherit the
//! shared API timeout) — so no second HTTP client sneaks in.

use std::io::Write;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{AppError, Result};
use crate::net::Client;

const REPO: &str = "warmblood-kr/monocle-cli";
const BINARY: &str = "monocle";

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
}

/// Map (`std::env::consts::OS`, `std::env::consts::ARCH`) → the release asset
/// platform string, matching `install.sh` exactly. macOS Intel is intentionally
/// unsupported (no prebuilt binary is shipped).
fn asset_platform(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Err(AppError::new(
            "macOS Intel is not shipped as a prebuilt binary — build from source",
        )),
        ("linux", "x86_64") => Ok("linux-x64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        ("windows", "x86_64") => Ok("windows-x64"),
        _ => Err(AppError::new(format!("unsupported platform: {os}/{arch}"))),
    }
}

/// The release asset filename for a platform: `.tar.gz` on unix, `.zip` on
/// windows (matches `install.sh`).
fn asset_filename(platform: &str) -> String {
    if platform.starts_with("windows") {
        format!("{BINARY}-{platform}.zip")
    } else {
        format!("{BINARY}-{platform}.tar.gz")
    }
}

/// The GitHub Releases download URL for `<vTAG>/<asset>`.
fn download_url(tag: &str, asset: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{tag}/{asset}")
}

/// Parse a `sha256sum`-style SHA256SUMS listing and find the hash for
/// `asset`. Tolerates the optional `*` binary-mode marker some tools prefix
/// onto the filename column.
fn find_checksum<'a>(sums: &'a str, asset: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == asset).then_some(hash)
    })
}

/// Fetch `SHA256SUMS` from the same release tag and verify `bytes` (the
/// already-downloaded asset) against its entry. Soft-skips (warns, returns
/// `Ok`) if the sums file can't be fetched or has no matching entry — old
/// releases predate this and must still be installable. An actual mismatch
/// is a hard error.
fn verify_checksum(client: &Client, tag: &str, asset: &str, bytes: &[u8]) -> Result<()> {
    let url = download_url(tag, "SHA256SUMS");
    let resp = match client.get(&url, &[("User-Agent", "monocle-cli")]) {
        Ok(r) if r.ok() => r,
        _ => {
            eprintln!("⚠ SHA256SUMS not available, skipping checksum verification");
            return Ok(());
        }
    };
    let sums = resp.text();
    let Some(expected) = find_checksum(&sums, asset) else {
        eprintln!("⚠ no checksum entry for {asset}, skipping checksum verification");
        return Ok(());
    };
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        return Err(AppError::new(format!(
            "checksum verification failed for {asset}: expected {expected}, got {actual}"
        )));
    }
    eprintln!("Checksum verified: {asset}");
    Ok(())
}

/// Compare `current` against `latest` as semver. Returns `None` if EITHER string
/// fails to parse — so the caller can distinguish "definitely not newer" from
/// "can't tell" (e.g. a non-semver release tag) instead of silently no-op'ing.
/// `Ordering::Less` means `current < latest` (an upgrade is available).
fn compare_versions(current: &str, latest: &str) -> Option<std::cmp::Ordering> {
    match (
        semver::Version::parse(current),
        semver::Version::parse(latest),
    ) {
        (Ok(cur), Ok(lat)) => Some(cur.cmp(&lat)),
        _ => None,
    }
}

/// Extract the `monocle` binary from the downloaded archive into `temp_path`.
/// cfg-gated: gzip+tar on unix, zip on windows.
#[cfg(unix)]
fn extract_binary(bytes: &[u8], temp_path: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        // Accept only a *regular file* named `monocle` — never a directory entry
        // or other odd layout, which would otherwise yield a 0-byte install.
        let is_file = entry.header().entry_type().is_file();
        let is_named = entry
            .path()?
            .file_name()
            .map(|n| n == BINARY)
            .unwrap_or(false);
        if !(is_file && is_named) {
            continue;
        }
        // Stream the entry straight to disk (no second full-size in-memory copy).
        let mut out = std::fs::File::create(temp_path)?;
        std::io::copy(&mut entry, &mut out)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp_path, std::fs::Permissions::from_mode(0o755))?;
        return Ok(());
    }
    Err(AppError::new(format!(
        "archive did not contain a `{BINARY}` binary"
    )))
}

/// Extract the `monocle.exe` binary from the downloaded zip into `temp_path`.
#[cfg(windows)]
fn extract_binary(bytes: &[u8], temp_path: &Path) -> Result<()> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| AppError::new(e.to_string()))?;
    let target = format!("{BINARY}.exe");
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| AppError::new(e.to_string()))?;
        // Accept only a *file* entry named `monocle.exe` — skip directory entries
        // so an odd archive layout can't produce a 0-byte install.
        if file.is_dir() {
            continue;
        }
        let matches = Path::new(file.name())
            .file_name()
            .map(|n| n == target.as_str())
            .unwrap_or(false);
        if !matches {
            continue;
        }
        // Stream the entry straight to disk (no second full-size in-memory copy).
        let mut out = std::fs::File::create(temp_path)?;
        std::io::copy(&mut file, &mut out)?;
        return Ok(());
    }
    Err(AppError::new(format!(
        "archive did not contain a `{target}` binary"
    )))
}

/// Map a `self_replace` failure to an actionable message. A permission error
/// almost always means the binary lives in a root-owned, system-wide install dir
/// (e.g. install.sh's sudo fallback to `/usr/local/bin`), so tell the user how to
/// recover instead of surfacing a raw `Permission denied (os error 13)`.
fn map_self_replace_error(e: std::io::Error) -> AppError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "the current executable".to_string());
        AppError::new(format!(
            "permission denied replacing {exe} — it looks installed system-wide. \
             Re-run with elevated privileges (e.g. sudo) or reinstall to a \
             user-writable location. ({e})"
        ))
    } else {
        AppError::new(e.to_string())
    }
}

pub fn upgrade_command(client: &Client, check_only: bool) -> Result<()> {
    use std::cmp::Ordering;

    let current = env!("CARGO_PKG_VERSION");

    // Checking for updates needs no downloadable asset, so it must work on every
    // platform (incl. unsupported ones like macOS Intel). `asset_platform()` is
    // only consulted later, on the actual download path.
    eprintln!("Checking for updates...");

    let resp = client.get(
        &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        &[
            ("User-Agent", "monocle-cli"),
            ("Accept", "application/vnd.github+json"),
        ],
    )?;
    if !resp.ok() {
        return Err(AppError::new(format!(
            "GitHub API error {}: {}",
            resp.status,
            resp.text()
        )));
    }
    let tag = resp.json::<LatestRelease>()?.tag_name;
    let latest = tag.strip_prefix('v').unwrap_or(&tag).to_string();

    let ordering = compare_versions(current, &latest);

    if check_only {
        // Data lines (machine-parseable) go to stdout; prose to stderr.
        let mut out = std::io::stdout();
        match ordering {
            Some(Ordering::Less) => {
                writeln!(
                    out,
                    "current=v{current} latest=v{latest} upgrade_available=true"
                )?;
                eprintln!("An upgrade is available: run `monocle upgrade`.");
            }
            Some(_) => {
                writeln!(
                    out,
                    "current=v{current} latest=v{latest} upgrade_available=false"
                )?;
                eprintln!("You are on the latest version.");
            }
            None => {
                writeln!(
                    out,
                    "current=v{current} latest=v{latest} upgrade_available=unknown"
                )?;
                eprintln!(
                    "⚠ could not parse release tag '{latest}'; unable to determine whether an update is available."
                );
            }
        }
        return Ok(());
    }

    match ordering {
        None => {
            // Don't falsely claim up-to-date and don't blindly replace on an
            // unparseable tag — warn and abort.
            eprintln!("⚠ could not parse release tag '{latest}'; unable to determine whether an update is available.");
            return Err(AppError::new(format!(
                "could not parse release tag '{latest}' as semver"
            )));
        }
        Some(Ordering::Less) => { /* an upgrade is available — proceed */ }
        Some(_) => {
            eprintln!("Already on the latest version (v{current}).");
            return Ok(());
        }
    }

    // Only now — on the actual download path — do we need a downloadable asset,
    // so this is where an unsupported platform legitimately errors out.
    let platform = asset_platform(std::env::consts::OS, std::env::consts::ARCH)?;
    let asset = asset_filename(platform);
    let url = download_url(&tag, &asset);
    eprintln!("Downloading {asset} (v{latest})...");

    // Use the dedicated download path: a multi-MB binary on a slow link must not
    // inherit the shared `send()`'s short total API timeout.
    let resp = client.get_download(&url, &[("User-Agent", "monocle-cli")])?;
    if !resp.ok() {
        return Err(AppError::new(format!(
            "download failed {}: {}",
            resp.status, url
        )));
    }
    let bytes = resp.bytes();
    verify_checksum(client, &tag, &asset, bytes)?;

    let temp_name = if cfg!(windows) {
        format!("monocle-upgrade-{}.exe", std::process::id())
    } else {
        format!("monocle-upgrade-{}", std::process::id())
    };
    let temp_path = std::env::temp_dir().join(temp_name);

    eprintln!("Extracting...");
    extract_binary(bytes, &temp_path)?;

    // Safety gate: never hand self_replace an empty/missing file — that would
    // brick the CLI (install a 0-byte binary) while printing success.
    let valid = std::fs::metadata(&temp_path)
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    if !valid {
        let _ = std::fs::remove_file(&temp_path);
        return Err(AppError::new(
            "downloaded archive did not contain a valid monocle binary",
        ));
    }

    eprintln!("Replacing the running binary...");
    if let Err(e) = self_replace::self_replace(&temp_path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(map_self_replace_error(e));
    }
    let _ = std::fs::remove_file(&temp_path);

    let location = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "the current executable".to_string());
    eprintln!("Upgraded monocle v{current} → v{latest} ({location}).");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_platform_maps_supported_targets() {
        assert_eq!(asset_platform("macos", "aarch64").unwrap(), "macos-arm64");
        assert_eq!(asset_platform("linux", "x86_64").unwrap(), "linux-x64");
        assert_eq!(asset_platform("linux", "aarch64").unwrap(), "linux-arm64");
        assert_eq!(asset_platform("windows", "x86_64").unwrap(), "windows-x64");
    }

    #[test]
    fn asset_platform_rejects_macos_intel() {
        let err = asset_platform("macos", "x86_64").unwrap_err();
        assert!(
            err.to_string().contains("macOS Intel"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn asset_platform_rejects_unknown() {
        assert!(asset_platform("linux", "riscv64").is_err());
        assert!(asset_platform("freebsd", "x86_64").is_err());
    }

    #[test]
    fn find_checksum_matches_exact_filename() {
        let sums = "abc123  monocle-linux-x64.tar.gz\ndef456  monocle-macos-arm64.tar.gz\n";
        assert_eq!(
            find_checksum(sums, "monocle-linux-x64.tar.gz"),
            Some("abc123")
        );
    }

    #[test]
    fn find_checksum_handles_binary_mode_marker() {
        let sums = "abc123 *monocle-linux-x64.tar.gz\n";
        assert_eq!(
            find_checksum(sums, "monocle-linux-x64.tar.gz"),
            Some("abc123")
        );
    }

    #[test]
    fn find_checksum_none_when_missing() {
        let sums = "abc123  monocle-linux-x64.tar.gz\n";
        assert_eq!(find_checksum(sums, "monocle-windows-x64.zip"), None);
    }

    #[test]
    fn compare_versions_orders_semver() {
        use std::cmp::Ordering;
        // Equal.
        assert_eq!(compare_versions("1.1.0", "1.1.0"), Some(Ordering::Equal));
        // current < latest → an upgrade is available.
        assert_eq!(compare_versions("1.1.0", "1.2.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("1.1.0", "2.0.0"), Some(Ordering::Less));
        // current > latest → already ahead.
        assert_eq!(compare_versions("1.1.0", "1.0.9"), Some(Ordering::Greater));
    }

    #[test]
    fn compare_versions_is_none_for_garbage() {
        // Either side unparseable → None (can't tell), never a silent no-op.
        assert_eq!(compare_versions("not-a-version", "1.0.0"), None);
        assert_eq!(compare_versions("1.0.0", "not-a-version"), None);
        assert_eq!(compare_versions("1.0.0", "latest"), None);
    }

    #[test]
    fn asset_filename_matches_install_sh() {
        assert_eq!(asset_filename("linux-x64"), "monocle-linux-x64.tar.gz");
        assert_eq!(asset_filename("macos-arm64"), "monocle-macos-arm64.tar.gz");
        assert_eq!(asset_filename("windows-x64"), "monocle-windows-x64.zip");
    }

    #[test]
    fn download_url_uses_v_prefixed_tag() {
        assert_eq!(
            download_url("v1.2.0", "monocle-linux-x64.tar.gz"),
            "https://github.com/warmblood-kr/monocle-cli/releases/download/v1.2.0/monocle-linux-x64.tar.gz"
        );
    }

    #[cfg(unix)]
    fn make_targz(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut out, flate2::Compression::fast());
            let mut builder = tar::Builder::new(enc);
            for (name, kind, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_path(name).unwrap();
                header.set_size(data.len() as u64);
                header.set_entry_type(*kind);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append(&header, *data).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        }
        out
    }

    #[cfg(unix)]
    #[test]
    fn extract_binary_writes_regular_file() {
        let payload = b"#!/bin/sh\necho hi\n";
        let archive = make_targz(&[("monocle", tar::EntryType::Regular, payload)]);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        extract_binary(&archive, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), payload);
    }

    #[cfg(unix)]
    #[test]
    fn extract_binary_rejects_directory_named_monocle() {
        // A directory entry named `monocle` must NOT be accepted (it would yield a
        // 0-byte install). Extraction must error and write nothing.
        let archive = make_targz(&[("monocle/", tar::EntryType::Directory, b"")]);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let err = extract_binary(&archive, &out).unwrap_err();
        assert!(err.to_string().contains("did not contain"), "got: {err}");
        assert!(
            !out.exists(),
            "no file must be created for a dir-only archive"
        );
    }
}
