use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nntune_core::{
    digest_enrollment::{DigestEnrollment, OriginManifest},
    digest_read_snapshot::DigestReadSnapshot,
    digest_readback::ReadbackBody,
    media_envelope::{wrap_scheme2, EnvelopeContext},
    s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
};
use nntune_stream::{
    enrolled_playback::{EnrolledMedia, EnrolledPlayback, PlaybackError},
    range::{content_range_value, RangeOutcome},
    s3_range_response::S3RangeResponse,
};

#[path = "enrolled_playback/refusals.rs"]
mod refusals;

struct Body<'a>(&'a [u8]);
impl ReadbackBody for Body<'_> {
    async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.0, dst)
    }
}

const LEN: u64 = 65_537;
const ROOT: [u8; 32] = [7; 32];

fn id(n: u8) -> [u8; 16] {
    [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, n]
}

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(10)
}

struct Fixture {
    context: EnvelopeContext,
    envelope: [u8; 230],
    first: Vec<u8>,
    last: Vec<u8>,
    page: Vec<u8>,
    manifest: OriginManifest,
    media: S3MediaMetadata,
    versions: [String; 1],
}

impl Fixture {
    fn new() -> Self {
        let mut seal =
            DigestEnrollment::new(LEN, [id(1), id(2), id(3), id(4), id(5)], id(6)).unwrap();
        let context = EnvelopeContext {
            identities: [id(7), id(1), id(2), id(3), id(4), id(5)],
            plaintext_len: LEN,
            header: seal.header().to_bytes(),
        };
        let envelope = wrap_scheme2(&ROOT, &context, seal.key().expose_secret()).unwrap();
        let mut first_plaintext = [b'A'; 65_536];
        first_plaintext[65_535] = b'Y';
        let first = seal.seal_chunk(&first_plaintext).unwrap().bytes;
        let last = seal.seal_chunk(b"Z").unwrap();
        let locator = S3Locator::new(
            "fixture-media".into(),
            "media/00000000000040008000000000000003/00000000000040008000000000000004".into(),
            S3Binding::VersionId("media-v1".into()),
        )
        .unwrap();
        Self {
            media: S3MediaMetadata::from_trusted_context(locator, &context).unwrap(),
            context,
            envelope,
            first,
            last: last.bytes,
            page: last.page.unwrap(),
            manifest: seal.finish().unwrap(),
            versions: ["page-v1".into()],
        }
    }

    fn pins(&self) -> DigestReadSnapshot<'_> {
        DigestReadSnapshot {
            environment_id: id(1),
            library_id: id(2),
            reference_id: id(9),
            object_id: id(3),
            generation_id: id(4),
            provider_configuration: id(5),
            root_version_id: id(7),
            plaintext_len: LEN,
            plaintext_sha256: *self.manifest.plaintext_sha256(),
            map_id: id(6),
            media_version: "media-v1",
            page_versions: &self.versions,
            usable_until: now() + Duration::from_secs(1),
        }
    }

    fn media_input(&self) -> EnrolledMedia<'_> {
        EnrolledMedia {
            media: &self.media,
            manifest: &self.manifest,
            context: &self.context,
            root: &ROOT,
            expected_record: &self.envelope,
            retained_record: &self.envelope,
        }
    }

    fn page(&self) -> (&str, &[u8]) {
        (&self.versions[0], &self.page)
    }

    fn response(&self, first: bool) -> S3RangeResponse<'_> {
        let (range, len) = if first {
            ("bytes 48-65599/65617", 65_552)
        } else {
            ("bytes 65600-65616/65617", 17)
        };
        S3RangeResponse {
            status: 206,
            content_range: Some(range),
            content_length: Some(len),
            binding: Some(self.media.locator().binding()),
        }
    }
}

#[tokio::test]
async fn suffix_opens_two_complete_chunks_and_counts_only_writer_bytes() {
    let f = Fixture::new();
    let (outcome, play) =
        EnrolledPlayback::start(Some("bytes=-2"), f.pins(), f.media_input(), now()).unwrap();
    let RangeOutcome::Partial(range) = outcome else {
        panic!("expected 206")
    };
    assert_eq!(content_range_value(range, LEN), "bytes 65535-65536/65537");
    let mut play = play.unwrap();
    assert_eq!(
        play.next_chunk(now()).unwrap().unwrap().ciphertext,
        48..65_600
    );
    let part = play
        .open_next(
            f.media.locator(),
            f.response(true),
            Body(&f.first),
            f.page(),
            Some(&f.context.header),
            now,
        )
        .await
        .unwrap();
    assert_eq!(part.as_slice(), b"Y");
    drop(part);
    assert_eq!(play.served_bytes(), 0);
    assert!(matches!(
        play.next_chunk(now()),
        Err(PlaybackError::Delivery)
    ));
    play.written(1).unwrap();
    let part = play
        .open_next(
            f.media.locator(),
            f.response(false),
            Body(&f.last),
            f.page(),
            None,
            now,
        )
        .await
        .unwrap();
    assert_eq!(part.as_slice(), b"Z");
    assert_eq!(play.served_bytes(), 1);
    play.written(1).unwrap();
    assert_eq!(play.served_bytes(), 2);
    assert!(play.next_chunk(now()).unwrap().is_none());
    play.finish().unwrap();
}
