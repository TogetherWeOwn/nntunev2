use super::{hex, Body, CommittedFlacUpload, CommittedOutcome, BUCKET, MEDIA, PAGE};
use nntune_core::{
    digest_enrollment::OriginManifest,
    digest_read_snapshot::DigestReadSnapshot,
    digest_readback::{EnrolledEnvelope, ReadbackPin},
    original_source::SourceError,
    s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
};
use nntune_stream::{
    delivered::{parse_delivered_report, DeliveredInterval},
    enrolled_playback::{
        route::{serve, CatalogueMedia, PrivateSource},
        EnrolledMedia, S3Get, S3GetReply,
    },
    grant::Grant,
    meter_sink::{MeterBook, WindowReport},
    original_play::OriginalWriter,
    play_route::PlayRouteRequest,
};
use std::{
    io,
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const USER: &str = "synthetic-seek-listener";
const PLAY: &str = "synthetic-seek-play";
const TRACK: &str = "synthetic-seek-track";
const NOW: u64 = 60;

struct Source<'a> {
    replay: &'a CommittedOutcome,
    envelope: EnrolledEnvelope<'a>,
    metadata: &'a S3MediaMetadata,
    origin: &'a OriginManifest,
    ciphertext: &'a [u8],
    page: &'a [u8],
    page_key: &'a str,
}

impl<'a> PrivateSource for Source<'a> {
    type Body = Body<'a>;

    async fn resolve(&self, grant: &Grant) -> Option<CatalogueMedia<'_>> {
        assert_eq!(grant.user_id, USER);
        assert_eq!(grant.track_id, TRACK);
        assert_eq!(grant.play_id.as_deref(), Some(PLAY));
        let id = |value: &str| {
            u128::from_str_radix(&value.replace('-', ""), 16)
                .unwrap()
                .to_be_bytes()
        };
        let reference = [0x77; 16];
        let context = self.envelope.context;
        Some(CatalogueMedia {
            reference,
            pins: DigestReadSnapshot {
                environment_id: context.identities[1],
                library_id: context.identities[2],
                reference_id: reference,
                object_id: id(&self.replay.object),
                generation_id: id(&self.replay.generation),
                provider_configuration: id(&self.replay.configuration),
                root_version_id: id(&self.replay.root_version),
                plaintext_len: context.plaintext_len,
                plaintext_sha256: *self.origin.plaintext_sha256(),
                map_id: id(&self.replay.map),
                media_version: &self.replay.media_version,
                page_versions: &self.replay.page_versions,
                usable_until: UNIX_EPOCH + Duration::from_secs(120),
            },
            media: EnrolledMedia {
                media: self.metadata,
                manifest: self.origin.into(),
                context,
                root: self.envelope.authorized_root,
                expected_record: self.envelope.trusted_record,
                retained_record: self.envelope.retained_record,
            },
        })
    }

    async fn lease_live(&self, _: &Grant) -> bool {
        true
    }

    async fn media(&self, get: S3Get<'_>) -> io::Result<S3GetReply<Self::Body>> {
        assert_eq!(get.configuration, self.envelope.context.identities[5]);
        assert!(get.locator == self.metadata.locator());
        assert!(matches!(get.locator.binding(), S3Binding::VersionId(v) if v == MEDIA));
        // The selected FLAC frames fit in the second sealed chunk.
        assert_eq!(get.range, 65_600..131_152);
        assert_eq!(get.sealed_len, 177_271);
        Ok(S3GetReply {
            status: 206,
            version: Some(MEDIA.into()),
            content_range: Some("bytes 65600-131151/177271".into()),
            content_length: Some(65_552),
            has_content_encoding: false,
            body: Body(&self.ciphertext[get.range.start as usize..get.range.end as usize]),
        })
    }

    async fn page(
        &self,
        bucket: &str,
        pin: ReadbackPin<'_>,
        length: u32,
        _: SystemTime,
    ) -> io::Result<(String, Vec<u8>)> {
        assert_eq!(bucket, BUCKET);
        assert_eq!(pin.configuration, self.envelope.context.identities[5]);
        assert_eq!(pin.object_key, self.page_key);
        assert_eq!(pin.version, Some(PAGE));
        assert_eq!(length as usize, self.page.len());
        Ok((PAGE.into(), self.page.to_vec()))
    }
}

