use super::*;
use nntune_core::{digest_enrollment::EnrollmentOutput, media::Header};
use nntune_ingest::multipart::{
    resume_checkpoint::matches_checkpoint, session_upload::DurableUpload, UploadPart, UploadedPart,
    PART_BYTES,
};
use std::collections::BTreeMap;

const ETAGS: [&str; 3] = [
    "\"fixture-first\"",
    "\"fixture-middle\"",
    "\"fixture-last\"",
];

pub(super) fn padded_flac() -> Vec<u8> {
    // Add one synthetic FLAC PADDING block so the ciphertext spans three parts.
    let padding = 2 * PART_BYTES - FLAC.len() - 4;
    let mut input = FLAC[..42].to_vec();
    input[4] = 0;
    input.push(0x81);
    input.extend_from_slice(&(padding as u32).to_be_bytes()[1..]);
    input.resize(input.len() + padding, 0);
    input.extend_from_slice(&FLAC[42..]);
    input
}

pub(super) fn partition<'a>(plan: Plan<'a>, sealed: &[EnrollmentOutput]) -> Vec<UploadPart<'a>> {
    let mut partition = plan.partition();
    let mut parts = Vec::new();
    for (index, chunk) in sealed.iter().enumerate() {
        parts.extend(
            partition
                .push(&SealedChunk {
                    digest: Sha256::digest(&chunk.bytes).into(),
                    bytes: chunk.bytes.clone(),
                    final_chunk: index + 1 == sealed.len(),
                })
                .unwrap(),
        );
    }
    parts.extend(partition.finish().unwrap());
    parts
}

pub(super) fn acknowledge<'a>(
    part: &UploadPart<'a>,
    etag: &'static str,
) -> (UploadedPart<'a>, CheckpointPart<'static>) {
    let (_, number, bytes, _) = part.request();
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let ack = part
        .verify_reply(etag.into(), &STANDARD.encode(digest))
        .unwrap();
    (
        ack,
        CheckpointPart {
            number,
            len: bytes.len(),
            etag,
            digest,
        },
    )
}

struct FixtureProvider<'a> {
    target: UploadTarget<'a>,
    stored: BTreeMap<u32, (Vec<u8>, CheckpointPart<'static>)>,
    requests: Vec<u32>,
}

impl FixtureProvider<'_> {
    fn put<'a>(&mut self, part: &UploadPart<'a>) -> (UploadedPart<'a>, CheckpointPart<'static>) {
        let (target, number, bytes, _) = part.request();
        assert!(target == self.target);
        self.requests.push(number);
        if let Some((previous, confirmation)) = self.stored.get_mut(&number) {
            assert_eq!(number, 3, "resume sent a committed neighbor");
            assert_eq!(bytes, previous.as_slice(), "retry changed ciphertext");
            let checksum = STANDARD.encode(confirmation.digest);
            let ack = part
                .verify_reply(confirmation.etag.into(), &checksum)
                .unwrap();
            previous.copy_from_slice(bytes);
            (ack, *confirmation)
        } else {
            let (ack, confirmation) = acknowledge(part, ETAGS[number as usize - 1]);
            self.stored.insert(number, (bytes.to_vec(), confirmation));
            (ack, confirmation)
        }
    }
}

