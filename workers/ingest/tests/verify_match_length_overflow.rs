//! Regression: ingest verify length-fallback overflow fails closed.
//!
//! The length fallback in `match_set_file` must never panic on segment-size
//! overflow (debug) nor match a wrapped length (release). The loader's
//! checked geometry fold already rejects overflowing part geometry with the
//! permanent `"file geometry out of bounds"` error before any allocation;
//! the matcher must agree by refusing to compute an unchecked sum.
//!
//! Both cases drive the public `run_verifying_step` with bounded,
//! metadata-only spool records and tiny present scratch objects — no
//! buffers of the declared enormous sizes are ever allocated:
//!
//! - overflow: three all-present segments sized `[2^63, 2^63, 1]` whose
//!   true sum overflows `u64` (wrapping to `1` in release). The par2 set
//!   covers a 1-byte file, so a wrapped sum would falsely match. The step
//!   must return the exact permanent geometry error, never panic and never
//!   stage a wrapped-length match.
//! - control: one all-present 1-byte segment over the same 1-byte par2 set
//!   with an obfuscated name. The unique length match still applies and the
//!   step completes healthy, proving valid matching is unchanged.

use std::cell::RefCell;
use std::collections::HashMap;

use nntune_ingest::parse::ParsedFile;
use nntune_ingest::verify::{
    run_verifying_step, ArticleFetch, BodySource, ScratchStore, StagedFile, StagedSpoolSegment,
    VerifyCaps, VerifyError, VerifyOutcome, VerifyProgress,
};

const JOB: &str = "job_0123456789abcdef";
const REAL_NAME: &str = "track01.flac";
const PAR2_NAME: &str = "track01.par2";
const OBFUSCATED: &str = "a1b2c3d4e5f6.obf";
const BLOCK: u64 = 256;
const GEOMETRY_ERROR: &str = "file geometry out of bounds";

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

// --- par2 set builder (same wire rules as the verifying e2e) ---

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

fn par2_set(name: &str, data: &[u8], recovery_blocks: u32) -> Vec<u8> {
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
    set
}

// --- driver doubles ---

/// No fetch may happen: every segment is pre-spooled, so any fetch is a bug.
struct NoFetch;

impl BodySource for NoFetch {
    async fn fetch_body(&mut self, segment_id: &str) -> Result<ArticleFetch, VerifyError> {
        panic!("unexpected fetch of {segment_id}");
    }
}

#[derive(Default)]
struct MemoryScratch {
    objects: RefCell<HashMap<String, (Vec<u8>, StagedFile)>>,
}

