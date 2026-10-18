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
    // A corrupt config must not break `token` (Claude Code's apiKeyHelper):
    // warn and fall back to defaults, as before profiles existed. Commands
    // that write config still use `read_strict`, so the file isn't clobbered.
    let data = config.read_strict().unwrap_or_else(|e| {
        eprintln!("Warning: {e}. Using default profile settings.");
        ConfigData::default()
    });
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
    let mut environment = env_flag
        .or(saved_environment)
        .or_else(|| {
            (profile == DEFAULT_PROFILE)
                .then(|| infer_environment(legacy.as_ref()).unwrap_or("prod"))
        })
        .unwrap_or("prod");
    validate_environment(environment)?;
    // The legacy file is the default profile's session. If the saved
    // environment has no slot of its own, use the legacy session rather than
    // reporting "Not logged in".
    if profile == DEFAULT_PROFILE && env_flag.is_none() {
        if let Some(inferred) = infer_environment(legacy.as_ref()) {
            let slot = Credentials::for_profile(home, profile, environment, false);
            if inferred != environment && !slot.path().exists() {
                environment = inferred;
            }
        }
    }

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

pub const CONFIG_VERSION: u32 = 1;

/// One-time, idempotent upgrade of a pre-profiles setup: the legacy
/// session becomes the `default` profile. Backs up the old config to
/// `config.json.pre-profiles` (never overwritten), leaves `credentials.json`
/// in place (it is the default profile's slot), and leaves a corrupt config
/// untouched.
pub fn migrate_legacy(config: &Config, home: &Path) -> Result<()> {
    let path = config.path();
    let original = std::fs::read_to_string(&path).ok();
    let Ok(mut data) = original.as_deref().map_or(
        Ok(ConfigData::default()),
        serde_json::from_str::<ConfigData>,
    ) else {
        return Ok(());
    };
    if data.config_version >= CONFIG_VERSION {
        return Ok(());
    }
    let legacy = Credentials::with_home(home).read();
    if original.is_none() && legacy.is_none() {
        return Ok(()); // fresh install: nothing to migrate
    }
    if let Some(original) = original {
        let backup = config.dir().join("config.json.pre-profiles");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
        {
            Ok(mut file) => std::io::Write::write_all(&mut file, original.as_bytes())?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(environment) = infer_environment(legacy.as_ref()) {
        let default = data
            .profiles
            .entry(DEFAULT_PROFILE.to_string())
            .or_default();
        default
            .environment
            .get_or_insert_with(|| environment.to_string());
    }
    data.config_version = CONFIG_VERSION;
    config.write(&data)
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

    fn write_legacy(home: &Path, tenant: &str) {
        Credentials::with_home(home)
            .write(&CredentialsData {
                tenant_domain: tenant.into(),
                tenant_name: "Warmblood".into(),
                email: "user@example.com".into(),
                access_token: "access".into(),
                refresh_token: "refresh".into(),
                id_token: "id".into(),
                access_token_expires_at: "2099-01-01T00:00:00.000Z".into(),
                refresh_token_expires_at: "2099-01-01T00:00:00.000Z".into(),
                router_url: None,
            })
            .unwrap();
    }

    // Blocker (a): `setup` keeps writing the short helper for the default
    // profile, and `status` accepts both the short and the long form.
    #[test]
    fn default_profile_keeps_short_token_helper_and_accepts_long_form() {
        let home = tempfile::tempdir().unwrap();
        write_legacy(home.path(), "warmblood.monocle-ai.com");
        let store = resolve_from_data(&ConfigData::default(), home.path(), None, None, None)
            .unwrap()
            .store(home.path());
        assert_eq!(store.token_helper(), "monocle token");
        assert!(store.accepts_token_helper("monocle token"));
        assert!(store.accepts_token_helper("monocle --profile default --env prod token"));
        assert!(!store.accepts_token_helper("monocle --profile other --env prod token"));

        let named = resolve_from_data(&data(), home.path(), Some("warmblood-stg"), None, None)
            .unwrap()
            .store(home.path());
        assert_eq!(
            named.token_helper(),
            "monocle --profile warmblood-stg --env stg token"
        );
        assert!(!named.accepts_token_helper("monocle token"));
    }

    // Blocker (b): saved env=prod but the legacy session is stg and no prod
    // slot exists -> use the legacy session instead of "Not logged in".
    #[test]
    fn saved_environment_without_slot_falls_back_to_legacy_session() {
        let home = tempfile::tempdir().unwrap();
        write_legacy(home.path(), "stg-warmblood.monocle-ai.com");
        let mut config = ConfigData::default();
        config.profiles.insert(
            DEFAULT_PROFILE.into(),
            ProfileData {
                environment: Some("prod".into()),
            },
        );
        let selection = resolve_from_data(&config, home.path(), None, None, None).unwrap();
        assert_eq!(selection.environment, "stg");
        assert!(selection.legacy_path);
        assert!(selection.store(home.path()).read().is_some());

        // An explicit --env still selects its own slot.
        let explicit = resolve_from_data(&config, home.path(), None, None, Some("prod")).unwrap();
        assert!(!explicit.legacy_path);
    }

    // Blocker (c): a corrupt config.json must not make `token` fail.
    #[test]
    fn corrupt_config_resolves_to_default_profile() {
        let home = tempfile::tempdir().unwrap();
        write_legacy(home.path(), "warmblood.monocle-ai.com");
        let config = Config::with_home(home.path());
        std::fs::write(config.path(), b"not json").unwrap();
        let selection = resolve(&config, home.path(), None, None).unwrap();
        assert_eq!(selection.profile, DEFAULT_PROFILE);
        assert!(selection.store(home.path()).read().is_some());
        // Migration leaves the corrupt file alone.
        migrate_legacy(&config, home.path()).unwrap();
        assert_eq!(std::fs::read(config.path()).unwrap(), b"not json");
    }

    #[test]
    fn migration_is_one_time_idempotent_and_keeps_backup() {
        let home = tempfile::tempdir().unwrap();
        write_legacy(home.path(), "stg-warmblood.monocle-ai.com");
        let config = Config::with_home(home.path());
        let legacy_config = br#"{"diag_always_on":true}"#;
        std::fs::write(config.path(), legacy_config).unwrap();
        let creds_before = std::fs::read(Credentials::with_home(home.path()).path()).unwrap();

        migrate_legacy(&config, home.path()).unwrap();
        let migrated = config.read_strict().unwrap();
        assert_eq!(migrated.config_version, CONFIG_VERSION);
        assert!(migrated.diag_always_on);
        assert_eq!(
            migrated.profiles[DEFAULT_PROFILE].environment.as_deref(),
            Some("stg")
        );
        let backup = config.dir().join("config.json.pre-profiles");
        assert_eq!(std::fs::read(&backup).unwrap(), legacy_config);
        // Credentials stay byte-identical in place.
        assert_eq!(
            std::fs::read(Credentials::with_home(home.path()).path()).unwrap(),
            creds_before
        );

        // Second run: no-op, backup not overwritten.
        std::fs::write(&backup, b"keep").unwrap();
        migrate_legacy(&config, home.path()).unwrap();
        assert_eq!(config.read_strict().unwrap(), migrated);
        assert_eq!(std::fs::read(&backup).unwrap(), b"keep");

        let selection = resolve(&config, home.path(), None, None).unwrap();
        assert_eq!(selection.environment, "stg");
        assert!(selection.legacy_path);
    }

    #[test]
    fn fresh_install_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let config = Config::with_home(home.path());
        migrate_legacy(&config, home.path()).unwrap();
        assert!(!config.dir().exists());
    }
}
