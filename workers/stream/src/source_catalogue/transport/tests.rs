use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::pending,
    rc::Rc,
};

use super::*;

const TRACK: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const AUTH: &str = "source-test-authorization";

struct State {
    now: Cell<u64>,
    posts: Cell<usize>,
    reads: Cell<usize>,
    drops: Cell<usize>,
    pending_post: Cell<bool>,
    pending_read: Cell<bool>,
    after_post: Cell<Option<u64>>,
    after_read: Cell<Option<u64>>,
    fail_post: Cell<bool>,
    head: RefCell<Option<Head>>,
    chunks: RefCell<VecDeque<Result<Vec<u8>, CatalogueUnavailable>>>,
    limits: RefCell<Vec<usize>>,
}

struct Fake(Rc<State>);
struct Guard(Rc<State>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.drops.set(self.0.drops.get() + 1);
    }
}
struct Response {
    head: Head,
    guard: Guard,
}

impl Fake {
    fn new() -> Self {
        Self(Rc::new(State {
            now: Cell::new(1000),
            posts: Cell::new(0),
            reads: Cell::new(0),
            drops: Cell::new(0),
            pending_post: Cell::new(false),
            pending_read: Cell::new(false),
            after_post: Cell::new(None),
            after_read: Cell::new(None),
            fail_post: Cell::new(false),
            head: RefCell::new(Some(Head {
                status: 200,
                content_type: "application/octet-stream".into(),
                cache_control: "no-store".into(),
                source_job_id: "generated-job".into(),
                content_length: None,
                content_encoding: None,
            })),
            chunks: RefCell::new(VecDeque::from([Ok(vec![1, 2]), Ok(vec![3])])),
            limits: RefCell::new(Vec::new()),
        }))
    }
    fn change_head(&self, change: impl FnOnce(&mut Head)) {
        change(self.0.head.borrow_mut().as_mut().unwrap());
    }
    async fn run(&self) -> Result<Payload, CatalogueUnavailable> {
        fetch(self, AUTH, &body(), 5000, || self.0.now.get(), pending()).await
    }
}
fn body() -> Vec<u8> {
    serde_json::to_vec(&Lookup {
        track_id: TRACK.into(),
        grant_expires_at: 10,
        issued_at_ms: 1000,
        deadline_ms: 5000,
    })
    .unwrap()
}

impl Client for Fake {
    type Reply = Response;
    async fn post(
        &self,
        authorization: &str,
        bytes: &[u8],
    ) -> Result<Response, CatalogueUnavailable> {
        assert_eq!(authorization, AUTH);
        assert_eq!(bytes, body());
        self.0.posts.set(self.0.posts.get() + 1);
        let guard = Guard(self.0.clone());
        if self.0.pending_post.get() {
            pending::<()>().await;
        }
        if self.0.fail_post.get() {
            return Err(CatalogueUnavailable);
        }
        if let Some(now) = self.0.after_post.get() {
            self.0.now.set(now);
        }
        Ok(Response {
            head: self.0.head.borrow_mut().take().unwrap(),
            guard,
        })
    }
}
impl Reply for Response {
    fn head(&self) -> &Head {
        &self.head
    }
    async fn next(&mut self, remaining: usize) -> Result<Option<Vec<u8>>, CatalogueUnavailable> {
        let state = &self.guard.0;
        state.reads.set(state.reads.get() + 1);
        state.limits.borrow_mut().push(remaining);
        if state.pending_read.get() {
            pending::<()>().await;
        }
        if let Some(now) = state.after_read.get() {
            state.now.set(now);
        }
        state.chunks.borrow_mut().pop_front().transpose()
    }
}

#[tokio::test]
async fn bounded_multi_chunk_success_drops_the_exchange() {
    let fake = Fake::new();
    fake.change_head(|h| h.content_length = Some("3".into()));
    let payload = fake.run().await.ok().unwrap();
    assert_eq!(payload.bytes, [1, 2, 3]);
    assert_eq!(payload.source_job_id, "generated-job");
    assert_eq!(fake.0.posts.get(), 1);
    assert_eq!(
        *fake.0.limits.borrow(),
        [MAX_RECORD_BYTES, MAX_RECORD_BYTES - 2, MAX_RECORD_BYTES - 3]
    );
    assert_eq!(fake.0.drops.get(), 1);
}

