use nntune_core::digest_enrollment::DigestEnrollment;
use nntune_core::digest_readback::{
    verify_enrolled_readback_with_seek, BodyReadbackError, CompletedInventory, EnrolledEnvelope,
    EnrolledReadbackError, FlacSeekIndex, ReadbackBody, ReadbackError, ReadbackPin,
    ReadbackResponse,
};
use nntune_core::media::PLAIN_CHUNK_SIZE;
use nntune_core::media_envelope::{wrap_scheme2, EnvelopeContext};

const ID: [u8; 16] = [1, 1, 1, 1, 1, 1, 0x40, 1, 0x80, 1, 1, 1, 1, 1, 1, 1];
const COMPACT_ID: &str = "01010101010140018001010101010101";
const CHUNK_BYTES: usize = PLAIN_CHUNK_SIZE as usize;
const AUDIO_START: usize = CHUNK_BYTES - 10;
// CRC-valid frames for 16 samples and a 3-sample tail, mono 8-bit at 44.1 kHz.
const AUDIO: &[u8] = &[
    255, 248, 105, 2, 0, 15, 183, 0, 42, 122, 221, 255, 248, 105, 2, 1, 2, 129, 0, 42, 157, 29,
];

struct Body<'a>(&'a [u8]);

impl ReadbackBody for Body<'_> {
    async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
        let n = dst.len().min(self.0.len());
        dst[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        Ok(n)
    }
}

async fn readback(
    plaintext: &[u8],
    collect_seek: bool,
) -> Result<Option<FlacSeekIndex>, EnrolledReadbackError> {
    let mut enrollment = DigestEnrollment::new(plaintext.len() as u64, [ID; 5], ID).unwrap();
    let context = EnvelopeContext {
        identities: [ID; 6],
        plaintext_len: plaintext.len() as u64,
        header: enrollment.header().to_bytes(),
    };
    let root = [9; 32];
    let record = wrap_scheme2(&root, &context, enrollment.key().expose_secret()).unwrap();
    let mut media = context.header.to_vec();
    let mut page = None;
    for part in plaintext.chunks(CHUNK_BYTES) {
        let chunk = enrollment.seal_chunk(part).unwrap();
        media.extend_from_slice(&chunk.bytes);
        page = chunk.page;
    }
    let page = page.unwrap();
    let manifest = enrollment.finish().ok().unwrap();
    let media_key = format!("media/{COMPACT_ID}/{COMPACT_ID}");
    let page_key = format!("media-digests/{COMPACT_ID}/{COMPACT_ID}/{COMPACT_ID}/00000000");
    let pin = |object_key| ReadbackPin {
        configuration: ID,
        object_key,
        version: Some("fixture"),
    };
    verify_enrolled_readback_with_seek(
        &manifest,
        EnrolledEnvelope {
            authorized_root: &root,
            context: &context,
            trusted_record: &record,
            retained_record: &record,
        },
        &CompletedInventory {
            identities: [ID; 5],
            map_id: ID,
            plaintext_len: plaintext.len() as u64,
            media_version: "fixture",
            page_versions: &["fixture"],
        },
        pin(&media_key),
        &[(0, pin(&page_key))],
        async |request| {
            let body = match request.object_key {
                key if key == media_key => media.as_slice(),
                key if key == page_key => page.as_slice(),
                _ => panic!("unexpected fixture request"),
            };
            Ok(ReadbackResponse {
                configuration: ID,
                object_key: request.object_key.into(),
                version: Some("fixture".into()),
                status: 200,
                content_length: Some(body.len() as u64),
                has_content_range: false,
                has_content_encoding: false,
                body: Body(body),
            })
        },
        collect_seek,
    )
    .await
}

fn flac(format: u64, min_frame: u8, max_frame: u8) -> Vec<u8> {
    let mut bytes = b"fLaC\0\0\0\x22\0\x10\0\x10\0\0\0\0\0\0".to_vec();
    bytes[14] = min_frame;
    bytes[17] = max_frame;
    bytes.extend_from_slice(&format.to_be_bytes());
    bytes.extend_from_slice(&[0; 16]);
    let padding = (AUDIO_START - bytes.len() - 4) as u32;
    bytes.extend_from_slice(&(0x8100_0000 | padding).to_be_bytes());
    bytes.resize(AUDIO_START, 0);
    bytes.extend_from_slice(AUDIO);
    bytes
}

const FORMAT: u64 = (44_100_u64 << 44) | (7 << 36) | 19;
const INVALID: EnrolledReadbackError =
    EnrolledReadbackError::Body(BodyReadbackError::Content(ReadbackError::MediaValidation));

#[tokio::test]
async fn refuses_frame_formats_that_disagree_with_streaminfo() {
    for (field, changed) in [
        ("sample rate", FORMAT ^ (1 << 44)),
        ("channels", FORMAT | (1 << 41)),
        ("bit depth", FORMAT ^ (1 << 36)),
    ] {
        let wire = flac(changed, 0, 0);
        for collect_seek in [false, true] {
            assert_eq!(
                readback(&wire, collect_seek).await.err(),
                Some(INVALID),
                "{field}"
            );
        }
    }
}

#[tokio::test]
async fn enforces_frame_byte_bounds_across_an_authenticated_chunk_cut() {
    // The cut splits the first frame's CRC bytes. Both frames are 11 bytes.
    for (min, max) in [(0, 0), (0, 11), (11, 0), (11, 11)] {
        let wire = flac(FORMAT, min, max);
        let seek = readback(&wire, true).await.unwrap().unwrap();
        assert_eq!(seek.duration_ms(), Some(1));
        assert_eq!(
            seek.plaintext_range(0..1),
            Some(AUDIO_START as u64..wire.len() as u64)
        );
    }
    for (min, max) in [(0, 10), (12, 0), (12, 11)] {
        let wire = flac(FORMAT, min, max);
        for collect_seek in [false, true] {
            assert_eq!(
                readback(&wire, collect_seek).await.err(),
                Some(INVALID),
                "{min}..={max}"
            );
        }
    }
}
