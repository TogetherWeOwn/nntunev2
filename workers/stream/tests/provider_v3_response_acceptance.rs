//! Independently owned inert L0 contract tests. Parsing is NOT authorization.
//! Oracle: pinned seam S §§2, 6.1, 6.2, 8; see provider-v3-acceptance.md.
//! This handoff is intentionally RED until the portable seam exists. No runtime wiring.

use nntune_stream::provider_response::{
    validate_provider_response, ExpectedProviderResponse, KeyBinding, Provider3Success,
    ProviderOnly,
};
use serde_json::{json, Value};

const OWNER: &str = "user_inert_é";
const REQUEST: &str = "00000000-0000-4000-8000-000000000001";
const LEASE: &str = "00000000-0000-4000-8000-000000000002";
const PROVIDER: &str = "00000000-0000-4000-8000-000000000003";
const SCOPE: &str = "00000000-0000-4000-8000-000000000004";
const OTHER: &str = "00000000-0000-4000-8000-000000000005";
const SCOPE_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTEXT_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const PLAN_DIGEST: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const SAFE_MAX: u64 = 9_007_199_254_740_991;

// Shape-only invented IDs/digests/key labels; no keys, tokens, signatures or cryptographic fixtures.
fn expected() -> ExpectedProviderResponse {
    ExpectedProviderResponse {
        request_id: REQUEST.into(),
        lease_id: LEASE.into(),
        lease_until: 1_000_015,
        pool_owner: OWNER.into(),
        provider_id: PROVIDER.into(),
        provider_version: 1,
        owner_epoch: 2,
        permission_epoch: 3,
        scope_id: SCOPE.into(),
        operation: "nntp.body".into(),
        scope_digest: SCOPE_DIGEST.into(),
        context_digest: CONTEXT_DIGEST.into(),
        plan_digest: PLAN_DIGEST.into(),
        cnf: KeyBinding {
            kid: "inert_stream".into(),
            kv: 4,
        },
        issuer_key: KeyBinding {
            kid: "inert_issuer".into(),
            kv: 5,
        },
    }
}

fn positive() -> Value {
    json!({
        "v":3,"request_id":REQUEST,"lease_id":LEASE,"lease_until":1_000_015,
        "pool_owner":OWNER,"provider_id":PROVIDER,"provider_version":1,
        "owner_epoch":2,"permission_epoch":3,"scope_id":SCOPE,"operation":"nntp.body",
        "provider":{"host":"news.example.invalid","port":563,"tls":true,
            "allow_insecure_plaintext":false,"username":"inert-user","password":"inert-password",
            "effective_max_connections":1},
        "scope_digest":SCOPE_DIGEST,"context_digest":CONTEXT_DIGEST,"plan_digest":PLAN_DIGEST,
        "cnf":{"kid":"inert_stream","kv":4},"issuer_key":{"kid":"inert_issuer","kv":5}
    })
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("serialize inert test input")
}

fn accept(value: &Value, binding: &ExpectedProviderResponse) {
    let Ok(parsed) = validate_provider_response(&bytes(value), binding) else {
        panic!("valid inert shape denied");
    };
    // Inert decoded views, never an authority handle. No output field may be defaulted.
    assert!(parsed.bindings() == binding);
    assert!(parsed.provider().username() == value["provider"]["username"].as_str().unwrap());
    assert!(parsed.provider().password() == value["provider"]["password"].as_str().unwrap());
    assert!(parsed.provider().host() == value["provider"]["host"].as_str().unwrap());
    assert!(u64::from(parsed.provider().port()) == value["provider"]["port"].as_u64().unwrap());
    assert!(parsed.provider().tls() == value["provider"]["tls"].as_bool().unwrap());
    assert!(
        parsed.provider().allow_insecure_plaintext()
            == value["provider"]["allow_insecure_plaintext"]
                .as_bool()
                .unwrap()
    );
    assert!(
        u64::from(parsed.provider().effective_max_connections())
            == value["provider"]["effective_max_connections"]
                .as_u64()
                .unwrap()
    );
}

fn deny_raw(raw: &[u8], binding: &ExpectedProviderResponse) -> String {
    match validate_provider_response(raw, binding) {
        Ok(_) => panic!("invalid inert shape accepted"),
        Err(error) => {
            let rendered = error.to_string();
            // S §8 permits fixed denial/unavailability, never raw parser/provider diagnostics.
            assert!(
                matches!(
                    rendered.as_str(),
                    "authorization_denied" | "provider_unavailable"
                ),
                "static error must be a fixed sanitized code"
            );
            rendered
        }
    }
}

