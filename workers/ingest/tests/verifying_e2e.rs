//! Verifying end to end against the synthetic NNTP server: real `BODY`
//! fetches, real yEnc articles, a real par2 recovery set built in-test,
//! driven through `verify::run_verifying_step` with an in-memory scratch
//! store standing in for R2.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::transports::{connect_native, NativeTlsTransport};
use nntune_core::yenc::{encode_segment, SegmentParams};
use nntune_ingest::nzb_intake::estimate_nzb;
use nntune_ingest::parse::parse_upload_files;
use nntune_ingest::verify::{
    run_verifying_step, ArticleFetch, BodySource, ScratchStore, StagedFile, VerifyCaps,
    VerifyError, VerifyOutcome, VerifyProgress,
};
use nntune_test_nntp::{Article, ServerConfig, TestNntpServer};

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

// --- par2 set builder (same wire rules as the core test builder) -------------

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

/// A recovery set over `data` named `name`, with `recovery_blocks` distinct
/// recovery-slice packets. Returns (set bytes, set-id hex).
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

/// The same set split across an index file (metadata packets only) and a
/// volume file carrying *only* recovery-slice packets, so the volume does
/// not parse standalone (no Main packet) and only contributes its slices
/// when merged with the index.
fn par2_set_split(name: &str, data: &[u8], recovery_blocks: u32) -> (Vec<u8>, Vec<u8>, String) {
    let (full, set_hex) = par2_set(name, data, recovery_blocks);
    let (index, _) = par2_set(name, data, 0);
    let set_id: [u8; 16] = hex::decode(&set_hex).expect("set hex").try_into().unwrap();
    let mut vol = Vec::new();
    for exp in 0..recovery_blocks {
        let mut body = Vec::new();
        body.extend_from_slice(&exp.to_le_bytes());
        body.extend_from_slice(&vec![0x5au8; BLOCK as usize]);
        vol.extend(packet(&set_id, &ptype(b"RecvSlic"), &body));
    }
    debug_assert_eq!(full.len(), index.len() + vol.len());
    (index, vol, set_hex)
}

// --- article + NZB construction ----------------------------------------------

fn yenc_article(
    id: &str,
    number: u32,
    subject: &str,
    params: &SegmentParams,
    data: &[u8],
) -> Article {
    let mut body = Vec::new();
    encode_segment(&mut body, params, data).expect("encode yEnc segment");
    let mut raw =
        format!("Subject: {subject}\r\nFrom: poster@example.invalid\r\n\r\n").into_bytes();
    raw.extend_from_slice(&body);
    Article {
        message_id: format!("<{id}>"),
        number,
        group: "alt.binaries.music".into(),
        subject: subject.to_string(),
        from: "poster@example.invalid".into(),
        date: "1".into(),
        raw,
        payload: data.to_vec(),
    }
}

fn encoded_len(params: &SegmentParams, data: &[u8]) -> usize {
    let mut body = Vec::new();
    encode_segment(&mut body, params, data).expect("encode yEnc segment");
    body.len()
}

/// One servable file: (nzb file name, [(message-id, yEnc segment data)]),
/// plus the articles to configure on the server.
struct ServableFile {
    name: String,
    segments: Vec<(String, usize)>, // (message-id, encoded bytes)
}

fn nzb_xml(files: &[ServableFile]) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n",
    );
    for (i, file) in files.iter().enumerate() {
        xml.push_str(&format!(
            "<file poster=\"a@b\" date=\"1\" subject=\"rel [{:02}/02] - &quot;{}&quot; yEnc (1/{})\">\n<groups><group>alt.binaries.music</group></groups>\n<segments>\n",
            i + 1,
            file.name,
            file.segments.len()
        ));
        for (n, (id, bytes)) in file.segments.iter().enumerate() {
            xml.push_str(&format!(
                "<segment bytes=\"{bytes}\" number=\"{}\">{id}</segment>\n",
                n + 1
            ));
        }
        xml.push_str("</segments>\n</file>\n");
    }
    xml.push_str("</nzb>");
    xml
}

async fn spawn(articles: Vec<Article>) -> TestNntpServer {
    let mut config = ServerConfig::for_test();
    config.articles = articles;
    TestNntpServer::spawn(config).await
}

// --- driver doubles ------------------------------------------------------------

struct NntpSource {
    client: NntpClient<NativeTlsTransport>,
    fetches: Vec<String>,
}

impl NntpSource {
    async fn connect(server: &TestNntpServer) -> Self {
        let config = NntpConfig {
            host: "127.0.0.1".into(),
            port: server.addr().port(),
            tls: false,
            username: Some("testuser".into()),
            password: Some("testpass".into()),
            timeout: Duration::from_secs(5),
            ..NntpConfig::default()
        };
        let transport = connect_native(&config).await.expect("connect");
        let client = NntpClient::connect(transport, &config)
            .await
            .expect("handshake");
        Self {
            client,
            fetches: Vec::new(),
        }
    }
}

impl BodySource for NntpSource {
    async fn fetch_body(&mut self, segment_id: &str) -> Result<ArticleFetch, VerifyError> {
        self.fetches.push(segment_id.to_string());
        let mut out = Vec::new();
        match self.client.body_to(segment_id, &mut out).await {
            Ok(()) => Ok(ArticleFetch::Present(out)),
            Err(nntune_core::Error::ArticleNotFound(_)) => Ok(ArticleFetch::Missing),
            // The client's own read limit: per-article data (judged
            // absent), mirroring the pool route's `413` — never a
            // transient that redelivers the same article forever.
            Err(nntune_core::Error::Io(e))
                if nntune_core::transport::is_article_limit_error(&e) =>
            {
                Ok(ArticleFetch::TooLarge)
            }
            Err(e) => Err(VerifyError::Transient(format!("nntp fetch failed: {e}"))),
        }
    }
}

