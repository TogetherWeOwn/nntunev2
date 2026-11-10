//! Scratch-backed prefix reader contract: bind-time trust checks over the
//! verified round, exact-range response validation, and one total deadline.
//! A fake store stands in for scratch; no network, spool or catalogue.
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

use futures_util::{
    future::{self, FutureExt, LocalBoxFuture},
    stream::{self, LocalBoxStream, StreamExt},
};
use nntune_ingest::{
    audio_scratch::{ScratchAudioReader, ScratchPrefixResponse, ScratchPrefixStore},
    audio_selection::{
        extract_hints, AudioSelectionPlan, ExtractedAudio, PrefixReader, SelectionError,
    },
    audio_tags::MAX_TAG_PREFIX_BYTES,
    dispatch::content_sha256_hex,
    jobs::MAX_REPAIR_ROUNDS,
    verify::{JudgeCursor, StagedFile, StagedSpoolSegment, VerifyProgress},
};

const JOB: &str = "job_0123456789abcdef";
const ROUND: u32 = 1;

fn flac(samples: u64) -> Vec<u8> {
    let mut bytes = b"fLaC".to_vec();
    bytes.extend([128, 0, 0, 34]);
    let mut info = [0; 34];
    info[10..18].copy_from_slice(&((44_100_u64 << 44) | samples).to_be_bytes());
    bytes.extend(info);
    bytes
}

/// One accepted original plus the scratch bytes behind each of its files.
struct Fixture {
    xml: String,
    hash: String,
    files: Vec<(String, Vec<u8>)>,
}

fn fixture(files: Vec<(&str, Vec<u8>)>) -> Fixture {
    let mut xml = String::from("<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">");
    for (i, (name, _)) in files.iter().enumerate() {
        xml.push_str(&format!("<file poster=\"test\" date=\"0\" subject=\"&quot;{name}&quot; yEnc (1/1)\"><groups><group>alt.binaries.test</group></groups><segments><segment bytes=\"100\" number=\"1\">part{i}@example.invalid</segment></segments></file>"));
    }
    xml.push_str("</nzb>");
    Fixture {
        hash: content_sha256_hex(xml.as_bytes()),
        xml,
        files: files.into_iter().map(|(n, b)| (n.to_owned(), b)).collect(),
    }
}

fn single() -> Fixture {
    fixture(vec![("a.flac", flac(88_200))])
}

impl Fixture {
    fn plan(&self) -> AudioSelectionPlan {
        AudioSelectionPlan::from_original(&self.xml, &self.hash).unwrap()
    }

    fn progress_for(&self, job: &str, round: u32) -> VerifyProgress {
        let mut progress = VerifyProgress::new(round);
        for (index, (name, bytes)) in self.files.iter().enumerate() {
            progress.staged.insert(
                index,
                StagedFile {
                    key: VerifyProgress::file_key(job, round, index, name),
                    size: bytes.len() as u64,
                    sha256: content_sha256_hex(bytes),
                    missing_blocks: 0,
                    size_refused_blocks: 0,
                    par2_set_id: None,
                    spool: vec![],
                    crc32: 0,
                    missing: false,
                },
            );
        }
        progress
    }

    fn progress(&self) -> VerifyProgress {
        self.progress_for(JOB, ROUND)
    }

    fn key(&self, index: usize) -> String {
        VerifyProgress::file_key(JOB, ROUND, index, &self.files[index].0)
    }

    fn store(&self) -> Store {
        Store {
            state: Rc::new(State::default()),
            objects: (0..self.files.len())
                .map(|i| (self.key(i), self.files[i].1.clone()))
                .collect(),
            stall_headers: false,
            stall_body: false,
            meta: |_| {},
            body: |_, data, len| vec![Ok(data[..len.min(data.len())].to_vec())],
        }
    }
}

/// Bind then extract with the supplied store/deadline. The extraction plan is
/// built separately from the reader's own, as production callers do.
async fn extract<D: Future<Output = ()> + Unpin>(
    fx: &Fixture,
    store: Store,
    deadline: D,
) -> Result<Vec<ExtractedAudio>, SelectionError> {
    let mut reader =
        ScratchAudioReader::bind(JOB, &fx.xml, &fx.hash, &fx.progress(), store, deadline)?;
    extract_hints(&fx.plan(), &mut reader).await
}

fn bind_error(fx: &Fixture, job: &str, progress: &VerifyProgress) -> Option<SelectionError> {
    match ScratchAudioReader::bind(
        job,
        &fx.xml,
        &fx.hash,
        progress,
        fx.store(),
        future::pending::<()>(),
    ) {
        Ok(_) => None,
        Err(error) => Some(error),
    }
}

// ---- fake scratch store ----

#[derive(Default)]
struct State {
    reads: RefCell<Vec<(String, usize)>>,
    polls: Cell<usize>,
    drops: Cell<usize>,
}

struct DropCount(Rc<State>);

impl Drop for DropCount {
    fn drop(&mut self) {
        self.0.drops.set(self.0.drops.get() + 1);
    }
}

struct Meta {
    staged: StagedFile,
    object_size: u64,
    range_offset: u64,
    range_length: u64,
}

type BodyChunks = fn(usize, &[u8], usize) -> Vec<Result<Vec<u8>, ()>>;
type ProgressMutation = fn(&mut VerifyProgress);
type StagedMutation = fn(&mut StagedFile);
type MetadataMutation = fn(&mut Meta);

struct Store {
    state: Rc<State>,
    objects: HashMap<String, Vec<u8>>,
    stall_headers: bool,
    stall_body: bool,
    /// Rewrites the otherwise-honest response metadata.
    meta: fn(&mut Meta),
    /// Chunks for the n-th read (0-based), given the object and requested length.
    body: BodyChunks,
}

fn body_stream<'a>(
    state: Rc<State>,
    chunks: Vec<Result<Vec<u8>, ()>>,
    stall: bool,
) -> LocalBoxStream<'a, Result<Vec<u8>, ()>> {
    let guard = DropCount(state.clone());
    let mut chunks = chunks.into_iter();
    // Counts every poll, including an EOF poll, and counts a drop once.
    stream::poll_fn(move |_| {
        let _guard = &guard;
        state.polls.set(state.polls.get() + 1);
        match chunks.next() {
            Some(chunk) => Poll::Ready(Some(chunk)),
            None if stall => Poll::Pending,
            None => Poll::Ready(None),
        }
    })
    .boxed_local()
}

