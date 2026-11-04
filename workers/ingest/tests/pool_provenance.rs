//! Pool provenance end to end: which pool accounts' successful BODY
//! operations contributed the bytes of a staged file, bound to the
//! bytes by hash.
//!
//! Contract under test:
//!
//! - A `Served { provider_id, bytes }` fetch makes `spool_one` record a
//!   content-bound proof on the spooled segment object's metadata;
//!   `Present` (legacy / unattributed) records none.
//! - An assembled file carries the full contributor set only if every
//!   segment read at assembly has a proof matching the bytes actually
//!   read back. A legacy, missing, unknown or changed segment makes the
//!   file's provenance `None` (never a partial set).
//! - A lost progress write recovers the proofs from scratch `HEAD`.
//! - Spooled staged files are conservatively `None`.
//! - Provenance is full byte coverage evidence only, never a grant.
//!
//! Driven through `verify::run_verifying_step` with a canned body
//! source and an in-memory scratch store, like `size_refused_outcome`.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use nntune_core::yenc::{encode_segment, SegmentParams};
use nntune_ingest::parse::ParsedFile;
use nntune_ingest::pool_provenance::{PoolProvenance, MAX_CONTRIBUTORS};
use nntune_ingest::transcode::select_transcode_inputs;
use nntune_ingest::verify::{
    run_verifying_step, ArticleFetch, BodySource, ScratchStore, StagedFile, VerifyCaps,
    VerifyError, VerifyOutcome, VerifyProgress,
};

const JOB: &str = "job_0123456789abcdef";
const BLOCK: u64 = 256;
const DATA_NAME: &str = "track01.flac";
const PAR2_NAME: &str = "track01.par2";

const ACCOUNT_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const ACCOUNT_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

// --- deterministic payload + par2 builder (same wire rules as verifying_e2e) ---

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

// --- doubles -----------------------------------------------------------------

#[derive(Default)]
struct MemoryScratch {
    objects: RefCell<HashMap<String, (Vec<u8>, StagedFile)>>,
}

impl MemoryScratch {
    fn meta(&self, key: &str) -> Option<StagedFile> {
        self.objects.borrow().get(key).map(|(_, m)| m.clone())
    }
    fn bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.objects.borrow().get(key).map(|(b, _)| b.clone())
    }
    /// Change stored bytes in place, leaving the recorded metadata alone.
    fn flip_byte(&self, key: &str, at: usize) {
        self.objects
            .borrow_mut()
            .get_mut(key)
            .expect("object present")
            .0[at] ^= 0xff;
    }
    /// Rewrite only the proof on an object's metadata.
    fn set_proof(&self, key: &str, proof: Option<PoolProvenance>) {
        self.objects
            .borrow_mut()
            .get_mut(key)
            .expect("object present")
            .1
            .pool_provenance = proof;
    }
}

