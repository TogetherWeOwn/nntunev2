//! Native tests for the bounded exchange. The fakes model the Worker adapter
//! contract only; the source-shape tests at the bottom do NOT prove Worker
//! execution, only that the adapter text keeps its reviewed structure.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::{pending, Future},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

use futures_util::{join, poll};

use super::*;
use crate::account_pool::{with_session_deadline, SessionExpired};

const PROVIDER: &str = "01234567-89ab-cdef-0123-456789abcdef";
const JSON: &str = "application/json";
const OCTET: &str = "application/octet-stream";

#[derive(Default)]
struct Probe {
    posts: Cell<usize>,
    nexts: Cell<usize>,
    copies: Cell<usize>,
    remainings: RefCell<Vec<usize>>,
    post_drops: Cell<usize>,
    reply_drops: Cell<usize>,
}

struct Guard(Rc<Probe>, bool);
impl Drop for Guard {
    fn drop(&mut self) {
        let cell = if self.1 {
            &self.0.post_drops
        } else {
            &self.0.reply_drops
        };
        cell.set(cell.get() + 1);
    }
}

enum Step {
    /// Goes through the real native `copy_chunk` preflight.
    Chunk(Vec<u8>),
    /// Bypasses preflight, modelling an implementation violating the bound.
    Raw(Vec<u8>),
    Eof,
    Err,
    Pending,
}

enum PostStep {
    Reply(Head, Vec<Step>),
    Err,
    Pending,
}

struct Fake {
    probe: Rc<Probe>,
    post: RefCell<Option<PostStep>>,
}

struct FakeReply {
    head: Head,
    steps: VecDeque<Step>,
    probe: Rc<Probe>,
    _guard: Guard,
}

impl Client for Fake {
    type Reply = FakeReply;
    async fn post(&self, _authorization: &str, _body: &[u8]) -> Result<FakeReply, RegistryError> {
        self.probe.posts.set(self.probe.posts.get() + 1);
        let step = self.post.borrow_mut().take().expect("single post scripted");
        match step {
            PostStep::Err => Err(RegistryError),
            PostStep::Pending => {
                let _guard = Guard(self.probe.clone(), true);
                pending::<()>().await;
                Err(RegistryError)
            }
            PostStep::Reply(head, steps) => Ok(FakeReply {
                head,
                steps: steps.into(),
                probe: self.probe.clone(),
                _guard: Guard(self.probe.clone(), false),
            }),
        }
    }
}

