use nntune_api::{
    catalogue::TrackDbRow,
    native_rest::Pepper,
    subsonic_auth::AuthMode,
    subsonic_dispatch::snapshot_order::OrderWork,
    subsonic_experiment_gate::{Deployment, Lease},
    subsonic_lifecycle::{issue, LifecycleError},
    subsonic_snapshot_plan::build_owned_bounded_snapshot,
};
use nntune_subsonic::construction_budget::ConstructionBudget;

#[test]
fn lease_requires_a_bare_canonical_staging_host() {
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    let raw = format!("v1:{SHA}:1000:2200:fixture-user");
    for (host, allowed) in [
        ("stage.example.test", true),
        ("Stage.EXAMPLE.test", false),
        ("stage.example.test:8443", false),
        ("stage..example.test", false),
        ("-stage.example.test", false),
        ("127.0.0.1", false),
        ("0x7f000001", false),
    ] {
        let deployment = Deployment {
            is_staging: true,
            staging_host: host,
            revision: SHA,
            now_utc_seconds: 1000,
        };
        assert_eq!(
            Lease::admit(&deployment, Some(host), Some("1"), Some(&raw)).is_some(),
            allowed
        );
    }
}

#[test]
fn issuance_rejects_raw_label_controls_and_out_of_bounds_configuration() {
    let issue_with = |label, now, bytes| {
        let pepper = Pepper { version: 1, bytes };
        issue(
            "fixture-user",
            label,
            AuthMode::ApiKey,
            now,
            &pepper,
            0,
            &[0; 32],
        )
    };
    let pepper = [0x31; 65];
    for (label, now, bytes) in [
        (" Phone\n", 1, &pepper[..32]),
        ("\tPhone", 1, &pepper[..32]),
        ("Phone", 253_402_300_800, &pepper[..32]),
        ("Phone", 1, pepper.as_slice()),
    ] {
        assert_eq!(
            issue_with(label, now, bytes).unwrap_err(),
            LifecycleError::Invalid
        );
    }
    let issued = issue_with(" Phone ", 253_402_300_799, &pepper[..64]).unwrap();
    assert_eq!(issued.prepared.label, "Phone");
}

#[test]
fn snapshot_refuses_negative_duration_before_debiting_existing_budget() {
    let mut track = TrackDbRow {
        release_mbid: "release".into(),
        recording_mbid: "recording".into(),
        title: "Title".into(),
        duration_ms: Some(-1),
        medium_no: None,
        track_no: None,
        release_title: "Album".into(),
        artist_mbid: None,
        artist_name: None,
    };
    let work = OrderWork {
        comparisons: 0,
        artist_row_probes: 0,
    };
    let mut budget = ConstructionBudget::new(4096);
    budget.charge_payload_bytes(9).unwrap();
    let build = |budget: &mut ConstructionBudget, track: &TrackDbRow| {
        build_owned_bounded_snapshot(
            budget,
            &[],
            &[],
            std::slice::from_ref(track),
            work,
            256,
            512,
        )
    };
    assert!(build(&mut budget, &track).is_err());
    assert_eq!(budget.charged(), 9);
    track.duration_ms = Some(0);
    let snapshot = build(&mut budget, &track).unwrap();
    assert_eq!(snapshot.2[0].duration, 0);
}