fn deny(value: &Value) {
    deny_raw(&bytes(value), &expected());
}

#[test]
fn accepts_exact_v3_with_native_non_uuid_owner_and_both_operations() {
    let mut value = positive();
    let mut binding = expected();
    accept(&value, &binding);
    value["operation"] = json!("nntp.stat");
    binding.operation = "nntp.stat".into();
    accept(&value, &binding);
}

#[test]
fn every_object_is_recursively_closed_and_required() {
    for pointer in ["", "/provider", "/cnf", "/issuer_key"] {
        let original = positive();
        let keys: Vec<_> = original
            .pointer(pointer)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for key in keys {
            let mut missing = original.clone();
            missing
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(&key);
            deny(&missing);
            let mut null = original.clone();
            null.pointer_mut(pointer).unwrap()[&key] = Value::Null;
            deny(&null);
        }
        let mut extra = original.clone();
        extra.pointer_mut(pointer).unwrap()["unexpected_inert_field"] = json!(true);
        deny(&extra);
    }
    // Legacy/combined fields must never be ignored or adopted.
    for key in [
        "indexer",
        "content_key_b64",
        "owner",
        "context",
        "schema_version",
    ] {
        let mut value = positive();
        value[key] = json!("inert-extra");
        deny(&value);
    }
}

#[test]
fn rejects_v2_and_wrong_scalar_representations() {
    for version in [
        json!(2),
        json!(1),
        json!(4),
        json!("3"),
        json!(true),
        json!([]),
        json!({}),
    ] {
        let mut value = positive();
        value["v"] = version;
        deny(&value);
    }
    for pointer in [
        "/lease_until",
        "/provider_version",
        "/owner_epoch",
        "/permission_epoch",
        "/provider/port",
        "/provider/effective_max_connections",
        "/cnf/kv",
        "/issuer_key/kv",
    ] {
        for bad in [json!("1"), json!(true), json!(-1), json!([]), json!({})] {
            let mut value = positive();
            *value.pointer_mut(pointer).unwrap() = bad;
            deny(&value);
        }
    }
    for pointer in [
        "/request_id",
        "/lease_id",
        "/pool_owner",
        "/provider_id",
        "/scope_id",
        "/operation",
        "/scope_digest",
        "/context_digest",
        "/plan_digest",
        "/cnf/kid",
        "/issuer_key/kid",
        "/provider/host",
        "/provider/username",
        "/provider/password",
    ] {
        for bad in [json!(1), json!(false), json!([]), json!({})] {
            let mut value = positive();
            *value.pointer_mut(pointer).unwrap() = bad;
            deny(&value);
        }
    }
    for pointer in ["/provider", "/cnf", "/issuer_key"] {
        for bad in [
            json!(0),
            json!(false),
            json!("inert"),
            json!([]),
            json!({}),
            Value::Null,
        ] {
            let mut value = positive();
            *value.pointer_mut(pointer).unwrap() = bad;
            deny(&value);
        }
    }
    for pointer in ["/provider/tls", "/provider/allow_insecure_plaintext"] {
        for bad in [json!(0), json!(1), json!("true"), json!([]), json!({})] {
            let mut value = positive();
            *value.pointer_mut(pointer).unwrap() = bad;
            deny(&value);
        }
    }
}

#[test]
fn raw_integer_lexemes_cannot_be_repaired_by_a_json_value_round_trip() {
    let wire = String::from_utf8(bytes(&positive())).unwrap();
    for (key, original) in [
        ("v", "3"),
        ("lease_until", "1000015"),
        ("provider_version", "1"),
        ("owner_epoch", "2"),
        ("permission_epoch", "3"),
        ("port", "563"),
        ("effective_max_connections", "1"),
        ("kv", "4"),
        ("kv", "5"),
    ] {
        let needle = format!("\"{key}\":{original}");
        assert_eq!(wire.matches(&needle).count(), 1);
        for bad in [
            format!("{original}.0"),
            format!("{original}e0"),
            format!("{original}E+0"),
            format!("+{original}"),
            format!("0{original}"),
        ] {
            deny_raw(
                wire.replacen(&needle, &format!("\"{key}\":{bad}"), 1)
                    .as_bytes(),
                &expected(),
            );
        }
    }
    deny_raw(
        wire.replacen("\"lease_until\":1000015", "\"lease_until\":-0", 1)
            .as_bytes(),
        &expected(),
    );
}

