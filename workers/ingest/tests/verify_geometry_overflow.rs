//! Geometry-overflow fail-closed pins through the public verify step.
//!
//! Contract under test (`workers/ingest/src/verify.rs`):
//!
//! - `complete_packet_prefix` / `packets_within_bound` walk whole par2
//!   packets by declared length with checked arithmetic, stopping at the
//!   first short, misaligned, unrepresentable, overflowing, or overrunning
//!   packet. Direct unit pins live in `verify.rs` `mod tests` (which calls
//!   the private functions themselves): MAX stops at prefix 0, the aligned
//!   maximum stops at 64 via the checked-add arm.
//! - `implied_len` derives a file length only from proven part geometry,
//!   yielding `None` on u64 overflow; the loader maps `None` to the exact
//!   permanent error `file geometry out of bounds` instead of allocating
//!   or walking an untrusted length. Direct pins also live in `mod tests`.
//! - `match_set_file`'s length fallback is checked: overflowing part sizes
//!   prove no length, so there is no match (fail closed, no panic).
//!
//! This file pins the observable behavior through `run_verifying_step`
//! with inputs where the geometry arithmetic is load-bearing (each geometry
//! case fails under its wrapping-arithmetic mutant):
//!
//! - two segments `[2^63 present, missing]`: the final checked add overflows
//!   to `None` (error); wrapping would yield total 0 and pass every cap.
//! - three segments `[2^63 present, missing, 5]`: the checked mul overflows
//!   to `None` (error); wrapping would yield 5 and pass every cap.
//! - all-present `[2^63, 2^63, 1]`: the checked sum is `None` (error, no
//!   panic); wrapping would yield 1 and hide the overflow.
//!
//! A par2 stream starting with a whole 64-byte packet followed by an
//! overflowing header is a no-panic smoke test: it reaches the packet walk
//! (>= 64 bytes, so `peek_set_id` routes it) and is dropped safely in both
//! assembled and spooled caps. The exact walk boundary is pinned directly
//! in `verify.rs` `mod tests`, which kills the walk wrapping mutants.

use std::cell::RefCell;
use std::collections::HashMap;

use nntune_core::yenc::{encode_segment, SegmentParams};
use nntune_ingest::parse::ParsedFile;
use nntune_ingest::verify::{
    run_verifying_step, ArticleFetch, BodySource, ScratchStore, StagedFile, StagedSpoolSegment,
    VerifyCaps, VerifyError, VerifyOutcome, VerifyProgress,
};

const JOB: &str = "job_0123456789abcdef";
const DATA_NAME: &str = "track01.flac";
const PAR2_NAME: &str = "track01.par2";
const GEOMETRY_MSG: &str = "file geometry out of bounds";
const TWO_POW_63: u64 = 1 << 63;
const ALIGNED_MAX: u64 = u64::MAX - 3;

fn srec(size: u64) -> StagedSpoolSegment {
    StagedSpoolSegment {
        key: "k".into(),
        size,
        crc32: 0,
        missing: false,
    }
}

fn smissing() -> StagedSpoolSegment {
    StagedSpoolSegment {
        missing: true,
        ..srec(0)
    }
}

fn header_with_len(len: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(b"PAR2\0PKT");
    out.extend_from_slice(&len.to_le_bytes());
    out
}

/// One whole 64-byte packet with a valid checksum (unknown kind, so the
/// builder ignores its body but the stream passes `peek_set_id`,
/// `complete_packet_prefix`, and `packets_within_bound` for the prefix).
fn valid_64b_packet(set_id: &[u8; 16]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"PAR2\0PKT");
    p.extend_from_slice(&64u64.to_le_bytes());
    p.extend_from_slice(&[0u8; 16]);
    p.extend_from_slice(set_id);
    p.extend_from_slice(&[0u8; 16]);
    let digest = md5::compute(&p[32..]).0;
    p[16..32].copy_from_slice(&digest);
    p
}

// --- public-step doubles ---

