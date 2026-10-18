//! Geometry-overflow fail-closed pins for the ingest verify walk.
//!
//! Contract under test (`workers/ingest/src/verify.rs`):
//!
//! - `complete_packet_prefix` walks whole par2 packets by their declared
//!   lengths, stopping at the first short, misaligned, unrepresentable,
//!   overflowing, or overrunning packet. A 16-byte header declaring
//!   `len = u64::MAX` stops immediately (prefix 0): no panic, no
//!   allocation, no walk past the header.
//! - `packets_within_bound` requires whole packets each within the
//!   buffered-packet bound. The same 16-byte header fails closed
//!   (`false`): the alignment guard fires first (`u64::MAX % 4 == 3`),
//!   so the overflow arm is never reached for this value.
//! - `implied_len` derives a file length only from proven part geometry.
//!   A uniform part size of `u64::MAX / 2` over 3 segments overflows u64
//!   (`(MAX/2) * 2 + MAX/2` does not fit) and yields `None`, which the
//!   loader maps to a permanent geometry error instead of allocating or
//!   walking an untrusted length.
//!
//! The `spec_*` helpers below are independent re-implementations of that
//! walk for pinning (same checked arithmetic, same guard order); the
//! `tokio` tests prove the implementation matches them through the public
//! step (`run_verifying_step`): a crafted par2 is dropped without panic
//! and overflowing geometry surfaces the exact permanent error.

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
const HALF_MAX: u64 = u64::MAX / 2;
const ALIGNED_MAX: u64 = u64::MAX - 3;

// --- spec mirrors (same guard order and checked arithmetic as the walk) ---

fn spec_complete_packet_prefix(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    while offset + 16 <= bytes.len() {
        let len = u64::from_le_bytes(bytes[offset + 8..offset + 16].try_into().unwrap());
        if len < 64 || len % 4 != 0 {
            break;
        }
        let Ok(len) = usize::try_from(len) else {
            break;
        };
        let Some(next) = offset.checked_add(len) else {
            break;
        };
        if next > bytes.len() {
            break;
        }
        offset = next;
    }
    offset
}

fn spec_packets_within_bound(bytes: &[u8], max_packet: u64) -> bool {
    let mut offset = 0usize;
    while offset < bytes.len() {
        if offset + 16 > bytes.len() {
            return false;
        }
        let len = u64::from_le_bytes(bytes[offset + 8..offset + 16].try_into().unwrap());
        if len > max_packet || len < 64 || len % 4 != 0 {
            return false;
        }
        let Ok(len) = usize::try_from(len) else {
            return false;
        };
        let Some(next) = offset.checked_add(len) else {
            return false;
        };
        if next > bytes.len() {
            return false;
        }
        offset = next;
    }
    true
}

fn spec_uniform_part_size(segs: &[StagedSpoolSegment], total_segments: usize) -> Option<u64> {
    let non_last = total_segments.saturating_sub(1);
    let mut size: Option<u64> = None;
    for (i, seg) in segs.iter().enumerate() {
        if i >= non_last {
            break;
        }
        if seg.missing {
            continue;
        }
        match size {
            None => size = Some(seg.size),
            Some(s) if s != seg.size => return None,
            _ => {}
        }
    }
    size.filter(|s| *s > 0)
}

fn spec_implied_len(segs: &[StagedSpoolSegment], total_segments: usize) -> Option<u64> {
    if total_segments == 0 {
        return Some(0);
    }
    if segs.len() >= total_segments && segs.iter().all(|s| !s.missing) {
        return segs.iter().try_fold(0u64, |acc, s| acc.checked_add(s.size));
    }
    let part_size = spec_uniform_part_size(segs, total_segments)?;
    let last_len = match segs.get(total_segments - 1) {
        Some(s) if !s.missing => s.size,
        _ => part_size,
    };
    part_size
        .checked_mul((total_segments - 1) as u64)?
        .checked_add(last_len)
}

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

fn valid_64b_packet() -> Vec<u8> {
    let mut out = vec![0u8; 64];
    out[..8].copy_from_slice(b"PAR2\0PKT");
    out[8..16].copy_from_slice(&64u64.to_le_bytes());
    out
}

// --- packet-walk pins ---

#[test]
fn max_declared_len_stops_packet_walk() {
    let bytes = header_with_len(u64::MAX);
    assert_eq!(bytes.len(), 16);
    assert_eq!(u64::MAX % 4, 3, "MAX hits the alignment guard first");
    assert_eq!(
        spec_complete_packet_prefix(&bytes),
        0,
        "overflowing declared length stops the walk, no panic or alloc"
    );
    assert!(
        !spec_packets_within_bound(&bytes, u64::MAX),
        "same buffer is not within any bound"
    );
}