#[derive(Default)]
struct MemoryScratch {
    objects: RefCell<HashMap<String, (Vec<u8>, StagedFile)>>,
    heads: RefCell<usize>,
    gets: RefCell<usize>,
    /// Objects that vanish at the next `get` (the mid-walk race a real
    /// bucket can present between cursor validation and read-back).
    vanish_on_get: RefCell<Vec<String>>,
}

impl MemoryScratch {
    fn bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.objects.borrow().get(key).map(|(b, _)| b.clone())
    }
    fn delete(&self, key: &str) {
        self.objects.borrow_mut().remove(key);
    }
    fn head_count(&self) -> usize {
        *self.heads.borrow()
    }
    fn get_count(&self) -> usize {
        *self.gets.borrow()
    }
    fn vanish_next_get(&self, key: &str) {
        self.vanish_on_get.borrow_mut().push(key.to_string());
    }
    fn manifest(&self, job: &str) -> Option<String> {
        let key = format!("ingest-scratch/{job}/r0/in/manifest.json");
        self.bytes(&key)
            .map(|b| String::from_utf8(b).expect("manifest is UTF-8"))
    }
}

impl ScratchStore for MemoryScratch {
    async fn head(&self, key: &str) -> Result<Option<StagedFile>, VerifyError> {
        *self.heads.borrow_mut() += 1;
        Ok(self.objects.borrow().get(key).map(|(_, m)| m.clone()))
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, VerifyError> {
        *self.gets.borrow_mut() += 1;
        if self
            .vanish_on_get
            .borrow()
            .iter()
            .any(|k| k.as_str() == key)
        {
            self.vanish_on_get
                .borrow_mut()
                .retain(|k| k.as_str() != key);
            self.objects.borrow_mut().remove(key);
            return Ok(None);
        }
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

async fn run_step(
    server: &TestNntpServer,
    nzb: &str,
    caps: &VerifyCaps,
    progress: &mut VerifyProgress,
    scratch: &MemoryScratch,
) -> (VerifyOutcome, Vec<String>) {
    run_step_with_deadline(server, nzb, caps, progress, scratch, &|| false).await
}

async fn run_step_with_deadline(
    server: &TestNntpServer,
    nzb: &str,
    caps: &VerifyCaps,
    progress: &mut VerifyProgress,
    scratch: &MemoryScratch,
    deadline: &dyn Fn() -> bool,
) -> (VerifyOutcome, Vec<String>) {
    let est = estimate_nzb(nzb).expect("intake");
    let files = parse_upload_files(nzb.as_bytes(), &est).expect("parse files");
    let mut source = NntpSource::connect(server).await;
    let outcome = run_verifying_step(JOB, &files, progress, &mut source, scratch, caps, deadline)
        .await
        .expect("verify step");
    (outcome, source.fetches)
}

/// A canned body source for behaviors the synthetic server cannot
/// produce (a specific article refused as too large).
struct StubSource {
    too_large: Vec<String>,
}

impl BodySource for StubSource {
    async fn fetch_body(&mut self, segment_id: &str) -> Result<ArticleFetch, VerifyError> {
        if self.too_large.iter().any(|id| id.as_str() == segment_id) {
            Ok(ArticleFetch::TooLarge)
        } else {
            Ok(ArticleFetch::Missing)
        }
    }
}

/// Single-part data file + its par2 file, both servable.
fn complete_case(data: &[u8], recovery_blocks: u32) -> (Vec<Article>, String, String) {
    let (set_bytes, set_hex) = par2_set(DATA_NAME, data, recovery_blocks);
    let data_params = SegmentParams::single(DATA_NAME);
    let par2_params = SegmentParams::single(PAR2_NAME);
    let articles = vec![
        yenc_article("d1@synthetic.invalid", 1, DATA_NAME, &data_params, data),
        yenc_article(
            "p1@synthetic.invalid",
            2,
            PAR2_NAME,
            &par2_params,
            &set_bytes,
        ),
    ];
    let nzb = nzb_xml(&[
        ServableFile {
            name: DATA_NAME.into(),
            segments: vec![(
                "d1@synthetic.invalid".into(),
                encoded_len(&data_params, data),
            )],
        },
        ServableFile {
            name: PAR2_NAME.into(),
            segments: vec![(
                "p1@synthetic.invalid".into(),
                encoded_len(&par2_params, &set_bytes),
            )],
        },
    ]);
    (articles, nzb, set_hex)
}

// --- acceptance cases -----------------------------------------------------------

#[tokio::test]
async fn complete_set_verifies_stages_and_writes_the_manifest() {
    let data = payload(7, 1000);
    let (articles, nzb, _set_hex) = complete_case(&data, 1);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(progress.missing_blocks, 0);

    let data_key = VerifyProgress::file_key(JOB, 0, 0, DATA_NAME);
    assert_eq!(scratch.bytes(&data_key).expect("staged data"), data);
    let manifest = scratch.manifest(JOB).expect("manifest staged");
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(
        manifest["staging_id"].as_str().map(str::len),
        Some(64),
        "manifest carries the per-staging id"
    );
    let entries = manifest["files"].as_array().expect("manifest files list");
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|e| e["file_name"] == DATA_NAME
        && e["role"] == "data"
        && e["key"] == data_key.as_str()));
    assert!(entries
        .iter()
        .any(|e| e["file_name"] == PAR2_NAME && e["role"] == "par2"));

    // Rewriting the manifest over identical staged bytes keeps the
    // staging id (content-derived); evicting and refetching a damaged
    // file's staging mints a different one.
    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    let again = scratch.manifest(JOB).expect("manifest staged");
    let again: serde_json::Value = serde_json::from_str(&again).unwrap();
    assert_eq!(again["staging_id"], manifest["staging_id"]);
}

#[tokio::test]
async fn one_corrupt_block_needs_repair_with_the_exact_count() {
    let data = payload(11, 1000);
    let (mut articles, nzb, set_hex) = complete_case(&data, 1);
    // Serve the data with one byte flipped inside block 2 (bytes 256..512):
    // the yEnc article is internally valid, the par2 block CRC is not.
    let mut corrupt = data.clone();
    corrupt[300] ^= 0xff;
    articles[0] = yenc_article(
        "d1@synthetic.invalid",
        1,
        DATA_NAME,
        &SegmentParams::single(DATA_NAME),
        &corrupt,
    );
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert_eq!(
        outcome,
        VerifyOutcome::RepairNeeded {
            par2_set_id: set_hex.clone(),
            missing_blocks: 1,
        }
    );
    assert_eq!(progress.par2_set_id.as_deref(), Some(set_hex.as_str()));
    assert_eq!(progress.missing_blocks, 1);
}

/// Two-part data file (512 B parts, 256 B blocks): the second part's state
/// decides between Repairing (recovery 2) and Failed (recovery 1).
fn two_part_case(serve_second: bool, recovery_blocks: u32) -> (Vec<Article>, String, Vec<u8>) {
    let data = payload(23, 1024);
    let (set_bytes, _hex) = par2_set(DATA_NAME, &data, recovery_blocks);
    let part = |n: u32, begin: u64, end: u64| SegmentParams {
        name: DATA_NAME.into(),
        part: n,
        total: 2,
        begin,
        end,
        file_size: data.len() as u64,
    };
    let mut articles = vec![yenc_article(
        "d1@synthetic.invalid",
        1,
        DATA_NAME,
        &part(1, 1, 512),
        &data[..512],
    )];
    if serve_second {
        articles.push(yenc_article(
            "d2@synthetic.invalid",
            2,
            DATA_NAME,
            &part(2, 513, 1024),
            &data[512..],
        ));
    }
    let par2_params = SegmentParams::single(PAR2_NAME);
    articles.push(yenc_article(
        "p1@synthetic.invalid",
        3,
        PAR2_NAME,
        &par2_params,
        &set_bytes,
    ));
    let nzb = nzb_xml(&[
        ServableFile {
            name: DATA_NAME.into(),
            segments: vec![
                (
                    "d1@synthetic.invalid".into(),
                    encoded_len(&part(1, 1, 512), &data[..512]),
                ),
                (
                    "d2@synthetic.invalid".into(),
                    encoded_len(&part(2, 513, 1024), &data[512..]),
                ),
            ],
        },
        ServableFile {
            name: PAR2_NAME.into(),
            segments: vec![(
                "p1@synthetic.invalid".into(),
                encoded_len(&par2_params, &set_bytes),
            )],
        },
    ]);
    (articles, nzb, data)
}

#[tokio::test]
async fn missing_segment_counts_its_blocks_and_repairs_when_covered() {
    let (articles, nzb, _data) = two_part_case(false, 2);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    match outcome {
        VerifyOutcome::RepairNeeded { missing_blocks, .. } => {
            assert_eq!(missing_blocks, 2, "512 missing bytes at 256 B blocks");
        }
        other => panic!("expected repair, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_segment_is_unrecoverable_when_recovery_runs_short() {
    let (articles, nzb, _data) = two_part_case(false, 1);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    match outcome {
        VerifyOutcome::Unrecoverable { missing_blocks, .. } => {
            assert_eq!(missing_blocks, 2);
        }
        other => panic!("expected unrecoverable, got {other:?}"),
    }
}

#[tokio::test]
async fn capped_step_resumes_without_refetching_staged_files() {
    let data = payload(31, 1000);
    let (articles, nzb, _hex) = complete_case(&data, 1);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    // One file per delivery: the par2 file is fetched and staged first.
    let caps = VerifyCaps {
        max_files_per_delivery: 1,
        ..VerifyCaps::default()
    };
    let (outcome, first_fetches) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Incomplete);
    assert_eq!(first_fetches, ["p1@synthetic.invalid"]);
    assert_eq!(progress.staged.len(), 1);

    // The redelivery finishes the job and never refetches the par2 segment.
    let (outcome, second_fetches) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(second_fetches, ["d1@synthetic.invalid"]);
}

#[tokio::test]
async fn lost_progress_is_rebuilt_from_staged_object_metadata() {
    let data = payload(37, 1000);
    let (articles, nzb, _hex) = complete_case(&data, 1);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    let caps = VerifyCaps {
        max_files_per_delivery: 1,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Incomplete);

    // The progress write is lost (fresh value): the staged par2 object's
    // metadata is adopted, so its segment is still never refetched.
    let mut fresh = VerifyProgress::new(0);
    let (outcome, fetches) =
        run_step(&server, &nzb, &VerifyCaps::default(), &mut fresh, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(fetches, ["d1@synthetic.invalid"]);
    assert_eq!(fresh.staged.len(), 2);
}

#[tokio::test]
async fn volume_only_recovery_slices_merge_with_the_index_set() {
    // Data file loses its second 512 B part (2 blocks); the index .par2
    // carries no recovery slices and the vol file carries only slices
    // (unparseable alone). The merged set covers the damage.
    let (articles, _nzb, data) = two_part_case(false, 0);
    let (index_bytes, vol_bytes, _hex) = par2_set_split(DATA_NAME, &data, 2);
    let index_params = SegmentParams::single(PAR2_NAME);
    let vol_name = "track01.vol01+02.par2";
    let vol_params = SegmentParams::single(vol_name);
    let mut articles = articles;
    // Replace the single par2 article from two_part_case with index+vol.
    articles.retain(|a| a.message_id != "<p1@synthetic.invalid>");
    articles.push(yenc_article(
        "p1@synthetic.invalid",
        3,
        PAR2_NAME,
        &index_params,
        &index_bytes,
    ));
    articles.push(yenc_article(
        "p2@synthetic.invalid",
        4,
        vol_name,
        &vol_params,
        &vol_bytes,
    ));
    let part = |n: u32, begin: u64, end: u64| SegmentParams {
        name: DATA_NAME.into(),
        part: n,
        total: 2,
        begin,
        end,
        file_size: data.len() as u64,
    };
    let nzb = nzb_xml(&[
        ServableFile {
            name: DATA_NAME.into(),
            segments: vec![
                (
                    "d1@synthetic.invalid".into(),
                    encoded_len(&part(1, 1, 512), &data[..512]),
                ),
                (
                    "d2@synthetic.invalid".into(),
                    encoded_len(&part(2, 513, 1024), &data[512..]),
                ),
            ],
        },
        ServableFile {
            name: PAR2_NAME.into(),
            segments: vec![(
                "p1@synthetic.invalid".into(),
                encoded_len(&index_params, &index_bytes),
            )],
        },
        ServableFile {
            name: vol_name.into(),
            segments: vec![(
                "p2@synthetic.invalid".into(),
                encoded_len(&vol_params, &vol_bytes),
            )],
        },
    ]);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    match outcome {
        VerifyOutcome::RepairNeeded { missing_blocks, .. } => assert_eq!(missing_blocks, 2),
        other => panic!("expected repair via merged volume slices, got {other:?}"),
    }
}

#[tokio::test]
async fn staged_par2_streams_packet_by_packet_past_any_aggregate_bound() {
    // Staged par2 streams into the set builder packet by packet, so the
    // aggregate across files/deliveries is never capped (and never a
    // permanent failure): here the staged set (~3 KiB over 8 recovery
    // slices) far exceeds the 1 KiB per-packet bound, yet the corrupt
    // block is still found and counted.
    let data = payload(43, 1000);
    let (mut articles, nzb, _hex) = complete_case(&data, 8);
    let mut corrupt = data.clone();
    corrupt[300] ^= 0xff;
    articles[0] = yenc_article(
        "d1@synthetic.invalid",
        1,
        DATA_NAME,
        &SegmentParams::single(DATA_NAME),
        &corrupt,
    );
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    let caps = VerifyCaps {
        max_par2_bytes: 1024,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    match outcome {
        VerifyOutcome::RepairNeeded { missing_blocks, .. } => assert_eq!(missing_blocks, 1),
        other => panic!("expected repair via streamed par2, got {other:?}"),
    }

    // With a per-packet bound below every packet, the set is unusable
    // in-Worker — but a healthy release still verifies (uncovered and
    // complete) instead of failing permanently for par2 size.
    let data = payload(47, 1000);
    let (articles, nzb, _hex) = complete_case(&data, 1);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let caps = VerifyCaps {
        max_par2_bytes: 1,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Complete);
}

#[tokio::test]
async fn evicted_staged_record_releases_its_par2_claim_on_refetch() {
    // A data file is staged and judged RepairNeeded; its staged object
    // then disappears. The refetch must be judged against its own par2
    // entry again — if the evicted record's claim leaked, the file
    // would be judged "uncovered: healthy iff every segment decoded"
    // and the corrupt block would pass as Complete.
    let data = payload(53, 1000);
    let (mut articles, nzb, _hex) = complete_case(&data, 1);
    let mut corrupt = data.clone();
    corrupt[300] ^= 0xff;
    articles[0] = yenc_article(
        "d1@synthetic.invalid",
        1,
        DATA_NAME,
        &SegmentParams::single(DATA_NAME),
        &corrupt,
    );
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert!(
        matches!(
            outcome,
            VerifyOutcome::RepairNeeded {
                missing_blocks: 1,
                ..
            }
        ),
        "{outcome:?}"
    );

    scratch.delete(&VerifyProgress::file_key(JOB, 0, 0, DATA_NAME));
    let (outcome, fetches) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert_eq!(
        fetches,
        ["d1@synthetic.invalid"],
        "only the data file refetches"
    );
    assert!(
        matches!(
            outcome,
            VerifyOutcome::RepairNeeded {
                missing_blocks: 1,
                ..
            }
        ),
        "refetched file must still be judged against its par2 entry, got {outcome:?}"
    );
}

/// Three-part data file (256 B parts) + its par2 file.
fn three_part_case(data: &[u8]) -> (Vec<Article>, String) {
    let (set_bytes, _hex) = par2_set(DATA_NAME, data, 1);
    let part = |n: u32, begin: u64, end: u64| SegmentParams {
        name: DATA_NAME.into(),
        part: n,
        total: 3,
        begin,
        end,
        file_size: data.len() as u64,
    };
    let mut articles = Vec::new();
    let mut segments = Vec::new();
    for (i, chunk) in data.chunks(256).enumerate() {
        let n = i as u32 + 1;
        let begin = (i * 256) as u64 + 1;
        let end = begin + chunk.len() as u64 - 1;
        let id = format!("d{n}@synthetic.invalid");
        articles.push(yenc_article(&id, n, DATA_NAME, &part(n, begin, end), chunk));
        segments.push((id, encoded_len(&part(n, begin, end), chunk)));
    }
    let par2_params = SegmentParams::single(PAR2_NAME);
    articles.push(yenc_article(
        "p1@synthetic.invalid",
        4,
        PAR2_NAME,
        &par2_params,
        &set_bytes,
    ));
    let nzb = nzb_xml(&[
        ServableFile {
            name: DATA_NAME.into(),
            segments,
        },
        ServableFile {
            name: PAR2_NAME.into(),
            segments: vec![(
                "p1@synthetic.invalid".into(),
                encoded_len(&par2_params, &set_bytes),
            )],
        },
    ]);
    (articles, nzb)
}

#[tokio::test]
async fn file_spanning_deliveries_resumes_at_the_segment_cursor() {
    // The data file has more segments than one delivery's budget: the
    // first delivery durably spools a prefix and stops mid-file, and
    // the next resumes at the cursor — the already-spooled segment is
    // never refetched (before the cursor, every delivery refetched the
    // same prefix, made no staged progress, and burned the attempt
    // budget until quarantine).
    let data = payload(59, 768);
    let (articles, nzb) = three_part_case(&data);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    // Two segments per delivery: par2 (1) + first data segment, then stop.
    let caps = VerifyCaps {
        max_segments_per_delivery: 2,
        ..VerifyCaps::default()
    };
    let (outcome, first_fetches) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Incomplete);
    assert_eq!(
        first_fetches,
        ["p1@synthetic.invalid", "d1@synthetic.invalid"]
    );
    assert_eq!(
        progress.spool.get(&0).map(Vec::len),
        Some(1),
        "one data segment durably spooled"
    );
    assert!(
        !progress.staged.contains_key(&0),
        "data file not staged yet"
    );

    let (outcome, second_fetches) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(
        second_fetches,
        ["d2@synthetic.invalid", "d3@synthetic.invalid"],
        "resume continues at the cursor, never refetching d1"
    );
    assert!(progress.spool.is_empty(), "assembled file drops its cursor");
}

#[tokio::test]
async fn file_over_the_in_memory_cap_is_judged_and_staged_from_its_spool() {
    // A file larger than max_file_bytes is ordinary (a FLAC track), not
    // a permanent failure: it is judged by streaming its spooled
    // segments and staged as the spool itself, with the manifest
    // carrying the segment keys for the repair handoff.
    let data = payload(61, 1000);
    let (articles, nzb, _hex) = complete_case(&data, 1);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let caps = VerifyCaps {
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Complete);

    let staged = progress.staged.get(&0).expect("data staged");
    assert_eq!(staged.key, VerifyProgress::spool_meta_key(JOB, 0, 0));
    assert_eq!(staged.size, 1000);
    assert_eq!(staged.spool.len(), 1);
    assert!(
        scratch
            .bytes(&VerifyProgress::file_key(JOB, 0, 0, DATA_NAME))
            .is_none(),
        "no assembled object for a spooled file"
    );
    let manifest = scratch.manifest(JOB).expect("manifest staged");
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let data_row = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["file_name"] == DATA_NAME)
        .expect("data row");
    assert_eq!(
        data_row["segments"].as_array().map(Vec::len),
        Some(1),
        "manifest carries the spool segments: {data_row}"
    );
}

#[tokio::test]
async fn spooled_file_counts_bad_blocks_by_streaming() {
    // Same oversize path with a corrupt block: the streaming block
    // counter finds exactly the one bad block the in-memory path finds.
    let data = payload(67, 1000);
    let (mut articles, nzb, _hex) = complete_case(&data, 1);
    let mut corrupt = data.clone();
    corrupt[300] ^= 0xff;
    articles[0] = yenc_article(
        "d1@synthetic.invalid",
        1,
        DATA_NAME,
        &SegmentParams::single(DATA_NAME),
        &corrupt,
    );
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let caps = VerifyCaps {
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert!(
        matches!(
            outcome,
            VerifyOutcome::RepairNeeded {
                missing_blocks: 1,
                ..
            }
        ),
        "streamed judgement must count the corrupt block, got {outcome:?}"
    );
}

#[tokio::test]
async fn duplicate_name_files_do_not_share_a_staged_object() {
    // Two NZB files resolve to the same name; only the first is servable.
    // The second must be fetched and judged on its own segments — if it
    // adopted the first file's staged object (same name, same key), the
    // step would wrongly return Complete.
    let data = payload(41, 500);
    let params = SegmentParams::single(DATA_NAME);
    let articles = vec![yenc_article(
        "d1@synthetic.invalid",
        1,
        DATA_NAME,
        &params,
        &data,
    )];
    let nzb = nzb_xml(&[
        ServableFile {
            name: DATA_NAME.into(),
            segments: vec![("d1@synthetic.invalid".into(), encoded_len(&params, &data))],
        },
        ServableFile {
            name: DATA_NAME.into(),
            segments: vec![("d2@synthetic.invalid".into(), encoded_len(&params, &data))],
        },
    ]);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    let (outcome, fetches) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert_ne!(outcome, VerifyOutcome::Complete);
    assert!(
        matches!(outcome, VerifyOutcome::Unrecoverable { .. }),
        "{outcome:?}"
    );
    assert_eq!(fetches, ["d1@synthetic.invalid", "d2@synthetic.invalid"]);
    assert_ne!(
        VerifyProgress::file_key(JOB, 0, 0, DATA_NAME),
        VerifyProgress::file_key(JOB, 0, 1, DATA_NAME)
    );
}

#[tokio::test]
async fn without_par2_a_missing_segment_is_unrecoverable() {
    let params = SegmentParams::single(DATA_NAME);
    let articles: Vec<Article> = vec![]; // nothing servable: every BODY is a 430
    let nzb = nzb_xml(&[ServableFile {
        name: DATA_NAME.into(),
        segments: vec![("d1@synthetic.invalid".into(), encoded_len(&params, b"x"))],
    }]);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);

    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert!(
        matches!(outcome, VerifyOutcome::Unrecoverable { .. }),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn spooled_healthy_file_is_not_read_back() {
    // A healthy oversize file is judged from its segment records plus
    // one spool listing: no segment read-back walk.
    let data = payload(71, 1000);
    let (articles, nzb, _hex) = complete_case(&data, 1);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let caps = VerifyCaps {
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(
        scratch.get_count(),
        1,
        "only the par2 restores, no data read-back walk"
    );
}

#[tokio::test]
async fn cursor_validation_lists_and_truncates_at_a_vanished_object() {
    // Segment 2's spool object is deleted between deliveries: cursor
    // validation truncates at it via one listing (no per-segment
    // HEADs), it refetches, and the file completes healthy.
    let data = payload(73, 768);
    let (articles, nzb) = three_part_case(&data);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let spool_caps = VerifyCaps {
        max_segments_per_delivery: 3,
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &spool_caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Incomplete);
    assert_eq!(progress.spool.get(&0).map(Vec::len), Some(2));
    scratch.delete(&VerifyProgress::spool_key(JOB, 0, 0, 1));
    let heads_before = scratch.head_count();
    let caps = VerifyCaps {
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let (outcome, fetches) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(
        fetches,
        ["d2@synthetic.invalid", "d3@synthetic.invalid"],
        "truncation refetches from the vanished segment"
    );
    assert_eq!(
        scratch.head_count() - heads_before,
        3,
        "staged-trust + two adoption probes only; cursor validation lists"
    );
}

#[tokio::test]
async fn spooled_counter_counts_a_range_that_vanishes_mid_walk() {
    // Corrupt first block forces the block-counter walk; segment 2's
    // spool object vanishes at its read during that walk and counts
    // as a second bad block (2 missing > 1 recovery slice).
    let data = payload(101, 768);
    let (mut articles, nzb) = three_part_case(&data);
    let mut corrupt_chunk = data[..256].to_vec();
    corrupt_chunk[10] ^= 0xff;
    let part1 = SegmentParams {
        name: DATA_NAME.into(),
        part: 1,
        total: 3,
        begin: 1,
        end: 256,
        file_size: data.len() as u64,
    };
    articles[0] = yenc_article("d1@synthetic.invalid", 1, DATA_NAME, &part1, &corrupt_chunk);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    scratch.vanish_next_get(&VerifyProgress::spool_key(JOB, 0, 0, 1));
    let caps = VerifyCaps {
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert!(
        matches!(
            outcome,
            VerifyOutcome::Unrecoverable {
                missing_blocks: 2,
                ..
            }
        ),
        "corrupt block + vanished block, got {outcome:?}"
    );
}

#[tokio::test]
async fn assembled_file_with_a_vanished_spool_object_is_damaged() {
    // Symmetric assembled-path case: segment 2 vanishes at assembly
    // read-back, zero-fills, and the block counter (not the decode-time
    // precheck) judges it.
    let data = payload(79, 768);
    let (articles, nzb) = three_part_case(&data);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    scratch.vanish_next_get(&VerifyProgress::spool_key(JOB, 0, 0, 1));
    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert!(
        matches!(
            outcome,
            VerifyOutcome::RepairNeeded {
                missing_blocks: 1,
                ..
            }
        ),
        "zero-filled bytes must not pass as healthy, got {outcome:?}"
    );
}

#[tokio::test]
async fn deadline_during_spooled_judging_restops_as_incomplete() {
    // Damaged oversize file: the block-counter walk polls the deadline
    // between segment reads. Tripping it after the walk's first
    // read-back stops as Incomplete with nothing judged.
    let data = payload(83, 768);
    let (mut articles, nzb) = three_part_case(&data);
    let mut corrupt_chunk = data[..256].to_vec();
    corrupt_chunk[10] ^= 0xff;
    let part1 = SegmentParams {
        name: DATA_NAME.into(),
        part: 1,
        total: 3,
        begin: 1,
        end: 256,
        file_size: data.len() as u64,
    };
    articles[0] = yenc_article("d1@synthetic.invalid", 1, DATA_NAME, &part1, &corrupt_chunk);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    // Spool the par2 and first two data segments.
    let spool_some = VerifyCaps {
        max_segments_per_delivery: 3,
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &spool_some, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Incomplete);
    let caps = VerifyCaps {
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let gets_before = scratch.get_count();
    // Trip once the walk has done its first segment read: par2 restore
    // is the first get of this delivery, the walk's first segment the
    // second.
    let deadline = || scratch.get_count() >= gets_before + 2;
    let (outcome, _) =
        run_step_with_deadline(&server, &nzb, &caps, &mut progress, &scratch, &deadline).await;
    assert_eq!(outcome, VerifyOutcome::Incomplete);
    assert!(!progress.staged.contains_key(&0), "nothing judged yet");
    assert_eq!(
        scratch.get_count(),
        gets_before + 2,
        "the walk stopped at its second segment boundary"
    );
}

#[tokio::test]
async fn deadline_stopped_judging_resumes_and_finishes() {
    // Same damaged oversize file, but every delivery gets the same
    // one-segment read budget that stopped the walk above. With the
    // persisted judging cursor each delivery advances the walk one
    // more segment (cursor `pos` strictly grows — the progress the
    // consumer settles on), and the file is judged within a few
    // deliveries instead of restarting until the attempt budget
    // runs out.
    let data = payload(83, 768);
    let (mut articles, nzb) = three_part_case(&data);
    let mut corrupt_chunk = data[..256].to_vec();
    corrupt_chunk[10] ^= 0xff;
    let part1 = SegmentParams {
        name: DATA_NAME.into(),
        part: 1,
        total: 3,
        begin: 1,
        end: 256,
        file_size: data.len() as u64,
    };
    articles[0] = yenc_article("d1@synthetic.invalid", 1, DATA_NAME, &part1, &corrupt_chunk);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let spool_some = VerifyCaps {
        max_segments_per_delivery: 3,
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &spool_some, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Incomplete);
    let caps = VerifyCaps {
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let mut last_pos = 0u64;
    let mut final_outcome = None;
    for _ in 0..6 {
        let gets_before = scratch.get_count();
        // Par2 restore reads once per delivery; the walk then gets
        // exactly one more segment read before the deadline trips.
        let deadline = || scratch.get_count() >= gets_before + 2;
        let (outcome, _) =
            run_step_with_deadline(&server, &nzb, &caps, &mut progress, &scratch, &deadline).await;
        if outcome != VerifyOutcome::Incomplete {
            final_outcome = Some(outcome);
            break;
        }
        let pos = progress
            .judging
            .get(&0)
            .map(|c| c.pos)
            .expect("a stopped walk persists its cursor");
        assert!(pos > last_pos, "cursor advanced: {last_pos} -> {pos}");
        last_pos = pos;
    }
    assert!(
        matches!(
            final_outcome,
            Some(VerifyOutcome::RepairNeeded {
                missing_blocks: 1,
                ..
            })
        ),
        "the resumed walk finishes and counts the corrupt block, got {final_outcome:?}"
    );
    assert!(progress.judging.is_empty(), "cursor cleared once judged");
}

#[tokio::test]
async fn too_large_article_counts_only_its_own_segment() {
    // Middle article refused as too large: it records missing, the
    // cursor advances, and the other two segments still fetch and
    // vouch for their blocks.
    let data = payload(89, 768);
    let (articles, nzb) = three_part_case(&data);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let est = estimate_nzb(&nzb).expect("intake");
    let files = parse_upload_files(nzb.as_bytes(), &est).expect("parse files");

    // Spool the par2 and first data segment through the real server.
    let caps = VerifyCaps {
        max_segments_per_delivery: 2,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Incomplete);

    // Middle article refuses as too large; the third fetches for real
    // via a fresh server-backed source on the last delivery.
    let mut stub = StubSource {
        too_large: vec!["d2@synthetic.invalid".into()],
    };
    let outcome = run_verifying_step(
        JOB,
        &files,
        &mut progress,
        &mut stub,
        &scratch,
        &VerifyCaps {
            max_segments_per_delivery: 1,
            ..VerifyCaps::default()
        },
        &|| false,
    )
    .await
    .expect("verify step");
    assert_eq!(outcome, VerifyOutcome::Incomplete);
    assert_eq!(progress.spool.get(&0).map(Vec::len), Some(2));

    let (outcome, fetches) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert_eq!(fetches, ["d3@synthetic.invalid"]);
    assert!(
        matches!(
            outcome,
            VerifyOutcome::RepairNeeded {
                missing_blocks: 1,
                ..
            }
        ),
        "only the refused segment's block counts, got {outcome:?}"
    );
}

#[tokio::test]
async fn corrupt_par2_stream_does_not_silence_the_set() {
    // A second par2 file of the same set is served with a corrupt
    // recovery packet (fails builder validation); only that stream
    // drops — the index file's own slice still covers the corrupt
    // data block.
    let data = payload(97, 1000);
    let (mut articles, _nzb, _hex) = complete_case(&data, 1);
    let mut corrupt = data.clone();
    corrupt[300] ^= 0xff;
    articles[0] = yenc_article(
        "d1@synthetic.invalid",
        1,
        DATA_NAME,
        &SegmentParams::single(DATA_NAME),
        &corrupt,
    );
    let (_index_bytes, mut vol_bytes, _hex) = par2_set_split(DATA_NAME, &data, 1);
    let last = vol_bytes.len() - 10;
    vol_bytes[last] ^= 0xff; // breaks the packet's own MD5
    let vol_name = "track01.vol01+01.par2";
    let vol_params = SegmentParams::single(vol_name);
    articles.push(yenc_article(
        "p2@synthetic.invalid",
        3,
        vol_name,
        &vol_params,
        &vol_bytes,
    ));
    let (full_set, _h) = par2_set(DATA_NAME, &data, 1);
    let index_params = SegmentParams::single(PAR2_NAME);
    let nzb = nzb_xml(&[
        ServableFile {
            name: DATA_NAME.into(),
            segments: vec![(
                "d1@synthetic.invalid".into(),
                encoded_len(&SegmentParams::single(DATA_NAME), &corrupt),
            )],
        },
        ServableFile {
            name: PAR2_NAME.into(),
            segments: vec![(
                "p1@synthetic.invalid".into(),
                encoded_len(&index_params, &full_set),
            )],
        },
        ServableFile {
            name: vol_name.into(),
            segments: vec![(
                "p2@synthetic.invalid".into(),
                encoded_len(&vol_params, &vol_bytes),
            )],
        },
    ]);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    assert!(
        matches!(
            outcome,
            VerifyOutcome::RepairNeeded {
                missing_blocks: 1,
                ..
            }
        ),
        "the good index slice must still cover, got {outcome:?}"
    );
}

fn two_part_par2_case() -> (Vec<Article>, String) {
    // Data loses its second part (2 blocks). The par2 set is split
    // index (metadata only) + volume (2 slices), and the volume is
    // served in two parts with the second part missing: its assembled
    // bytes are zero-filled past the gap.
    let (articles, _nzb, data) = two_part_case(false, 0);
    let (index_bytes, vol_bytes, _hex) = par2_set_split(DATA_NAME, &data, 2);
    let mid = vol_bytes.len() / 2;
    let split = (mid / 4) * 4;
    let index_params = SegmentParams::single(PAR2_NAME);
    let vol_name = "track01.vol01+02.par2";
    let vpart = |n: u32, begin: u64, end: u64| SegmentParams {
        name: vol_name.into(),
        part: n,
        total: 2,
        begin,
        end,
        file_size: vol_bytes.len() as u64,
    };
    let mut articles = articles;
    articles.retain(|a| a.message_id != "<p1@synthetic.invalid>");
    articles.push(yenc_article(
        "p1@synthetic.invalid",
        3,
        PAR2_NAME,
        &index_params,
        &index_bytes,
    ));
    articles.push(yenc_article(
        "v1@synthetic.invalid",
        4,
        vol_name,
        &vpart(1, 1, split as u64),
        &vol_bytes[..split],
    ));
    // v2 is never served: the volume's second half is a 430.
    let part = |n: u32, begin: u64, end: u64| SegmentParams {
        name: DATA_NAME.into(),
        part: n,
        total: 2,
        begin,
        end,
        file_size: data.len() as u64,
    };
    let nzb = nzb_xml(&[
        ServableFile {
            name: DATA_NAME.into(),
            segments: vec![
                (
                    "d1@synthetic.invalid".into(),
                    encoded_len(&part(1, 1, 512), &data[..512]),
                ),
                (
                    "d2@synthetic.invalid".into(),
                    encoded_len(&part(2, 513, 1024), &data[512..]),
                ),
            ],
        },
        ServableFile {
            name: PAR2_NAME.into(),
            segments: vec![(
                "p1@synthetic.invalid".into(),
                encoded_len(&index_params, &index_bytes),
            )],
        },
        ServableFile {
            name: vol_name.into(),
            segments: vec![
                (
                    "v1@synthetic.invalid".into(),
                    encoded_len(&vpart(1, 1, split as u64), &vol_bytes[..split]),
                ),
                (
                    "v2@synthetic.invalid".into(),
                    encoded_len(
                        &vpart(2, split as u64 + 1, vol_bytes.len() as u64),
                        &vol_bytes[split..],
                    ),
                ),
            ],
        },
    ]);
    (articles, nzb)
}

#[tokio::test]
async fn damaged_assembled_par2_volume_keeps_its_leading_slices() {
    // Only two slices exist (both in the volume) and the volume's
    // second part is missing. The complete-packet prefix keeps slice
    // 1: the set then joins and counts 1 recovery block, so the
    // verdict is Unrecoverable (2 missing > 1) rather than the
    // set-absent path.
    let (articles, nzb) = two_part_par2_case();
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let (outcome, _) = run_step(
        &server,
        &nzb,
        &VerifyCaps::default(),
        &mut progress,
        &scratch,
    )
    .await;
    match outcome {
        VerifyOutcome::Unrecoverable {
            par2_set_id: Some(_),
            missing_blocks: 2,
        } => {}
        other => panic!("leading slice must keep the set joined, got {other:?}"),
    }
}

#[tokio::test]
async fn staged_spooled_record_is_rechecked_against_its_spool() {
    // A healthy spooled staging is trusted by its index record alone
    // until a segment object vanishes: the resume listing then evicts
    // the record and the segment refetches instead of the manifest
    // shipping a dangling key.
    let data = payload(103, 1000);
    let (articles, nzb, _hex) = complete_case(&data, 1);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let caps = VerifyCaps {
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Complete);

    scratch.delete(&VerifyProgress::spool_key(JOB, 0, 0, 0));
    let (outcome, fetches) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(
        fetches,
        ["d1@synthetic.invalid"],
        "the vanished spool segment refetches"
    );
}

#[tokio::test]
async fn spooled_adoption_is_refused_when_a_spool_object_vanished() {
    // Lost-progress twin of the resume re-check: with a fresh progress
    // value, the staged spool index is only adopted while its segment
    // objects still exist; a vanished one refuses adoption and the
    // segment refetches through the cursor path.
    let data = payload(107, 1000);
    let (articles, nzb, _hex) = complete_case(&data, 1);
    let server = spawn(articles).await;
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let caps = VerifyCaps {
        max_file_bytes: 100,
        ..VerifyCaps::default()
    };
    let (outcome, _) = run_step(&server, &nzb, &caps, &mut progress, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Complete);

    scratch.delete(&VerifyProgress::spool_key(JOB, 0, 0, 0));
    let mut fresh = VerifyProgress::new(0);
    let (outcome, fetches) = run_step(&server, &nzb, &caps, &mut fresh, &scratch).await;
    assert_eq!(outcome, VerifyOutcome::Complete);
    assert_eq!(
        fetches,
        ["d1@synthetic.invalid"],
        "adoption refused: the vanished segment refetches"
    );
}