impl ScratchStore for MemoryScratch {
    async fn head(&self, key: &str) -> Result<Option<StagedFile>, VerifyError> {
        Ok(self.meta(key))
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, VerifyError> {
        Ok(self.bytes(key))
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
    fetched: Vec<String>,
}

impl BodySource for StubSource {
    async fn fetch_body(&mut self, segment_id: &str) -> Result<ArticleFetch, VerifyError> {
        self.fetched.push(segment_id.to_string());
        Ok(self
            .by_segment
            .get(segment_id)
            .cloned()
            .unwrap_or(ArticleFetch::Missing))
    }
}

// --- case builder ----------------------------------------------------------------

fn encoded(params: &SegmentParams, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    encode_segment(&mut body, params, data).expect("encode yEnc segment");
    body
}

struct Case {
    files: Vec<ParsedFile>,
    data: Vec<u8>,
    /// Encoded article per segment id (data segments `d1..`, par2 `p1`).
    articles: HashMap<String, Vec<u8>>,
}

impl Case {
    fn data_ids(&self) -> Vec<String> {
        self.files[0].segment_ids.clone()
    }
    /// Every article served by `provider_for(segment_id)`; `None` serves
    /// it unattributed (`Present`).
    fn source(&self, provider_for: impl Fn(&str) -> Option<String>) -> StubSource {
        let by_segment = self
            .articles
            .iter()
            .map(|(id, article)| {
                let fetch = match provider_for(id) {
                    Some(provider_id) => ArticleFetch::Served {
                        provider_id,
                        bytes: article.clone(),
                    },
                    None => ArticleFetch::Present(article.clone()),
                };
                (id.clone(), fetch)
            })
            .collect();
        StubSource {
            by_segment,
            fetched: Vec::new(),
        }
    }
    fn all_from(&self, provider: &str) -> StubSource {
        self.source(|_| Some(provider.to_string()))
    }
}

/// `parts`-part data file (256 B parts) plus its par2 file.
fn case(parts: usize) -> Case {
    let data = payload(41, parts * BLOCK as usize);
    let set_bytes = par2_set(DATA_NAME, &data, 1);
    let mut articles = HashMap::new();
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
        articles.insert(id.clone(), encoded(&params, chunk));
        segment_ids.push(id);
    }
    articles.insert(
        "p1@synthetic.invalid".to_string(),
        encoded(&SegmentParams::single(PAR2_NAME), &set_bytes),
    );
    let n = segment_ids.len();
    let files = vec![
        ParsedFile {
            name: DATA_NAME.into(),
            is_par2: false,
            segment_ids,
            segment_bytes: vec![None; n],
        },
        ParsedFile {
            name: PAR2_NAME.into(),
            is_par2: true,
            segment_ids: vec!["p1@synthetic.invalid".into()],
            segment_bytes: vec![None],
        },
    ];
    Case {
        files,
        data,
        articles,
    }
}

async fn step(
    case: &Case,
    progress: &mut VerifyProgress,
    source: &mut StubSource,
    scratch: &MemoryScratch,
    caps: &VerifyCaps,
) -> VerifyOutcome {
    run_verifying_step(JOB, &case.files, progress, source, scratch, caps, &|| false)
        .await
        .expect("verify step")
}

fn two_segments() -> VerifyCaps {
    VerifyCaps {
        max_segments_per_delivery: 2,
        ..VerifyCaps::default()
    }
}

fn ids(set: &[&str]) -> BTreeSet<String> {
    set.iter().map(|s| (*s).to_string()).collect()
}

fn data_key() -> String {
    VerifyProgress::file_key(JOB, 0, 0, DATA_NAME)
}

fn account(n: usize) -> String {
    format!("{n:08x}-0000-4000-8000-000000000000")
}

// --- end to end ---------------------------------------------------------------------

#[tokio::test]
async fn single_account_gives_the_staged_file_full_provenance() {
    let c = case(3);
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = c.all_from(ACCOUNT_A);

    let outcome = step(
        &c,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
    )
    .await;
    assert_eq!(outcome, VerifyOutcome::Complete);

    let staged = &progress.staged[&0];
    let proof = staged.pool_provenance.as_ref().expect("full provenance");
    assert_eq!(proof.provider_ids(), &ids(&[ACCOUNT_A]));
    assert!(proof.matches_bytes(&c.data));
    assert!(proof.matches_identity(&staged.sha256, staged.size));
    // The proof is stored atomically with the bytes it covers.
    assert_eq!(
        scratch.bytes(&data_key()).as_deref(),
        Some(c.data.as_slice())
    );
    assert_eq!(
        scratch
            .meta(&data_key())
            .expect("meta")
            .pool_provenance
            .as_ref(),
        Some(proof)
    );
    // The provider id never leaks into the JSON a different account would
    // read as a grant: the record is evidence of coverage only.
    let json = serde_json::to_value(proof).unwrap();
    assert_eq!(json.as_object().map(|o| o.len()), Some(3));
}

#[tokio::test]
async fn different_accounts_across_deliveries_union_their_contributions() {
    let c = case(3);
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    // Delivery 1: par2 + first data segment from A, then budget stop.
    let mut a = c.all_from(ACCOUNT_A);
    let outcome = step(&c, &mut progress, &mut a, &scratch, &two_segments()).await;
    assert_eq!(outcome, VerifyOutcome::Incomplete);
    assert!(!progress.staged.contains_key(&0));
    let seg0 = VerifyProgress::spool_key(JOB, 0, 0, 0);
    let seg0_proof = scratch
        .meta(&seg0)
        .and_then(|m| m.pool_provenance)
        .expect("segment proof recorded at decode time");
    assert_eq!(seg0_proof.provider_ids(), &ids(&[ACCOUNT_A]));

    // Delivery 2: the rest from B.
    let mut b = c.all_from(ACCOUNT_B);
    let outcome = step(&c, &mut progress, &mut b, &scratch, &VerifyCaps::default()).await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(b.fetched, ["d2@synthetic.invalid", "d3@synthetic.invalid"]);

    let proof = progress.staged[&0]
        .pool_provenance
        .as_ref()
        .expect("union provenance");
    assert_eq!(proof.provider_ids(), &ids(&[ACCOUNT_A, ACCOUNT_B]));
    assert!(proof.matches_bytes(&c.data));
}

#[tokio::test]
async fn lost_progress_adopts_segment_proofs_from_scratch_head() {
    let c = case(3);
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut a = c.all_from(ACCOUNT_A);
    let outcome = step(&c, &mut progress, &mut a, &scratch, &two_segments()).await;
    assert_eq!(outcome, VerifyOutcome::Incomplete);

    // The progress write was lost: only scratch survives.
    let mut progress = VerifyProgress::new(0);
    let mut b = c.all_from(ACCOUNT_B);
    let outcome = step(&c, &mut progress, &mut b, &scratch, &VerifyCaps::default()).await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(
        b.fetched,
        ["d2@synthetic.invalid", "d3@synthetic.invalid"],
        "d1 and the par2 file were adopted, not refetched"
    );
    let proof = progress.staged[&0]
        .pool_provenance
        .as_ref()
        .expect("adopted proof joins the new contributions");
    assert_eq!(proof.provider_ids(), &ids(&[ACCOUNT_A, ACCOUNT_B]));
}

#[tokio::test]
async fn legacy_unattributed_segment_leaves_the_file_without_provenance() {
    let c = case(3);

    // One legacy `Present` segment among attributed ones.
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = c.source(|id| (id != "d2@synthetic.invalid").then(|| ACCOUNT_A.to_string()));
    let outcome = step(
        &c,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
    )
    .await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(progress.staged[&0].pool_provenance, None);
    assert_eq!(
        scratch.meta(&data_key()).expect("meta").pool_provenance,
        None
    );
    let seg1 = VerifyProgress::spool_key(JOB, 0, 0, 1);
    assert_eq!(scratch.meta(&seg1).expect("seg").pool_provenance, None);

    // Entirely unattributed.
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = c.source(|_| None);
    let outcome = step(
        &c,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
    )
    .await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(progress.staged[&0].pool_provenance, None);
}

#[tokio::test]
async fn changed_scratch_bytes_or_swapped_proof_lose_provenance() {
    for swap_proof_instead in [false, true] {
        let c = case(3);
        let scratch = MemoryScratch::default();
        let mut progress = VerifyProgress::new(0);
        let mut a = c.all_from(ACCOUNT_A);
        let outcome = step(&c, &mut progress, &mut a, &scratch, &two_segments()).await;
        assert_eq!(outcome, VerifyOutcome::Incomplete);

        let seg0 = VerifyProgress::spool_key(JOB, 0, 0, 0);
        if swap_proof_instead {
            // A well-formed proof, but for different bytes.
            let other = PoolProvenance::from_body(ACCOUNT_A, b"some other bytes").unwrap();
            scratch.set_proof(&seg0, Some(other));
        } else {
            // Same length, same recorded proof, different content.
            scratch.flip_byte(&seg0, 3);
        }

        // Recover from metadata, rather than the still-valid old cursor.
        let mut progress = VerifyProgress::new(0);
        let mut b = c.all_from(ACCOUNT_B);
        // Damage may or may not be repairable; the step must still conclude.
        let _ = step(&c, &mut progress, &mut b, &scratch, &VerifyCaps::default()).await;
        let proof = progress
            .staged
            .get(&0)
            .and_then(|s| s.pool_provenance.as_ref());
        assert_eq!(
            proof, None,
            "stale or mismatched segment bytes never yield provenance (swap={swap_proof_instead})"
        );
        if let Some(meta) = scratch.meta(&data_key()) {
            assert_eq!(meta.pool_provenance, None);
        }
    }
}

#[tokio::test]
async fn contributor_overflow_drops_provenance_at_the_bound() {
    // Exactly MAX_CONTRIBUTORS distinct accounts: still complete.
    let c = case(MAX_CONTRIBUTORS);
    let ids_by_segment: HashMap<String, String> = c
        .data_ids()
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, account(i + 1)))
        .collect();
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = c.source(|id| {
        Some(
            ids_by_segment
                .get(id)
                .cloned()
                .unwrap_or_else(|| ACCOUNT_A.to_string()),
        )
    });
    let outcome = step(
        &c,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
    )
    .await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    let proof = progress.staged[&0]
        .pool_provenance
        .as_ref()
        .expect("at the bound");
    assert_eq!(proof.provider_ids().len(), MAX_CONTRIBUTORS);

    // One more distinct account: bytes stay usable, provenance does not.
    let c = case(MAX_CONTRIBUTORS + 1);
    let ids_by_segment: HashMap<String, String> = c
        .data_ids()
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, account(i + 1)))
        .collect();
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = c.source(|id| {
        Some(
            ids_by_segment
                .get(id)
                .cloned()
                .unwrap_or_else(|| ACCOUNT_A.to_string()),
        )
    });
    let outcome = step(
        &c,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
    )
    .await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(progress.staged[&0].pool_provenance, None);
    assert_eq!(
        scratch.bytes(&data_key()).as_deref(),
        Some(c.data.as_slice())
    );
}

