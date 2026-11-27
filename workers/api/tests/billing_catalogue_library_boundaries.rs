use nntune_api::billing::{resolve_effective_plan, SubscriptionView};
use nntune_api::billing_reconcile::RENEWAL_GRACE_SECS;
use nntune_api::catalogue::{assemble_release_detail, ReleaseDbRow, ReleaseTrackDbRow};
use nntune_api::library::{prepare_genre_insert, prepare_playlist_insert, LibraryError};
use nntune_api::native_rest::{
    authorize, issue_credential, scope_for_operation, CredentialError, NativeRestRecord, Pepper,
    RouteDecision, Scope, MAX_CREDENTIAL_TTL_SECS,
};
use nntune_api::target_identity::{canonical_target_id, MAX_TARGET_ID_LEN};

#[test]
fn renewal_grace_never_outlasts_the_paid_end_plus_seven_days() {
    let paid = 1_900_000_000;
    let end = paid + RENEWAL_GRACE_SECS;
    let subscription = SubscriptionView {
        plan_id: "plus",
        status: "past_due",
        cancel_at_period_end: false,
        paid_through_at_unix: Some(paid),
        grace_until_unix: Some(i64::MAX),
        current_period_end_unix: None,
    };
    assert!(resolve_effective_plan(Some(subscription), end - 1).entitled);
    assert!(!resolve_effective_plan(Some(subscription), end).entitled);
}

#[test]
fn release_detail_omits_art_for_malformed_release_identity() {
    let release = ReleaseDbRow {
        mbid: "../me".into(),
        title: "Synthetic release".into(),
        release_type: None,
        year: None,
        artist_mbid: None,
        artist_name: None,
    };
    let tracks = [ReleaseTrackDbRow {
        recording_mbid: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
        title: "Synthetic track".into(),
        duration_ms: None,
        medium_no: Some(1),
        track_no: Some(1),
    }];
    let detail = assemble_release_detail(&release, &tracks);
    assert_eq!(detail.release.art_url, None);
    assert_eq!(detail.tracks[0].art_url, None);
}

#[test]
fn library_rejects_nul_before_constructing_a_write() {
    assert!(matches!(
        prepare_genre_insert(Some("member"), "Genre\0name", false, 0),
        Err(LibraryError::Invalid)
    ));
    assert!(matches!(
        prepare_playlist_insert(
            Some("member"),
            "Valid name",
            vec!["track\0ref".into()],
            false,
            0,
        ),
        Err(LibraryError::Invalid)
    ));
}

#[test]
fn bearer_scope_requires_a_listed_path_and_valid_target_segment() {
    for (method, path) in [
        ("GET", "/v1/playlists/list/tracks"),
        ("GET", "/v1/releases/%2F"),
        ("GET", "/v1/stream/track/metadata"),
        ("POST", "/v1/ingest/not-listed"),
        ("DELETE", "/v1/credentials/id/extra"),
    ] {
        assert_eq!(scope_for_operation(method, path), RouteDecision::Deny);
    }
    let opaque = "Track-A_2";
    assert_eq!(canonical_target_id(opaque), opaque);
    assert_eq!(
        scope_for_operation("GET", &format!("/v1/stream/{opaque}")),
        RouteDecision::Bearer(Scope::StreamRead)
    );
    let oversized = "a".repeat(MAX_TARGET_ID_LEN + 1);
    assert_eq!(
        scope_for_operation("GET", &format!("/v1/stream/{oversized}")),
        RouteDecision::Deny
    );
}

#[test]
fn native_credential_rejects_invalid_stored_lifetimes() {
    let peppers = [Pepper {
        version: 1,
        bytes: &[7; 32],
    }];
    let mut issued =
        issue_credential("member", &[Scope::LibraryRead], 60, 100, &peppers[0]).unwrap();
    let authorize_at = |record: &NativeRestRecord, now| {
        authorize(
            record,
            &issued.display_once,
            Scope::LibraryRead,
            now,
            "member",
            &peppers,
        )
    };
    assert_eq!(authorize_at(&issued.record, 160), Ok(()));
    let denied = Err(CredentialError::Unauthenticated);
    assert_eq!(authorize_at(&issued.record, 99), denied);
    issued.record.expires_at = 101 + MAX_CREDENTIAL_TTL_SECS;
    assert_eq!(authorize_at(&issued.record, 100), denied);
}
