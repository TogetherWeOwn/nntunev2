//! Acceptance tests: API log `fields.release` mirrors the normalized
//! build identity (tests only; no implementation is changed here).
//!
//! Contract under test — the paired build implements this in the API
//! worker's existing emission paths and must not edit this file:
//!
//! - every existing API structured log emission — `api.request.served`,
//!   `api.start.misconfigured`, and `api.db.connection_error` — carries
//!   `fields.release` in addition to `fields.build_sha`, and
//!   `fields.release == fields.build_sha` on every record;
//! - the shared identity is the existing normalized build identity: a
//!   `BUILD_SHA` that is exactly 40 lowercase hex chars passes through
//!   unchanged; an absent, invalid, truncated, oversize, or uppercase
//!   candidate maps BOTH fields to exactly `"unknown"`;
//! - there is no separate `RELEASE` env var: `release` derives from the
//!   same normalized `build_sha` value, never from its own source;
//! - the rendered record keeps exactly the six top-level envelope keys
//!   (`ts`, `level`, `worker`, `trace_id`, `event`, `fields`), and route,
//!   `error_class`, trace, and redaction behavior are preserved.
//!
//! Why two layers: the emission paths need a wasm `Env`, so — exactly
//! like the existing `dispatch_wires_build_sha_into_every_emit_site`
//! unit test — the wiring layer pins the pre-test production source of
//! `workers/api/src/worker_entry.rs`, sliced per emission site so a
//! dropped `release` at any one site fails. A helper-only test that
//! leaves the emission paths unverified would not satisfy this
//! contract. The wiring layer accepts a `release` set from the same
//! normalized identity at each site, or one central mirror in the
//! shared emitter path (`emit_record`, or `LogRecord` construction /
//! rendering / field validation in `nntune-logging`) that copies
//! `build_sha` into `release` for every record — either shape emits
//! the same records. The rendered-record layer then exercises the
//! public `nntune-logging` surface with site-shaped fields to pin the
//! envelope, the `release == build_sha` rendering, normalization
//! pairing, and redaction.
//!
//! Sensitivity (each defeats at least one pin):
//!
//! - missing-`release` mutant (the base tree): no site and no central
//!   path sets `"release"` from the build identity, so the per-site
//!   wiring pins fail — this suite is RED before the build;
//! - mismatched-`release` mutant (a hardcoded literal such as
//!   `"unknown"`, the `BUILD_SHA_UNKNOWN` constant, a fresh env read,
//!   or any value other than the site's normalized `build_sha`): the
//!   mirror patterns do not match and the literal/env bans fire, so
//!   the wiring pins stay red even though a `release` key exists;
//! - dropping `release` at exactly one of the three sites: the
//!   per-site counts (two in `dispatch`, one in the `PgStore` impl)
//!   fall short;
//! - relaxing normalization (accepting 39/41-char, uppercase, or
//!   non-hex candidates, or echoing the raw candidate): the pairing
//!   table's both-fields-`"unknown"` assertions fail;
//! - adding a seventh envelope key or renaming one: the exact six-key
//!   set assertions fail;
//! - weakening redaction so a banned key, an address-carrying route,
//!   or a credential-shaped `release` renders: the fail-closed
//!   assertions fail.
//!
//! Static fixtures and the public logging API only: no live tail, no
//! network, no new dependencies, no deployed-environment access.

use std::collections::BTreeSet;
use std::path::PathBuf;

const VALID_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const TRACE_ID: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";
const TS_MILLIS: u64 = 1_700_000_000_000;
const ENVELOPE_KEYS: &[&str] = &["ts", "level", "worker", "trace_id", "event", "fields"];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_source(rel: &str) -> String {
    let path = manifest_dir().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("must read {}: {e}", path.display()))
}

/// Production (pre-test-module) source, so this file's own pattern
/// literals can never satisfy the pins it checks.
fn production_source(rel: &str) -> String {
    let source = read_source(rel);
    source
        .split_once("#[cfg(test)]")
        .expect("source must have a test module bounding production code")
        .0
        .to_string()
}

fn worker_entry_production() -> String {
    production_source("src/worker_entry.rs")
}

fn logging_production() -> String {
    production_source("../../crates/nntune-logging/src/lib.rs")
}

fn slice_between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("production source must contain {start:?}"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("{start:?} must be bounded by a following {end:?}"))
        .0
}

