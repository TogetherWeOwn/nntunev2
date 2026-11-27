use super::{parse_object, validate_mirror_evidence, MirrorEvidence, TrialError};
use crate::worker_entry::private_browser_worker::uuid_v4;
use nntune_ingest::matching::admin_publish_match::{
    release_lookup, CompleteReleaseCandidates, FileNumbering,
};

const RELEASE: &str = "c4000000-0000-4000-8000-000000000001";
const OTHER: &str = "c4000000-0000-4000-8000-000000000002";
const TITLE: &str = "Synthetic fixture";

#[test]
fn mirror_object_and_rows_refuse_ambiguous_shapes() {
    let valid = format!(
        r#"{{"release_mbid":"{RELEASE}","title":"{TITLE}","exact_title_candidates":[],"recordings":[]}}"#
    );
    assert!(parse_object::<MirrorEvidence>(&valid, valid.len()).is_ok());
    assert!(parse_object::<MirrorEvidence>(&valid, valid.len() - 1).is_err());
    let positional = format!(r#"["{RELEASE}","{TITLE}",[],[]]"#);
    let array_row = valid.replace(
        r#""recordings":[]"#,
        &format!(r#""recordings":[["{RELEASE}","{OTHER}","Tone",1,1]]"#),
    );
    let duplicate = valid.replace(
        r#""recordings":[]"#,
        &format!(
            r#""recordings":[{{"release_mbid":"{RELEASE}","mbid":"{OTHER}","title":"Tone","track_no":2,"track_no":1,"medium_no":1}}]"#
        ),
    );
    for raw in [positional, array_row, duplicate] {
        assert!(parse_object::<MirrorEvidence>(&raw, 2_048).is_err());
    }
}

#[test]
fn mirror_match_requires_selected_release_and_exact_occurrence() {
    let evidence: MirrorEvidence = parse_object(
        &format!(
            r#"{{"release_mbid":"{RELEASE}","title":"{TITLE}",
            "exact_title_candidates":[{{"mbid":"{RELEASE}","title":"{TITLE}","score":1}}],
            "recordings":[{{"release_mbid":"{RELEASE}","mbid":"{OTHER}","title":"Tone","track_no":1,"medium_no":1}}]}}"#
        ),
        2_048,
    )
    .unwrap();
    let lookup = release_lookup(&evidence.title).unwrap();
    let recordings = serde_json::json!({
        "release_mbid": evidence.release_mbid, "recordings": evidence.recordings
    })
    .to_string();
    let verify = |release, recordings: &str| {
        validate_mirror_evidence(
            &evidence.title,
            release,
            CompleteReleaseCandidates {
                lookup: &lookup,
                hits: evidence.exact_title_candidates.clone(),
            },
            recordings,
            FileNumbering {
                disc: Some("1"),
                track: Some("1"),
            },
        )
    };
    assert_eq!(verify(RELEASE, &recordings), Ok(()));
    assert_eq!(verify(OTHER, &recordings), Err(TrialError::Refused));
    let wrong_track = recordings.replace(r#""track_no":1"#, r#""track_no":2"#);
    assert_eq!(verify(RELEASE, &wrong_track), Err(TrialError::Refused));
}

#[test]
fn fixture_scope_ids_require_rfc_variant_and_version_four() {
    assert!(uuid_v4(RELEASE).is_some());
    for id in [
        "c4000000-0000-7000-8000-000000000001",
        "c4000000-0000-4000-c000-000000000001",
        "c4000000000040008000000000000001",
    ] {
        assert!(uuid_v4(id).is_none());
    }
}
