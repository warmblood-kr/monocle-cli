use monocle_cli::util::{form_urlencode, parse_iso_ms, parse_query, to_iso};

#[test]
fn to_iso_uses_millisecond_z_format() {
    assert_eq!(to_iso(0), "1970-01-01T00:00:00.000Z");
}

#[test]
fn iso_round_trips() {
    let s = "2025-01-01T01:00:00.000Z";
    assert_eq!(to_iso(parse_iso_ms(s).unwrap()), s);
}

#[test]
fn parse_iso_rejects_garbage() {
    assert!(parse_iso_ms("not a date").is_none());
}

#[test]
fn form_urlencode_matches_urlsearchparams() {
    assert_eq!(
        form_urlencode(&[("scope", "openid profile email")]),
        "scope=openid+profile+email"
    );
    let enc = form_urlencode(&[("redirect_uri", "http://127.0.0.1:8080/oauth/oidc/callback")]);
    assert_eq!(
        enc,
        "redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Foauth%2Foidc%2Fcallback"
    );
}

#[test]
fn parse_query_decodes_values() {
    let q = parse_query("code=abc&state=xy%20z&empty=");
    assert_eq!(q[0], ("code".to_string(), "abc".to_string()));
    assert_eq!(q[1], ("state".to_string(), "xy z".to_string()));
    assert_eq!(q[2], ("empty".to_string(), "".to_string()));
}
