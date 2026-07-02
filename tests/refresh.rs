use base64::Engine;
use serde_json::Value;

use monocle_cli::refresh::decode_id_token_payload;

fn jwt(payload: &str) -> String {
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
    format!("header.{b}.signature")
}

#[test]
fn decodes_id_token_payload() {
    let token =
        jwt(r#"{"email":"a@b.com","tenant_name":"Org","tenant_domain":"x.monocle-ai.com"}"#);
    let v: Value = decode_id_token_payload(&token).unwrap();
    assert_eq!(v["email"], "a@b.com");
    assert_eq!(v["tenant_name"], "Org");
    assert_eq!(v["tenant_domain"], "x.monocle-ai.com");
}

#[test]
fn rejects_malformed_token() {
    assert!(decode_id_token_payload("only.two").is_err());
    assert!(decode_id_token_payload("a.!!!notbase64!!!.c").is_err());
}
