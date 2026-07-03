use monocle_cli::oidc::{
    generate_code_challenge, generate_code_verifier, generate_state, resolve_stark_domain,
};

#[test]
fn code_verifier_has_requested_length() {
    assert_eq!(generate_code_verifier(64).unwrap().len(), 64);
    assert_eq!(generate_code_verifier(128).unwrap().len(), 128);
}

#[test]
fn code_verifier_rejects_out_of_range() {
    assert!(generate_code_verifier(42)
        .unwrap_err()
        .to_string()
        .contains("between 43 and 128"));
    assert!(generate_code_verifier(129)
        .unwrap_err()
        .to_string()
        .contains("between 43 and 128"));
}

#[test]
fn code_verifier_uses_only_unreserved_chars() {
    let v = generate_code_verifier(128).unwrap();
    assert!(v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')));
}

#[test]
fn code_challenge_matches_known_s256_vector() {
    // RFC 7636 appendix B test vector.
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = generate_code_challenge(verifier);
    assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    assert!(!challenge.contains('=') && !challenge.contains('+') && !challenge.contains('/'));
}

#[test]
fn state_values_are_unique_and_nonempty() {
    let a = generate_state();
    let b = generate_state();
    assert_ne!(a, b);
    assert!(!a.is_empty());
}

#[test]
fn resolves_stg_tenant_to_stg_stark_domain() {
    assert_eq!(
        resolve_stark_domain("stg-warmblood091803.monocle-ai.com").unwrap(),
        "stg.monocle-ai.com"
    );
}

#[test]
fn resolves_production_tenant_to_base_domain() {
    assert_eq!(
        resolve_stark_domain("warmblood.monocle-ai.com").unwrap(),
        "monocle-ai.com"
    );
}

#[test]
fn rejects_bare_domain() {
    let err = resolve_stark_domain("monocle-ai.com")
        .unwrap_err()
        .to_string();
    assert!(err.contains("Invalid tenant domain"));
    assert!(err.contains("Tenant must be a subdomain"));
}

#[test]
fn preserves_localhost_and_loopback() {
    assert_eq!(
        resolve_stark_domain("localhost:8080").unwrap(),
        "localhost:8080"
    );
    assert_eq!(
        resolve_stark_domain("127.0.0.1:8080").unwrap(),
        "127.0.0.1:8080"
    );
}