#[tokio::test]
async fn invalid_heads_are_rejected_before_any_body_read() {
    let changes: [fn(&mut Head); 8] = [
        |h| h.status = 302,
        |h| h.status = 503,
        |h| h.content_type = "application/json".into(),
        |h| h.cache_control.clear(),
        |h| h.cache_control = "public, no-store".into(),
        |h| h.source_job_id = "a".repeat(65),
        |h| h.source_job_id = "../job".into(),
        |h| h.content_encoding = Some("gzip".into()),
    ];
    for change in changes {
        let fake = Fake::new();
        fake.change_head(change);
        assert!(fake.run().await.is_err());
        assert_eq!(fake.0.reads.get(), 0);
        assert_eq!(fake.0.posts.get(), 1);
        assert_eq!(fake.0.drops.get(), 1);
    }
}

#[tokio::test]
async fn invalid_lengths_are_rejected_before_any_body_read() {
    for length in [
        "".into(),
        "0".into(),
        "-1".into(),
        "+3".into(),
        "3 ".into(),
        "99999999999".into(),
        (MAX_RECORD_BYTES + 1).to_string(),
    ] {
        let fake = Fake::new();
        fake.change_head(|h| h.content_length = Some(length));
        assert!(fake.run().await.is_err());
        assert_eq!(fake.0.reads.get(), 0);
    }
}

#[tokio::test]
async fn actual_body_length_must_match_the_declared_length() {
    for length in ["2", "4"] {
        let fake = Fake::new();
        fake.change_head(|h| h.content_length = Some(length.into()));
        assert!(fake.run().await.is_err());
        assert_eq!(fake.0.drops.get(), 1);
    }
}

#[tokio::test]
async fn body_cap_is_enforced_without_content_length() {
    let fake = Fake::new();
    *fake.0.chunks.borrow_mut() = VecDeque::from([Ok(vec![1; MAX_RECORD_BYTES]), Ok(vec![2])]);
    assert!(fake.run().await.is_err());
    assert_eq!(*fake.0.limits.borrow(), [MAX_RECORD_BYTES, 0]);
    assert_eq!(fake.0.posts.get(), 1);
    assert_eq!(fake.0.drops.get(), 1);
}

#[tokio::test]
async fn exact_body_cap_is_allowed_when_followed_by_eof() {
    let fake = Fake::new();
    *fake.0.chunks.borrow_mut() = VecDeque::from([Ok(vec![1; MAX_RECORD_BYTES])]);
    assert_eq!(fake.run().await.ok().unwrap().bytes.len(), MAX_RECORD_BYTES);
    assert_eq!(*fake.0.limits.borrow(), [MAX_RECORD_BYTES, 0]);
}

#[tokio::test]
async fn empty_body_empty_chunk_and_read_errors_fail_closed() {
    for chunks in [
        VecDeque::new(),
        VecDeque::from([Ok(Vec::new())]),
        VecDeque::from([Ok(vec![1]), Err(CatalogueUnavailable)]),
    ] {
        let fake = Fake::new();
        *fake.0.chunks.borrow_mut() = chunks;
        assert!(fake.run().await.is_err());
        assert_eq!(fake.0.posts.get(), 1);
        assert_eq!(fake.0.drops.get(), 1);
    }
}

#[tokio::test]
async fn post_failure_never_retries_or_reads() {
    let fake = Fake::new();
    fake.0.fail_post.set(true);
    assert!(fake.run().await.is_err());
    assert_eq!(fake.0.posts.get(), 1);
    assert_eq!(fake.0.reads.get(), 0);
    assert_eq!(fake.0.drops.get(), 1);
}