impl ScratchPrefixStore for Store {
    fn read<'a>(
        &'a self,
        expected: &'a StagedFile,
        length: usize,
    ) -> LocalBoxFuture<'a, Result<ScratchPrefixResponse<'a>, ()>> {
        async move {
            // Recorded on first poll, so an unpolled future leaves no trace.
            let read_no = self.state.reads.borrow().len();
            self.state
                .reads
                .borrow_mut()
                .push((expected.key.clone(), length));
            if self.stall_headers {
                future::pending::<()>().await;
            }
            let data = self.objects.get(&expected.key).ok_or(())?;
            let mut meta = Meta {
                staged: expected.clone(),
                object_size: data.len() as u64,
                range_offset: 0,
                range_length: length as u64,
            };
            (self.meta)(&mut meta);
            let chunks = (self.body)(read_no, data, length);
            Ok(ScratchPrefixResponse {
                staged: meta.staged,
                object_size: meta.object_size,
                range_offset: meta.range_offset,
                range_length: meta.range_length,
                body: body_stream(self.state.clone(), chunks, self.stall_body),
            })
        }
        .boxed_local()
    }
}

/// Deterministic deadline: pending (self-waking) for `limit` polls, then fires.
/// The poll counter is shared so tests can see the budget spent across files.
struct Countdown {
    polls: Rc<Cell<usize>>,
    limit: usize,
}

