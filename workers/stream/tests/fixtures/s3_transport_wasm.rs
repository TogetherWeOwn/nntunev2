//! Synthetic artifact only. The ordinary Worker build has no fixture exports.

use std::pin::pin;

use futures_util::future::{select, Either};
use nntune_core::{
    digest_enrollment::{DigestEnrollment, OriginManifest},
    digest_page::page_hash,
    digest_read_snapshot::DigestReadSnapshot,
    media_envelope::{wrap_scheme2, EnvelopeContext},
    playback_custody::CustodySnapshot,
    s3_locator::{S3Locator, S3MediaMetadata},
};
use worker::{
    wasm_bindgen::{self, prelude::*},
    Delay,
};

use super::*;
use crate::enrolled_playback::{EnrolledMedia, EnrolledPlayback};

const BUCKET: &str = "fixture-media";
const KEY: &str = "media/00000000000040008000000000000003/00000000000040008000000000000004";
const VERSION: &str = "version+/%?=&";
const PAGE_VERSION: &str = "page+/%?=&";
const LEN: u64 = 65_537;
const ROOT: [u8; 32] = [7; 32];

#[wasm_bindgen]
pub fn delivered_meter_window(body: &[u8], user: &str, now: u32) -> Result<String, JsValue> {
    use crate::{delivered::parse_delivered_report, meter_sink::MeterBook};

    let interval = parse_delivered_report(body, user)
        .map_err(|_| JsValue::from_str("invalid delivery report"))?;
    let mut meter = MeterBook::new(user);
    if !meter.record(&interval, u64::from(now)) {
        return Err(JsValue::from_str("delivery report refused by meter"));
    }
    serde_json::to_string(&meter.pending_windows())
        .map_err(|_| JsValue::from_str("invalid meter window"))
}

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
    let mut first_chunk = vec![b'A'; 65_536];
    first_chunk[65_534..].copy_from_slice(b"XY");
    let first = seal.seal_chunk(&first_chunk).unwrap();
    let last = seal.seal_chunk(b"Z").unwrap();
    PlaybackFixture {
        cipher: [context.header.as_slice(), &first.bytes, &last.bytes].concat(),
        page: last.page.unwrap(),
        manifest: seal.finish().unwrap(),
        context,
        envelope,
    }
}

#[wasm_bindgen(wasm_bindgen_futures = worker::wasm_bindgen_futures)]
impl PlaybackFixture {
    pub fn ciphertext(&self) -> Vec<u8> {
        self.cipher[self.cipher.len() - 17..].to_vec()
    }

    pub fn full_ciphertext(&self) -> Vec<u8> {
        self.cipher.clone()
    }

    pub fn digest_page(&self) -> Vec<u8> {
        self.page.clone()
    }

    pub fn custody(&self, bearer: &str, expires: u32) -> String {
        serde_json::to_string(&CustodySnapshot {
            bearer_sha256: sha256(bearer.as_bytes()),
            catalogue_reference: id(9),
            reference_id: id(9),
            identities: self.context.identities,
            map_id: id(6),
            plaintext_len: LEN,
            plaintext_sha256: *self.manifest.plaintext_sha256(),
            bucket: BUCKET.into(),
            key: KEY.into(),
            media_version: VERSION.into(),
            page_versions: vec![PAGE_VERSION.into()],
            usable_until_unix: u64::from(expires),
            header: self.context.header.to_vec(),
            envelope_record: self.envelope.to_vec(),
            page_hashes: vec![page_hash(&self.page).unwrap()],
        })
        .unwrap()
    }

    pub async fn read_digest_page(&self, endpoint: String) -> Result<Vec<u8>, JsValue> {
        let result = async {
            let reader = S3Reader::new(
                id(5),
                BUCKET.into(),
                &endpoint,
                "fixture-1".into(),
                "FixtureAccess".into(),
                Zeroizing::new("fixture-reader-signing-material".into()),
            )?;
            let lifecycle = KEY.strip_prefix("media/").unwrap();
            let key = format!("media-digests/{lifecycle}/{}/00000000", hex(&id(6)));
            let pin = ReadbackPin {
                configuration: id(5),
                object_key: &key,
                version: Some(PAGE_VERSION),
            };
            let (_, bytes) = reader
                .page(
                    BUCKET,
                    pin,
                    self.page.len() as u32,
                    now() + Duration::from_secs(5),
                )
                .await?;
            Ok::<_, io::Error>(bytes)
        }
        .await;
        result.map_err(|_| JsValue::from_str("storage read refused"))
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
        let alternate = match fault {
            "comma" => Some(S3Binding::VersionId("version,duplicate".into())),
            "etag" => Some(S3Binding::Etag("\"version\"".into())),
            _ => None,
        };
        let alternate =
            alternate.map(|binding| S3Locator::new(BUCKET.into(), KEY.into(), binding).unwrap());
        let locator = S3Locator::new(
            BUCKET.into(),
            KEY.into(),
            S3Binding::VersionId(VERSION.into()),
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
            media_version: VERSION,
            page_versions: &versions,
            usable_until: now() + Duration::from_millis(milliseconds.into()),
        };
        use nntune_core::digest_enrollment::{PlaybackManifest, StoredManifest};
        let [_, identities @ ..] = self.context.identities;
        let rows = self
            .manifest
            .candidate_page_hashes(
                &identities,
                pins.plaintext_len,
                &pins.map_id,
                &self.context.header,
            )
            .ok_or_else(refused)?;
        let manifest =
            StoredManifest::from_committed_storage(&pins, self.context.header, rows.to_vec())
                .ok_or_else(refused)?;
        let enrolled = EnrolledMedia {
            media: &media,
            manifest: PlaybackManifest::Stored(&manifest),
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
                if let Some(locator) = alternate.as_ref() {
                    get.locator = locator;
                }
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
        if fault == "drop-read" {
            let delay = pin!(Delay::from(Duration::from_millis(100)));
            let read = pin!(play.next((&versions[0], &self.page), now));
            return match select(delay, read).await {
                Either::Left(((), _)) => Ok(Vec::new()),
                Either::Right(_) => Err(refused()),
            };
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
