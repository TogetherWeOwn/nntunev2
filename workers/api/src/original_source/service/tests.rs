//! DB-free tests for the off-route source service: a lazy fake factory and a
//! sealed in-memory repository prove ordering (auth before connect), deadline,
//! clock and cancellation behaviour without any network or database.

use std::{
    cell::{Cell, RefCell},
    future::Future,
    rc::Rc,
    time::Duration,
};

use nntune_core::{
    nzb::{File, Segment},
    original_source::{
        service::{MAX_LOOKUP_MS, PATH},
        OriginalSource, RangeHint,
    },
    pool_channel::Channel,
};
use nntune_ingest::{
    matching::MbRecording,
    track_writers::{self, SourceTrack},
};
use serde_json::{json, Value};

use super::{read, read_timed, Call, RepositoryFactory};
use crate::{
    field_envelope::FieldEnvelope,
    original_source::{
        seal_original_source, AsyncSourceRepository, SourceMetadata, SourceServiceError,
    },
    rotation::KekSource,
};
use nntune_core::original_source::service::Payload;

const A: &str = "00000000-0000-0000-0000-000000000001";
const RELEASE: &str = "00000000-0000-0000-0000-000000000003";
const RECORDING: &str = "00000000-0000-0000-0000-000000000004";
const JOB: &str = "job_generated";
const NOW: u64 = 1_800_000_000_000;
const KEY: &[u8] = &[7u8; 64];

struct Keys;
impl KekSource for Keys {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        (version == 7).then_some(&[19u8; 32])
    }
}

fn source() -> OriginalSource {
    let files: Vec<File> = (0..4)
        .map(|i| File {
            ordinal: i,
            subject: String::new(),
            filename: None,
            role: if i % 2 == 0 { "nfo" } else { "content" }.into(),
            container: if i % 2 == 0 { "other" } else { "flac" }.into(),
            posted_at_unix: 0,
            segments: vec![Segment {
                ordinal: 0,
                number: 1,
                message_id: format!("generated-file-{i}@fixture.invalid"),
                encoded_bytes: 128,
            }],
        })
        .collect();
    let recordings = vec![MbRecording {
        mbid: RECORDING.into(),
        title: "Generated one".into(),
        medium_no: Some(1),
        track_no: Some(1),
    }];
    let track = SourceTrack {
        id: A,
        release_id: RELEASE,
        medium_no: 1,
        track_no: 1,
        recording_id: RECORDING,
    };
    let hint = RangeHint {
        part_size: 3,
        final_begin: 0,
        final_end: 3,
    };
    track_writers::original_source_for_track(
        JOB,
        RELEASE,
        &files,
        &recordings,
        &track,
        3,
        Some(hint),
    )
    .unwrap()
}

