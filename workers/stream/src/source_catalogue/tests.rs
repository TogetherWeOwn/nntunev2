use super::*;
use nntune_core::original_source::{service::MAX_GRANT_REMAINING_SECS, MAX_RECORD_BYTES};
use std::{
    future::Future,
    task::{Context, Poll},
};

const TRACK: &str = "01234567-89ab-cdef-0123-456789abcdef";
const OTHER: &str = "11234567-89ab-cdef-0123-456789abcdef";
const KEY: &[u8] = b"source-catalogue-test-key-0123456789";
const NOW: u64 = 1_800_000_000_000;

#[derive(Clone, Copy)]
enum Reply {
    Good,
    WrongTrack,
    WrongJob,
    Oversized,
    Invalid,
    Failed,
    Pending,
}

struct Transport<'a> {
    calls: Cell<usize>,
    now: &'a Cell<u64>,
    end_ms: Cell<u64>,
    dropped: Cell<bool>,
    reply: Reply,
}

impl<'a> Transport<'a> {
    fn new(now: &'a Cell<u64>, reply: Reply) -> Self {
        Self {
            calls: Cell::new(0),
            now,
            end_ms: Cell::new(NOW),
            dropped: Cell::new(false),
            reply,
        }
    }
}

fn payload(track: &str) -> Payload {
    Payload {
        source_job_id: "job-source".into(),
        bytes: serde_json::to_vec(&serde_json::json!({
            "track_id": track, "source_job_id": "job-source", "file_ordinal": 0,
            "file_size": 10, "article_count": 1,
            "articles": [{"number": 1, "reference": "source@test.invalid", "encoded_bytes": 100}],
            "range_hint": null
        }))
        .unwrap(),
    }
}

struct Dropped<'a>(&'a Cell<bool>);
impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

impl SourceTransport for Transport<'_> {
    async fn fetch(
        &self,
        authorization: &str,
        body: &[u8],
        deadline_ms: u64,
    ) -> Result<Payload, CatalogueUnavailable> {
        self.calls.set(self.calls.get() + 1);
        Channel::OriginalSource
            .verify(
                Some(KEY),
                PATH,
                Some(authorization),
                body,
                self.now.get() / 1000,
            )
            .unwrap();
        assert!(Channel::Resolver
            .verify(
                Some(KEY),
                PATH,
                Some(authorization),
                body,
                self.now.get() / 1000
            )
            .is_err());
        assert!(Channel::Account
            .verify(
                Some(KEY),
                PATH,
                Some(authorization),
                body,
                self.now.get() / 1000
            )
            .is_err());
        let lookup: Lookup = serde_json::from_slice(body).unwrap();
        assert_eq!(lookup.track_id, TRACK);
        assert!(lookup.valid_at(self.now.get()));
        assert_eq!(deadline_ms, lookup.deadline_ms);
        assert!(!String::from_utf8_lossy(body).contains("user"));
        assert!(!String::from_utf8_lossy(body).contains("play-id"));
        self.now.set(self.end_ms.get());
        let mut out = payload(TRACK);
        match self.reply {
            Reply::Good => {}
            Reply::WrongTrack => out = payload(OTHER),
            Reply::WrongJob => out.source_job_id = "other-job".into(),
            Reply::Oversized => out.bytes = vec![b' '; MAX_RECORD_BYTES + 1],
            Reply::Invalid => out.bytes = b"{}".to_vec(),
            Reply::Failed => return Err(CatalogueUnavailable),
            Reply::Pending => {
                let _dropped = Dropped(&self.dropped);
                std::future::pending::<()>().await;
            }
        }
        Ok(out)
    }
}

fn token(exp: u64) -> String {
    grant::issue_v2("user", TRACK, "play-id", 1, exp, KEY)
}

#[tokio::test]
async fn grant_bound_lookup_uses_shared_channel_and_selected_job_without_cache() {
    let now = Cell::new(NOW);
    let transport = Transport::new(&now, Reply::Good);
    let token = token(NOW / 1000 + 600);
    let catalogue = SourceCatalogue::new(&transport, &token, TRACK, KEY, || now.get())
        .ok()
        .unwrap();
    for _ in 0..2 {
        let source = catalogue.source(TRACK).await.ok().flatten().unwrap();
        assert_eq!(source.track_id(), TRACK);
        assert_eq!(source.source_job_id(), "job-source");
        assert_eq!(source.article_count(), 1);
    }
    assert_eq!(transport.calls.get(), 2);
}

#[tokio::test]
async fn invalid_v1_expired_wrong_track_and_short_key_never_call_service() {
    let now = Cell::new(NOW);
    let transport = Transport::new(&now, Reply::Good);
    for token in [
        "invalid".into(),
        grant::issue("user", TRACK, NOW / 1000 + 10, KEY),
        token(NOW / 1000),
        token(NOW / 1000 + 961),
        grant::issue_v2("user", OTHER, "play-id", 1, NOW / 1000 + 10, KEY),
    ] {
        assert!(SourceCatalogue::new(&transport, &token, TRACK, KEY, || now.get()).is_err());
    }
    let token = token(NOW / 1000 + 600);
    assert!(SourceCatalogue::new(&transport, &token, TRACK, b"short", || now.get()).is_err());
    assert!(SourceCatalogue::new(&transport, &token, "bad-track", KEY, || now.get()).is_err());
    assert_eq!(transport.calls.get(), 0);
}

