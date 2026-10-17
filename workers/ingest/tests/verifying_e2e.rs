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
    run_verifying_step, BodySource, ScratchStore, StagedFile, VerifyCaps, VerifyError,
    VerifyOutcome, VerifyProgress,
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
    while aligned.len() % 4 != 0 {
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
    fn fetch_body<'a>(
        &'a mut self,
        segment_id: &'a str,
    ) -> impl std::future::Future<Output = Result<Option<Vec<u8>>, VerifyError>> + 'a {
        async move {
            self.fetches.push(segment_id.to_string());
            let mut out = Vec::new();
            match self.client.body_to(segment_id, &mut out).await {
                Ok(()) => Ok(Some(out)),
                Err(nntune_core::Error::ArticleNotFound(_)) => Ok(None),
                Err(e) => Err(VerifyError::Transient(format!("nntp fetch failed: {e}"))),
            }
        }
    }
}

#[derive(Default)]
struct MemoryScratch {
    objects: RefCell<HashMap<String, (Vec<u8>, StagedFile)>>,
}

impl MemoryScratch {
    fn bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.objects.borrow().get(key).map(|(b, _)| b.clone())
    }
    fn manifest(&self, job: &str) -> Option<String> {
        let key = format!("ingest-scratch/{job}/r0/manifest.json");
        self.bytes(&key)
            .map(|b| String::from_utf8(b).expect("manifest is UTF-8"))
    }
}

impl ScratchStore for MemoryScratch {
    fn head<'a>(
        &'a self,
        key: &'a str,
    ) -> impl std::future::Future<Output = Result<Option<StagedFile>, VerifyError>> + 'a {
        async move { Ok(self.objects.borrow().get(key).map(|(_, m)| m.clone())) }
    }

    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> impl std::future::Future<Output = Result<Option<Vec<u8>>, VerifyError>> + 'a {
        async move { Ok(self.bytes(key)) }
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        bytes: Vec<u8>,
        meta: &'a StagedFile,
    ) -> impl std::future::Future<Output = Result<(), VerifyError>> + 'a {
        async move {
            self.objects
                .borrow_mut()
                .insert(key.to_string(), (bytes, meta.clone()));
            Ok(())
        }
    }
}

async fn run_step(
    server: &TestNntpServer,
    nzb: &str,
    caps: &VerifyCaps,
    progress: &mut VerifyProgress,
    scratch: &MemoryScratch,
) -> (VerifyOutcome, Vec<String>) {
    let est = estimate_nzb(nzb).expect("intake");
    let files = parse_upload_files(nzb.as_bytes(), &est).expect("parse files");
    let mut source = NntpSource::connect(server).await;
    let outcome = run_verifying_step(JOB, &files, progress, &mut source, scratch, caps, &|| false)
        .await
        .expect("verify step");
    (outcome, source.fetches)
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

    let data_key = VerifyProgress::file_key(JOB, 0, DATA_NAME);
    assert_eq!(scratch.bytes(&data_key).expect("staged data"), data);
    let manifest = scratch.manifest(JOB).expect("manifest staged");
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let entries = manifest.as_array().expect("manifest is a list");
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|e| e["file_name"] == DATA_NAME
        && e["role"] == "data"
        && e["key"] == data_key.as_str()));
    assert!(entries
        .iter()
        .any(|e| e["file_name"] == PAR2_NAME && e["role"] == "par2"));
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
    let mut caps = VerifyCaps::default();
    caps.max_files_per_delivery = 1;
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

    let mut caps = VerifyCaps::default();
    caps.max_files_per_delivery = 1;
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
