//! Acceptance tests: a size-refused article is not a missing article.
//!
//! Contract under test (paired build card owns the implementation):
//!
//! - `ArticleFetch::TooLarge` (pool `413` / client read limit) surfaces
//!   distinctly from `ArticleFetch::Missing` in the job outcome. Today
//!   `spool_one` collapses both into the same missing spool record
//!   (`workers/ingest/src/verify.rs`, the `TooLarge => return
//!   Ok(missing_rec)` arm), so a size-refused segment is
//!   indistinguishable from a `430` all the way to repair dispatch.
//! - Repair dispatch is skipped for unrepairable-by-size blocks: a
//!   refetch would be refused identically, so spending a repair round
//!   on them only delays the terminal verdict. In the existing outcome
//!   surface that means size-refused damage concludes
//!   `VerifyOutcome::Unrecoverable` (which the queue consumer maps to a
//!   permanent failure with no `decide_repair` dispatch), never
//!   `RepairNeeded` — even when the par2 recovery slices would cover
//!   the same block count for genuinely missing segments.
//! - The in-Worker article cap (`VerifyCaps::max_article_bytes`, checked
//!   in `spool_one` right after the fetch) is the same refusal class:
//!   a present-but-over-cap article is size-refused, not missing.
//! - Each refusal emits an `article_size_refused` log event carrying
//!   job/segment facts, so triage can tell "provider lost it" from
//!   "article over cap" on repair-exhausted jobs. There is no native
//!   log-capture seam for the worker target, so this one pin is a
//!   source-presence check over the two modules that can emit it
//!   (`verify.rs` at the refusal site, or `queue_consumer.rs` at the
//!   outcome mapping); all other pins are behavioral.
//!
//! Control that must stay green before and after: a genuinely missing
//! segment covered by recovery slices still concludes `RepairNeeded`.
//!
//! Sensitivity: collapsing `TooLarge` back into the missing record (the
//! current code) fails every `size_refused_*` / mixed / over-cap pin
//! with `RepairNeeded` where `Unrecoverable` is required; treating
//! plain missing segments as size-refused fails the control.
//!
//! Note for the paired build: `tests/verifying_e2e.rs`
//! (`too_large_article_counts_only_its_own_segment`) pins the OLD
//! collapse (`RepairNeeded` for a refused segment) and must be flipped
//! to the new contract by whoever changes the behavior; this file does
//! not edit it.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test size_refused_outcome
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use nntune_core::yenc::{encode_segment, SegmentParams};
use nntune_ingest::parse::ParsedFile;
use nntune_ingest::verify::{
    run_verifying_step, ArticleFetch, BodySource, ScratchStore, StagedFile, VerifyCaps,
    VerifyError, VerifyOutcome, VerifyProgress,
};

const JOB: &str = "job_0123456789abcdef";
const BLOCK: u64 = 256;
const DATA_NAME: &str = "track01.flac";
const PAR2_NAME: &str = "track01.par2";

// --- deterministic payload ---------------------------------------------------

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

// --- par2 set builder (same wire rules as the verifying_e2e builder) ---------

fn packet(set_id: &[u8; 16], kind: &[u8; 16], body: &[u8]) -> Vec<u8> {
    let mut aligned = body.to_vec();
    while !aligned.len().is_multiple_of(4) {
        aligned.push(0);
    }
    let mut p = Vec::new();
    p.extend_from_slice(b"PAR2\0PKT");
    p.extend_from_slice(&((64 + aligned.len()) as u64).to_le_bytes());
    p.extend_from_slice(&[0u8; 16]);
    p.extend_from_slice(set_id);
    p.extend_from_slice(kind);
    p.extend_from_slice(&aligned);
    let digest = md5::compute(&p[32..]).0;
    p[16..32].copy_from_slice(&digest);
    p
}

fn ptype(suffix: &[u8]) -> [u8; 16] {
    let mut t = [0u8; 16];
    t[..8].copy_from_slice(b"PAR 2.0\0");
    t[8..8 + suffix.len().min(8)].copy_from_slice(&suffix[..suffix.len().min(8)]);
    t
}

