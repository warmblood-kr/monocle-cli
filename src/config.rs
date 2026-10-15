//! Persisted user-preference config — `~/.monocle/config.json`.
//!
//! Unlike `credentials.rs`, this file carries no backward-compat contract
//! with the TypeScript CLI (it's new). Best-effort, same posture as
//! `diag.rs`'s log file: a missing or unparseable file just means defaults,
//! never a hard error.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::util::home_dir;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ConfigData {
    /// `monocle chat`'s `/diag on`/`/diag off` — when true, every turn's
    /// diagnostics (see `commands::repl::format_diag`) print automatically
    /// instead of only on demand via `/diag`.
    #[serde(default)]
    pub diag_always_on: bool,
    /// Default connection selected by `monocle profile use`.
    #[serde(default)]
    pub default_profile: Option<String>,
    /// Per-profile non-secret connection settings.
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileData>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProfileData {
    #[serde(default)]
    pub environment: Option<String>,
}

pub struct Config {
    home: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
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
        self.dir().join("config.json")
    }

    /// A missing or unparseable file yields `ConfigData::default()` — this is
    /// a best-effort preference store, not the locked-schema
    /// `credentials.json`, so a corrupt file degrades gracefully rather than
    /// blocking the REPL from starting. Warns to stderr on a parse failure so
    /// the degraded state isn't silent.
    pub fn read(&self) -> ConfigData {
        let path = self.path();
        if !path.exists() {
            return ConfigData::default();
        }
        match fs::read_to_string(&path).and_then(|c| {
            serde_json::from_str::<ConfigData>(&c)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("Warning: Failed to read config: {e}");
                ConfigData::default()
            }
        }
    }

    /// Strict read for connection selection. Falling back to a default after
    /// malformed profile config could silently route credentials to the wrong
    /// tenant, so profile resolution must surface parse errors.
    pub fn read_strict(&self) -> Result<ConfigData> {
        let path = self.path();
        if !path.exists() {
            return Ok(ConfigData::default());
        }
        let contents = fs::read_to_string(&path)
            .map_err(|e| crate::error::AppError::new(format!("Failed to read config: {e}")))?;
        serde_json::from_str::<ConfigData>(&contents)
            .map_err(|e| crate::error::AppError::new(format!("Invalid config: {e}")))
    }

    pub fn write(&self, data: &ConfigData) -> Result<()> {
        let dir = self.dir();
        let path = self.path();
        fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(data)?;
        fs::write(&path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_missing_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::with_home(dir.path());
        assert_eq!(cfg.read(), ConfigData::default());
        assert!(!cfg.read().diag_always_on);
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::with_home(dir.path());
        cfg.write(&ConfigData {
            diag_always_on: true,
            ..ConfigData::default()
        })
        .unwrap();
        assert_eq!(
            cfg.read(),
            ConfigData {
                diag_always_on: true,
                ..ConfigData::default()
            }
        );
    }

    #[test]
    fn read_tolerates_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::with_home(dir.path());
        fs::create_dir_all(cfg.dir()).unwrap();
        fs::write(cfg.path(), b"not json").unwrap();
        assert_eq!(cfg.read(), ConfigData::default());
        assert!(cfg.read_strict().is_err());
    }
}