/// Sets a flag when dropped, proving the fake's pending work was cancelled.
struct DropFlag(Rc<Cell<bool>>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

/// Clock mutation applied when the named stage is reached.
type Hook = Option<(&'static str, u64)>;

struct Repo {
    rows: [Option<SourceMetadata>; 2],
    envelope: Option<FieldEnvelope>,
    clock: Rc<Cell<u64>>,
    hook: Hook,
    calls: Rc<RefCell<Vec<&'static str>>>,
    pend_metadata: bool,
    _drop: DropFlag,
}

impl Repo {
    fn hit(&self, stage: &'static str) {
        self.calls.borrow_mut().push(stage);
        if let Some((s, t)) = self.hook {
            if s == stage {
                self.clock.set(t);
            }
        }
    }
}

impl AsyncSourceRepository for Repo {
    async fn metadata(
        &mut self,
        _: &str,
    ) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
        self.hit("metadata");
        if self.pend_metadata {
            std::future::pending::<()>().await;
        }
        Ok(self.rows.clone())
    }
    async fn envelope(
        &mut self,
        _: &SourceMetadata,
    ) -> Result<Option<FieldEnvelope>, SourceServiceError> {
        self.hit("envelope");
        Ok(self.envelope.clone())
    }
}

struct Factory {
    clock: Rc<Cell<u64>>,
    connects: Cell<usize>,
    hook: Hook,
    pend_connect: bool,
    pend_metadata: bool,
    calls: Rc<RefCell<Vec<&'static str>>>,
    dropped_connect: Rc<Cell<bool>>,
    dropped_repo: Rc<Cell<bool>>,
    rows: [Option<SourceMetadata>; 2],
    envelope: Option<FieldEnvelope>,
}

impl Factory {
    fn new() -> Self {
        let sealed = seal_original_source(&Keys, 7, &source()).unwrap();
        Self {
            clock: Rc::new(Cell::new(NOW)),
            connects: Cell::new(0),
            hook: None,
            pend_connect: false,
            pend_metadata: false,
            calls: Default::default(),
            dropped_connect: Default::default(),
            dropped_repo: Default::default(),
            rows: [Some(sealed.metadata), None],
            envelope: Some(sealed.envelope),
        }
    }
}

impl RepositoryFactory for Factory {
    type Repository = Repo;
    async fn connect(&self) -> Result<Repo, SourceServiceError> {
        self.connects.set(self.connects.get() + 1);
        if self.pend_connect {
            let _guard = DropFlag(self.dropped_connect.clone());
            std::future::pending::<()>().await;
        }
        if let Some(("connect", t)) = self.hook {
            self.clock.set(t);
        }
        Ok(Repo {
            rows: self.rows.clone(),
            envelope: self.envelope.clone(),
            clock: self.clock.clone(),
            hook: self.hook,
            calls: self.calls.clone(),
            pend_metadata: self.pend_metadata,
            _drop: DropFlag(self.dropped_repo.clone()),
        })
    }
}

fn lookup() -> Value {
    json!({
        "track_id": A,
        "grant_expires_at": NOW / 1000 + 10,
        "issued_at_ms": NOW,
        "deadline_ms": NOW + 1000,
    })
}

fn lookup_with(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v = lookup();
    edit(&mut v);
    serde_json::to_vec(&v).unwrap()
}

struct Req {
    method: &'static str,
    path: &'static str,
    auth: Option<String>,
    body: Vec<u8>,
    browser: bool,
    key: Option<&'static [u8]>,
}

impl Req {
    fn signed(body: Vec<u8>) -> Self {
        let auth = Channel::OriginalSource.authorization_header(Some(KEY), PATH, NOW / 1000, &body);
        Self {
            method: "POST",
            path: PATH,
            auth,
            body,
            browser: false,
            key: Some(KEY),
        }
    }
    fn valid() -> Self {
        Self::signed(serde_json::to_vec(&lookup()).unwrap())
    }
    async fn send_timed(&self, f: &Factory) -> Result<Payload, SourceServiceError> {
        let clock = f.clock.clone();
        read_timed(
            f,
            &Keys,
            self.key,
            Call {
                method: self.method,
                path: self.path,
                authorization: self.auth.as_deref(),
                body: &self.body,
                browser_context: self.browser,
            },
            move || clock.get(),
        )
        .await
    }

    async fn send(
        &self,
        f: &Factory,
        deadline: impl Future<Output = ()>,
    ) -> Result<Payload, SourceServiceError> {
        let clock = f.clock.clone();
        let call = Call {
            method: self.method,
            path: self.path,
            authorization: self.auth.as_deref(),
            body: &self.body,
            browser_context: self.browser,
        };
        read(f, &Keys, self.key, call, move || clock.get(), deadline).await
    }
}

fn never() -> impl Future<Output = ()> {
    std::future::pending()
}

fn denied<T>(r: Result<T, SourceServiceError>) -> bool {
    matches!(r, Err(SourceServiceError::Denied))
}

#[tokio::test]
async fn timed_lookup_uses_the_same_authenticated_read() {
    let f = Factory::new();
    let payload = Req::valid().send_timed(&f).await.ok().unwrap();
    assert_eq!(payload.bytes, source().encode().unwrap());
    assert_eq!(f.connects.get(), 1);
    assert!(f.dropped_repo.get());
}

#[tokio::test]
async fn timed_lookup_denies_before_connect_or_timer_io() {
    let f = Factory::new();
    let mut request = Req::valid();
    request.auth = None;
    assert!(denied(request.send_timed(&f).await));
    let request = Req::signed(lookup_with(|v| v["deadline_ms"] = json!(NOW)));
    assert!(denied(request.send_timed(&f).await));
    assert_eq!(f.connects.get(), 0);
}

#[tokio::test]
async fn signed_remaining_timer_drops_stalled_connect_and_query() {
    for connect in [true, false] {
        let mut f = Factory::new();
        f.pend_connect = connect;
        f.pend_metadata = !connect;
        let request = Req::signed(lookup_with(|v| {
            v["issued_at_ms"] = json!(NOW - 4990);
            v["deadline_ms"] = json!(NOW + 10);
        }));
        // Frozen wall-clock authority remains valid: only the real timer can
        // terminate this. A reset to MAX_LOOKUP_MS would miss the outer bound.
        let result = tokio::time::timeout(Duration::from_millis(500), request.send_timed(&f))
            .await
            .expect("signed remaining budget, not a fresh five seconds");
        assert!(denied(result));
        assert_eq!(f.connects.get(), 1);
        assert!(if connect {
            f.dropped_connect.get()
        } else {
            f.dropped_repo.get()
        });
    }
}

#[tokio::test]
async fn caller_cancels_the_timed_connect_without_a_driver_task() {
    let mut f = Factory::new();
    f.pend_connect = true;
    let request = Req::valid();
    {
        let pending = request.send_timed(&f);
        tokio::pin!(pending);
        assert!(futures_util::poll!(&mut pending).is_pending());
        assert!(!f.dropped_connect.get());
    }
    assert!(f.dropped_connect.get());
}

#[tokio::test]
async fn timed_lookup_rejects_rollback_after_connect() {
    let mut f = Factory::new();
    f.hook = Some(("connect", NOW - 1));
    assert!(denied(Req::valid().send_timed(&f).await));
    assert_eq!(f.connects.get(), 1);
    assert!(f.calls.borrow().is_empty());
    assert!(f.dropped_repo.get());
}

#[tokio::test]
async fn signed_lookup_returns_the_sealed_source_payload() {
    let f = Factory::new();
    let payload = Req::valid().send(&f, never()).await.ok().unwrap();
    assert_eq!(payload.source_job_id, JOB);
    assert_eq!(payload.bytes, source().encode().unwrap());
    assert_eq!(payload.parse(A).ok().unwrap().track_id(), A);
    assert_eq!(f.connects.get(), 1);
    assert_eq!(*f.calls.borrow(), ["metadata", "envelope"]);
}

#[tokio::test]
async fn unauthenticated_or_misrouted_calls_never_connect() {
    let other: &'static [u8] = &[9u8; 64];
    let short: &'static [u8] = b"short";
    let mut cases: Vec<(&str, Req)> = vec![
        (
            "missing key",
            Req {
                key: None,
                ..Req::valid()
            },
        ),
        (
            "short key",
            Req {
                key: Some(short),
                ..Req::valid()
            },
        ),
        (
            "wrong key",
            Req {
                key: Some(other),
                ..Req::valid()
            },
        ),
        (
            "missing header",
            Req {
                auth: None,
                ..Req::valid()
            },
        ),
    ];
    let mut forged = Req::valid();
    let sig = forged.auth.take().unwrap();
    let flipped = if sig.ends_with('0') { '1' } else { '0' };
    forged.auth = Some(format!("{}{flipped}", &sig[..sig.len() - 1]));
    cases.push(("bad signature", forged));
    let mut swapped = Req::valid();
    swapped.body = lookup_with(|v| v["deadline_ms"] = json!(NOW + 2000));
    cases.push(("body not signed", swapped));
    cases.push((
        "wrong method",
        Req {
            method: "GET",
            ..Req::valid()
        },
    ));
    cases.push((
        "wrong path",
        Req {
            path: "/internal/v1/other",
            ..Req::valid()
        },
    ));
    cases.push((
        "browser context",
        Req {
            browser: true,
            ..Req::valid()
        },
    ));
    for (name, req) in cases {
        let f = Factory::new();
        assert!(denied(req.send(&f, never()).await), "{name}");
        assert_eq!(f.connects.get(), 0, "{name}");
    }
}

#[tokio::test]
async fn signed_malformed_or_invalid_lookups_never_connect() {
    let valid = serde_json::to_string(&lookup()).unwrap();
    let mut bodies: Vec<(&str, Vec<u8>)> = vec![
        ("not json", b"{nope".to_vec()),
        ("array", format!("[{valid}]").into_bytes()),
        ("scalar", b"7".to_vec()),
        ("empty", Vec::new()),
        (
            "duplicate field",
            format!(r#"{{"track_id":"{A}","track_id":"{A}","grant_expires_at":{},"issued_at_ms":{NOW},"deadline_ms":{}}}"#, NOW / 1000 + 10, NOW + 1000).into_bytes(),
        ),
        ("unknown field", lookup_with(|v| v["user_id"] = json!("u"))),
        ("missing field", lookup_with(|v| { v.as_object_mut().unwrap().remove("deadline_ms"); })),
        ("invalid track", lookup_with(|v| v["track_id"] = json!("not-a-track"))),
        ("uppercase track", lookup_with(|v| v["track_id"] = json!(A.replace('0', "A")))),
        ("expired", lookup_with(|v| { v["issued_at_ms"] = json!(NOW - 500); v["deadline_ms"] = json!(NOW); })),
        ("future", lookup_with(|v| { v["issued_at_ms"] = json!(NOW + 1); v["deadline_ms"] = json!(NOW + 1000); })),
        ("overlong", lookup_with(|v| v["deadline_ms"] = json!(NOW + MAX_LOOKUP_MS + 1))),
        ("deadline after grant", lookup_with(|v| v["grant_expires_at"] = json!(NOW / 1000))),
        ("grant expired", lookup_with(|v| { v["grant_expires_at"] = json!(NOW / 1000 - 1); })),
        ("grant too far", lookup_with(|v| v["grant_expires_at"] = json!(NOW / 1000 + 961))),
    ];
    bodies.push(("padded non-object", b"  \n\"x\"".to_vec()));
    for (name, body) in bodies {
        let f = Factory::new();
        assert!(denied(Req::signed(body).send(&f, never()).await), "{name}");
        assert_eq!(f.connects.get(), 0, "{name}");
        assert!(f.calls.borrow().is_empty(), "{name}");
    }
}

#[tokio::test]
async fn ready_deadline_prevents_connect() {
    let f = Factory::new();
    assert!(denied(Req::valid().send(&f, std::future::ready(())).await));
    assert_eq!(f.connects.get(), 0);
}

#[tokio::test]
async fn expiry_or_clock_rollback_at_any_stage_denies_further_work() {
    // (stage, calls observed before the following authorization check denies)
    let stages: [(&str, &[&str]); 3] = [
        ("connect", &[]),
        ("metadata", &["metadata"]),
        ("envelope", &["metadata", "envelope"]),
    ];
    for (stage, expected) in stages {
        for (label, time) in [("expiry", NOW + 1000), ("rollback", NOW - 1)] {
            let mut f = Factory::new();
            f.hook = Some((stage, time));
            assert!(
                denied(Req::valid().send(&f, never()).await),
                "{stage} {label}"
            );
            assert_eq!(*f.calls.borrow(), expected, "{stage} {label}");
            assert_eq!(f.connects.get(), 1, "{stage} {label}");
        }
    }
}

#[tokio::test]
async fn cancelling_a_pending_connect_drops_it() {
    let mut f = Factory::new();
    f.pend_connect = true;
    let req = Req::valid();
    {
        let mut op = Box::pin(req.send(&f, never()));
        assert!(futures_util::poll!(&mut op).is_pending());
        assert!(!f.dropped_connect.get());
    }
    assert!(f.dropped_connect.get());
    assert_eq!(f.connects.get(), 1);
    assert!(f.calls.borrow().is_empty());
}

#[tokio::test]
async fn cancelling_pending_repository_work_drops_the_repository() {
    let mut f = Factory::new();
    f.pend_metadata = true;
    let req = Req::valid();
    {
        let mut op = Box::pin(req.send(&f, never()));
        assert!(futures_util::poll!(&mut op).is_pending());
        assert!(!f.dropped_repo.get());
    }
    assert!(f.dropped_repo.get());
    assert_eq!(*f.calls.borrow(), ["metadata"]);
}

#[tokio::test]
async fn deadline_firing_mid_operation_drops_work_and_denies() {
    let mut f = Factory::new();
    f.pend_metadata = true;
    let fired = Rc::new(Cell::new(false));
    let flag = fired.clone();
    let deadline = std::future::poll_fn(move |_| {
        if flag.get() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    });
    let req = Req::valid();
    let mut op = Box::pin(req.send(&f, deadline));
    assert!(futures_util::poll!(&mut op).is_pending());
    fired.set(true);
    match futures_util::poll!(&mut op) {
        std::task::Poll::Ready(r) => assert!(denied(r)),
        std::task::Poll::Pending => panic!("deadline must end the operation"),
    }
    drop(op);
    assert!(f.dropped_repo.get());
    assert_eq!(*f.calls.borrow(), ["metadata"]);
}

#[tokio::test]
async fn bad_envelope_fails_without_a_payload() {
    let mut f = Factory::new();
    f.envelope.as_mut().unwrap().ciphertext[0] ^= 1;
    let r = Req::valid().send(&f, never()).await;
    assert!(matches!(r, Err(SourceServiceError::Crypto)));
    assert_eq!(*f.calls.borrow(), ["metadata", "envelope"]);
}

#[tokio::test]
async fn absent_snapshot_is_a_generic_missing_error() {
    let mut f = Factory::new();
    f.rows = [None, None];
    let r = Req::valid().send(&f, never()).await;
    assert!(matches!(r, Err(SourceServiceError::Missing)));
    assert_eq!(*f.calls.borrow(), ["metadata"]);

    let mut f = Factory::new();
    f.envelope = None;
    let r = Req::valid().send(&f, never()).await;
    assert!(matches!(r, Err(SourceServiceError::Missing)));
}
