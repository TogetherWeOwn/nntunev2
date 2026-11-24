use super::*;
use crate::{meter_sink::TrackMeterMeta, play_grant::IssuedGrant, playback::LeaseRow};

pub(super) const PLAY: &str = "private_1234567890abcdefghijkl";
pub(super) const RANGE: Option<&str> = Some("bytes=2-7");
pub(super) const EXPECTED: &str =
    include_str!("../../../../fixtures/play-grant/private-enrolled.token");

pub(super) fn receipt() -> (Harness, Request) {
    let mut h = harness();
    const USER: &str = "member";
    const TRACK: &str = "track";
    h.config = cfg().with_play_grant_secret(b"test only, not a deployed secret".to_vec());
    h.config.private_listening = crate::private_listening::PrivateListening::parse(
        r#"{"owner_user_id":"member","invite_ids":[],"expires_at":220}"#,
    );
    seed_user(&h, USER, "test@example.com");
    let ctx = context(&h);
    let req = Request {
        method: "GET".into(),
        path: format!("/v1/plays/media/{TRACK}/tab-1/{PLAY}"),
        cookie: block_on(ctx.start_session(USER)).unwrap().set_cookie,
        origin: None,
        authorization: None,
        stripe_signature: None,
        body: Vec::new(),
    };
    let (claims, _) = block_on(ctx.authenticate(&req, false)).unwrap();
    let lease = LeaseRow {
        session_id: "tab-1".into(),
        track_id: TRACK.into(),
        play_id: PLAY.into(),
        epoch: 42,
        expires_at: 12,
        continuation_deadline: 222,
    };
    h.store
        .playback_leases
        .borrow_mut()
        .insert(USER.into(), lease.clone());
    h.store.meter_meta.borrow_mut().insert(
        (USER.into(), TRACK.into()),
        TrackMeterMeta {
            track_duration_ms: Some(60_000),
            ..Default::default()
        },
    );
    h.store
        .browser_receipt
        .replace(Some(browser_playback_relay::BrowserReceipt {
            user_id: USER.into(),
            session_binding: session_binding(&claims),
            lease,
            artifact_sha256: "e96d0ce34def34abaa429581f626f574ac45129a52459a35dc2d78acacf26bd3"
                .into(),
            duration_ms: 60_000.try_into().unwrap(),
            grant: IssuedGrant {
                token: EXPECTED.into(),
                exp_unix: 12,
            },
        }));
    (h, req)
}

pub(super) fn context(h: &Harness) -> Ctx<'_, FakeStore, FakeMailer, FakeDialer, FakeDns> {
    Ctx { now: 10, ..h.ctx() }
}

pub(super) fn shorten_lease(h: &Harness) {
    h.store
        .playback_leases
        .borrow_mut()
        .get_mut("member")
        .unwrap()
        .expires_at = 11;
}
