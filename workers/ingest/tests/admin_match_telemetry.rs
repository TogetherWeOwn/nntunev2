//! Offline redaction and bounded-field tests for off-route admin matcher
//! counters, structured diagnostics, and metering error projections.

use std::collections::BTreeSet;

use nntune_ingest::{
    admin_auto_publish::{telemetry::*, StageOutcome},
    catalogue_publication::PublicationError,
    dispatch::{build_search_dispatch, build_upload_dispatch},
    fixtures::SMALL_NZB,
    matching::{
        admin_publish_match::{
            decide_publish_match, release_lookup, select_release, CompleteReleaseCandidates,
            FileNumbering as Numbers, NeedsReview, NeedsReview::*,
        },
        derive_upload_query, MbSearchHit,
    },
    metering::*,
};
use nntune_logging::{
    fields_from_json, is_banned_field_key, is_valid_trace_id, looks_like_email_value,
    looks_like_ipv4_value, looks_like_ipv6_value, looks_like_secret_value, Level, LogRecord,
    Worker as LogWorker, BUILD_SHA_UNKNOWN,
};
use serde_json::{json, Value};

const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const TS: u64 = 1_792_094_400_123;
const REL_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const REL_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const FILE: &str = "01_private_rip_secret-filename.flac";
const USER: &str = "usr_synthetic_acct_9f2a";
const EMAIL: &str = "owner@sentinel.invalid";
const BEARER: &str = "Bearer tok_synthetic_admin_secret_01";
const KEY: &str = "apikey=synthetic_secret_key_99";
const OBJ: &str = "media/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd";
const DIGEST: &str = "media-digests/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd/06060606060640068006060606060606/00000000";
const ALBUM: &str = "Tin Skies";
const ARTIST: &str = "Paper Moons";

fn assert_clean(text: &str) {
    let extra = "tok_synthetic_admin_secret_01|synthetic_secret_key_99|private-staging-bucket|eeeeeeee-5555-4555-8555-555555555555|cccccccc-3333-4333-8333-333333333333|dddddddd-4444-4444-8444-444444444444";
    for m in [
        FILE, USER, EMAIL, BEARER, KEY, OBJ, DIGEST, REL_A, REL_B, ALBUM, ARTIST,
    ]
    .into_iter()
    .chain(extra.split('|'))
    {
        assert!(!text.contains(m), "leaked {m:?} in {text}");
    }
}

fn hit(mbid: &str, title: &str, artist: Option<&str>, score: f32) -> MbSearchHit {
    MbSearchHit {
        mbid: mbid.into(),
        title: title.into(),
        score,
        artist_name: artist.map(str::to_owned),
        artist_mbid: None,
    }
}

fn assert_bounded(
    res: &Result<StageOutcome, PublicationError>,
    outcome: &str,
    reason: Option<&str>,
    level: &str,
) {
    let counter = serde_json::to_value(matcher_counter(res)).unwrap();
    let labels = reason.map_or_else(
        || json!({"outcome": outcome}),
        |r| json!({"outcome": outcome, "reason": r}),
    );
    assert_eq!(
        counter,
        json!({"name": MATCHER_COUNTER_NAME, "labels": labels, "increment": 1})
    );
    assert_clean(&counter.to_string());

    let mut want_fields = labels;
    for (in_t, in_s, want_t, want_s) in [
        (Some(TRACE), Some(SHA), Some(TRACE), SHA),
        (Some(BEARER), Some(OBJ), None, BUILD_SHA_UNKNOWN),
        (Some(FILE), Some(USER), None, BUILD_SHA_UNKNOWN),
        (Some(EMAIL), Some(KEY), None, BUILD_SHA_UNKNOWN),
        (None, None, None, BUILD_SHA_UNKNOWN),
    ] {
        let line = matcher_diagnostic(res, in_t, in_s, TS)
            .unwrap()
            .to_json_line();
        assert!(!line.contains('\n'));
        assert_clean(&line);
        let got: Value = serde_json::from_str(&line).unwrap();
        let tid = got["trace_id"].as_str().unwrap().to_owned();
        assert!(
            is_valid_trace_id(&tid)
                && want_t.map_or_else(|| in_t != Some(tid.as_str()), |t| tid == t)
        );
        want_fields["build_sha"] = json!(want_s);
        assert_eq!(
            got,
            json!({"ts": "2026-10-15T20:00:00.123Z", "level": level, "worker": "ingest", "trace_id": tid, "event": MATCHER_DIAGNOSTIC_EVENT, "fields": want_fields})
        );
        for (k, v) in got["fields"].as_object().unwrap() {
            let s = v.as_str().unwrap();
            assert!(!is_banned_field_key(k) && !looks_like_secret_value(s));
            assert!(
                !looks_like_email_value(s)
                    && !looks_like_ipv4_value(s)
                    && !looks_like_ipv6_value(s)
            );
        }
    }
}

