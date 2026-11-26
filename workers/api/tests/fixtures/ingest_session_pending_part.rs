use super::*;
use nntune_ingest::multipart::{
    resume_checkpoint::matches_checkpoint, UploadPart, UploadedPart, PART_BYTES,
};

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

pub(super) async fn resume<'a>(
    upload_parts: &[UploadPart<'a>],
    binding: ResumeBinding<'a>,
    reconnect: impl AsyncFnOnce(),
) -> (Vec<UploadedPart<'a>>, Vec<CheckpointPart<'static>>) {
    let [first, middle, last] = upload_parts else {
        panic!("expected three provider parts");
    };
    // The fixture stands in for the trusted receipt checkpoint. PostgreSQL
    // holds the upload identity and lease and records progress at completion.
    let (mut receipts, mut saved): (Vec<_>, Vec<_>) = [
        acknowledge(first, "\"fixture-first\""),
        acknowledge(middle, "\"fixture-middle\""),
    ]
    .into_iter()
    .unzip();
    let committed_prefix = saved.clone();
    let (late_ack, _) = acknowledge(last, "\"fixture-last\"");
    receipts.push(late_ack);

    reconnect().await;

    let checkpoint = ResumeCheckpoint {
        binding,
        parts: &saved,
    };
    assert!(!matches_checkpoint(binding, &receipts, &checkpoint));
    let late_ack = receipts.pop().unwrap();
    assert!(matches_checkpoint(binding, &receipts, &checkpoint));
    let [missing] = &upload_parts[checkpoint.parts.len()..] else {
        panic!("resume must only send the uncommitted part");
    };
    let (target, number, bytes, checksum) = missing.request();
    let (original_target, _, original_bytes, original_checksum) = last.request();
    assert!(target == original_target);
    assert_eq!(
        (number, bytes, checksum),
        (3, original_bytes, original_checksum)
    );
    let (_, confirmed) = acknowledge(missing, "\"fixture-last\"");
    saved.push(confirmed);
    assert!(saved.starts_with(&committed_prefix));

    // The late original acknowledgement agrees with the now-confirmed retry.
    // Use it for the exact manifest that will be dispatched once by the caller.
    receipts.push(late_ack);
    assert!(matches_checkpoint(
        binding,
        &receipts,
        &ResumeCheckpoint {
            binding,
            parts: &saved
        },
    ));
    (receipts, saved)
}

pub(super) fn assert_manifest(body: &[u8], upload_parts: &[UploadPart<'_>]) {
    let mut expected =
        String::from("<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    for (part, etag) in upload_parts.iter().zip(["first", "middle", "last"]) {
        use std::fmt::Write;
        let (_, number, _, checksum) = part.request();
        write!(expected, "<Part><PartNumber>{number}</PartNumber><ETag>&quot;fixture-{etag}&quot;</ETag><ChecksumSHA256>{checksum}</ChecksumSHA256></Part>").unwrap();
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