/// A recovery set over `data` named `name`, with `recovery_blocks`
/// distinct recovery-slice packets. Returns (set bytes, set-id hex).
fn par2_set(name: &str, data: &[u8], recovery_blocks: u32) -> (Vec<u8>, String) {
    let whole_md5 = md5::compute(data).0;
    let first16k = md5::compute(&data[..data.len().min(16 * 1024)]).0;
    let mut seed = Vec::new();
    seed.extend_from_slice(&first16k);
    seed.extend_from_slice(&(data.len() as u64).to_le_bytes());
    seed.extend_from_slice(name.as_bytes());
    let file_id = md5::compute(&seed).0;

    let mut main_body = Vec::new();
    main_body.extend_from_slice(&BLOCK.to_le_bytes());
    main_body.extend_from_slice(&1u32.to_le_bytes());
    main_body.extend_from_slice(&file_id);
    let set_id = md5::compute(&main_body).0;

    let mut fd_body = Vec::new();
    fd_body.extend_from_slice(&file_id);
    fd_body.extend_from_slice(&whole_md5);
    fd_body.extend_from_slice(&first16k);
    fd_body.extend_from_slice(&(data.len() as u64).to_le_bytes());
    fd_body.extend_from_slice(name.as_bytes());

    let mut ifsc_body = Vec::new();
    ifsc_body.extend_from_slice(&file_id);
    for block in data.chunks(BLOCK as usize) {
        let mut h = crc32fast::Hasher::new();
        h.update(block);
        if block.len() < BLOCK as usize {
            h.update(&vec![0u8; BLOCK as usize - block.len()]);
        }
        ifsc_body.extend_from_slice(&[0u8; 16]);
        ifsc_body.extend_from_slice(&h.finalize().to_le_bytes());
    }

    let mut set = Vec::new();
    set.extend(packet(&set_id, &ptype(b"Main\0\0\0\0"), &main_body));
    set.extend(packet(&set_id, &ptype(b"FileDesc"), &fd_body));
    set.extend(packet(&set_id, &ptype(b"IFSC\0\0\0\0"), &ifsc_body));
    for exp in 0..recovery_blocks {
        let mut body = Vec::new();
        body.extend_from_slice(&exp.to_le_bytes());
        body.extend_from_slice(&vec![0x5au8; BLOCK as usize]);
        set.extend(packet(&set_id, &ptype(b"RecvSlic"), &body));
    }
    (set, hex::encode(set_id))
}

// --- driver doubles ------------------------------------------------------------