fn check_review(got: NeedsReview, want: NeedsReview, code: &str) {
    assert_eq!(got, want);
    assert_bounded(
        &Ok(StageOutcome::NeedsReview(got)),
        "needs_review",
        Some(code),
        "warn",
    );
}

#[test]
fn matched_ambiguous_and_refused_matcher_outcomes_emit_only_bounded_redacted_telemetry() {
    let sub = [format!(
        r#"{ARTIST}.-.{ALBUM}.[01/02].-."{FILE}" yEnc (1/3)"#
    )];
    let q = derive_upload_query(&sub).unwrap();
    let lp = release_lookup(&q).unwrap();
    let cand = |lookup, hits| Some(CompleteReleaseCandidates { lookup, hits });
    let sel = select_release(&q, cand(&lp, vec![hit(REL_A, ALBUM, Some(ARTIST), 1.0)])).unwrap();
    let body = json!({
        "release_mbid": REL_A,
        "recordings": [
            {"release_mbid": REL_A, "mbid": "cccccccc-3333-4333-8333-333333333333", "title": "One", "medium_no": 1, "track_no": 1},
            {"release_mbid": REL_A, "mbid": "dddddddd-4444-4444-8444-444444444444", "title": "Two", "medium_no": 1, "track_no": 2},
        ]
    })
    .to_string();
    let num = |t| Numbers {
        disc: Some("1"),
        track: Some(t),
    };
    let nums = [num("01"), num("02")];
    let ok = decide_publish_match(sel, 2, &nums, 200, &body).unwrap();
    assert_eq!((ok.release_mbid(), ok.recordings().len()), (REL_A, 2));
    assert_bounded(
        &Ok(StageOutcome::Staged(vec![
            "eeeeeeee-5555-4555-8555-555555555555".into(),
        ])),
        "staged",
        None,
        "info",
    );

    let plain = release_lookup(ALBUM).unwrap();
    let near = f32::from_bits(0.5_f32.to_bits() + 1);
    let tied = vec![hit(REL_A, ALBUM, None, 0.9), hit(REL_B, ALBUM, None, 0.9)];
    let close = vec![
        hit(REL_A, ALBUM, None, 0.6),
        hit(REL_B, "Tin Sk'ies", None, near),
    ];
    let dup = vec![
        hit(REL_A, ALBUM, Some(ARTIST), 1.0),
        hit(REL_B, ALBUM, Some(ARTIST), 1.0),
    ];
    for (query, look, hits) in [
        (ALBUM, &plain, tied),
        (ALBUM, &plain, close),
        (&q, &lp, dup),
    ] {
        check_review(
            select_release(query, cand(look, hits)).unwrap_err(),
            ReleaseUnresolved,
            "release_unresolved",
        );
    }

    let mut wrong = release_lookup(ALBUM).unwrap();
    wrong.score_query = format!("{FILE} {BEARER} {OBJ}");
    for ev in [None, cand(&wrong, vec![hit(REL_A, ALBUM, None, 0.9)])] {
        check_review(
            select_release(ALBUM, ev).unwrap_err(),
            SearchEvidence,
            "search_evidence",
        );
    }

    let dec = |n, files: &[Numbers<'_>], st, b: &str| {
        let rel = select_release(ALBUM, cand(&plain, vec![hit(REL_A, ALBUM, None, 0.9)])).unwrap();
        decide_publish_match(rel, n, files, st, b).err().unwrap()
    };
    let bad_q = format!("{FILE}\u{0007}{USER} {BEARER}");
    let bad_t = format!("{FILE}:{OBJ}");
    let bad_b = format!(
        r#"{{"err":"{BEARER} {KEY} {FILE} {USER} private-staging-bucket/{OBJ} {DIGEST}"}}"#
    );
    check_review(
        select_release(&bad_q, None).unwrap_err(),
        InvalidQuery,
        "invalid_query",
    );
    check_review(
        dec(2, &[num("2"), num("1")], 200, &body),
        PositionMismatch,
        "position_mismatch",
    );
    check_review(
        dec(2, &[num(&bad_t), num("2")], 200, &body),
        FileNumbering,
        "file_numbering",
    );
    check_review(
        dec(1, &nums[..1], 200, &body),
        CountMismatch,
        "count_mismatch",
    );
    check_review(
        dec(2, &nums, 503, &bad_b),
        RecordingEvidence,
        "recording_evidence",
    );
    check_review(
        dec(2, &nums, 200, &bad_b),
        RecordingEvidence,
        "recording_evidence",
    );
    assert_bounded(&Err(PublicationError::Refused), "refused", None, "warn");
    assert_bounded(
        &Err(PublicationError::Indeterminate),
        "indeterminate",
        None,
        "error",
    );
}