struct Writer<'a>(&'a mut Vec<u8>);

impl OriginalWriter for Writer<'_> {
    fn poll_cancel(&mut self, _: &mut Context<'_>) -> Poll<SourceError> {
        Poll::Pending
    }

    fn poll_write(
        &mut self,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<Result<usize, SourceError>> {
        self.0.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_close(&mut self, _: &mut Context<'_>) -> Poll<Result<(), SourceError>> {
        Poll::Ready(Ok(()))
    }

    fn abort(&mut self) {
        panic!("the indexed response was aborted");
    }
}

pub(super) async fn verify(
    committed: &CommittedFlacUpload<'_, '_>,
    replay: &CommittedOutcome,
    envelope: EnrolledEnvelope<'_>,
    input: &[u8],
    ciphertext: &[u8],
    page: &[u8],
    page_key: &str,
) {
    let verified = committed.verified();
    assert!(verified.matches_committed_outcome(replay));
    let object = replay.object.replace('-', "");
    let generation = replay.generation.replace('-', "");
    let locator = S3Locator::new(
        BUCKET.into(),
        format!("media/{object}/{generation}"),
        S3Binding::VersionId(replay.media_version.clone()),
    )
    .unwrap();
    let metadata = S3MediaMetadata::from_trusted_context(locator, envelope.context).unwrap();
    let origin = verified.origin();
    let digest = origin.plaintext_sha256();

    // Tone A's sparse anchors at 5.015 and 10.031 seconds bracket this seek.
    let seek = committed
        .flac_plaintext_range(&metadata, digest, 5020..5021)
        .expect("the completed FLAC has a later seek anchor");
    assert_eq!(seek, 68_861..129_212);
    let source = Source {
        replay,
        envelope,
        metadata: &metadata,
        origin,
        ciphertext,
        page,
        page_key,
    };
    let secret = b"synthetic upload seek playback grant";
    let grant =
        crate::private_playback::mint_grant(secret, USER, TRACK, PLAY, 1, 120, &hex(digest))
            .unwrap();
    let request = PlayRouteRequest {
        track_id: TRACK.into(),
        token: Some(grant.token),
        range: Some(format!("bytes={}-{}", seek.start, seek.end - 1)),
        trace_id: None,
    };
    let mut output = Vec::new();
    let mut reports = Vec::new();
    let mut meter = MeterBook::new(USER);
    serve(
        &request,
        secret,
        Some(&source),
        || UNIX_EPOCH + Duration::from_secs(NOW),
        |head| {
            assert_eq!(head.status, 206);
            assert_eq!(head.len, 60_351);
            assert_eq!(
                head.content_range.as_deref(),
                Some("bytes 68861-129211/177175")
            );
            Ok(Some(Writer(&mut output)))
        },
        |report| {
            let body = serde_json::to_vec(&report).unwrap();
            let interval = parse_delivered_report(&body, USER).unwrap();
            assert!(meter.record(&interval, NOW));
            reports.push(report);
        },
    )
    .await
    .unwrap();
    assert_eq!(output, input[68_861..129_212]);
    assert_eq!(
        reports,
        [DeliveredInterval {
            user_id: USER.into(),
            play_id: PLAY.into(),
            track_id: TRACK.into(),
            start: 68_861,
            end: 129_212,
            file_bytes: Some(177_175),
        }]
    );
    assert_eq!(
        meter.pending_windows(),
        [WindowReport {
            user_id: USER.into(),
            play_id: PLAY.into(),
            track_id: TRACK.into(),
            union_bytes: 60_351,
            file_bytes: Some(177_175),
            started_at: NOW,
        }]
    );
}