#[test]
fn strict_wire_rejects_duplicates_invalid_utf8_bom_surrogates_and_trailing_json() {
    let wire = String::from_utf8(bytes(&positive())).unwrap();
    for (needle, duplicate) in [
        ("\"v\":3", "\"v\":3,\"v\":3"),
        (
            "\"host\":\"news.example.invalid\"",
            "\"host\":\"news.example.invalid\",\"host\":\"news.example.invalid\"",
        ),
        (
            "\"kid\":\"inert_stream\"",
            "\"kid\":\"inert_stream\",\"kid\":\"inert_stream\"",
        ),
        ("\"kv\":5", "\"kv\":5,\"kv\":5"),
    ] {
        assert_eq!(wire.matches(needle).count(), 1);
        deny_raw(wire.replace(needle, duplicate).as_bytes(), &expected());
    }
    for suffix in ["{}", "null", "inert-junk"] {
        deny_raw(format!("{wire}{suffix}").as_bytes(), &expected());
    }
    deny_raw(
        &[b"\xef\xbb\xbf".as_slice(), wire.as_bytes()].concat(),
        &expected(),
    );
    deny_raw(&[wire.as_bytes(), &[0xff]].concat(), &expected());
    let mut invalid_utf8 = wire.as_bytes().to_vec();
    let position = wire.find("inert-password").unwrap();
    invalid_utf8[position] = 0xff;
    deny_raw(&invalid_utf8, &expected());
    let escaped_duplicate = format!("{}{}{}", r#""v":3,""#, char::from(92), r#"u0076":3"#);
    deny_raw(
        wire.replace("\"v\":3", &escaped_duplicate).as_bytes(),
        &expected(),
    );
    for invalid in ["\\ud800", "\\udc00"] {
        deny_raw(
            wire.replace("inert-password", invalid).as_bytes(),
            &expected(),
        );
    }
    for raw in [b"".as_slice(), b"null", b"[]", b"{}"] {
        deny_raw(raw, &expected());
    }
}

#[test]
fn every_supplied_static_binding_is_compared_exactly() {
    for (pointer, different) in [
        ("/request_id", json!(OTHER)),
        ("/lease_id", json!(OTHER)),
        ("/lease_until", json!(1_000_014)),
        ("/pool_owner", json!("other_inert_user")),
        ("/provider_id", json!(OTHER)),
        ("/provider_version", json!(2)),
        ("/owner_epoch", json!(3)),
        ("/permission_epoch", json!(4)),
        ("/scope_id", json!(OTHER)),
        ("/operation", json!("nntp.stat")),
        ("/scope_digest", json!(PLAN_DIGEST)),
        ("/context_digest", json!(PLAN_DIGEST)),
        ("/plan_digest", json!(CONTEXT_DIGEST)),
        ("/cnf/kid", json!("other_stream")),
        ("/cnf/kv", json!(5)),
        ("/issuer_key/kid", json!("other_issuer")),
        ("/issuer_key/kv", json!(6)),
    ] {
        let mut value = positive();
        *value.pointer_mut(pointer).unwrap() = different;
        deny(&value);
    }
}

#[test]
fn native_owner_bounds_and_binary_equality_do_not_coerce_or_normalize() {
    for owner in [
        "user_native".to_string(),
        "é".repeat(128),
        format!("{}a", "é".repeat(127)),
        " user_native ".into(),
        "用户_inert".into(),
    ] {
        let mut value = positive();
        let mut binding = expected();
        value["pool_owner"] = json!(owner);
        binding.pool_owner = owner;
        accept(&value, &binding);
    }
    for owner in [
        "".to_string(),
        "a".repeat(257),
        format!("{}a", "é".repeat(128)),
        "user\0native".into(),
        "user\nnative".into(),
        "user\u{1f}native".into(),
        "user\u{7f}native".into(),
    ] {
        let mut value = positive();
        let mut binding = expected();
        value["pool_owner"] = json!(owner);
        binding.pool_owner = owner;
        deny_raw(&bytes(&value), &binding);
    }
    for owner in [
        "USER_inert_é",
        "user_inert_e\u{301}",
        " user_inert_é",
        "user_inert_é ",
    ] {
        let mut value = positive();
        value["pool_owner"] = json!(owner);
        deny(&value);
    }
}

#[test]
fn malformed_identity_digest_key_and_epoch_are_denied_even_if_expected_agrees() {
    for id in [
        "native_not_uuid",
        "00000000-0000-1000-8000-000000000004",
        "00000000-0000-4000-7000-000000000004",
        "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
    ] {
        for field in ["request_id", "lease_id", "provider_id", "scope_id"] {
            let mut value = positive();
            let mut binding = expected();
            value[field] = json!(id);
            match field {
                "request_id" => binding.request_id = id.into(),
                "lease_id" => binding.lease_id = id.into(),
                "provider_id" => binding.provider_id = id.into(),
                _ => binding.scope_id = id.into(),
            }
            deny_raw(&bytes(&value), &binding);
        }
    }
    for digest in [
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
        "a_".repeat(32),
    ] {
        for field in ["scope_digest", "context_digest", "plan_digest"] {
            let mut value = positive();
            let mut binding = expected();
            value[field] = json!(digest);
            match field {
                "scope_digest" => binding.scope_digest = digest.clone(),
                "context_digest" => binding.context_digest = digest.clone(),
                _ => binding.plan_digest = digest.clone(),
            }
            deny_raw(&bytes(&value), &binding);
        }
    }
    for operation in ["", "connect", "nntp.head", "NNTP.BODY"] {
        let mut value = positive();
        let mut binding = expected();
        value["operation"] = json!(operation);
        binding.operation = operation.into();
        deny_raw(&bytes(&value), &binding);
    }
    for kid in [
        "".to_string(),
        "a".repeat(65),
        "Upper".into(),
        "https://inert.invalid".into(),
        "inert.key".into(),
    ] {
        for field in ["cnf", "issuer_key"] {
            let mut value = positive();
            let mut binding = expected();
            value[field]["kid"] = json!(kid);
            if field == "cnf" {
                binding.cnf.kid = kid.clone();
            } else {
                binding.issuer_key.kid = kid.clone();
            }
            deny_raw(&bytes(&value), &binding);
        }
    }
    for epoch in [0, SAFE_MAX + 1] {
        for field in [
            "provider_version",
            "owner_epoch",
            "permission_epoch",
            "cnf",
            "issuer_key",
        ] {
            let mut value = positive();
            let mut binding = expected();
            match field {
                "provider_version" => {
                    value[field] = json!(epoch);
                    binding.provider_version = epoch;
                }
                "owner_epoch" => {
                    value[field] = json!(epoch);
                    binding.owner_epoch = epoch;
                }
                "permission_epoch" => {
                    value[field] = json!(epoch);
                    binding.permission_epoch = epoch;
                }
                "cnf" => {
                    value[field]["kv"] = json!(epoch);
                    binding.cnf.kv = epoch;
                }
                _ => {
                    value[field]["kv"] = json!(epoch);
                    binding.issuer_key.kv = epoch;
                }
            }
            deny_raw(&bytes(&value), &binding);
        }
    }
    let mut value = positive();
    let mut binding = expected();
    value["lease_until"] = json!(SAFE_MAX + 1);
    binding.lease_until = SAFE_MAX + 1;
    deny_raw(&bytes(&value), &binding);
}

#[test]
fn safe_epoch_and_time_maxima_and_key_label_boundaries_are_representable() {
    let mut value = positive();
    let mut binding = expected();
    value["provider_version"] = json!(SAFE_MAX);
    binding.provider_version = SAFE_MAX;
    value["owner_epoch"] = json!(SAFE_MAX);
    binding.owner_epoch = SAFE_MAX;
    value["permission_epoch"] = json!(SAFE_MAX);
    binding.permission_epoch = SAFE_MAX;
    value["lease_until"] = json!(SAFE_MAX);
    binding.lease_until = SAFE_MAX;
    value["cnf"]["kv"] = json!(SAFE_MAX);
    binding.cnf.kv = SAFE_MAX;
    value["issuer_key"]["kv"] = json!(SAFE_MAX);
    binding.issuer_key.kv = SAFE_MAX;
    value["cnf"]["kid"] = json!("a");
    binding.cnf.kid = "a".into();
    value["issuer_key"]["kid"] = json!("a_0-".repeat(16));
    binding.issuer_key.kid = "a_0-".repeat(16);
    accept(&value, &binding);
    // M2 §1.2 Time includes zero. This is lexical/static validity, NOT a live lease.
    value["lease_until"] = json!(0);
    binding.lease_until = 0;
    accept(&value, &binding);
}

#[test]
fn provider_field_byte_bounds_no_normalization_or_truncation() {
    for (field, maximum) in [("username", 255), ("password", 1024)] {
        for good in [
            "a".to_string(),
            "a".repeat(maximum),
            format!(
                "{}{}",
                "é".repeat(maximum / 2),
                if maximum % 2 == 1 { "a" } else { "" }
            ),
            " inert untrimmed ".to_string(),
        ] {
            let mut value = positive();
            value["provider"][field] = json!(good);
            accept(&value, &expected());
        }
        for bad in [
            "".to_string(),
            "a".repeat(maximum + 1),
            "é".repeat(maximum / 2 + 1),
            "inert\0value".into(),
            "inert\rvalue".into(),
            "inert\nvalue".into(),
        ] {
            let mut value = positive();
            value["provider"][field] = json!(bad);
            deny(&value);
        }
    }
    // Legal DNS labels at the exact total bound, not an oversized label.
    let max_host = format!(
        "{}.{}.{}.{}.invalid",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(58),
        "d".repeat(58)
    );
    assert_eq!(max_host.len(), 253);
    let mut value = positive();
    value["provider"]["host"] = json!(max_host);
    accept(&value, &expected());
    value["provider"]["host"] = json!("x");
    accept(&value, &expected());
    for host in [
        "".to_string(),
        "a".repeat(254),
        "nëws.invalid".into(),
        "https://news.example.invalid".into(),
        "news.example.invalid/path".into(),
        "news\n.example.invalid".into(),
        "news.example.invalid:563".into(),
        "news..example.invalid".into(),
    ] {
        let mut value = positive();
        value["provider"]["host"] = json!(host);
        deny(&value);
    }
}

#[test]
fn port_tls_acknowledgement_and_positive_capacity_are_not_clamped() {
    for port in [119, 563] {
        for (tls, ack) in [(true, false), (true, true), (false, true)] {
            for cap in 1..=50 {
                let mut value = positive();
                value["provider"]["port"] = json!(port);
                value["provider"]["tls"] = json!(tls);
                value["provider"]["allow_insecure_plaintext"] = json!(ack);
                value["provider"]["effective_max_connections"] = json!(cap);
                accept(&value, &expected());
            }
        }
    }
    for (field, bad) in [
        ("port", 0),
        ("port", 80),
        ("port", 120),
        ("port", 564),
        ("port", 65536),
        ("effective_max_connections", 0),
        ("effective_max_connections", 51),
    ] {
        let mut value = positive();
        value["provider"][field] = json!(bad);
        deny(&value);
    }
    let mut value = positive();
    value["provider"]["tls"] = json!(false);
    deny(&value);
}

#[test]
fn whole_response_eight_kib_boundary_is_refusal_not_truncation() {
    let mut raw = bytes(&positive());
    assert!(raw.len() < 8192);
    raw.resize(8192, b' ');
    assert!(
        validate_provider_response(&raw, &expected()).is_ok(),
        "legal JSON at exact byte cap"
    );
    raw.push(b' ');
    deny_raw(&raw, &expected());
    // A useful valid prefix cannot be returned when the entire body is oversized.
    raw.extend_from_slice(b"inert-invalid-tail");
    deny_raw(&raw, &expected());
}

#[test]
fn malformed_credentials_never_appear_in_fixed_error_output() {
    let mut first = positive();
    first["provider"]["password"] = json!("INERT_PASSWORD_A\n");
    let mut second = positive();
    second["provider"]["password"] = json!("INERT_PASSWORD_B\r");
    assert_eq!(
        deny_raw(&bytes(&first), &expected()),
        deny_raw(&bytes(&second), &expected())
    );
}

// Stable negative trait assertion: inference becomes ambiguous if Debug is implemented.
macro_rules! assert_not_debug {
    ($ty:ty) => {
        const _: fn() = || {
            trait AmbiguousIfDebug<A> {
                fn marker() {}
            }
            impl<T: ?Sized> AmbiguousIfDebug<()> for T {}
            struct DebugMarker;
            impl<T: ?Sized + std::fmt::Debug> AmbiguousIfDebug<DebugMarker> for T {}
            let _ = <$ty as AmbiguousIfDebug<_>>::marker;
        };
    };
}
assert_not_debug!(Provider3Success);
assert_not_debug!(ProviderOnly);
