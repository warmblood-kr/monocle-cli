//! Profile and environment selection for credentials and API calls.

use std::path::Path;

use crate::config::{Config, ConfigData, ProfileData};
use crate::credentials::{Credentials, CredentialsData};
use crate::error::{AppError, Result};

pub const DEFAULT_PROFILE: &str = "default";
const ENVIRONMENTS: &[&str] = &["prod", "stg", "local"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub profile: String,
    pub environment: String,
    pub legacy_path: bool,
}

impl Selection {
    pub fn store(&self, home: &Path) -> Credentials {
        Credentials::for_profile(
            home,
            self.profile.clone(),
            self.environment.clone(),
            self.legacy_path,
        )
    }
}

pub fn validate_profile(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(AppError::new(
            "Invalid profile name. Use 1-64 letters, numbers, dots, underscores, or hyphens.",
        ));
    }
    Ok(())
}

pub fn validate_environment(environment: &str) -> Result<()> {
    if ENVIRONMENTS.contains(&environment) {
        Ok(())
    } else {
        Err(AppError::new(format!(
            "Invalid environment '{environment}'. Choose prod, stg, or local."
        )))
    }
}

pub fn infer_environment(creds: Option<&CredentialsData>) -> Option<&'static str> {
    let domain = &creds?.tenant_domain;
    let host = domain.to_ascii_lowercase();
    if host.starts_with("localhost") || host.starts_with("127.0.0.1") {
        Some("local")
    } else if host.starts_with("stg-") || host.starts_with("stg.") {
        Some("stg")
    } else {
        Some("prod")
    }
}

pub fn resolve(
    config: &Config,
    home: &Path,
    profile_flag: Option<&str>,
    env_flag: Option<&str>,
) -> Result<Selection> {
    let data = config.read_strict()?;
    resolve_from_data(
        &data,
        home,
        profile_flag,
        std::env::var("MONOCLE_PROFILE").ok().as_deref(),
        env_flag,
    )
}

pub fn resolve_from_data(
    data: &ConfigData,
    home: &Path,
    profile_flag: Option<&str>,
    profile_env: Option<&str>,
    env_flag: Option<&str>,
) -> Result<Selection> {
    let profile = profile_flag
        .or(profile_env)
        .or(data.default_profile.as_deref())
        .unwrap_or(DEFAULT_PROFILE);
    validate_profile(profile)?;
    if let Some(default) = data.default_profile.as_deref() {
        validate_profile(default)?;
    }

    let legacy = Credentials::with_home(home).read();
    let saved_environment = data
        .profiles
        .get(profile)
        .and_then(|settings| settings.environment.as_deref());
    if let Some(environment) = saved_environment {
        validate_environment(environment)?;
    }
    let environment = env_flag
        .or(saved_environment)
        .or_else(|| {
            (profile == DEFAULT_PROFILE)
                .then(|| infer_environment(legacy.as_ref()).unwrap_or("prod"))
        })
        .unwrap_or("prod");
    validate_environment(environment)?;

    // Preserve the TypeScript-compatible file for the current legacy session.
    // A new default-profile prod login also creates that same file.
    let legacy_path = profile == DEFAULT_PROFILE
        && ((legacy.is_none() && environment == "prod")
            || (legacy.is_some() && infer_environment(legacy.as_ref()) == Some(environment)));
    Ok(Selection {
        profile: profile.to_string(),
        environment: environment.to_string(),
        legacy_path,
    })
}

pub fn save_environment(config: &Config, profile: &str, environment: &str) -> Result<()> {
    validate_profile(profile)?;
    validate_environment(environment)?;
    let mut data = config.read_strict()?;
    data.profiles
        .entry(profile.to_string())
        .or_insert_with(ProfileData::default)
        .environment = Some(environment.to_string());
    config.write(&data)
}

pub fn use_profile(config: &Config, profile: &str) -> Result<()> {
    validate_profile(profile)?;
    let mut data = config.read_strict()?;
    if profile != DEFAULT_PROFILE && !data.profiles.contains_key(profile) {
        return Err(AppError::new(format!(
            "Unknown profile '{profile}'. Log in with --profile {profile} first."
        )));
    }
    data.default_profile = Some(profile.to_string());
    config.write(&data)
}

pub fn list_profiles(config: &Config) -> Result<ConfigData> {
    config.read_strict()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn data() -> ConfigData {
        ConfigData {
            default_profile: Some("warmblood".into()),
            profiles: BTreeMap::from([
                (
                    "warmblood".into(),
                    ProfileData {
                        environment: Some("prod".into()),
                    },
                ),
                (
                    "warmblood-stg".into(),
                    ProfileData {
                        environment: Some("stg".into()),
                    },
                ),
            ]),
            ..ConfigData::default()
        }
    }

    #[test]
    fn profile_precedence_and_saved_environment() {
        let home = tempfile::tempdir().unwrap();
        let selected =
            resolve_from_data(&data(), home.path(), Some("warmblood-stg"), None, None).unwrap();
        assert_eq!(selected.profile, "warmblood-stg");
        assert_eq!(selected.environment, "stg");
        assert!(!selected.legacy_path);
    }

    #[test]
    fn explicit_environment_selects_a_separate_slot() {
        let home = tempfile::tempdir().unwrap();
        let selected =
            resolve_from_data(&data(), home.path(), Some("warmblood"), None, Some("stg")).unwrap();
        assert_eq!(selected.environment, "stg");
        assert!(!selected.legacy_path);
    }

    #[test]
    fn malformed_profile_and_environment_names_fail_closed() {
        assert!(validate_profile("../escape").is_err());
        assert!(validate_environment("production").is_err());
    }

    #[test]
    fn legacy_credentials_keep_their_inferred_environment_without_migration() {
        let home = tempfile::tempdir().unwrap();
        let legacy = Credentials::with_home(home.path());
        let mut data = CredentialsData {
            tenant_domain: "stg-warmblood.monocle-ai.com".into(),
            tenant_name: "Warmblood".into(),
            email: "user@example.com".into(),
            access_token: "access".into(),
            refresh_token: "refresh".into(),
            id_token: "id".into(),
            access_token_expires_at: "2099-01-01T00:00:00.000Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00.000Z".into(),
            router_url: None,
        };
        legacy.write(&data).unwrap();
        let selection =
            resolve_from_data(&ConfigData::default(), home.path(), None, None, None).unwrap();
        assert_eq!(selection.environment, "stg");
        assert!(selection.legacy_path);
        data.tenant_domain = "warmblood.monocle-ai.com".into();
        legacy.write(&data).unwrap();
        let explicit =
            resolve_from_data(&ConfigData::default(), home.path(), None, None, Some("stg"))
                .unwrap();
        assert_eq!(explicit.environment, "stg");
        assert!(!explicit.legacy_path);
        assert!(legacy.path().exists());
    }

    #[test]
    fn command_profile_beats_environment_and_config_defaults() {
        let home = tempfile::tempdir().unwrap();
        let selected = resolve_from_data(
            &data(),
            home.path(),
            Some("warmblood-stg"),
            Some("unknown"),
            None,
        )
        .unwrap();
        assert_eq!(selected.profile, "warmblood-stg");
        assert_eq!(selected.environment, "stg");
    }
}
