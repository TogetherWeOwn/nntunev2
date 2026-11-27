use nntune_config::Config;
use std::collections::HashMap;

fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|&(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn identity(rp_id: &str, origin: &str) -> HashMap<String, String> {
    values(&[("auth.rp_id", rp_id), ("auth.rp_origin", origin)])
}

#[test]
fn rejects_noncanonical_or_mismatched_origins() {
    let long_label = format!("{}.example", "a".repeat(64));
    let long_host = format!("{0}.{0}.{0}.{0}", "a".repeat(63));
    for (rp_id, origin) in [
        ("example.test", "https://other.example.test"),
        ("example.test", "http://example.test"),
        ("example.test", "https://example.test/"),
        ("example.test", "https://example.test/path"),
        ("example.test", "https://example.test?query"),
        ("example.test", "https://example.test#fragment"),
        ("example.test", "https://example.test:443"),
        ("example.test", "https://example.test:0444"),
        ("example.test", "https://example.test:0"),
        ("example.test", "https://example.test:65536"),
        ("example.test", "https://example.test:"),
        ("example.test", "https://example.test:+444"),
        ("example.test", "https://example.test:444/"),
        ("example.test", "https://example.test\n"),
        ("example.test", "https://%65xample.test"),
        ("example.test", "https://@example.test"),
        ("example.test", "https://example.test\\"),
        ("example.test", "HTTPS://example.test"),
        ("Example.test", "https://Example.test"),
        ("example.test.", "https://example.test."),
        ("example..test", "https://example..test"),
        ("-example.test", "https://-example.test"),
        ("example-.test", "https://example-.test"),
        ("ex_ample.test", "https://ex_ample.test"),
        ("example.0x2", "https://example.0x2"),
        ("éxample.test", "https://éxample.test"),
        ("xn--exmple-cua.test", "https://xn--exmple-cua.test"),
        ("127.1", "http://127.1"),
        ("localhost", "http://localhost:80"),
        ("::1", "http://[0:0:0:0:0:0:0:1]"),
        (&long_label, &format!("https://{long_label}")),
        (&long_host, &format!("https://{long_host}")),
    ] {
        let loaded = Config::load(&identity(rp_id, origin));
        assert!(loaded.is_err(), "{origin:?}");
    }
}

#[test]
fn accepts_canonical_dns_and_loopback_origins() {
    for (rp_id, origin) in [
        ("app-1.example.test", "https://app-1.example.test"),
        ("example.test", "https://example.test:65535"),
        ("localhost", "http://localhost:5173"),
        ("127.0.0.1", "http://127.0.0.1"),
        ("::1", "https://[::1]:8443"),
    ] {
        let config = Config::load(&identity(rp_id, origin)).expect("canonical origin");
        assert_eq!(config.get_str("auth.rp_id"), Some(rp_id));
        assert_eq!(config.get_str("auth.rp_origin"), Some(origin));
    }
}

#[test]
fn environment_checks_the_final_identity_and_rolls_back_on_failure() {
    let mut config = Config::load(&identity("a.example", "https://a.example")).unwrap();
    let original = config.clone();
    for env in [
        values(&[("RP_ID", "b.example"), ("SESSION_TTL_SECS", "3600")]),
        values(&[("RP_ORIGIN", "https://b.example")]),
        values(&[("RP_ID", "a.example"), ("RP_ORIGIN", "https://a.example/")]),
        values(&[
            ("RP_ID", "b.example"),
            ("RP_ORIGIN", "https://b.example"),
            ("SESSION_TTL_SECS", "0"),
        ]),
    ] {
        assert!(config.apply_env(&env).is_err());
        assert_eq!(config, original);
    }
    config
        .apply_env(&values(&[
            ("RP_ID", "b.example"),
            ("RP_ORIGIN", "https://b.example:8443"),
            ("SESSION_TTL_SECS", " 3600 "),
        ]))
        .expect("coherent replacement");
    assert_eq!(config.get_str("auth.rp_id"), Some("b.example"));
    let origin = config.get_str("auth.rp_origin");
    assert_eq!(origin, Some("https://b.example:8443"));
    assert_eq!(config.get_u64("session.ttl_secs"), Some(3600));
}

#[test]
fn rejects_runtime_values_outside_worker_limits() {
    for (key, bad) in [
        ("auth.challenge_ttl_secs", "0"),
        ("auth.challenge_ttl_secs", "301"),
        ("auth.magic_link_ttl_secs", "901"),
        ("auth.invite_redemption_max_attempts", "0"),
        ("auth.invite_redemption_max_attempts", "11"),
        ("auth.invite_redemption_window_secs", "299"),
        ("auth.invite_redemption_window_secs", "301"),
        ("ingest.max_nzb_bytes", "2097153"),
        ("ingest.payload_retry_attempts", "3"),
        ("ingest.payload_retry_attempts", "5"),
        ("ingest.payload_ttl_secs", "1295999"),
        ("ingest.payload_ttl_secs", "2592001"),
        ("mb.lookup_cache_ttl_s", "59"),
        ("mb.search_cache_ttl_s", "4294967296"),
        ("mb.max_query_chars", "101"),
        ("mb.search_limit", "0"),
        ("mb.search_limit", "26"),
        ("session.ttl_secs", "0"),
        ("stream.r2_min_part_bytes", "5242879"),
        ("stream.r2_min_part_bytes", "5242881"),
    ] {
        let mut kv = identity("example.test", "https://example.test");
        kv.insert(key.to_owned(), bad.to_owned());
        assert!(Config::load(&kv).is_err(), "{key} accepted {bad}");
    }
}

#[test]
fn accepts_in_range_runtime_tuning() {
    let mut raw = identity("example.test", "https://example.test");
    raw.extend(values(&[
        ("auth.invite_redemption_max_attempts", "1"),
        ("ingest.payload_ttl_secs", "1296000"),
        ("mb.lookup_cache_ttl_s", "60"),
        ("mb.search_limit", "1"),
    ]));
    let config = Config::load(&raw).expect("worker limits are inclusive");
    assert_eq!(config.get_u64("ingest.payload_ttl_secs"), Some(1_296_000));
    assert_eq!(config.get_u64("mb.lookup_cache_ttl_s"), Some(60));
}

#[test]
fn caps_positive_session_ttl_on_load_and_override() {
    let mut raw = identity("example.test", "https://example.test");
    raw.insert("session.ttl_secs".into(), "86401".into());
    let config = Config::load(&raw).expect("TTL above the cap is valid");
    assert_eq!(config.get_u64("session.ttl_secs"), Some(86_400));
    raw.insert("session.ttl_secs".into(), "3600".into());
    let mut config = Config::load(&raw).expect("short TTL is valid");
    let env = values(&[("SESSION_TTL_SECS", "18446744073709551615")]);
    config.apply_env(&env).expect("large TTL override is valid");
    assert_eq!(config.get_u64("session.ttl_secs"), Some(86_400));
}

#[test]
fn validation_errors_redact_input_in_display_and_debug() {
    let marker = "synthetic-private-fixture";
    let mut unknown = identity("example.test", "https://example.test");
    unknown.insert(format!("{marker}\nfixture-line"), marker.to_owned());
    let err = Config::load(&unknown).expect_err("undeclared key");
    for rendered in [err.to_string(), format!("{err:?}")] {
        assert!(!rendered.contains(marker));
        assert!(!rendered.contains('\n'));
    }
}

#[test]
fn origin_errors_redact_input_in_display_and_debug() {
    let marker = "synthetic-private-fixture";
    let origin = format!("https://{marker}@example.test");
    let mut config = Config::load(&identity("example.test", "https://example.test")).unwrap();
    let invalid_identity = identity("example.test", &origin);
    let env = values(&[("RP_ORIGIN", &origin)]);
    let kv_error = Config::load(&invalid_identity).err().expect("userinfo");
    let env_error = config.apply_env(&env).expect_err("userinfo override");
    for err in [kv_error, env_error] {
        for rendered in [err.to_string(), format!("{err:?}")] {
            assert!(!rendered.contains(marker));
            assert!(!rendered.contains('\n'));
        }
    }
}
