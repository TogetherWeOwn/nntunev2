//! Synthetic artifact only. The ordinary Worker build has no fixture exports.

use nntune_core::{
    digest_enrollment::{DigestEnrollment, OriginManifest},
    digest_read_snapshot::DigestReadSnapshot,
    media_envelope::{wrap_scheme2, EnvelopeContext},
    s3_locator::{S3Locator, S3MediaMetadata},
};
use worker::wasm_bindgen::{self, prelude::*};

use super::*;
use crate::enrolled_playback::{EnrolledMedia, EnrolledPlayback};

const BUCKET: &str = "fixture-media";
const KEY: &str = "media/00000000000040008000000000000003/00000000000040008000000000000004";
const VERSION: &str = "version+/%?=&";
const LEN: u64 = 65_537;
const ROOT: [u8; 32] = [7; 32];

fn id(n: u8) -> [u8; 16] {
    [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, n]
}

#[wasm_bindgen]
pub struct PlaybackFixture {
    cipher: Vec<u8>,
    page: Vec<u8>,
    manifest: OriginManifest,
    context: EnvelopeContext,
    envelope: [u8; 230],
}

#[wasm_bindgen]
pub fn make_s3_fixture() -> PlaybackFixture {
    let mut seal = DigestEnrollment::new(LEN, [id(1), id(2), id(3), id(4), id(5)], id(6)).unwrap();
    let context = EnvelopeContext {
        identities: [id(7), id(1), id(2), id(3), id(4), id(5)],
        plaintext_len: LEN,
        header: seal.header().to_bytes(),
    };
    let envelope = wrap_scheme2(&ROOT, &context, seal.key().expose_secret()).unwrap();
    seal.seal_chunk(&vec![b'A'; 65_536]).unwrap();
    let last = seal.seal_chunk(b"Z").unwrap();
    PlaybackFixture {
        cipher: last.bytes,
        page: last.page.unwrap(),
        manifest: seal.finish().unwrap(),
        context,
        envelope,
    }
}

#[wasm_bindgen]
impl PlaybackFixture {
    pub fn ciphertext(&self) -> Vec<u8> {
        self.cipher.clone()
    }

    pub async fn play(
        &self,
        endpoint: String,
        milliseconds: u32,
        fault: String,
    ) -> Result<Vec<u8>, JsValue> {
        self.play_inner(&endpoint, milliseconds, &fault)
            .await
            .map_err(|_| JsValue::from_str("storage read refused"))
    }
}

impl PlaybackFixture {
    async fn play_inner(
        &self,
        endpoint: &str,
        milliseconds: u32,
        fault: &str,
    ) -> io::Result<Vec<u8>> {
        let configuration = if fault == "provider" { id(8) } else { id(5) };
        let bucket = if fault == "bucket" {
            "other-media"
        } else {
            BUCKET
        };
        let reader = S3Reader::new(
            configuration,
            bucket.into(),
            endpoint,
            "fixture-1".into(),
            "FixtureAccess".into(),
            Zeroizing::new("fixture-reader-signing-material".into()),
        )?;
        let version = match fault {
            "comma" => "version,duplicate",
            _ => VERSION,
        };
        let locator = S3Locator::new(
            BUCKET.into(),
            KEY.into(),
            S3Binding::VersionId(version.into()),
        )
        .unwrap();
        let media = S3MediaMetadata::from_trusted_context(locator, &self.context).unwrap();
        let versions = ["page-v1".into()];
        let pins = DigestReadSnapshot {
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
            media_version: version,
            page_versions: &versions,
            usable_until: now() + Duration::from_millis(milliseconds.into()),
        };
        let enrolled = EnrolledMedia {
            media: &media,
            manifest: &self.manifest,
            context: &self.context,
            root: &ROOT,
            expected_record: &self.envelope,
            retained_record: &self.envelope,
        };
        let (_, play) = EnrolledPlayback::start(Some("bytes=-1"), pins, enrolled, now())
            .map_err(|_| refused())?;
        let mut play = play
            .ok_or_else(refused)?
            .open_s3(now, async |mut get| {
                match fault {
                    "stale" => get.usable_until = UNIX_EPOCH,
                    "range" => get.range.end = get.sealed_len + 1,
                    _ => {}
                }
                reader.open(get).await
            })
            .await
            .map_err(|_| refused())?;
        if fault == "drop-body" {
            return Ok(Vec::new());
        }
        let part = play
            .next((&versions[0], &self.page), now)
            .await
            .map_err(|_| refused())?
            .ok_or_else(refused)?;
        play.written(part.as_slice().len()).map_err(|_| refused())?;
        play.finish().map_err(|_| refused())?;
        Ok(part.as_slice().to_vec())
    }
}