#[tokio::test]
async fn spooled_staged_file_is_conservatively_unattributed() {
    let c = case(3);
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = c.all_from(ACCOUNT_A);
    // Smaller than the 768 B file: staged as its spool.
    let caps = VerifyCaps {
        max_file_bytes: 512,
        ..VerifyCaps::default()
    };
    let outcome = step(&c, &mut progress, &mut source, &scratch, &caps).await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    let staged = &progress.staged[&0];
    assert!(!staged.spool.is_empty(), "file staged as its spool");
    assert_eq!(staged.pool_provenance, None);
}

#[tokio::test]
async fn transcode_input_carries_the_staged_provenance() {
    let c = case(3);
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = c.all_from(ACCOUNT_A);
    let outcome = step(
        &c,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
    )
    .await;
    assert_eq!(outcome, VerifyOutcome::Complete);

    let inputs = select_transcode_inputs(&c.files, &progress.staged, JOB, 0).expect("inputs");
    assert_eq!(inputs.len(), 1);
    assert_eq!(
        inputs[0].pool_provenance,
        progress.staged[&0].pool_provenance
    );
    assert!(inputs[0].pool_provenance.is_some());

    // Legacy checkpoint JSON without the field reads back as `None`.
    let mut json = serde_json::to_value(&inputs[0]).unwrap();
    json.as_object_mut().unwrap().remove("pool_provenance");
    let legacy: nntune_ingest::transcode::TranscodeInput = serde_json::from_value(json).unwrap();
    assert_eq!(legacy.pool_provenance, None);
}