impl Future for Countdown {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.polls.set(self.polls.get() + 1);
        if self.polls.get() > self.limit {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

fn countdown(limit: usize) -> (Countdown, Rc<Cell<usize>>) {
    let polls = Rc::new(Cell::new(0));
    (
        Countdown {
            polls: polls.clone(),
            limit,
        },
        polls,
    )
}

// ---- bind ----

#[test]
fn bind_rechecks_the_original_digest() {
    let fx = single();
    let changed = fx.xml.replace("a.flac", "b.flac");
    for (xml, hash) in [
        (fx.xml.as_str(), "A".repeat(64)),
        (changed.as_str(), fx.hash.clone()),
    ] {
        let result = ScratchAudioReader::bind(
            JOB,
            xml,
            &hash,
            &fx.progress(),
            fx.store(),
            future::pending::<()>(),
        );
        assert!(matches!(result, Err(SelectionError::Identity)));
    }
}

#[test]
fn bind_accepts_url_safe_jobs_and_every_repair_round_only() {
    let fx = single();
    let long = "a".repeat(64);
    for job in ["not-a-job", "job_1", "A", long.as_str()] {
        assert!(bind_error(&fx, job, &fx.progress_for(job, ROUND)).is_none());
    }
    for round in 1..=MAX_REPAIR_ROUNDS + 1 {
        assert!(bind_error(&fx, JOB, &fx.progress_for(JOB, round)).is_none());
    }
    for round in [0, MAX_REPAIR_ROUNDS + 2] {
        assert_eq!(
            bind_error(&fx, JOB, &fx.progress_for(JOB, round)),
            Some(SelectionError::Identity)
        );
    }
    let oversized = "a".repeat(65);
    for job in ["", "job/path", "job\n", "jób", oversized.as_str()] {
        assert_eq!(
            bind_error(&fx, job, &fx.progress_for(job, ROUND)),
            Some(SelectionError::Identity)
        );
    }
}

#[test]
fn bind_refuses_unfinished_or_damaged_rounds() {
    let fx = single();
    let cases: [(&str, ProgressMutation); 3] = [
        ("missing blocks", |p| p.missing_blocks = 1),
        ("size refused", |p| p.size_refused_blocks = 1),
        ("judging", |p| {
            p.judging.insert(
                0,
                JudgeCursor {
                    segment: 0,
                    pos: 0,
                    block_index: 0,
                    in_block: 0,
                    crc32: 0,
                    bad: 0,
                    fingerprint: "x".into(),
                },
            );
        }),
    ];
    for (name, mutate) in cases {
        let mut progress = fx.progress();
        mutate(&mut progress);
        assert_eq!(
            bind_error(&fx, JOB, &progress),
            Some(SelectionError::Identity),
            "{name}"
        );
    }
}

#[test]
fn bind_refuses_untrusted_selected_audio_records() {
    let fx = single();
    let cases: [(&str, StagedMutation); 13] = [
        ("wrong job key", |s| {
            s.key = VerifyProgress::file_key("other", ROUND, 0, "a.flac")
        }),
        ("wrong round key", |s| {
            s.key = VerifyProgress::file_key(JOB, 2, 0, "a.flac")
        }),
        ("wrong index key", |s| {
            s.key = VerifyProgress::file_key(JOB, ROUND, 1, "a.flac")
        }),
        ("wrong name key", |s| {
            s.key = VerifyProgress::file_key(JOB, ROUND, 0, "b.flac")
        }),
        ("missing blocks", |s| s.missing_blocks = 1),
        ("size refused", |s| s.size_refused_blocks = 1),
        ("empty", |s| s.size = 0),
        ("over job cap", |s| s.size = 32 * 1024 * 1024 + 1),
        ("uppercase hash", |s| s.sha256 = s.sha256.to_uppercase()),
        ("short hash", |s| s.sha256.truncate(63)),
        ("nonhex hash", |s| s.sha256 = "g".repeat(64)),
        ("missing", |s| s.missing = true),
        ("segment crc", |s| s.crc32 = 1),
    ];
    for (name, mutate) in cases {
        let mut progress = fx.progress();
        mutate(progress.staged.get_mut(&0).unwrap());
        assert_eq!(
            bind_error(&fx, JOB, &progress),
            Some(SelectionError::Identity),
            "{name}"
        );
    }
    let mut progress = fx.progress();
    progress.staged.remove(&0);
    assert_eq!(
        bind_error(&fx, JOB, &progress),
        Some(SelectionError::Identity),
        "missing audio"
    );
}

#[test]
fn bind_reports_spooled_audio_as_unsupported() {
    let fx = single();
    let mut progress = fx.progress();
    progress.staged.get_mut(&0).unwrap().spool = vec![StagedSpoolSegment {
        key: "segment".into(),
        size: 1,
        crc32: 0,
        missing: false,
        size_refused: false,
    }];
    assert_eq!(
        bind_error(&fx, JOB, &progress),
        Some(SelectionError::Unsupported)
    );
    // In-flight spool cursors for the round are refused the same way.
    let mut progress = fx.progress();
    progress.spool.insert(0, vec![]);
    assert_eq!(
        bind_error(&fx, JOB, &progress),
        Some(SelectionError::Unsupported)
    );
}

#[tokio::test]
async fn selections_bind_by_original_ordinal_despite_duplicate_names_and_extras() {
    let fx = fixture(vec![
        ("readme.nfo", b"notes".to_vec()),
        ("same.flac", flac(88_200)),
        ("same.par2", b"par".to_vec()),
        ("same.flac", flac(132_300)),
    ]);
    // Non-audio extras are not trusted or read: damaged nfo, absent par2.
    let mut progress = fx.progress();
    progress.staged.get_mut(&0).unwrap().missing_blocks = 2;
    progress.staged.remove(&2);
    let store = fx.store();
    let state = store.state.clone();
    let mut reader = ScratchAudioReader::bind(
        JOB,
        &fx.xml,
        &fx.hash,
        &progress,
        store,
        future::pending::<()>(),
    )
    .unwrap();
    assert_eq!(reader.plan().selections().len(), 2);
    let entries = extract_hints(&fx.plan(), &mut reader).await.unwrap();
    let durations: Vec<_> = entries.iter().map(|e| e.hints.duration_ms).collect();
    assert_eq!(durations, [Some(2000), Some(3000)]);
    let keys: Vec<_> = state
        .reads
        .borrow()
        .iter()
        .map(|(k, _)| k.clone())
        .collect();
    assert_eq!(keys, [fx.key(1), fx.key(3)]);
    assert_ne!(keys[0], keys[1]);
}

// ---- prefix requests ----

#[tokio::test]
async fn selections_outside_the_bound_plan_or_cap_are_refused_without_io() {
    let fx = fixture(vec![("a.nfo", b"x".to_vec()), ("b.flac", flac(88_200))]);
    let foreign = fixture(vec![("b.flac", flac(88_200))]);
    let store = fx.store();
    let state = store.state.clone();
    let mut reader = ScratchAudioReader::bind(
        JOB,
        &fx.xml,
        &fx.hash,
        &fx.progress(),
        store,
        future::pending::<()>(),
    )
    .unwrap();
    // Same ordinal, different original file ordinal: not this plan's selection.
    assert_eq!(
        extract_hints(&foreign.plan(), &mut reader).await.err(),
        Some(SelectionError::Transport)
    );
    // An identical selector/ordinal from a different original is still foreign.
    let other_original = fixture(vec![("a.nfo", b"x".to_vec()), ("other.flac", flac(88_200))]);
    assert_eq!(
        extract_hints(&other_original.plan(), &mut reader)
            .await
            .err(),
        Some(SelectionError::Transport)
    );
    let own = fx.plan();
    for cap in [0, MAX_TAG_PREFIX_BYTES + 1] {
        let first = reader.prefix(&own.selections()[0], cap).next().await;
        assert!(matches!(first, Some(Err(()))), "cap {cap}");
    }
    assert!(state.reads.borrow().is_empty());
}

#[tokio::test]
async fn requested_length_is_the_smaller_of_cap_and_record_size() {
    let large = vec![0_u8; MAX_TAG_PREFIX_BYTES + 1024];
    let fx = fixture(vec![("big.flac", large), ("tiny.flac", flac(88_200))]);
    let store = fx.store();
    let state = store.state.clone();
    extract(&fx, store, future::pending::<()>()).await.unwrap();
    let lengths: Vec<_> = state.reads.borrow().iter().map(|(_, l)| *l).collect();
    assert_eq!(lengths, [MAX_TAG_PREFIX_BYTES, flac(88_200).len()]);
}

#[tokio::test]
async fn one_valid_flac_prefix_yields_hints() {
    let fx = single();
    let entries = extract(&fx, fx.store(), future::pending::<()>())
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].audio_ordinal, 1);
    assert_eq!(entries[0].hints.duration_ms, Some(2000));
}