impl Reply for FakeReply {
    fn head(&self) -> &Head {
        &self.head
    }
    async fn next(
        &mut self,
        remaining: usize,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, RegistryError> {
        self.probe.nexts.set(self.probe.nexts.get() + 1);
        self.probe.remainings.borrow_mut().push(remaining);
        match self.steps.pop_front() {
            None | Some(Step::Eof) => Ok(None),
            Some(Step::Err) => Err(RegistryError),
            Some(Step::Pending) => {
                pending::<()>().await;
                Ok(None)
            }
            Some(Step::Raw(bytes)) => Ok(Some(Zeroizing::new(bytes))),
            Some(Step::Chunk(bytes)) => {
                let probe = self.probe.clone();
                copy_chunk(bytes.len() as u32, remaining, || {
                    probe.copies.set(probe.copies.get() + 1);
                    bytes
                })
                .map(Some)
            }
        }
    }
}

fn head(content_type: &str) -> Head {
    Head {
        status: 200,
        content_type: content_type.into(),
        provider_id: None,
        cache_control: "no-store".into(),
        content_length: None,
        content_encoding: None,
    }
}

fn op_head(content_type: &str) -> Head {
    Head {
        provider_id: Some(PROVIDER.into()),
        ..head(content_type)
    }
}

fn fake(step: PostStep) -> Fake {
    Fake {
        probe: Rc::new(Probe::default()),
        post: RefCell::new(Some(step)),
    }
}

fn scripted(head: Head, steps: Vec<Step>) -> Fake {
    fake(PostStep::Reply(head, steps))
}

async fn run(fake: &Fake, kind: Kind) -> Result<Response, RegistryError> {
    exchange(fake, kind, "Bearer x", b"{}").await
}

fn is_err(result: Result<Response, RegistryError>) -> bool {
    result.err() == Some(RegistryError)
}

// ---- request caps ----

#[tokio::test]
async fn request_caps_are_per_kind_and_checked_before_post() {
    for (kind, limit) in [
        (Kind::Registry, MAX_REQUEST_BYTES),
        (Kind::Resolve, MAX_REQUEST_BYTES),
        (Kind::Candidate, MAX_REQUEST_BYTES),
        (Kind::Operation, MAX_OPERATION_BYTES),
    ] {
        let ct = JSON;
        let h = || {
            if matches!(kind, Kind::Operation) {
                op_head(ct)
            } else {
                head(ct)
            }
        };
        let ok = scripted(h(), vec![]);
        assert!(exchange(&ok, kind, "Bearer x", &vec![b'a'; limit])
            .await
            .is_ok());
        assert_eq!(ok.probe.posts.get(), 1);
        let over = scripted(h(), vec![]);
        assert!(is_err(
            exchange(&over, kind, "Bearer x", &vec![b'a'; limit + 1]).await
        ));
        assert_eq!(over.probe.posts.get(), 0);
    }
    const { assert!(MAX_OPERATION_BYTES > MAX_REQUEST_BYTES) };
}

#[tokio::test]
async fn authorization_bounds_are_checked_before_post() {
    for auth in ["".to_string(), "a".repeat(257)] {
        let f = scripted(head(JSON), vec![]);
        assert!(is_err(exchange(&f, Kind::Registry, &auth, b"{}").await));
        assert_eq!(f.probe.posts.get(), 0);
    }
    let f = scripted(head(JSON), vec![]);
    assert!(exchange(&f, Kind::Registry, &"a".repeat(256), b"{}")
        .await
        .is_ok());
}

// ---- response caps ----

#[test]
fn response_limits_match_kind_and_media() {
    let l = |k: Kind, ct: &str| k.response_limit(ct).ok();
    assert_eq!(l(Kind::Registry, JSON), Some(MAX_REGISTRY_BYTES));
    assert_eq!(l(Kind::Resolve, JSON), Some(MAX_DIAL_BYTES));
    assert_eq!(l(Kind::Candidate, JSON), Some(MAX_REQUEST_BYTES));
    assert_eq!(l(Kind::Operation, JSON), Some(MAX_OPERATION_BYTES));
    assert_eq!(
        l(Kind::Operation, OCTET),
        Some(crate::body::MAX_BODY_ARTICLE_BYTES)
    );
    for kind in [Kind::Registry, Kind::Resolve, Kind::Candidate] {
        assert_eq!(l(kind, OCTET), None);
    }
    for ct in [
        "",
        "text/plain",
        "Application/JSON",
        "application/json; charset=latin1",
        " application/json",
    ] {
        for kind in [
            Kind::Registry,
            Kind::Resolve,
            Kind::Candidate,
            Kind::Operation,
        ] {
            assert_eq!(l(kind, ct), None, "{ct:?}");
        }
    }
    assert_eq!(
        l(Kind::Registry, "application/json; charset=utf-8"),
        Some(MAX_REGISTRY_BYTES)
    );
    assert_eq!(
        l(Kind::Resolve, "application/json; charset=utf-8"),
        Some(MAX_DIAL_BYTES)
    );
    for kind in [Kind::Candidate, Kind::Operation] {
        assert_eq!(l(kind, "application/json; charset=utf-8"), None);
    }
}

#[tokio::test]
async fn exact_cap_then_eof_succeeds_for_every_kind() {
    let cases = [
        (Kind::Registry, JSON, MAX_REGISTRY_BYTES),
        (Kind::Resolve, JSON, MAX_DIAL_BYTES),
        (Kind::Candidate, JSON, MAX_REQUEST_BYTES),
        (Kind::Operation, JSON, MAX_OPERATION_BYTES),
        (Kind::Operation, OCTET, crate::body::MAX_BODY_ARTICLE_BYTES),
    ];
    for (kind, ct, cap) in cases {
        let h = if matches!(kind, Kind::Operation) {
            op_head(ct)
        } else {
            head(ct)
        };
        let half = cap / 2;
        let f = scripted(
            h,
            vec![
                Step::Chunk(vec![1; half]),
                Step::Chunk(vec![2; cap - half]),
                Step::Eof,
            ],
        );
        let response = run(&f, kind).await.expect("exact cap succeeds");
        assert_eq!(response.bytes.len(), cap);
        assert_eq!(*f.probe.remainings.borrow(), vec![cap, cap - half, 0]);
    }
}

#[tokio::test]
async fn one_byte_over_cap_fails_for_every_kind() {
    let cases = [
        (Kind::Registry, JSON, MAX_REGISTRY_BYTES),
        (Kind::Resolve, JSON, MAX_DIAL_BYTES),
        (Kind::Candidate, JSON, MAX_REQUEST_BYTES),
        (Kind::Operation, JSON, MAX_OPERATION_BYTES),
    ];
    for (kind, ct, cap) in cases {
        let h = if matches!(kind, Kind::Operation) {
            op_head(ct)
        } else {
            head(ct)
        };
        let f = scripted(
            h,
            vec![Step::Chunk(vec![1; cap]), Step::Chunk(vec![2]), Step::Eof],
        );
        assert!(is_err(run(&f, kind).await));
        // The over-cap chunk is refused before its copy closure runs.
        assert_eq!(f.probe.copies.get(), 1);
        assert_eq!(f.probe.remainings.borrow().last(), Some(&0));
    }
}

#[tokio::test]
async fn single_chunk_over_cap_never_copies() {
    let f = scripted(
        head(JSON),
        vec![Step::Chunk(vec![0; MAX_REQUEST_BYTES + 1])],
    );
    assert!(is_err(run(&f, Kind::Candidate).await));
    assert_eq!(f.probe.copies.get(), 0);
}

#[tokio::test]
async fn multichunk_success_concatenates_and_clones_head() {
    let mut h = head(JSON);
    h.content_length = Some("512".into());
    h.content_encoding = Some("identity".into());
    let f = scripted(
        h,
        vec![
            Step::Chunk(vec![1; 100]),
            Step::Chunk(vec![2; 200]),
            Step::Chunk(vec![3; 212]),
            Step::Eof,
        ],
    );
    let r = run(&f, Kind::Candidate).await.expect("success");
    assert_eq!(r.bytes.len(), 512);
    assert!(r.bytes[..100].iter().all(|b| *b == 1));
    assert!(r.bytes[100..300].iter().all(|b| *b == 2));
    assert!(r.bytes[300..].iter().all(|b| *b == 3));
    assert_eq!(*f.probe.remainings.borrow(), vec![512, 412, 212, 0]);
    assert_eq!((r.head.status, r.head.content_type.as_str()), (200, JSON));
    assert_eq!(r.head.cache_control, "no-store");
    assert_eq!(r.head.content_length.as_deref(), Some("512"));
    assert_eq!(r.head.content_encoding.as_deref(), Some("identity"));
    assert_eq!(f.probe.copies.get(), 3);
}

#[tokio::test]
async fn operation_success_keeps_provider_id() {
    let f = scripted(op_head(OCTET), vec![Step::Chunk(vec![9; 4]), Step::Eof]);
    let r = run(&f, Kind::Operation).await.expect("success");
    assert_eq!(r.head.provider_id.as_deref(), Some(PROVIDER));
    assert_eq!(&*r.bytes, &[9; 4]);
}

#[tokio::test]
async fn empty_chunks_fail_through_preflight_and_defense() {
    let f = scripted(head(JSON), vec![Step::Chunk(vec![]), Step::Eof]);
    assert!(is_err(run(&f, Kind::Candidate).await));
    assert_eq!(f.probe.copies.get(), 0);
    let f = scripted(head(JSON), vec![Step::Raw(vec![]), Step::Eof]);
    assert!(is_err(run(&f, Kind::Candidate).await));
}

#[tokio::test]
async fn raw_chunk_over_remaining_is_rejected_by_defense() {
    let f = scripted(head(JSON), vec![Step::Raw(vec![0; MAX_REQUEST_BYTES + 1])]);
    assert!(is_err(run(&f, Kind::Candidate).await));
    let f = scripted(
        head(JSON),
        vec![Step::Raw(vec![0; 500]), Step::Raw(vec![0; 13])],
    );
    assert!(is_err(run(&f, Kind::Candidate).await));
}

// ---- content-length ----

#[tokio::test]
async fn content_length_excess_and_malformed_fail_before_any_read() {
    let too_big = (MAX_REQUEST_BYTES + 1).to_string();
    for value in [
        too_big.as_str(),
        "",
        "abc",
        "+1",
        "-1",
        "1 ",
        "0x10",
        "12345678901",
        "99999999999999999999",
    ] {
        let mut h = head(JSON);
        h.content_length = Some(value.into());
        let f = scripted(h, vec![Step::Chunk(vec![1]), Step::Eof]);
        assert!(is_err(run(&f, Kind::Candidate).await), "{value:?}");
        assert_eq!(f.probe.nexts.get(), 0, "{value:?}");
    }
}

#[tokio::test]
async fn content_length_mismatch_after_read_fails() {
    for (declared, body) in [("10", 9usize), ("10", 11), ("1", 0), ("0", 1)] {
        let mut h = head(JSON);
        h.content_length = Some(declared.into());
        let steps = if body == 0 {
            vec![Step::Eof]
        } else {
            vec![Step::Chunk(vec![1; body]), Step::Eof]
        };
        let f = scripted(h, steps);
        assert!(
            is_err(run(&f, Kind::Candidate).await),
            "{declared} vs {body}"
        );
    }
    let mut h = head(JSON);
    h.content_length = Some("10".into());
    let f = scripted(h, vec![Step::Chunk(vec![1; 10]), Step::Eof]);
    assert!(run(&f, Kind::Candidate).await.is_ok());
}

// ---- header constraints ----

#[tokio::test]
async fn api_registry_and_resolver_accept_existing_rendered_headers() {
    // API render appends its default no-store and the resolver's no-store.
    // Account DO replies have their own exact JSON/binary media contract.
    for kind in [Kind::Registry, Kind::Resolve] {
        let mut h = head("application/json; charset=utf-8");
        h.cache_control = "no-store, no-store".into();
        for value in [&h.content_type, &h.cache_control] {
            assert_eq!(
                copy_header(value.len() as u32, 32, || Some(value.clone()))
                    .as_ref()
                    .ok(),
                Some(value)
            );
        }
        let f = scripted(h, vec![Step::Chunk(b"{}".to_vec()), Step::Eof]);
        assert!(run(&f, kind).await.is_ok());
    }
}

#[tokio::test]
async fn header_constraints_fail_before_any_read() {
    let mut bad: Vec<(Kind, Head)> = Vec::new();
    for kind in [
        Kind::Registry,
        Kind::Resolve,
        Kind::Candidate,
        Kind::Operation,
    ] {
        let base = |ct: &str| {
            if matches!(kind, Kind::Operation) {
                op_head(ct)
            } else {
                head(ct)
            }
        };
        bad.push((kind, base("text/plain")));
        bad.push((kind, base("")));
        bad.push((
            kind,
            Head {
                cache_control: "no-cache".into(),
                ..base(JSON)
            },
        ));
        bad.push((
            kind,
            Head {
                cache_control: String::new(),
                ..base(JSON)
            },
        ));
        bad.push((
            kind,
            Head {
                cache_control: "no-store, private".into(),
                ..base(JSON)
            },
        ));
        bad.push((
            kind,
            Head {
                content_encoding: Some("gzip".into()),
                ..base(JSON)
            },
        ));
        bad.push((
            kind,
            Head {
                content_encoding: Some(String::new()),
                ..base(JSON)
            },
        ));
        if matches!(kind, Kind::Candidate | Kind::Operation) {
            bad.push((
                kind,
                Head {
                    cache_control: "no-store, no-store".into(),
                    ..base(JSON)
                },
            ));
            bad.push((kind, base("application/json; charset=utf-8")));
        }
    }
    for kind in [Kind::Registry, Kind::Resolve, Kind::Candidate] {
        bad.push((
            kind,
            Head {
                provider_id: Some(PROVIDER.into()),
                ..head(JSON)
            },
        ));
        bad.push((kind, head(OCTET)));
    }
    for id in [
        None,
        Some(PROVIDER.to_uppercase()),
        Some("short".to_string()),
        Some(String::new()),
    ] {
        bad.push((
            Kind::Operation,
            Head {
                provider_id: id,
                ..head(JSON)
            },
        ));
    }
    for (index, (kind, h)) in bad.into_iter().enumerate() {
        let f = scripted(h, vec![Step::Chunk(vec![1]), Step::Eof]);
        assert!(is_err(run(&f, kind).await), "case {index}");
        assert_eq!(f.probe.nexts.get(), 0, "case {index}");
    }
}

#[tokio::test]
async fn header_case_insensitive_values_are_accepted() {
    let mut h = head(JSON);
    h.cache_control = "NO-STORE".into();
    h.content_encoding = Some("IDENTITY".into());
    let f = scripted(h, vec![Step::Chunk(vec![1]), Step::Eof]);
    assert!(run(&f, Kind::Registry).await.is_ok());
}

// ---- status, retries ----

#[tokio::test]
async fn error_status_body_is_not_consumed_or_exposed() {
    for status in [201u16, 204, 400, 401, 404, 429, 500, 503] {
        let mut h = Head {
            status,
            ..head("text/html")
        };
        h.provider_id = Some("secret".into());
        h.content_length = Some("999999999".into());
        let f = scripted(h, vec![Step::Chunk(b"secret body".to_vec()), Step::Eof]);
        let r = run(&f, Kind::Operation).await.expect("status is preserved");
        assert_eq!(r.head.status, status);
        assert!(r.bytes.is_empty());
        assert!(r.head.content_type.is_empty() && r.head.cache_control.is_empty());
        assert!(
            r.head.provider_id.is_none()
                && r.head.content_length.is_none()
                && r.head.content_encoding.is_none()
        );
        assert_eq!((f.probe.nexts.get(), f.probe.copies.get()), (0, 0));
    }
}

#[tokio::test]
async fn post_error_is_not_retried() {
    let f = fake(PostStep::Err);
    assert!(is_err(run(&f, Kind::Registry).await));
    assert_eq!(f.probe.posts.get(), 1);
}

#[tokio::test]
async fn read_error_is_not_retried_and_drops_reply() {
    let f = scripted(
        head(JSON),
        vec![Step::Chunk(vec![1; 4]), Step::Err, Step::Chunk(vec![1])],
    );
    assert!(is_err(run(&f, Kind::Registry).await));
    assert_eq!((f.probe.posts.get(), f.probe.nexts.get()), (1, 2));
    assert_eq!(f.probe.reply_drops.get(), 1);
}

// ---- native preflight closures ----

#[test]
fn copy_header_rejects_raw_length_before_copy() {
    let called = Cell::new(false);
    let result = copy_header(25, 24, || {
        called.set(true);
        Some("x".into())
    });
    assert_eq!(result, Err(RegistryError));
    assert!(!called.get());
    assert_eq!(
        copy_header(24, 24, || Some("a".repeat(24))),
        Ok("a".repeat(24))
    );
    assert_eq!(
        copy_header(0, 24, || Some(String::new())),
        Ok(String::new())
    );
}

#[test]
fn copy_header_defends_utf8_and_ascii() {
    // Raw UTF-16 length 2 passes the preflight but the UTF-8 copy is 4 bytes.
    assert_eq!(copy_header(2, 2, || Some("éé".into())), Err(RegistryError));
    // Within byte limit but not ASCII.
    assert_eq!(copy_header(1, 8, || Some("é".into())), Err(RegistryError));
    // Copy longer than the limit despite an honest-looking raw length.
    assert_eq!(
        copy_header(1, 8, || Some("a".repeat(9))),
        Err(RegistryError)
    );
    // Lone surrogates make as_string fail.
    assert_eq!(copy_header(1, 8, || None), Err(RegistryError));
}

#[test]
fn copy_chunk_rejects_before_copy() {
    let called = Cell::new(0);
    let copy = |bytes: Vec<u8>| {
        let called = &called;
        move || {
            called.set(called.get() + 1);
            bytes
        }
    };
    assert_eq!(copy_chunk(0, 10, copy(vec![])).err(), Some(RegistryError));
    assert_eq!(
        copy_chunk(11, 10, copy(vec![0; 11])).err(),
        Some(RegistryError)
    );
    assert_eq!(copy_chunk(1, 0, copy(vec![0])).err(), Some(RegistryError));
    assert_eq!(called.get(), 0);
    assert_eq!(
        copy_chunk(10, 10, copy(vec![7; 10])).ok().map(|b| b.len()),
        Some(10)
    );
    assert_eq!(called.get(), 1);
}

#[test]
fn copy_chunk_defends_length_disagreement() {
    assert_eq!(copy_chunk(4, 10, || vec![0; 5]).err(), Some(RegistryError));
    assert_eq!(copy_chunk(4, 10, || vec![0; 3]).err(), Some(RegistryError));
}

// ---- drop / cancellation ----

#[tokio::test]
async fn dropping_pending_post_drops_its_guard() {
    let f = fake(PostStep::Pending);
    let mut fut = Box::pin(run(&f, Kind::Registry));
    assert!(poll!(fut.as_mut()).is_pending());
    assert_eq!((f.probe.posts.get(), f.probe.post_drops.get()), (1, 0));
    drop(fut);
    assert_eq!(f.probe.post_drops.get(), 1);
}

#[tokio::test]
async fn dropping_pending_read_drops_the_reply() {
    let f = scripted(head(JSON), vec![Step::Chunk(vec![1; 3]), Step::Pending]);
    let mut fut = Box::pin(run(&f, Kind::Registry));
    assert!(poll!(fut.as_mut()).is_pending());
    assert_eq!((f.probe.nexts.get(), f.probe.reply_drops.get()), (2, 0));
    drop(fut);
    assert_eq!(f.probe.reply_drops.get(), 1);
}

struct Shared {
    fired: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

struct ManualDeadline(Rc<Shared>);
impl Future for ManualDeadline {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0.fired.get() {
            return Poll::Ready(());
        }
        *self.0.waker.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    }
}

fn manual() -> (Rc<Shared>, ManualDeadline) {
    let shared = Rc::new(Shared {
        fired: Cell::new(false),
        waker: RefCell::new(None),
    });
    (shared.clone(), ManualDeadline(shared))
}

fn fire(shared: &Shared) {
    shared.fired.set(true);
    if let Some(waker) = shared.waker.borrow_mut().take() {
        waker.wake();
    }
}

#[tokio::test]
async fn expired_deadline_wins_before_post() {
    let f = scripted(head(JSON), vec![]);
    let result = with_session_deadline(run(&f, Kind::Registry), std::future::ready(())).await;
    assert!(matches!(result, Err(SessionExpired)));
    assert_eq!(f.probe.posts.get(), 0);
}

#[tokio::test]
async fn deadline_wake_cancels_pending_post() {
    let f = fake(PostStep::Pending);
    let (shared, deadline) = manual();
    let (result, ()) = join!(
        with_session_deadline(run(&f, Kind::Registry), deadline),
        async {
            assert_eq!((f.probe.posts.get(), f.probe.post_drops.get()), (1, 0));
            fire(&shared);
        }
    );
    assert!(matches!(result, Err(SessionExpired)));
    assert_eq!(f.probe.post_drops.get(), 1);
}

#[tokio::test]
async fn deadline_wake_cancels_pending_read() {
    let f = scripted(head(JSON), vec![Step::Chunk(vec![1; 3]), Step::Pending]);
    let (shared, deadline) = manual();
    let (result, ()) = join!(
        with_session_deadline(run(&f, Kind::Registry), deadline),
        async {
            assert_eq!((f.probe.nexts.get(), f.probe.reply_drops.get()), (2, 0));
            fire(&shared);
        }
    );
    assert!(matches!(result, Err(SessionExpired)));
    assert_eq!(f.probe.reply_drops.get(), 1);
}

// ---- source-shape regression (does NOT prove Worker execution) ----

const ADAPTER: &str = include_str!("worker.rs");
const RUNTIME: &str = include_str!("../account_runtime.rs");

fn at(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("missing {needle:?}"))
}