// --- pure record API ------------------------------------------------------------------

#[test]
fn proof_json_with_unknown_or_malformed_content_is_invalid() {
    let good = PoolProvenance::from_body(ACCOUNT_A, b"decoded bytes").expect("proof");
    assert!(good.matches_bytes(b"decoded bytes"));
    assert!(!good.matches_bytes(b"decoded byteZ"));
    assert!(!good.matches_bytes(b"decoded bytes "));
    let value = serde_json::to_value(&good).unwrap();
    assert_eq!(
        serde_json::from_value::<PoolProvenance>(value.clone()).unwrap(),
        good
    );
    let sha = value["sha256"].as_str().unwrap().to_string();

    // Unknown fields are rejected outright (no grant-like extensions).
    let mut extra = value.clone();
    extra["grant"] = serde_json::json!(true);
    assert!(serde_json::from_value::<PoolProvenance>(extra).is_err());

    // Shapes that parse but must never match an identity.
    let invalid = |sha256: &str, size: u64, providers: serde_json::Value| {
        let doc = serde_json::json!({"sha256": sha256, "size": size, "provider_ids": providers});
        serde_json::from_value::<PoolProvenance>(doc)
            .map(|p| p.matches_identity(sha256, size))
            .unwrap_or(false)
    };
    assert!(invalid(&sha, 13, serde_json::json!([ACCOUNT_A])), "control");
    assert!(
        !invalid(&sha, 0, serde_json::json!([ACCOUNT_A])),
        "zero size"
    );
    assert!(!invalid(&sha, 13, serde_json::json!([])), "no contributors");
    assert!(
        !invalid(&sha, 13, serde_json::json!(["not-a-uuid"])),
        "bad id"
    );
    assert!(
        !invalid(&sha, 13, serde_json::json!([ACCOUNT_A.to_uppercase()])),
        "non-canonical id"
    );
    assert!(!invalid(
        &sha.to_uppercase(),
        13,
        serde_json::json!([ACCOUNT_A])
    ));
    assert!(
        !invalid("abc", 13, serde_json::json!([ACCOUNT_A])),
        "short sha"
    );
    // Wrong types and unknown shapes do not even parse.
    let junk = serde_json::json!({"sha256": 1, "size": "x", "provider_ids": "a"});
    assert!(serde_json::from_value::<PoolProvenance>(junk).is_err());

    // The same applies when it rides inside a staged record.
    let staged = |proof: serde_json::Value| {
        serde_json::from_value::<StagedFile>(serde_json::json!({
            "key": "k", "size": 13, "sha256": sha, "pool_provenance": proof
        }))
    };
    assert!(staged(value).unwrap().pool_provenance.is_some());
    assert!(staged(serde_json::json!({"sha256": sha})).is_err());
    let legacy = serde_json::from_value::<StagedFile>(
        serde_json::json!({"key": "k", "size": 13, "sha256": sha}),
    )
    .unwrap();
    assert_eq!(legacy.pool_provenance, None);
    assert!(serde_json::to_value(&legacy)
        .unwrap()
        .get("pool_provenance")
        .is_none());
}

#[test]
fn from_body_rejects_unattributable_input_and_bounds_deserialized_contributors() {
    assert!(
        PoolProvenance::from_body(ACCOUNT_A, b"").is_none(),
        "empty body"
    );
    assert!(PoolProvenance::from_body("", b"x").is_none());
    assert!(PoolProvenance::from_body("not-a-uuid", b"x").is_none());
    assert!(PoolProvenance::from_body(&ACCOUNT_A.to_uppercase(), b"x").is_none());

    let base = PoolProvenance::from_body(ACCOUNT_A, b"x").unwrap();
    let sha = serde_json::to_value(&base).unwrap()["sha256"]
        .as_str()
        .unwrap()
        .to_string();
    let with_n = |n: usize| {
        let providers: Vec<String> = (1..=n).map(account).collect();
        serde_json::from_value::<PoolProvenance>(
            serde_json::json!({"sha256": sha, "size": 1, "provider_ids": providers}),
        )
        .unwrap()
    };
    assert!(with_n(MAX_CONTRIBUTORS).matches_identity(&sha, 1));
    assert!(
        !with_n(MAX_CONTRIBUTORS + 1).matches_identity(&sha, 1),
        "over the metadata budget is not complete provenance"
    );
}