#[tokio::test]
async fn metadata_range_and_size_drift_is_refused_and_drops_the_body() {
    let fx = single();
    let cases: [(&str, MetadataMutation); 9] = [
        ("key", |m| m.staged.key.push('x')),
        ("size", |m| m.staged.size += 1),
        ("sha", |m| m.staged.sha256 = "0".repeat(64)),
        ("damage", |m| m.staged.missing_blocks = 1),
        ("object size grew", |m| m.object_size += 1),
        ("object size shrank", |m| m.object_size -= 1),
        ("offset", |m| m.range_offset = 1),
        ("short range", |m| m.range_length -= 1),
        ("long range", |m| m.range_length += 1),
    ];
    for (name, tweak) in cases {
        let mut store = fx.store();
        store.meta = tweak;
        let state = store.state.clone();
        let result = extract(&fx, store, future::pending::<()>()).await;
        assert_eq!(result.err(), Some(SelectionError::Transport), "{name}");
        assert_eq!(state.drops.get(), 1, "{name}");
    }
}

#[tokio::test]
async fn bad_body_lengths_and_transport_errors_refuse_everything() {
    let fx = single();
    let cases: [(&str, BodyChunks); 4] = [
        ("truncated", |_, d, n| vec![Ok(d[..n - 1].to_vec())]),
        ("oversize", |_, _, n| vec![Ok(vec![0; n + 1])]),
        ("transport", |_, d, n| {
            vec![Ok(d[..n - 1].to_vec()), Err(())]
        }),
        ("empty chunk flood", |_, _, _| vec![Ok(vec![]); 4097]),
    ];
    for (name, body) in cases {
        let mut store = fx.store();
        store.body = body;
        let state = store.state.clone();
        let result = extract(&fx, store, future::pending::<()>()).await;
        assert!(result.is_err(), "{name}");
        assert_eq!(state.drops.get(), 1, "{name}");
    }
}