#[derive(Default)]
struct MemoryScratch {
    objects: RefCell<HashMap<String, (Vec<u8>, StagedFile)>>,
}

impl MemoryScratch {
    fn insert_synthetic(&self, key: &str, bytes: Vec<u8>) {
        let meta = StagedFile {
            key: key.to_string(),
            size: bytes.len() as u64,
            sha256: format!("synthetic-{key}"),
            missing_blocks: 0,
            par2_set_id: None,
            spool: Vec::new(),
            crc32: 0,
            missing: false,
        };
        self.objects
            .borrow_mut()
            .insert(key.to_string(), (bytes, meta));
    }
}

impl ScratchStore for MemoryScratch {
    async fn head(&self, key: &str) -> Result<Option<StagedFile>, VerifyError> {
        Ok(self.objects.borrow().get(key).map(|(_, m)| m.clone()))
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, VerifyError> {
        Ok(self.objects.borrow().get(key).map(|(b, _)| b.clone()))
    }

    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, VerifyError> {
        Ok(self
            .objects
            .borrow()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn put(&self, key: &str, bytes: Vec<u8>, meta: &StagedFile) -> Result<(), VerifyError> {
        self.objects
            .borrow_mut()
            .insert(key.to_string(), (bytes, meta.clone()));
        Ok(())
    }
}

struct MapSource {
    articles: HashMap<String, Vec<u8>>,
}

impl BodySource for MapSource {
    async fn fetch_body(&mut self, segment_id: &str) -> Result<ArticleFetch, VerifyError> {
        Ok(match self.articles.get(segment_id) {
            Some(article) => ArticleFetch::Present(article.clone()),
            None => ArticleFetch::Missing,
        })
    }
}

fn payload(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn yenc_single(name: &str, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    encode_segment(&mut body, &SegmentParams::single(name), data).expect("encode yEnc");
    body
}

fn parsed(
    name: &str,
    is_par2: bool,
    segment_ids: Vec<String>,
    encoded: Vec<Option<u64>>,
) -> ParsedFile {
    ParsedFile {
        name: name.into(),
        is_par2,
        segment_ids,
        segment_bytes: encoded,
    }
}

async fn geometry_err(spool: Vec<StagedSpoolSegment>, total_segments: usize) -> VerifyError {
    let seg_ids: Vec<String> = (0..total_segments)
        .map(|n| format!("g{n}@synthetic.invalid"))
        .collect();
    let files = vec![parsed(
        DATA_NAME,
        false,
        seg_ids.clone(),
        vec![None; total_segments],
    )];
    let scratch = MemoryScratch::default();
    let mut staged_spool = Vec::with_capacity(total_segments);
    for (n, seg) in spool.into_iter().enumerate() {
        let key = VerifyProgress::spool_key(JOB, 0, 0, n);
        if !seg.missing {
            scratch.insert_synthetic(&key, vec![0u8; 1]);
        }
        staged_spool.push(StagedSpoolSegment { key, ..seg });
    }
    let mut progress = VerifyProgress::new(0);
    progress.spool.insert(0, staged_spool);
    let mut source = MapSource {
        articles: HashMap::new(),
    };
    run_verifying_step(
        JOB,
        &files,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
        &|| false,
    )
    .await
    .expect_err("overflowing geometry must fail the step")
}

#[tokio::test]
async fn geometry_overflow_present_missing_two_pow_63_fails_closed() {
    // Part size 2^63 over 2 segments: 2^63 * 1 + 2^63 overflows u64, so the
    // loader reports no geometry and the step maps it to the exact permanent
    // error. Wrapping would yield total 0 (passes every cap), so this case
    // kills the `implied_len` wrapping mutant.
    assert_eq!(TWO_POW_63.wrapping_add(TWO_POW_63), 0);
    let err = geometry_err(vec![srec(TWO_POW_63), smissing()], 2).await;
    match err {
        VerifyError::Permanent(msg) => assert_eq!(msg, GEOMETRY_MSG),
        other => panic!("want permanent geometry error, got {other:?}"),
    }
}

#[tokio::test]
async fn geometry_overflow_checked_mul_fails_closed() {
    // [2^63 present, missing, 5] over 3 segments: part size 2^63, last
    // length 5, and 2^63 * 2 overflows u64, so the loader reports no
    // geometry and the step maps it to the exact permanent error. Wrapping
    // would yield 5 (passes every cap), so this case kills the
    // `implied_len` checked-mul -> wrapping-mul mutant.
    assert_eq!(TWO_POW_63.wrapping_mul(2).wrapping_add(5), 5);
    let err = geometry_err(vec![srec(TWO_POW_63), smissing(), srec(5)], 3).await;
    match err {
        VerifyError::Permanent(msg) => assert_eq!(msg, GEOMETRY_MSG),
        other => panic!("want permanent geometry error, got {other:?}"),
    }
}

#[tokio::test]
async fn geometry_overflow_all_present_sum_fails_closed_without_panic() {
    // Every segment present with sizes [2^63, 2^63, 1]: the checked sum
    // overflows, so there is no geometry — and `match_set_file`'s checked
    // length fallback must not panic first. Wrapping would yield 1 and hide
    // the overflow, so this case kills the `load_segments` wrapping mutant.
    assert_eq!(TWO_POW_63.wrapping_add(TWO_POW_63).wrapping_add(1), 1);
    let err = geometry_err(vec![srec(TWO_POW_63), srec(TWO_POW_63), srec(1)], 3).await;
    match err {
        VerifyError::Permanent(msg) => assert_eq!(msg, GEOMETRY_MSG),
        other => panic!("want permanent geometry error, got {other:?}"),
    }
}

#[tokio::test]
async fn crafted_par2_with_valid_prefix_is_dropped_without_panic_or_alloc() {
    // A whole 64-byte packet followed by a header declaring ALIGNED_MAX
    // (the largest multiple of 4): the stream is >= 64 bytes, so it passes
    // `peek_set_id` and reaches the packet walk, which stops at prefix 64
    // via the checked-add arm (pinned directly in `verify.rs` mod tests).
    // The trailing overflow is never fed; the stream validates to no set
    // (unknown kind, no Main packet) and healthy data still completes.
    assert_eq!(ALIGNED_MAX % 4, 0);
    let mut crafted = valid_64b_packet(&[0x42; 16]);
    crafted.extend_from_slice(&header_with_len(ALIGNED_MAX));
    assert_eq!(crafted.len(), 80);
    let data = payload(7, 128);
    let data_id = "d1@synthetic.invalid".to_string();
    let par2_id = "p1@synthetic.invalid".to_string();
    let data_article = yenc_single(DATA_NAME, &data);
    let par2_article = yenc_single(PAR2_NAME, &crafted);
    let files = vec![
        parsed(
            DATA_NAME,
            false,
            vec![data_id.clone()],
            vec![Some(data_article.len() as u64)],
        ),
        parsed(
            PAR2_NAME,
            true,
            vec![par2_id.clone()],
            vec![Some(par2_article.len() as u64)],
        ),
    ];

    for caps in [
        VerifyCaps::default(),
        VerifyCaps {
            max_file_bytes: 1,
            ..VerifyCaps::default()
        },
    ] {
        let mut source = MapSource {
            articles: HashMap::from([
                (data_id.clone(), data_article.clone()),
                (par2_id.clone(), par2_article.clone()),
            ]),
        };
        let scratch = MemoryScratch::default();
        let mut progress = VerifyProgress::new(0);
        let outcome = run_verifying_step(
            JOB,
            &files,
            &mut progress,
            &mut source,
            &scratch,
            &caps,
            &|| false,
        )
        .await
        .expect("crafted par2 must not fail the step");
        assert_eq!(
            outcome,
            VerifyOutcome::Complete,
            "crafted par2 is dropped, healthy data completes (caps {caps:?})"
        );
        assert_eq!(progress.missing_blocks, 0);
    }
}