/// Patterns that set the `release` field directly from the site's
/// normalized `build_sha` value. `"release": build_sha` also covers
/// `build_sha.clone()` / `build_sha.as_str()` spellings.
const DIRECT_MIRROR_PATTERNS: &[&str] = &[
    "\"release\": build_sha",
    "\"release\": &build_sha",
    "[\"release\"] = serde_json::Value::String(build_sha",
    "[\"release\"] = fields[\"build_sha\"]",
    "insert(\"release\".to_string(), serde_json::Value::String(build_sha",
    "insert(\"release\".into(), serde_json::Value::String(build_sha",
];

/// Patterns that set `release` from a local `release` variable; these
/// only count when that variable is itself derived from the build
/// identity (checked by `release_var_is_build_identity`).
const RELEASE_VAR_PATTERNS: &[&str] = &[
    "\"release\": release",
    "\"release\": &release",
    "[\"release\"] = serde_json::Value::String(release",
    "insert(\"release\".to_string(), serde_json::Value::String(release",
    "insert(\"release\".into(), serde_json::Value::String(release",
];

fn release_var_is_build_identity(production: &str) -> bool {
    [
        "let release = build_sha",
        "let release = build_sha_from_env",
        "let release = crate::logging::parse_build_sha",
        "let release = parse_build_sha",
        "let release: String = build_sha",
    ]
    .iter()
    .any(|pattern| production.contains(pattern))
}

fn count_release_mirrors(slice: &str, production: &str) -> usize {
    let direct: usize = DIRECT_MIRROR_PATTERNS
        .iter()
        .map(|pattern| slice.matches(pattern).count())
        .sum();
    if release_var_is_build_identity(production) {
        direct
            + RELEASE_VAR_PATTERNS
                .iter()
                .map(|pattern| slice.matches(pattern).count())
                .sum::<usize>()
    } else {
        direct
    }
}

/// A single central mirror — in `emit_record` or in the shared
/// `nntune-logging` record path — that copies `build_sha` into
/// `release` covers every emission site that flows through it.
fn central_release_mirror_exists(worker_entry: &str, logging: &str) -> bool {
    let emit_body = slice_between(worker_entry, "fn emit_record(", "\npub async fn dispatch(");
    if emit_body.contains("\"release\"") && emit_body.contains("build_sha") {
        return true;
    }
    for (start, end) in [
        (
            "pub fn fields_from_json(",
            "\n/// One structured log record",
        ),
        ("impl LogRecord", "pub fn to_json_line"),
        (
            "pub fn to_json_line",
            "/// Request trace carried by this record.",
        ),
    ] {
        let body = slice_between(logging, start, end);
        if body.contains("\"release\"") && body.contains("build_sha") {
            return true;
        }
    }
    false
}

/// The core wiring pin: each of the three existing API emission sites
/// carries `release` mirroring its `build_sha`, either site-locally or
/// through one central mirror. RED on the base tree, where no site
/// sets `release` at all (the missing-release mutant), and still red
/// under a mismatched literal, which matches no mirror pattern.
#[test]
fn every_api_emission_site_carries_release_mirroring_build_sha() {
    let production = worker_entry_production();
    let logging = logging_production();

    // Anti-vacuity: the reader, the shared normalizer, and the three
    // events exist before any release assertion is evaluated.
    assert!(
        production.contains("fn build_sha_from_env("),
        "the BUILD_SHA reader must exist"
    );
    assert!(
        production.contains("crate::logging::parse_build_sha"),
        "the reader must normalize through the shared validator"
    );

    let dispatch_body = slice_between(&production, "pub async fn dispatch(", "\nfn ");
    for event in ["api.request.served", "api.start.misconfigured"] {
        assert!(dispatch_body.contains(event), "dispatch must emit {event}");
    }
    // `dispatch` holds two emit sites (served + misconfigured); the
    // existing build_sha wiring is the shape `release` must mirror.
    assert_eq!(
        dispatch_body.matches("\"build_sha\": build_sha").count(),
        2,
        "served and misconfigured must each carry the build_sha key"
    );

    let store_body = slice_between(&production, "impl PgStore", "\nfn user_from_row");
    assert!(
        store_body.contains("api.db.connection_error"),
        "PgStore::connect must emit api.db.connection_error"
    );
    assert!(
        store_body.contains("\"build_sha\": build_sha"),
        "the db connection_error site must carry the build_sha key"
    );

    if central_release_mirror_exists(&production, &logging) {
        // One central build_sha -> release copy in the shared emitter
        // path covers all three sites above.
        return;
    }
    assert_eq!(
        count_release_mirrors(dispatch_body, &production),
        2,
        "served and misconfigured must each carry release == build_sha \
         (missing or mismatched release at a dispatch site)"
    );
    assert_eq!(
        count_release_mirrors(store_body, &production),
        1,
        "the db connection_error site must carry release == build_sha \
         (missing or mismatched release at the PgStore site)"
    );
}