pub(super) async fn resume<'a>(
    upload_parts: &[UploadPart<'a>],
    sealed: &[EnrollmentOutput],
    binding: ResumeBinding<'a>,
    (header, length): (Header, u64),
    reconnect: impl AsyncFnOnce() -> DurableUpload<'a>,
) -> (Vec<UploadedPart<'a>>, Vec<CheckpointPart<'static>>, Vec<u8>) {
    let [first, middle, last] = upload_parts else {
        panic!("expected three provider parts");
    };
    let mut provider = FixtureProvider {
        target: last.request().0,
        stored: BTreeMap::new(),
        requests: Vec::new(),
    };

    // The fixture stands in for the trusted receipt checkpoint. PostgreSQL
    // holds the upload identity and lease and records progress at completion.
    let (mut receipts, mut saved): (Vec<_>, Vec<_>) = [provider.put(first), provider.put(middle)]
        .into_iter()
        .unzip();
    let (late_ack, _) = provider.put(last);
    receipts.push(late_ack);

    let recovered = reconnect().await;

    let checkpoint = ResumeCheckpoint {
        binding,
        parts: &saved,
    };
    assert!(!matches_checkpoint(binding, &receipts, &checkpoint));
    let late_ack = receipts.pop().unwrap();
    assert!(matches_checkpoint(binding, &receipts, &checkpoint));

    let wrong_prefix = [
        saved[0],
        CheckpointPart {
            etag: "\"different-middle\"",
            ..saved[1]
        },
    ];
    assert!(!matches_checkpoint(
        binding,
        &receipts,
        &ResumeCheckpoint {
            binding,
            parts: &wrong_prefix
        },
    ));

    // Rebuild requests from retained sealed chunks after reconnect. The fixture
    // uses the verified checkpoint prefix to select the provider retry.
    let retry_plan = Plan::new(recovered.target(), header, length).unwrap();
    assert!(retry_plan == binding.plan);
    let rebuilt = partition(retry_plan, sealed);
    let (prefix, suffix) = rebuilt.split_at(checkpoint.parts.len());
    let [missing] = suffix else {
        panic!("resume must only send the uncommitted part");
    };
    assert!(SystemTime::now() < recovered.usable_until());
    let (retry_ack, confirmed) = provider.put(missing);
    saved.push(confirmed);

    let checkpoint = ResumeCheckpoint {
        binding,
        parts: &saved,
    };
    let retry_prefix = prefix.iter().zip(&saved).map(|(part, record)| {
        part.verify_reply(record.etag.into(), &STANDARD.encode(record.digest))
            .unwrap()
    });
    let retry_receipts: Vec<_> = retry_prefix.chain([retry_ack]).collect();
    let retry_binding = ResumeBinding {
        plan: retry_plan,
        ..binding
    };
    assert!(matches_checkpoint(
        retry_binding,
        &retry_receipts,
        &checkpoint
    ));

    // The late original acknowledgement must match the same checkpoint.
    let mut conflicting = saved.clone();
    conflicting[2].etag = "\"different-last\"";
    receipts.push(late_ack);
    assert!(matches_checkpoint(binding, &receipts, &checkpoint));
    assert!(!matches_checkpoint(
        binding,
        &receipts,
        &ResumeCheckpoint {
            binding,
            parts: &conflicting
        },
    ));
    assert_eq!(provider.requests, [1, 2, 3, 3]);
    let ciphertext = provider
        .stored
        .into_values()
        .flat_map(|(bytes, _)| bytes)
        .collect();
    (receipts, saved, ciphertext)
}

pub(super) fn assert_manifest(body: &[u8], upload_parts: &[UploadPart<'_>]) {
    let mut expected =
        String::from("<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    for (part, etag) in upload_parts.iter().zip(ETAGS) {
        use std::fmt::Write;
        let (_, number, _, checksum) = part.request();
        let etag = etag.trim_matches('"');
        write!(expected, "<Part><PartNumber>{number}</PartNumber><ETag>&quot;{etag}&quot;</ETag><ChecksumSHA256>{checksum}</ChecksumSHA256></Part>").unwrap();
    }
    expected.push_str("</CompleteMultipartUpload>");
    assert_eq!(body, expected.as_bytes());
}

pub(super) async fn reservation(observer: &Client, session: &str) -> (String, String) {
    let row = observer
        .query_one(
            "SELECT write_lease_id::text, xmin::text
             FROM public.ingest_session_reservations
             WHERE id = $1::text::uuid AND multipart_upload_id = $2
               AND terminal_reason IS NULL AND completed_map_id IS NULL
               AND checkpoint_revision = 0 AND checkpoint_offset = 0",
            &[&session, &UPLOAD],
        )
        .await
        .expect("the acknowledged part must leave the same upload pending");
    (row.get(0), row.get(1))
}

pub(super) async fn assert_replay(
    auth: Authorization<'_>,
    prepared: PreparedUpload<'_>,
    until: SystemTime,
    reload: impl Copy
        + AsyncFnOnce(&str, &CompletionScope<'_>) -> Result<Option<CurrentAuthorization>, StoreError>,
    committed: &CommittedOutcome,
) {
    let mut reconnect = connect(WRITER).await;
    let status = read(&mut reconnect, auth, prepared, reload)
        .await
        .unwrap()
        .unwrap();
    let UploadAttempt::Reused(resume) = open_or_reuse(
        &mut reconnect,
        auth,
        prepared,
        until,
        reload,
        async |_, _, _, _| panic!("completed session reached the upload provider"),
    )
    .await
    .unwrap() else {
        panic!("the same session must return its committed outcome");
    };
    assert_eq!(committed.media_version, MEDIA);
    assert_eq!(committed.page_versions, [PAGE]);
    for outcome in [&status, resume.outcome()] {
        assert_eq!(outcome.map, committed.map);
        assert_eq!(outcome.object, committed.object);
        assert_eq!(outcome.generation, committed.generation);
        assert_eq!(outcome.configuration, committed.configuration);
        assert_eq!(outcome.root_version, committed.root_version);
        assert_eq!(outcome.media_version, committed.media_version);
        assert_eq!(outcome.page_versions, committed.page_versions);
    }
}