#[tokio::test]
async fn cross_track_query_and_authority_expiring_before_lookup_do_no_io() {
    let now = Cell::new(NOW);
    let transport = Transport::new(&now, Reply::Good);
    let token = token(NOW / 1000 + 1);
    let catalogue = SourceCatalogue::new(&transport, &token, TRACK, KEY, || now.get())
        .ok()
        .unwrap();
    assert!(catalogue.source(OTHER).await.is_err());
    now.set(NOW + 1000);
    assert!(catalogue.source(TRACK).await.is_err());
    assert_eq!(transport.calls.get(), 0);
}

#[tokio::test]
async fn catalogue_rejects_clock_regression_before_io() {
    let now = Cell::new(NOW);
    let transport = Transport::new(&now, Reply::Good);
    let token = token(NOW / 1000 + 600);
    let catalogue = SourceCatalogue::new(&transport, &token, TRACK, KEY, || now.get())
        .ok()
        .unwrap();
    now.set(NOW - 1);
    assert!(catalogue.source(TRACK).await.is_err());
    // A rejected call must not lower the retained clock high-water mark.
    assert!(catalogue.source(TRACK).await.is_err());
    assert_eq!(transport.calls.get(), 0);
}

#[tokio::test]
async fn late_or_backwards_reply_is_never_accepted_or_retried() {
    for end in [NOW - 1, NOW + MAX_LOOKUP_MS, NOW + MAX_LOOKUP_MS + 1] {
        let now = Cell::new(NOW);
        let transport = Transport::new(&now, Reply::Good);
        transport.end_ms.set(end);
        let token = token(NOW / 1000 + 600);
        let catalogue = SourceCatalogue::new(&transport, &token, TRACK, KEY, || now.get())
            .ok()
            .unwrap();
        assert!(catalogue.source(TRACK).await.is_err());
        assert_eq!(transport.calls.get(), 1);
    }
}

#[tokio::test]
async fn malformed_wrong_bound_and_failed_replies_are_generic_without_retry() {
    for reply in [
        Reply::WrongTrack,
        Reply::WrongJob,
        Reply::Oversized,
        Reply::Invalid,
        Reply::Failed,
    ] {
        let now = Cell::new(NOW);
        let transport = Transport::new(&now, reply);
        let token = token(NOW / 1000 + 600);
        let catalogue = SourceCatalogue::new(&transport, &token, TRACK, KEY, || now.get())
            .ok()
            .unwrap();
        assert!(catalogue.source(TRACK).await.is_err());
        assert_eq!(transport.calls.get(), 1);
    }
}

#[tokio::test]
async fn grant_expiry_caps_lookup_deadline_even_when_channel_signature_is_fresh() {
    let now = Cell::new(NOW + 999);
    let transport = Transport::new(&now, Reply::Good);
    transport.end_ms.set(NOW + 1000);
    let token = token(NOW / 1000 + 1);
    let catalogue = SourceCatalogue::new(&transport, &token, TRACK, KEY, || now.get())
        .ok()
        .unwrap();
    assert!(catalogue.source(TRACK).await.is_err());
    assert_eq!(transport.calls.get(), 1);
}

#[test]
fn cancellation_drops_pending_catalogue_transport() {
    let now = Cell::new(NOW);
    let transport = Transport::new(&now, Reply::Pending);
    let token = token(NOW / 1000 + 600);
    let catalogue = SourceCatalogue::new(&transport, &token, TRACK, KEY, || now.get())
        .ok()
        .unwrap();
    let mut future = Box::pin(catalogue.source(TRACK));
    let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
    assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
    drop(future);
    assert!(transport.dropped.get());
    assert_eq!(transport.calls.get(), 1);
}

#[test]
fn lookup_bounds_include_grant_expiry_and_reject_overflow_edges() {
    let mut lookup = Lookup {
        track_id: TRACK.into(),
        issued_at_ms: NOW,
        deadline_ms: NOW + MAX_LOOKUP_MS,
        grant_expires_at: NOW / 1000 + MAX_GRANT_REMAINING_SECS,
    };
    assert!(lookup.valid_at(NOW));
    lookup.deadline_ms += 1;
    assert!(!lookup.valid_at(NOW));
    lookup.deadline_ms = NOW + MAX_LOOKUP_MS;
    lookup.grant_expires_at += 1;
    assert!(!lookup.valid_at(NOW));
    lookup.grant_expires_at = u64::MAX;
    lookup.issued_at_ms = u64::MAX - 1;
    lookup.deadline_ms = u64::MAX;
    assert!(!lookup.valid_at(u64::MAX - 1));
}