/// `release` is the normalized build identity itself, never a separate
/// source: no `RELEASE` env var, no hardcoded literal, no always-
/// unknown constant at any emission site.
#[test]
fn release_has_no_separate_env_var_or_hardcoded_value() {
    let production = worker_entry_production();
    let logging = logging_production();
    for (name, source) in [
        ("worker_entry.rs", &production),
        ("nntune-logging lib.rs", &logging),
    ] {
        assert!(
            !source.contains("\"RELEASE\""),
            "{name} must not read a RELEASE env var; release mirrors build_sha"
        );
    }
    for banned in [
        "\"release\": \"",
        "\"release\": crate::logging::BUILD_SHA_UNKNOWN",
        "\"release\": BUILD_SHA_UNKNOWN",
        "\"release\": String::new",
        "\"release\": env_string",
        "\"release\": std::env",
    ] {
        assert!(
            !production.contains(banned),
            "release must mirror the normalized build_sha, never {banned:?} \
             (mismatched-release mutant)"
        );
    }
}

fn rendered(
    level: nntune_logging::Level,
    event: &str,
    fields: serde_json::Value,
) -> serde_json::Value {
    let field_map = nntune_logging::fields_from_json(fields)
        .expect("site-shaped fields with release must pass the shared validator");
    let record = nntune_logging::LogRecord::new(
        nntune_logging::Worker::Api,
        level,
        TRACE_ID,
        event,
        field_map,
        TS_MILLIS,
    )
    .expect("site-shaped record with release must construct");
    serde_json::from_str(&record.to_json_line()).expect("rendered line must be one JSON object")
}

fn assert_envelope_and_release(record: &serde_json::Value, event: &str, identity: &str) {
    let object = record.as_object().expect("record must be an object");
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = ENVELOPE_KEYS.iter().copied().collect();
    assert_eq!(
        actual, expected,
        "exactly the six envelope keys, no seventh key for release"
    );
    assert_eq!(object["worker"], "api");
    assert_eq!(object["event"], event);
    assert_eq!(
        object["trace_id"], TRACE_ID,
        "trace behavior is preserved alongside release"
    );
    assert_eq!(object["fields"]["build_sha"], identity);
    assert_eq!(
        object["fields"]["release"], object["fields"]["build_sha"],
        "fields.release must equal fields.build_sha on {event}"
    );
    assert_eq!(object["fields"]["release"], identity);
}

/// Rendered shape for all three emission events, with a known SHA and
/// with the explicit unknown marker: `release` renders inside
/// `fields`, equal to `build_sha`, without disturbing the envelope or
/// the route / `error_class` fields each site already carries.
#[test]
fn rendered_records_for_every_event_carry_release_equal_to_build_sha() {
    for identity in [VALID_SHA, nntune_logging::BUILD_SHA_UNKNOWN] {
        let served = rendered(
            nntune_logging::Level::Info,
            "api.request.served",
            serde_json::json!({
                "route": "GET /healthz",
                "status_code": 200,
                "duration_ms": 3,
                "build_sha": identity,
                "release": identity,
            }),
        );
        assert_envelope_and_release(&served, "api.request.served", identity);
        assert_eq!(served["fields"]["route"], "GET /healthz");

        let misconfigured = rendered(
            nntune_logging::Level::Error,
            "api.start.misconfigured",
            serde_json::json!({
                "error_class": "internal_error",
                "build_sha": identity,
                "release": identity,
            }),
        );
        assert_envelope_and_release(&misconfigured, "api.start.misconfigured", identity);
        assert_eq!(misconfigured["fields"]["error_class"], "internal_error");

        let connection_error = rendered(
            nntune_logging::Level::Error,
            "api.db.connection_error",
            serde_json::json!({
                "error_class": "service_unavailable",
                "build_sha": identity,
                "release": identity,
            }),
        );
        assert_envelope_and_release(&connection_error, "api.db.connection_error", identity);
        assert_eq!(
            connection_error["fields"]["error_class"],
            "service_unavailable"
        );
    }
}