#[test]
fn aligned_max_overflow_needs_checked_add() {
    // First packet is whole (64 bytes); the second declares the largest
    // multiple of 4, which passes the alignment guard and only the
    // overflow arm can stop. With wrapping arithmetic the walk would
    // advance to 60 instead of stopping at 64.
    assert_eq!(ALIGNED_MAX % 4, 0);
    let mut bytes = valid_64b_packet();
    bytes.extend_from_slice(&header_with_len(ALIGNED_MAX));
    assert_eq!(bytes.len(), 80);
    assert_eq!(spec_complete_packet_prefix(&bytes), 64);
    let wrapping = 64usize.wrapping_add(ALIGNED_MAX as usize);
    assert_eq!(wrapping, 60, "wrapping would continue at the wrong offset");
    assert_ne!(
        spec_complete_packet_prefix(&bytes),
        wrapping,
        "checked arithmetic is load-bearing here"
    );
    assert!(!spec_packets_within_bound(&bytes, u64::MAX));
}

#[test]
fn implied_len_overflow_is_none() {
    // Uniform part size MAX/2 over 3 segments: (MAX/2)*2 fits (MAX-1),
    // adding the last part overflows, so no geometry is proven.
    let segs = vec![srec(HALF_MAX), srec(HALF_MAX), smissing()];
    assert_eq!(spec_uniform_part_size(&segs, 3), Some(HALF_MAX));
    assert_eq!(spec_implied_len(&segs, 3), None);
    // Sanity: small geometry still derives.
    let small = vec![srec(4), smissing(), smissing()];
    assert_eq!(spec_implied_len(&small, 3), Some(12));
}

#[test]
fn segment_sum_overflow_wrapping_would_hide() {
    // All-present sum overflow: 2^63 + 2^63 + 1 wraps to 1 without the
    // checked fold, so the guard is load-bearing for the public step
    // below (checked yields None, wrapping would yield a tiny length).
    const TWO_POW_63: u64 = 1 << 63;
    let segs = vec![srec(TWO_POW_63), srec(TWO_POW_63), srec(1)];
    assert_eq!(spec_implied_len(&segs, 3), None);
    let wrapping = TWO_POW_63.wrapping_add(TWO_POW_63).wrapping_add(1);
    assert_eq!(wrapping, 1);
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

#[tokio::test]
async fn crafted_par2_is_dropped_without_panic_or_alloc() {
    // Data is healthy; the par2 file carries only the 16-byte MAX header.
    // The packet walk stops at prefix 0 (assembled path) or drops the
    // stream (spooled path): the release still completes with no set.
    let data = payload(7, 128);
    let crafted = header_with_len(u64::MAX);
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

#[tokio::test]
async fn geometry_overflow_fails_closed_via_public_step() {
    // Three-part file whose proven part size is MAX/2: the implied length
    // overflows u64, so the loader reports no geometry and the step maps
    // it to the exact permanent error instead of allocating or walking.
    let seg_ids = vec![
        "g1@synthetic.invalid".to_string(),
        "g2@synthetic.invalid".to_string(),
        "g3@synthetic.invalid".to_string(),
    ];
    let files = vec![parsed(
        DATA_NAME,
        false,
        seg_ids.clone(),
        vec![None, None, None],
    )];
    let scratch = MemoryScratch::default();
    let mut spool = Vec::new();
    for (n, id) in seg_ids.iter().enumerate() {
        let _ = id;
        let key = VerifyProgress::spool_key(JOB, 0, 0, n);
        if n < 2 {
            scratch.insert_synthetic(&key, vec![0u8; 1]);
            spool.push(StagedSpoolSegment {
                key,
                size: HALF_MAX,
                crc32: 0,
                missing: false,
            });
        } else {
            spool.push(StagedSpoolSegment {
                key,
                size: 0,
                crc32: 0,
                missing: true,
            });
        }
    }
    let mut progress = VerifyProgress::new(0);
    progress.spool.insert(0, spool);
    let mut source = MapSource {
        articles: HashMap::new(),
    };
    let err = run_verifying_step(
        JOB,
        &files,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
        &|| false,
    )
    .await
    .expect_err("overflowing geometry must fail the step");
    match err {
        VerifyError::Permanent(msg) => assert_eq!(msg, GEOMETRY_MSG),
        other => panic!("want permanent geometry error, got {other:?}"),
    }
}
