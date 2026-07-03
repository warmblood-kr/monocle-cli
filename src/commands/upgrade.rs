//! `monocle upgrade [--check]` — self-update from GitHub Releases.
//!
//! Downloads the prebuilt binary for the current platform and swaps it in place
//! over the running executable. The platform→asset mapping is the same contract
//! as `install.sh`. All network I/O goes through the `net.rs` facade (the same
//! `client.get` used everywhere else) — both the GitHub API call and the asset
//! download — so no second HTTP client sneaks in.

use std::io::Write;
use std::path::Path;

use serde::Deserialize;

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

/// Whether `latest` is a strictly newer semver than `current`. Any unparseable
/// version is treated as "not newer" (a safe no-op rather than a bad upgrade).
fn is_newer(current: &str, latest: &str) -> bool {
    match (
        semver::Version::parse(current),
        semver::Version::parse(latest),
    ) {
        (Ok(cur), Ok(lat)) => lat > cur,
        _ => false,
    }
}

/// Extract the `monocle` binary from the downloaded archive into `temp_path`.
/// cfg-gated: gzip+tar on unix, zip on windows.
#[cfg(unix)]
fn extract_binary(bytes: &[u8], temp_path: &Path) -> Result<()> {
    use std::io::Read;

    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let is_target = entry
            .path()?
            .file_name()
            .map(|n| n == BINARY)
            .unwrap_or(false);
        if !is_target {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        std::fs::write(temp_path, &buf)?;
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
    use std::io::Read;

    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| AppError::new(e.to_string()))?;
    let target = format!("{BINARY}.exe");
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| AppError::new(e.to_string()))?;
        let matches = Path::new(file.name())
            .file_name()
            .map(|n| n == target.as_str())
            .unwrap_or(false);
        if !matches {
            continue;
        }
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        std::fs::write(temp_path, &buf)?;
        return Ok(());
    }
    Err(AppError::new(format!(
        "archive did not contain a `{target}` binary"
    )))
}

pub fn upgrade_command(client: &Client, check_only: bool) -> Result<()> {
    let platform = asset_platform(std::env::consts::OS, std::env::consts::ARCH)?;
    let current = env!("CARGO_PKG_VERSION");

    eprintln!("Checking for updates ({platform})...");

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

    if check_only {
        let mut out = std::io::stdout();
        writeln!(out, "current: v{current} / latest: v{latest}")?;
        if is_newer(current, &latest) {
            writeln!(out, "An upgrade is available: run `monocle upgrade`.")?;
        } else {
            writeln!(out, "You are on the latest version.")?;
        }
        return Ok(());
    }

    if !is_newer(current, &latest) {
        println!("Already on the latest version (v{current}).");
        return Ok(());
    }

    let asset = asset_filename(platform);
    let url = download_url(&tag, &asset);
    eprintln!("Downloading {asset} (v{latest})...");

    let resp = client.get(&url, &[("User-Agent", "monocle-cli")])?;
    if !resp.ok() {
        return Err(AppError::new(format!(
            "download failed {}: {}",
            resp.status, url
        )));
    }
    let bytes = resp.bytes();

    let temp_name = if cfg!(windows) {
        format!("monocle-upgrade-{}.exe", std::process::id())
    } else {
        format!("monocle-upgrade-{}", std::process::id())
    };
    let temp_path = std::env::temp_dir().join(temp_name);

    eprintln!("Extracting...");
    extract_binary(bytes, &temp_path)?;

    eprintln!("Replacing the running binary...");
    self_replace::self_replace(&temp_path).map_err(|e| AppError::new(e.to_string()))?;
    let _ = std::fs::remove_file(&temp_path);

    let location = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "the current executable".to_string());
    println!("Upgraded monocle v{current} → v{latest} ({location}).");
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
    fn is_newer_compares_semver() {
        assert!(!is_newer("1.1.0", "1.1.0"));
        assert!(is_newer("1.1.0", "1.2.0"));
        assert!(!is_newer("1.1.0", "1.0.9"));
        assert!(is_newer("1.1.0", "2.0.0"));
    }

    #[test]
    fn is_newer_is_false_for_garbage() {
        assert!(!is_newer("not-a-version", "1.0.0"));
        assert!(!is_newer("1.0.0", "not-a-version"));
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
}
