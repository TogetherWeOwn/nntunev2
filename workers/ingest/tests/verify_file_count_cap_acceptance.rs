//! Verify file-count cap literal pins through the public verify step.
//!
//! Contract under test (`workers/ingest/src/verify.rs:72,489`):
//!
//! - `MAX_VERIFY_FILES == 1024` (most files one Verifying step walks).
//! - `run_verifying_step` rejects an empty file list or more than
//!   `MAX_VERIFY_FILES` files with the exact permanent
//!   `"file count out of bounds"` error before any fetch or scratch I/O.
//!
//! Pins in this file:
//!
//! - the literal stays exactly 1024;
//! - exactly 1024 one-segment files complete healthy through the public
//!   entry (the boundary passes the count gate);
//! - 1025 files fail closed with the exact permanent count error through
//!   the same entry (one past the cap never reaches fetch or scratch).
//!
//! Sensitivity (implementation stays untouched):
//!
//! - lowering the cap (e.g. 1023) fails `exactly_1024_passes_count_gate`
//!   with the count error instead of `Complete`;
//! - raising the cap (e.g. 1025) fails `count_1025_fails_closed` because
//!   the step proceeds past the gate (and `NoFetch` panics on the first
//!   fetch it must never reach);
//! - rewording the error string fails `count_1025_fails_closed` on the
//!   exact-message assertion;
//! - moving the count check after fetch/scratch I/O fails
//!   `count_1025_fails_closed` on the `NoFetch` / `NoScratch` panics
//!   (reads included, not just writes).
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test verify_file_count_cap_acceptance
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use nntune_core::yenc::{encode_segment, SegmentParams};
use nntune_ingest::parse::ParsedFile;
use nntune_ingest::verify::{
    run_verifying_step, ArticleFetch, BodySource, ScratchStore, StagedFile, VerifyCaps,
    VerifyError, VerifyOutcome, VerifyProgress, MAX_VERIFY_FILES,
};

const JOB: &str = "job_0123456789abcdef";
const COUNT_MSG: &str = "file count out of bounds";

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

/// The count gate runs before any fetch: any fetch here is a bug.
struct NoFetch;

impl BodySource for NoFetch {
    async fn fetch_body(&mut self, segment_id: &str) -> Result<ArticleFetch, VerifyError> {
        panic!("count gate must reject before fetching {segment_id}");
    }
}

/// The count gate runs before any scratch access: any access here is a bug.
/// Unlike `MemoryScratch` (which only records writes), this panics on reads
/// too, so moving the gate after even a read-only scratch pass goes red.
struct NoScratch;

impl ScratchStore for NoScratch {
    async fn head(&self, key: &str) -> Result<Option<StagedFile>, VerifyError> {
        panic!("count gate must reject before scratch head {key}");
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, VerifyError> {
        panic!("count gate must reject before scratch get {key}");
    }

    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, VerifyError> {
        panic!("count gate must reject before scratch listing {prefix}");
    }

    async fn put(&self, key: &str, _bytes: Vec<u8>, _meta: &StagedFile) -> Result<(), VerifyError> {
        panic!("count gate must reject before scratch put {key}");
    }
}

/// One shared 1-byte yEnc article served for every segment id.
struct AlwaysPresent {
    article: Vec<u8>,
}

impl BodySource for AlwaysPresent {
    async fn fetch_body(&mut self, _segment_id: &str) -> Result<ArticleFetch, VerifyError> {
        Ok(ArticleFetch::Present(self.article.clone()))
    }
}

fn one_byte_article() -> Vec<u8> {
    let mut body = Vec::new();
    encode_segment(&mut body, &SegmentParams::single("probe.flac"), b"X").expect("encode yEnc");
    body
}

fn files_with(n: usize) -> Vec<ParsedFile> {
    (0..n)
        .map(|i| ParsedFile {
            name: format!("file{i:04}.flac"),
            is_par2: false,
            segment_ids: vec![format!("seg{i}@synthetic.invalid")],
            segment_bytes: vec![None],
        })
        .collect()
}

#[test]
fn max_verify_files_is_1024() {
    assert_eq!(
        MAX_VERIFY_FILES, 1024,
        "verify file-count cap must stay exactly 1024"
    );
}

#[tokio::test]
async fn exactly_1024_passes_count_gate() {
    // Boundary: exactly the cap must not hit the count gate. Each file
    // carries one present 1-byte segment with no par2 cover, so a healthy
    // run stages all 1024 and completes.
    let article = one_byte_article();
    let files = files_with(1024);
    assert_eq!(files.len(), 1024);
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = AlwaysPresent { article };
    let caps = VerifyCaps {
        max_files_per_delivery: 2048,
        max_segments_per_delivery: 4096,
        ..VerifyCaps::default()
    };
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
    .expect("exactly 1024 files must pass the count gate");
    assert_eq!(
        outcome,
        VerifyOutcome::Complete,
        "1024 present one-byte files with no par2 cover must complete healthy"
    );
    assert_eq!(progress.missing_blocks, 0);
    assert_eq!(
        progress.staged.len(),
        1024,
        "every boundary file must stage"
    );
}

#[tokio::test]
async fn count_1025_fails_closed() {
    // One past the cap must fail closed with the exact permanent error
    // before any fetch or scratch access (`NoFetch` / `NoScratch` panic on
    // any access, so reaching either is a loud failure, not a quiet pass).
    let files = files_with(1025);
    assert_eq!(files.len(), 1025);
    let scratch = NoScratch;
    let mut progress = VerifyProgress::new(0);
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
        Err(VerifyError::Permanent(COUNT_MSG.into())),
        "1025 files must fail closed with the exact count error"
    );
    assert!(
        progress.staged.is_empty(),
        "the rejected step must stage nothing"
    );
}