impl MemoryScratch {
    fn put_obj(&self, key: &str, bytes: Vec<u8>) {
        let meta = StagedFile {
            key: key.to_string(),
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            missing_blocks: 0,
            size_refused_blocks: 0,
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

fn parsed(name: &str, is_par2: bool, segments: usize) -> ParsedFile {
    ParsedFile {
        name: name.into(),
        is_par2,
        segment_ids: (0..segments).map(|n| format!("seg{n}@x")).collect(),
        segment_bytes: (0..segments).map(|_| None).collect(),
    }
}

/// Stage the par2 index as one assembled object so `load_sets` parses it.
fn stage_par2(scratch: &MemoryScratch, progress: &mut VerifyProgress, set_bytes: &[u8]) {
    let key = VerifyProgress::file_key(JOB, 0, 0, PAR2_NAME);
    let staged = StagedFile {
        key: key.clone(),
        size: set_bytes.len() as u64,
        sha256: sha256_hex(set_bytes),
        missing_blocks: 0,
        size_refused_blocks: 0,
        par2_set_id: None,
        spool: Vec::new(),
        crc32: 0,
        missing: false,
    };
    scratch
        .objects
        .borrow_mut()
        .insert(key.clone(), (set_bytes.to_vec(), staged.clone()));
    progress.staged.insert(0, staged);
}

/// Pre-spool the data file (index 1) with the given declared sizes, each
/// backed by a tiny present scratch object. Returns the declared sizes.
fn prespool(
    scratch: &MemoryScratch,
    progress: &mut VerifyProgress,
    sizes: &[u64],
    bodies: &[Vec<u8>],
) {
    assert_eq!(sizes.len(), bodies.len());
    let mut recs = Vec::new();
    for (n, (size, body)) in sizes.iter().zip(bodies.iter()).enumerate() {
        let key = VerifyProgress::spool_key(JOB, 0, 1, n);
        scratch.put_obj(&key, body.clone());
        recs.push(StagedSpoolSegment {
            key,
            size: *size,
            crc32: crc32fast::hash(body),
            missing: false,
            size_refused: false,
        });
    }
    // The scratch objects stay tiny: the test never materializes the
    // declared enormous lengths.
    let total_staged: usize = scratch
        .objects
        .borrow()
        .values()
        .map(|(b, _)| b.len())
        .sum();
    assert!(
        total_staged < 4096,
        "scratch must stay tiny, got {total_staged} bytes"
    );
    progress.spool.insert(1, recs);
}

fn files_for(segments: usize) -> Vec<ParsedFile> {
    vec![
        parsed(PAR2_NAME, true, 1),
        ParsedFile {
            name: OBFUSCATED.into(),
            is_par2: false,
            segment_ids: (0..segments).map(|n| format!("seg{n}@x")).collect(),
            segment_bytes: (0..segments).map(|_| None).collect(),
        },
    ]
}

#[tokio::test]
async fn match_length_overflow_fails_closed_with_geometry_error() {
    // A 1-byte file: its recovery entry has length 1, exactly the value a
    // wrapping `[2^63, 2^63, 1]` sum would falsely produce in release
    // (2^63 + 2^63 wraps to 0, + 1 = 1). The step must not panic (debug)
    // nor claim that wrapped match (release); it must report the loader's
    // exact permanent geometry error.
    let data = b"X".to_vec();
    let set_bytes = par2_set(REAL_NAME, &data, 1);
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    stage_par2(&scratch, &mut progress, &set_bytes);

    let huge = 1u64 << 63;
    prespool(
        &scratch,
        &mut progress,
        &[huge, huge, 1],
        &[b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
    );

    let files = files_for(3);
    let mut source = NoFetch;
    let result = run_verifying_step(
        JOB,
        &files,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
        &|| false,
    )
    .await;
    assert_eq!(
        result,
        Err(VerifyError::Permanent(GEOMETRY_ERROR.into())),
        "overflowing match length must fail closed with the exact geometry error"
    );
    // Nothing staged for the overflowing file: no wrapped-length match.
    assert!(
        !progress.staged.contains_key(&1),
        "the overflowing file must not stage"
    );
}

#[tokio::test]
async fn non_overflow_length_match_still_completes() {
    // Control over the same 1-byte set: one present 1-byte segment with an
    // obfuscated name still matches by unique length and stages healthy.
    let data = b"X".to_vec();
    let set_bytes = par2_set(REAL_NAME, &data, 1);
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    stage_par2(&scratch, &mut progress, &set_bytes);
    prespool(&scratch, &mut progress, &[1], std::slice::from_ref(&data));

    let files = files_for(1);
    let mut source = NoFetch;
    let outcome = run_verifying_step(
        JOB,
        &files,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
        &|| false,
    )
    .await
    .expect("control step succeeds");
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(progress.missing_blocks, 0);
    let staged = progress.staged.get(&1).expect("data file staged");
    assert_eq!(staged.size, 1);
    assert!(staged.par2_set_id.is_some(), "length match covers the file");
    let staged_key = VerifyProgress::file_key(JOB, 0, 1, OBFUSCATED);
    let bytes = scratch
        .get(&staged_key)
        .await
        .expect("scratch read")
        .expect("staged bytes present");
    assert_eq!(bytes, data);
}