/// Normalization pairing: every candidate that is not exactly 40
/// lowercase hex chars maps BOTH fields to `"unknown"`, a valid
/// candidate passes through to both, and no invalid candidate is ever
/// echoed into either field.
#[test]
fn normalization_pairs_release_and_build_sha_for_every_candidate() {
    let valid = nntune_logging::parse_build_sha(Some(VALID_SHA));
    assert_eq!(valid, VALID_SHA);
    assert!(nntune_logging::is_build_sha_known(&valid));

    // (candidate, label) pairs; `None` is the absent-var shape. The
    // oversize case is a 100-char hex run, far past any plausible cap.
    let oversize = "a".repeat(100);
    let invalid: &[(Option<&str>, &str)] = &[
        (None, "absent"),
        (Some(""), "empty"),
        (
            Some("0123456789abcdef0123456789abcdef0123456"),
            "39-char truncated",
        ),
        (
            Some("0123456789abcdef0123456789abcdef012345670"),
            "41-char over-long",
        ),
        (Some(oversize.as_str()), "100-char oversize"),
        (
            Some("0123456789ABCDEF0123456789ABCDEF01234567"),
            "uppercase hex",
        ),
        (Some("0123456789abcdef0123456789abcdef0123456g"), "non-hex"),
        (
            Some(" 0123456789abcdef0123456789abcdef01234567"),
            "whitespace-padded",
        ),
        (
            Some(nntune_logging::BUILD_SHA_UNKNOWN),
            "unknown sentinel itself",
        ),
    ];
    for (raw, label) in invalid {
        let normalized = nntune_logging::parse_build_sha(*raw);
        assert_eq!(
            normalized,
            nntune_logging::BUILD_SHA_UNKNOWN,
            "{label} candidate must normalize to unknown, got {normalized:?}"
        );
        assert!(
            !nntune_logging::is_build_sha_known(&normalized),
            "{label} normalization must not be known"
        );
        // Both fields take the one normalized value, so the rendered
        // record can never pair a known build_sha with a different
        // release (or vice versa) for an invalid candidate.
        let record = rendered(
            nntune_logging::Level::Info,
            "api.request.served",
            serde_json::json!({
                "route": "GET /healthz",
                "status_code": 200,
                "duration_ms": 1,
                "build_sha": normalized,
                "release": normalized,
            }),
        );
        assert_envelope_and_release(
            &record,
            "api.request.served",
            nntune_logging::BUILD_SHA_UNKNOWN,
        );
        if let Some(candidate) = raw {
            if *candidate != nntune_logging::BUILD_SHA_UNKNOWN {
                assert_ne!(
                    record["fields"]["release"], *candidate,
                    "{label} candidate must never be echoed as release"
                );
                assert_ne!(
                    record["fields"]["build_sha"], *candidate,
                    "{label} candidate must never be echoed as build_sha"
                );
            }
        }
    }
}

/// Redaction stays fail-closed alongside the new field: a banned key,
/// an address-carrying route value, a credential-shaped `release`
/// value, and an invalid trace are each rejected rather than rendered.
#[test]
fn redaction_and_trace_validation_stay_fail_closed_with_release() {
    // The `release` key itself is loggable (not a banned key).
    assert!(!nntune_logging::is_banned_field_key("release"));

    assert!(
        nntune_logging::fields_from_json(serde_json::json!({
            "route": "GET /healthz",
            "build_sha": VALID_SHA,
            "release": VALID_SHA,
            "session_id": "s",
        }))
        .is_err(),
        "a banned key must still drop the record when release is present"
    );
    assert!(
        nntune_logging::fields_from_json(serde_json::json!({
            "route": "GET /v1/vault/victim@example.com",
            "build_sha": VALID_SHA,
            "release": VALID_SHA,
        }))
        .is_err(),
        "an address-carrying route value must never become a log line"
    );
    assert!(
        nntune_logging::fields_from_json(serde_json::json!({
            "build_sha": VALID_SHA,
            "release": "Bearer abc123",
        }))
        .is_err(),
        "a credential-shaped release value must be rejected, never logged"
    );

    let fields = nntune_logging::fields_from_json(serde_json::json!({
        "build_sha": VALID_SHA,
        "release": VALID_SHA,
    }))
    .expect("release fields must validate");
    assert!(
        nntune_logging::LogRecord::new(
            nntune_logging::Worker::Api,
            nntune_logging::Level::Info,
            "not-a-trace-id",
            "api.request.served",
            fields,
            TS_MILLIS,
        )
        .is_err(),
        "an invalid trace id must still reject the record"
    );
}
