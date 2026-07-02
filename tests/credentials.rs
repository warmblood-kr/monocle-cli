use monocle_cli::credentials::{Credentials, CredentialsData};

fn sample() -> CredentialsData {
    CredentialsData {
        tenant_domain: "test.stark.com".into(),
        tenant_name: "Test Org".into(),
        email: "user@test.com".into(),
        access_token: "at_123".into(),
        refresh_token: "rt_456".into(),
        id_token: "idt_789".into(),
        access_token_expires_at: "2025-01-01T01:00:00.000Z".into(),
        refresh_token_expires_at: "2025-01-31T00:00:00.000Z".into(),
        router_url: None,
    }
}

/// Exactly what the TypeScript CLI wrote: 2-space pretty, these keys in this
/// order, `router_url` omitted, no trailing newline. The drop-in contract.
const TS_FIXTURE: &str = r#"{
  "tenant_domain": "test.stark.com",
  "tenant_name": "Test Org",
  "email": "user@test.com",
  "access_token": "at_123",
  "refresh_token": "rt_456",
  "id_token": "idt_789",
  "access_token_expires_at": "2025-01-01T01:00:00.000Z",
  "refresh_token_expires_at": "2025-01-31T00:00:00.000Z"
}"#;

#[test]
fn read_returns_none_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    assert!(creds.read().is_none());
}

#[test]
fn write_then_read_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    creds.write(&sample()).unwrap();
    assert_eq!(creds.read().unwrap(), sample());
}

#[test]
fn path_is_dot_monocle_credentials_json() {
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    assert_eq!(
        creds.path(),
        dir.path().join(".monocle").join("credentials.json")
    );
}

#[cfg(unix)]
#[test]
fn write_applies_mode_600() {
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    creds.write(&sample()).unwrap();
    assert_eq!(creds.file_mode(), Some(0o600));
}

#[test]
fn read_returns_none_on_bad_json() {
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    std::fs::create_dir_all(creds.dir()).unwrap();
    std::fs::write(creds.path(), "not json{{{").unwrap();
    assert!(creds.read().is_none());
}

#[test]
fn delete_removes_file() {
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    creds.write(&sample()).unwrap();
    assert!(creds.path().exists());
    creds.delete();
    assert!(!creds.path().exists());
}

#[test]
fn serialization_byte_matches_ts_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    creds.write(&sample()).unwrap();
    assert_eq!(std::fs::read_to_string(creds.path()).unwrap(), TS_FIXTURE);
}

#[test]
fn reads_credentials_written_by_typescript_cli() {
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    std::fs::create_dir_all(creds.dir()).unwrap();
    std::fs::write(creds.path(), TS_FIXTURE).unwrap();
    // Existing users must not have to re-auth.
    assert_eq!(creds.read().unwrap(), sample());
}