#[tokio::test]
async fn bad_inputs_deny_before_post() {
    for bytes in [
        vec![b'x'; MAX_REQUEST_BYTES + 1],
        b"{}".to_vec(),
        b"[]".to_vec(),
    ] {
        let fake = Fake::new();
        assert!(fetch(&fake, AUTH, &bytes, 5000, || 1000, pending())
            .await
            .is_err());
        assert_eq!(fake.0.posts.get(), 0);
    }
    for (auth, deadline, now) in [
        ("", 5000, 1000),
        (AUTH, 5001, 1000),
        (AUTH, 5000, 5000),
        (AUTH, 5000, 999),
    ] {
        let fake = Fake::new();
        assert!(fetch(&fake, auth, &body(), deadline, || now, pending())
            .await
            .is_err());
        assert_eq!(fake.0.posts.get(), 0);
    }
}

#[tokio::test]
async fn elapsed_timer_prevents_the_first_post() {
    let fake = Fake::new();
    assert!(fetch(&fake, AUTH, &body(), 5000, || 1000, async {})
        .await
        .is_err());
    assert_eq!(fake.0.posts.get(), 0);
}

#[tokio::test]
async fn expiry_and_clock_rollback_after_post_prevent_body_reads() {
    for now in [999, 5000] {
        let fake = Fake::new();
        fake.0.after_post.set(Some(now));
        assert!(fake.run().await.is_err());
        assert_eq!(fake.0.reads.get(), 0);
        assert_eq!(fake.0.drops.get(), 1);
    }
}

#[tokio::test]
async fn expiry_and_clock_rollback_after_chunk_prevent_later_reads() {
    for now in [999, 5000] {
        let fake = Fake::new();
        fake.0.after_read.set(Some(now));
        assert!(fake.run().await.is_err());
        assert_eq!(fake.0.reads.get(), 1);
        assert_eq!(fake.0.drops.get(), 1);
    }
}

#[tokio::test]
async fn cancellation_drops_pending_post_or_body_read() {
    for pending_post in [true, false] {
        let fake = Fake::new();
        fake.0.pending_post.set(pending_post);
        fake.0.pending_read.set(!pending_post);
        let mut work = Box::pin(fake.run());
        assert!(futures_util::poll!(&mut work).is_pending());
        drop(work);
        assert_eq!(fake.0.posts.get(), 1);
        assert_eq!(fake.0.drops.get(), 1);
    }
}

#[tokio::test]
async fn timer_cancels_stalled_post_and_body_read() {
    for pending_post in [true, false] {
        let fake = Fake::new();
        fake.0.pending_post.set(pending_post);
        fake.0.pending_read.set(!pending_post);
        let result = fetch(
            &fake,
            AUTH,
            &body(),
            5000,
            || 1000,
            tokio::time::sleep(std::time::Duration::from_millis(10)),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(fake.0.posts.get(), 1);
        assert_eq!(fake.0.drops.get(), 1);
    }
}

#[test]
fn header_limit_is_checked_before_copying_into_rust() {
    let copied = Cell::new(false);
    assert!(copy_header(65536, 64, || {
        copied.set(true);
        Some("a".repeat(65536))
    })
    .is_err());
    assert!(!copied.get());
    let value = copy_header(64, 64, || {
        copied.set(true);
        Some("a".repeat(64))
    })
    .ok()
    .unwrap();
    assert!(copied.get());
    assert_eq!(value.len(), 64);
}

#[test]
fn header_copy_also_rejects_non_ascii_and_oversized_utf8() {
    assert!(copy_header(64, 64, || Some("é".repeat(64))).is_err());
    assert!(copy_header(1, 64, || Some("é".into())).is_err());
    assert!(copy_header(1, 64, || None).is_err());
}

#[test]
fn worker_transport_has_only_binding_fetch_and_retains_abort_ownership() {
    let source = include_str!("worker.rs");
    assert!(source.contains("self.0.service(\"API\")"));
    assert!(source.contains("RequestRedirect::Error"));
    assert!(source.contains("raw.set_signal(Some(&abort.0.signal()))"));
    assert!(source.contains("_abort: abort"));
    assert!(source.contains("self.0.abort()"));
    assert!(source.contains("chunk.length() as usize > remaining"));
    assert!(source.contains("copy_header(value.length(), limit, || value.as_string())"));
    assert!(!source.contains(".get(name)"));
    assert!(!source.contains("spawn_local"));
    assert!(!source.contains("Fetch::"));
}