#[tokio::test]
async fn a_full_prefix_is_not_polled_for_eof_and_the_body_is_dropped() {
    let fx = single();
    let mut store = fx.store();
    // A trailing error must stay unread once the requested length arrived.
    store.body = |_, d, n| vec![Ok(d[..n].to_vec()), Err(())];
    let state = store.state.clone();
    extract(&fx, store, future::pending::<()>()).await.unwrap();
    assert_eq!(state.polls.get(), 1);
    assert_eq!(state.drops.get(), 1);
}

// ---- deadline ----

#[tokio::test]
async fn an_expired_deadline_refuses_before_the_store_is_polled() {
    let fx = single();
    let store = fx.store();
    let state = store.state.clone();
    let result = extract(&fx, store, future::ready(())).await;
    assert_eq!(result.err(), Some(SelectionError::Transport));
    assert!(state.reads.borrow().is_empty());
    assert_eq!(state.polls.get(), 0);
}

#[tokio::test]
async fn stalled_headers_hit_the_deadline() {
    let fx = single();
    let mut store = fx.store();
    store.stall_headers = true;
    let state = store.state.clone();
    let (deadline, polls) = countdown(5);
    let result = extract(&fx, store, deadline).await;
    assert_eq!(result.err(), Some(SelectionError::Transport));
    assert_eq!(state.reads.borrow().len(), 1);
    assert_eq!(state.polls.get(), 0);
    assert_eq!(polls.get(), 6);
}

#[tokio::test]
async fn a_stalled_body_hits_the_deadline_and_is_dropped() {
    let fx = single();
    let mut store = fx.store();
    store.stall_body = true;
    store.body = |_, d, n| vec![Ok(d[..n - 1].to_vec())];
    let state = store.state.clone();
    let (deadline, polls) = countdown(5);
    let result = extract(&fx, store, deadline).await;
    assert_eq!(result.err(), Some(SelectionError::Transport));
    assert_eq!(state.drops.get(), 1);
    assert_eq!(polls.get(), 6);
}

#[tokio::test]
async fn one_deadline_budget_covers_all_files_without_rearming() {
    let fx = fixture(vec![("a.flac", flac(88_200)), ("b.flac", flac(88_200))]);
    let mut store = fx.store();
    store.stall_body = true;
    // The first file completes; the second delivers nothing and stalls.
    store.body = |n, d, len| {
        if n == 0 {
            vec![Ok(d[..len].to_vec())]
        } else {
            vec![]
        }
    };
    let state = store.state.clone();
    let (deadline, polls) = countdown(8);
    let result = extract(&fx, store, deadline).await;
    assert_eq!(result.err(), Some(SelectionError::Transport));
    assert_eq!(state.reads.borrow().len(), 2);
    assert_eq!(state.drops.get(), 2);
    // The first file's work spent part of the same budget: exactly one more
    // poll than the limit in total, never a fresh allowance for file two.
    assert_eq!(polls.get(), 9);
}