#[test]
fn adapter_source_keeps_cancellation_shape() {
    let signal = at(ADAPTER, "raw.set_signal(Some(&abort.0.signal()))");
    assert!(signal < at(ADAPTER, ".fetch_request("));
    assert!(signal < at(ADAPTER, ".fetch_with_request("));
    assert!(at(ADAPTER, "AbortController::new()") < signal);
    let compact = ADAPTER.split_whitespace().collect::<String>();
    assert!(compact.contains("Ok(BindingReply{head,reader,_abort:abort,})"));
    assert!(ADAPTER.contains("_abort: Abort,"));
    let abort_drop = at(ADAPTER, "impl Drop for Abort");
    assert!(ADAPTER[abort_drop..].contains("self.0.abort();"));
    let reply_drop = at(ADAPTER, "impl Drop for BindingReply");
    assert!(ADAPTER[reply_drop..].contains("reader.cancel()"));
    assert!(!ADAPTER.contains("spawn_local"));
}

#[test]
fn adapter_source_keeps_raw_helpers_and_fixed_bindings() {
    assert!(ADAPTER.contains("Reflect::get(headers.as_ref(), &\"get\".into())"));
    assert!(ADAPTER.contains("super::copy_header(value.length(), limit, || value.as_string())"));
    assert!(ADAPTER.contains("super::copy_chunk(chunk.length(), remaining, || chunk.to_vec())"));
    assert!(ADAPTER.contains("&\"getReader\".into()"));
    for banned in [
        "response.stream(",
        ".get_reader(",
        "headers.get(",
        "worker::Fetch",
        "Fetch::Request",
        "RequestRedirect::Follow",
    ] {
        assert!(!ADAPTER.contains(banned), "{banned}");
    }
    assert!(ADAPTER.contains("RequestRedirect::Error"));
    assert!(ADAPTER.contains("env.service(\"API\")"));
    assert!(ADAPTER.contains(".durable_object(\"NNTP_ACCOUNT\")"));
    assert!(ADAPTER.contains("header(headers, \"content-type\", 32)"));
    assert!(ADAPTER.contains("header(headers, \"cache-control\", 32)"));
    assert!(ADAPTER.contains("https://nntune-api{path}"));
    assert!(ADAPTER.contains("https://nntune-account{path}"));
    assert!(ADAPTER.contains("crate::account_pool::uuid_shaped(account_id)"));
    assert!(at(ADAPTER, "uuid_shaped(account_id)") < at(ADAPTER, ".fetch_with_request("));
}

#[test]
fn runtime_operation_gate_stays_hard_false() {
    assert!(RUNTIME.contains("const OPERATIONS_QUALIFIED: bool = false;"));
    assert!(!RUNTIME.contains("OPERATIONS_QUALIFIED: bool = true"));
    assert!(RUNTIME.contains("path == OPERATION_PATH && OPERATIONS_QUALIFIED"));
    assert_eq!(RUNTIME.matches("const OPERATIONS_QUALIFIED").count(), 1);
}