#[derive(Default)]
struct MemoryScratch {
    objects: RefCell<HashMap<String, (Vec<u8>, StagedFile)>>,
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

/// Canned per-segment fetch results; an unmapped segment is `Missing`.
#[derive(Default)]
struct StubSource {
    by_segment: HashMap<String, ArticleFetch>,
}

impl BodySource for StubSource {
    async fn fetch_body(&mut self, segment_id: &str) -> Result<ArticleFetch, VerifyError> {
        Ok(self
            .by_segment
            .get(segment_id)
            .cloned()
            .unwrap_or(ArticleFetch::Missing))
    }
}

fn encoded(params: &SegmentParams, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    encode_segment(&mut body, params, data).expect("encode yEnc segment");
    body
}

/// `parts`-part data file (256 B parts = one par2 block each) plus its
/// par2 file. Returns the parsed files, the present-article map (every
/// data segment and the par2 segment, all healthy), and the set-id hex.
/// Each test then overrides individual data segments to `Missing` /
/// `TooLarge` / over-cap `Present` in the map copy it drives.
fn case(
    parts: usize,
    recovery_blocks: u32,
) -> (Vec<ParsedFile>, HashMap<String, ArticleFetch>, String) {
    let data = payload(17, parts * BLOCK as usize);
    let (set_bytes, set_hex) = par2_set(DATA_NAME, &data, recovery_blocks);

    let mut present = HashMap::new();
    let mut segment_ids = Vec::new();
    for (i, chunk) in data.chunks(BLOCK as usize).enumerate() {
        let n = i as u32 + 1;
        let begin = (i * BLOCK as usize) as u64 + 1;
        let params = SegmentParams {
            name: DATA_NAME.into(),
            part: n,
            total: parts as u32,
            begin,
            end: begin + chunk.len() as u64 - 1,
            file_size: data.len() as u64,
        };
        let id = format!("d{n}@synthetic.invalid");
        present.insert(id.clone(), ArticleFetch::Present(encoded(&params, chunk)));
        segment_ids.push(id);
    }
    let par2_params = SegmentParams::single(PAR2_NAME);
    present.insert(
        "p1@synthetic.invalid".to_string(),
        ArticleFetch::Present(encoded(&par2_params, &set_bytes)),
    );

    let segments = segment_ids.len();
    let files = vec![
        ParsedFile {
            name: DATA_NAME.into(),
            is_par2: false,
            segment_ids,
            segment_bytes: vec![None; segments],
        },
        ParsedFile {
            name: PAR2_NAME.into(),
            is_par2: true,
            segment_ids: vec!["p1@synthetic.invalid".into()],
            segment_bytes: vec![None],
        },
    ];
    (files, present, set_hex)
}

async fn run(
    files: &[ParsedFile],
    mut by_segment: HashMap<String, ArticleFetch>,
    caps: &VerifyCaps,
) -> VerifyOutcome {
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = StubSource {
        by_segment: std::mem::take(&mut by_segment),
    };
    run_verifying_step(
        JOB,
        files,
        &mut progress,
        &mut source,
        &scratch,
        caps,
        &|| false,
    )
    .await
    .expect("verify step must conclude, not error")
}

// --- acceptance cases ----------------------------------------------------------

#[tokio::test]
async fn missing_segment_still_dispatches_repair_when_covered() {
    // Control (green before and after the paired fix): the second of two
    // parts is genuinely missing and the set's recovery slices cover its
    // one block, so the job dispatches repair.
    let (files, mut present, set_hex) = case(2, 2);
    present.insert("d2@synthetic.invalid".to_string(), ArticleFetch::Missing);
    let outcome = run(&files, present, &VerifyCaps::default()).await;
    match outcome {
        VerifyOutcome::RepairNeeded {
            par2_set_id,
            missing_blocks,
            ..
        } => {
            assert_eq!(
                par2_set_id, set_hex,
                "repair dispatches for the covering set"
            );
            assert_eq!(missing_blocks, 1, "one 256 B part at 256 B blocks");
        }
        other => panic!("covered missing segment must dispatch repair, got {other:?}"),
    }
}

#[tokio::test]
async fn size_refused_segment_skips_repair_dispatch() {
    // Identical geometry to the control, but the second part is refused
    // for size instead of missing. A refetch would be refused
    // identically, so no repair round may be spent on it: the job
    // outcome is terminally Unrecoverable, never RepairNeeded.
    let (files, mut present, set_hex) = case(2, 2);
    present.insert("d2@synthetic.invalid".to_string(), ArticleFetch::TooLarge);
    let outcome = run(&files, present, &VerifyCaps::default()).await;
    match outcome {
        VerifyOutcome::Unrecoverable {
            par2_set_id,
            missing_blocks,
            ..
        } => {
            assert_eq!(
                par2_set_id.as_deref(),
                Some(set_hex.as_str()),
                "the covering set is still identified in the outcome"
            );
            assert_eq!(missing_blocks, 1, "the refused part's block is still counted");
        }
        VerifyOutcome::RepairNeeded { .. } => panic!(
            "size-refused block must not dispatch repair (a refetch is refused identically), got {outcome:?}"
        ),
        other => panic!("size-refused segment must conclude Unrecoverable, got {other:?}"),
    }
}

#[tokio::test]
async fn size_refused_outcome_differs_from_missing_outcome() {
    // The headline distinction: two jobs identical in every input except
    // Missing vs TooLarge on one segment must not conclude identically.
    let (files, present, _set_hex) = case(2, 2);

    let mut missing_arm = present.clone();
    missing_arm.insert("d2@synthetic.invalid".to_string(), ArticleFetch::Missing);
    let missing_outcome = run(&files, missing_arm, &VerifyCaps::default()).await;

    let mut refused_arm = present;
    refused_arm.insert("d2@synthetic.invalid".to_string(), ArticleFetch::TooLarge);
    let refused_outcome = run(&files, refused_arm, &VerifyCaps::default()).await;

    assert_ne!(
        missing_outcome, refused_outcome,
        "TooLarge must surface distinctly from Missing in the job outcome \
         (missing arm: {missing_outcome:?}, size-refused arm: {refused_outcome:?})"
    );
}

#[tokio::test]
async fn mixed_missing_and_size_refused_damage_skips_repair() {
    // Three parts: part 2 genuinely missing, part 3 size-refused, and
    // recovery covers both blocks. Repairing only the missing block can
    // never make the file whole, so dispatching a round on the set
    // wastes it — the job concludes Unrecoverable, not RepairNeeded.
    let (files, mut present, set_hex) = case(3, 3);
    present.insert("d2@synthetic.invalid".to_string(), ArticleFetch::Missing);
    present.insert("d3@synthetic.invalid".to_string(), ArticleFetch::TooLarge);
    let outcome = run(&files, present, &VerifyCaps::default()).await;
    match outcome {
        VerifyOutcome::Unrecoverable {
            par2_set_id,
            missing_blocks,
            ..
        } => {
            assert_eq!(par2_set_id.as_deref(), Some(set_hex.as_str()));
            assert_eq!(
                missing_blocks, 2,
                "both absent parts' blocks are counted (one missing, one size-refused)"
            );
        }
        other => {
            panic!("any size-refused damage must skip repair dispatch for the job, got {other:?}")
        }
    }
}

#[tokio::test]
async fn article_over_the_in_worker_cap_is_size_refused_not_missing() {
    // The in-Worker cap is the same refusal class as the pool's 413: a
    // present article whose body exceeds `max_article_bytes` is refused
    // before decode. With the cap set above every legitimate article in
    // the case but below the refused segment's body, the outcome must
    // match the TooLarge arm — Unrecoverable, no repair dispatch.
    const CAP: usize = 4096;
    let (files, mut present, set_hex) = case(2, 2);
    for (id, fetch) in &present {
        if let ArticleFetch::Present(bytes) = fetch {
            assert!(
                bytes.len() <= CAP,
                "case article {id} ({} bytes) must fit under the test cap",
                bytes.len()
            );
        }
    }
    present.insert(
        "d2@synthetic.invalid".to_string(),
        ArticleFetch::Present(vec![b'x'; CAP + 1]),
    );
    let caps = VerifyCaps {
        max_article_bytes: CAP,
        ..VerifyCaps::default()
    };
    let outcome = run(&files, present, &caps).await;
    match outcome {
        VerifyOutcome::Unrecoverable {
            par2_set_id,
            missing_blocks,
            ..
        } => {
            assert_eq!(par2_set_id.as_deref(), Some(set_hex.as_str()));
            assert_eq!(missing_blocks, 1);
        }
        other => panic!(
            "an over-cap article must conclude like a size refusal (Unrecoverable, no repair), got {other:?}"
        ),
    }
}

#[test]
fn article_size_refused_event_is_emitted_from_the_verify_path() {
    // No native log-capture seam exists for the worker target, so the
    // event's presence is pinned at the source: the refusal site
    // (`verify.rs`) or the outcome mapping (`queue_consumer.rs`) must
    // carry the `article_size_refused` event name from the acceptance
    // criteria.
    let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut combined = String::new();
    for file in ["verify.rs", "queue_consumer.rs"] {
        let path = src_dir.join(file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        combined.push_str(&text);
    }
    assert!(
        combined.contains("article_size_refused"),
        "each size refusal must emit an `article_size_refused` log event with job/segment facts"
    );
}
