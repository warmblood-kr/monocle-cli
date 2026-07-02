//! Credential storage — the one true backward-compat surface.
//!
//! The on-disk format MUST stay byte-compatible with what the TypeScript CLI
//! wrote: `~/.monocle/credentials.json`, file mode 0600, JSON with these exact
//! snake_case keys in this order, ISO-8601 (`...Z`, millisecond) expiry strings,
//! and `router_url` omitted when absent. Existing users must not have to re-auth.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::util::home_dir;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialsData {
    pub tenant_domain: String,
    pub tenant_name: String,
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    /// ISO 8601
    pub access_token_expires_at: String,
    /// ISO 8601
    pub refresh_token_expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub router_url: Option<String>,
}

pub struct Credentials {
    home: PathBuf,
}

impl Default for Credentials {
    fn default() -> Self {
        Self::new()
    }
}

impl Credentials {
    pub fn new() -> Self {
        Self { home: home_dir() }
    }

    /// Test/seam constructor: root the `.monocle` dir at an arbitrary base.
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    pub fn dir(&self) -> PathBuf {
        self.home.join(".monocle")
    }

    pub fn path(&self) -> PathBuf {
        self.dir().join("credentials.json")
    }

    /// Returns `None` if the file is missing or unreadable/unparseable. On a
    /// read/parse failure it prints a warning to stderr (matching the TS CLI).
    pub fn read(&self) -> Option<CredentialsData> {
        let path = self.path();
        if !path.exists() {
            return None;
        }
        match fs::read_to_string(&path).and_then(|c| {
            serde_json::from_str::<CredentialsData>(&c)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }) {
            Ok(data) => Some(data),
            Err(e) => {
                eprintln!("Warning: Failed to read credentials: {e}");
                None
            }
        }
    }

    pub fn write(&self, data: &CredentialsData) -> Result<()> {
        let dir = self.dir();
        let path = self.path();
        fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(data)?;
        fs::write(&path, json)?;
        set_mode_600(&path)?;
        Ok(())
    }

    pub fn delete(&self) {
        let path = self.path();
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
    }

    /// The file's permission bits (`& 0o777`), or `None` if unavailable.
    pub fn file_mode(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(self.path())
                .ok()
                .map(|m| m.permissions().mode() & 0o777)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

#[cfg(unix)]
fn set_mode_600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode_600(_path: &Path) -> Result<()> {
    Ok(())
}
