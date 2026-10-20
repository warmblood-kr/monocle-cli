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

/// The one "not logged in" text: `--profile` appears only when non-default.
pub fn not_logged_in_message(profile: &str, env: &str) -> String {
    let flag = if profile == "default" {
        String::new()
    } else {
        format!(" --profile {profile}")
    };
    format!("Not logged in (profile {profile}, env {env}). Run: monocle login{flag} --env {env}")
}

#[cfg(test)]
mod not_logged_in_tests {
    use super::*;

    #[test]
    fn profile_flag_only_when_non_default() {
        assert_eq!(
            not_logged_in_message("default", "stg"),
            "Not logged in (profile default, env stg). Run: monocle login --env stg"
        );
        assert_eq!(
            not_logged_in_message("work", "prod"),
            "Not logged in (profile work, env prod). Run: monocle login --profile work --env prod"
        );
    }

    #[test]
    fn unset_slot_means_default_prod() {
        assert_eq!(
            Credentials::with_home("/nonexistent").not_logged_in(),
            not_logged_in_message("default", "prod")
        );
    }
}

#[derive(Clone)]
pub struct Credentials {
    home: PathBuf,
    profile: Option<String>,
    environment: Option<String>,
    legacy_path: bool,
}

impl Default for Credentials {
    fn default() -> Self {
        Self::new()
    }
}

impl Credentials {
    pub fn new() -> Self {
        Self {
            home: home_dir(),
            profile: None,
            environment: None,
            legacy_path: true,
        }
    }

    /// Test/seam constructor: root the `.monocle` dir at an arbitrary base.
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            profile: None,
            environment: None,
            legacy_path: true,
        }
    }

    /// Construct an isolated profile/environment credential slot. Only the
    /// legacy default-profile slot uses the TypeScript-compatible file path.
    pub fn for_profile(
        home: impl Into<PathBuf>,
        profile: impl Into<String>,
        environment: impl Into<String>,
        legacy_path: bool,
    ) -> Self {
        Self {
            home: home.into(),
            profile: Some(profile.into()),
            environment: Some(environment.into()),
            legacy_path,
        }
    }

    /// [`not_logged_in_message`] for this slot (unset profile/env mean default/prod).
    pub fn not_logged_in(&self) -> String {
        not_logged_in_message(
            self.profile.as_deref().unwrap_or("default"),
            self.environment.as_deref().unwrap_or("prod"),
        )
    }

    pub fn dir(&self) -> PathBuf {
        match self.profile.as_deref() {
            None => self.home.join(".monocle"),
            Some(_) if self.legacy_path => self.home.join(".monocle"),
            Some(profile) => self.home.join(".monocle").join("profiles").join(profile),
        }
    }

    pub fn path(&self) -> PathBuf {
        if self.profile.is_none() || self.legacy_path {
            self.dir().join("credentials.json")
        } else {
            self.dir().join(format!(
                "credentials.{}.json",
                self.environment.as_deref().unwrap_or("prod")
            ))
        }
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
        #[cfg(unix)]
        if self.profile.is_some() && !self.legacy_path {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        if fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(crate::error::AppError::new(
                "Refusing to write credentials through a symbolic link.",
            ));
        }
        if path.exists() {
            set_mode_600(&path)?;
        }
        let json = serde_json::to_string_pretty(data)?;
        write_credentials(&path, json.as_bytes())?;
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

    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    pub fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }

    /// The `apiKeyHelper` command `setup` writes. The default profile's
    /// legacy slot keeps the pre-profiles short form so existing Claude Code
    /// setups stay valid; other slots pin profile and environment.
    pub fn token_helper(&self) -> String {
        match self.pinned_token_helper() {
            Some(pinned)
                if !(self.legacy_path
                    && self.profile.as_deref() == Some(crate::profiles::DEFAULT_PROFILE)) =>
            {
                pinned
            }
            _ => "monocle token".to_string(),
        }
    }

    /// Whether an existing `apiKeyHelper` targets this slot: the current
    /// form or the long pinned form both count.
    pub fn accepts_token_helper(&self, helper: &str) -> bool {
        helper == self.token_helper() || Some(helper.to_string()) == self.pinned_token_helper()
    }

    fn pinned_token_helper(&self) -> Option<String> {
        let (profile, environment) = (self.profile.as_ref()?, self.environment.as_ref()?);
        Some(format!(
            "monocle --profile {profile} --env {environment} token"
        ))
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

#[cfg(unix)]
fn write_credentials(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_credentials(path: &Path, contents: &[u8]) -> Result<()> {
    fs::write(path, contents)?;
    Ok(())
}
