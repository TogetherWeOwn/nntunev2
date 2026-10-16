//! Source-level acceptance pins for the runtime attestation and finish-route
//! budget contract. These deliberately run on the pre-implementation tree and
//! fail until the guarded startup and reservations are present.

const WORKER_ENTRY: &str = include_str!("../src/worker_entry.rs");
const HANDLERS: &str = include_str!("../src/handlers.rs");

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let body_start = source[start..]
        .find('{')
        .map(|offset| start + offset + 1)
        .expect("function body opening brace");
    let mut depth = 1usize;
    for (offset, byte) in source.as_bytes()[body_start..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start..body_start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function body: {signature}");
}

#[test]
fn runtime_config_requires_explicit_hyperdrive_cache_attestation() {
    let config = function_body(WORKER_ENTRY, "fn config_from_env(");
    let gate = config
        .find("hyperdrive_caching_disabled_attested")
        .expect("config_from_env must reject un-attested Hyperdrive caching configuration");
    let required_config = config
        .find("let rp_id = env_string")
        .expect("RP config load");
    assert!(
        gate < required_config,
        "attestation must gate config construction"
    );

    let decision = function_body(WORKER_ENTRY, "fn hyperdrive_caching_disabled_attested(");
    for accepted in ["\"1\"", "\"true\"", "\"TRUE\""] {
        assert!(
            decision.contains(accepted),
            "attestation accepts {accepted}"
        );
    }
}

#[test]
fn passkey_finish_routes_reserve_before_database_reads() {
    for route in ["register_finish", "login_finish"] {
        let body = function_body(HANDLERS, &format!("async fn {route}<"));
        let reserve = body
            .find(".reserve_rate_limit(")
            .unwrap_or_else(|| panic!("{route} must bound finish attempts"));
        let challenge_read = body
            .find(".challenge_by_id(")
            .unwrap_or_else(|| panic!("{route} challenge read"));
        assert!(
            reserve < challenge_read,
            "{route} reserves before challenge read"
        );
        assert!(
            body.contains("if !admitted"),
            "{route} fails closed at budget"
        );
        assert!(
            body.contains("return generic_auth()"),
            "{route} denial is opaque"
        );
    }
}

#[test]
fn magic_finish_reserves_after_origin_pin_and_before_token_lookup() {
    let body = function_body(HANDLERS, "async fn magic_finish<");
    let origin_pin = body
        .find("req.origin.as_deref() != Some(ctx.cfg.origin.as_str())")
        .expect("magic finish retains exact-Origin CSRF pin");
    let reserve = body
        .find(".reserve_rate_limit(")
        .expect("magic finish bounds token lookup attempts");
    let token_lookup = body.find(".magic_by_hash(").expect("magic token lookup");
    assert!(
        origin_pin < reserve,
        "cross-origin requests cannot spend budget"
    );
    assert!(reserve < token_lookup, "reserve before DB token lookup");
    assert!(
        body.contains("&ctx.source_ip"),
        "magic budget is per client IP"
    );
    assert!(
        body.contains("if !admitted"),
        "magic finish fails closed at budget"
    );
}