#[test]
fn metering_events_and_failure_diagnostics_exclude_private_input_markers() {
    struct Sink(Result<u16, MeterTransportReason>, Vec<(String, Value)>);
    impl MeterSink for Sink {
        fn send(&mut self, ds: &str, body: Value) -> Result<u16, MeterTransportReason> {
            self.1.push((ds.into(), body));
            self.0
        }
    }

    let q = format!("{FILE} {BEARER} {OBJ}");
    let search = build_search_dispatch(USER, &q, "vault_idx_01-ab", &[3000], 10, 0).unwrap();
    let upload = build_upload_dispatch(USER, SMALL_NZB).unwrap();
    let meter_keys = BTreeSet::from(["attempt", "job_id", "outcome", "stage", "user_id"]);
    for (msg, stage) in [(&search, "match"), (&upload, "parse")] {
        let mut sink = Sink(Ok(200), Vec::new());
        emit_meter(&mut sink, msg, stage, MeterOutcome::Ok).unwrap();
        assert_eq!((sink.1.len(), sink.1[0].0.as_str()), (1, METER_DATASET));
        let keys: BTreeSet<&str> = sink.1[0]
            .1
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, meter_keys);
        let wire = sink.1[0].1.to_string();
        assert!([FILE, BEARER, KEY, OBJ, "vault_idx_01-ab", "nzb:sha256:"]
            .iter()
            .all(|m| !wire.contains(m)));
    }

    let detail = format!("{USER} {EMAIL} {FILE} {BEARER} {KEY} {OBJ}");
    let miss = Err::<u16, &str>(&detail).map_err(|_| MeterTransportReason::BindingMissing);
    let fail = Err::<u16, &str>(&detail).map_err(|_| MeterTransportReason::WriteFailed);
    let check_err = |reply, stage, code| {
        let mut sink = Sink(reply, Vec::new());
        let err = emit_meter(&mut sink, &search, stage, MeterOutcome::Failed).unwrap_err();
        assert_eq!((err.reason_code(), err.to_string().as_str()), (code, code));
        assert_clean(&format!("{err:?}"));
        assert_eq!(sink.1.is_empty(), err == MeterWriteError::UnknownStage);
        let ev = if code == "meter_binding_missing" {
            "ingest.meter.binding_missing"
        } else {
            "ingest.meter.write_failed"
        };
        let f = fields_from_json(json!({"build_sha": BUILD_SHA_UNKNOWN, "reason": code})).unwrap();
        let line = LogRecord::new(LogWorker::Ingest, Level::Error, TRACE, ev, f, TS)
            .unwrap()
            .to_json_line();
        assert_clean(&line);
    };
    check_err(miss, "match", "meter_binding_missing");
    check_err(fail, "match", "meter_write_failed");
    check_err(Ok(502), "match", "meter_write_rejected");
    check_err(miss, detail.as_str(), "meter_unknown_stage");
}

#[test]
fn admin_auto_publish_and_telemetry_stay_off_route_without_live_sinks() {
    let main_fn = include_str!("../src/lib.rs")
        .split_once("pub async fn main(")
        .unwrap()
        .1;
    let main_fn = main_fn.split_once("#[cfg(test)]").unwrap().0;
    let syms =
        "admin_auto_publish|admin_review|catalogue_publication|matcher_counter|matcher_diagnostic";
    assert!(syms.split('|').all(|s| !main_fn.contains(s)));
    let sinks =
        "console_log!|console_error!|console_warn!|console_debug!|crate::emit(|analytics_engine(";
    for src in [
        include_str!("../src/admin_auto_publish.rs"),
        include_str!("../src/admin_auto_publish/completed_upload.rs"),
        include_str!("../src/admin_auto_publish/telemetry.rs"),
    ] {
        let prod = src.split_once("#[cfg(test)]").map_or(src, |(p, _)| p);
        assert!(sinks.split('|').all(|s| !prod.contains(s)));
    }
}
