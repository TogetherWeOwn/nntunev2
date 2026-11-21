//! Bounded content verification against an existing sealer-origin manifest.
//!
//! The frame verifier accepts externally framed bytes. The pinned adapter
//! additionally checks full responses and EOF against an existing inventory.
//! The private enrolled path also validates FLAC, but neither path establishes
//! inventory authority, custody, or eligibility.

use crate::digest_enrollment::OriginManifest;
use crate::digest_range::{DigestRange, RangeError};
use crate::flac_stream::FlacStream;
use crate::media::FileKey;
use crate::media_envelope::{enrolled_scheme2_key, EnvelopeContext};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, future::Future, ops::AsyncFnMut};

#[derive(Debug, PartialEq, Eq)]
pub enum ReadbackError {
    Frame(RangeError),
    Incomplete,
    Failed,
    PlaintextDigest,
    MediaValidation,
}

// Private to this module: a decoder may inspect authenticated frames, but
// must not expose plaintext or commit any result while verification is pending.
trait MediaValidation {
    fn push(&mut self, plaintext: &[u8]) -> Result<(), ()>;
    fn finish(self) -> Result<(), ()>;
}

impl MediaValidation for () {
    fn push(&mut self, _: &[u8]) -> Result<(), ()> {
        Ok(())
    }

    fn finish(self) -> Result<(), ()> {
        Ok(())
    }
}

impl MediaValidation for FlacStream {
    fn push(&mut self, plaintext: &[u8]) -> Result<(), ()> {
        FlacStream::push(self, plaintext)
    }

    fn finish(self) -> Result<(), ()> {
        self.finish_at_eof()
    }
}

/// Authenticates each frame before hashing plaintext and releases no plaintext.
/// Retains no chunk buffers between calls; the incremental hasher may retain
/// a partial plaintext block and does not guarantee erasure. Do not log state.
pub struct DigestReadback<'a> {
    range: DigestRange<'a>,
    expected: &'a [u8; 32],
    hash: Option<Sha256>,
}

impl<'a> DigestReadback<'a> {
    pub fn new(manifest: &'a OriginManifest, key: &'a FileKey) -> Result<Self, ReadbackError> {
        let range = DigestRange::new(manifest, key, 0, manifest.geometry().plaintext_len())
            .map_err(ReadbackError::Frame)?;
        Ok(Self {
            range,
            expected: manifest.plaintext_sha256(),
            hash: Some(Sha256::new()),
        })
    }

    /// Verify one complete sealed chunk against its enrolled page. Supply the
    /// exact wire header only for index zero. Any refusal poisons the readback.
    pub fn push(
        &mut self,
        index: u32,
        page: &[u8],
        sealed: Vec<u8>,
        wire_header: Option<&[u8]>,
    ) -> Result<(), ReadbackError> {
        self.push_validating(index, page, sealed, wire_header, &mut ())
    }

    fn push_validating(
        &mut self,
        index: u32,
        page: &[u8],
        sealed: Vec<u8>,
        wire_header: Option<&[u8]>,
        validation: &mut impl MediaValidation,
    ) -> Result<(), ReadbackError> {
        let mut hash = self.hash.take().ok_or(ReadbackError::Failed)?;
        if (index == 0) != wire_header.is_some() {
            return Err(ReadbackError::Frame(RangeError::Header));
        }
        let part = self
            .range
            .open_next(index, page, sealed, wire_header)
            .map_err(ReadbackError::Frame)?;
        let plaintext = part.as_slice();
        hash.update(plaintext);
        validation
            .push(plaintext)
            .map_err(|()| ReadbackError::MediaValidation)?;
        self.hash = Some(hash);
        Ok(())
    }

    /// Require every expected frame and the original whole-plaintext digest.
    /// This does not prove EOF; the transport must also reject surplus data.
    pub fn finish(self) -> Result<(), ReadbackError> {
        let hash = self.hash.ok_or(ReadbackError::Failed)?;
        if self.range.next_indices().is_some() {
            return Err(ReadbackError::Incomplete);
        }
        if <[u8; 32]>::from(hash.finalize()) != *self.expected {
            return Err(ReadbackError::PlaintextDigest);
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BodyReadbackError {
    Inventory,
    Response,
    Transport,
    Truncated,
    Trailing,
    Geometry,
    Content(ReadbackError),
}

pub trait ReadbackBody {
    /// Fill up to `dst.len()` bytes. With nonempty `dst`, `Ok(0)` must mean
    /// clean EOF of this entire object body, not a temporary lack of bytes,
    /// a declared-length cap, or failed HTTP framing. Return errors as errors.
    fn read(&mut self, dst: &mut [u8]) -> impl Future<Output = std::io::Result<usize>>;
}

async fn read_exact(
    body: &mut impl ReadbackBody,
    mut dst: &mut [u8],
) -> Result<(), BodyReadbackError> {
    while !dst.is_empty() {
        let n = body
            .read(dst)
            .await
            .map_err(|_| BodyReadbackError::Transport)?;
        if n == 0 {
            return Err(BodyReadbackError::Truncated);
        }
        dst = dst.get_mut(n..).ok_or(BodyReadbackError::Transport)?;
    }
    Ok(())
}

async fn require_eof(body: &mut impl ReadbackBody) -> Result<(), BodyReadbackError> {
    match body.read(&mut [0]).await {
        Ok(0) => Ok(()),
        Ok(1) => Err(BodyReadbackError::Trailing),
        _ => Err(BodyReadbackError::Transport),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ReadbackPin<'a> {
    pub configuration: [u8; 16],
    pub object_key: &'a str,
    /// `None` is only for a qualified immutable, unversioned profile.
    pub version: Option<&'a str>,
}

pub struct ReadbackResponse<B> {
    /// Configuration and key actually used for the request.
    pub configuration: [u8; 16],
    pub object_key: String,
    /// Version actually observed in the response, not the requested version.
    pub version: Option<String>,
    pub status: u16,
    pub content_length: Option<u64>,
    pub has_content_range: bool,
    pub has_content_encoding: bool,
    pub body: B,
}

async fn open_pinned<'a, B>(
    open: &mut impl AsyncFnMut(ReadbackPin<'a>) -> std::io::Result<ReadbackResponse<B>>,
    pin: ReadbackPin<'a>,
    length: u64,
) -> Result<B, BodyReadbackError> {
    let response = open(pin).await.map_err(|_| BodyReadbackError::Transport)?;
    if response.configuration != pin.configuration
        || response.object_key != pin.object_key
        || response.version.as_deref() != pin.version
        || response.status != 200
        || response.content_length != Some(length)
        || response.has_content_range
        || response.has_content_encoding
    {
        return Err(BodyReadbackError::Response);
    }
    Ok(response.body)
}

/// Check a complete ordered locator inventory against the origin manifest,
/// then verify full responses without releasing plaintext. The caller must
/// supply the full inventory from one authorized immutable snapshot, with
/// canonical keys and a qualified provider profile. The opener must use the
/// requested pin with deadlines, no redirects or decoding, and reject
/// ambiguous metadata. This does not list storage or issue a receipt.
pub async fn verify_pinned_readback<'a, B: ReadbackBody>(
    manifest: &OriginManifest,
    key: &FileKey,
    identities: &[[u8; 16]; 5],
    media: ReadbackPin<'a>,
    pages: &[(u32, ReadbackPin<'a>)],
    open: impl AsyncFnMut(ReadbackPin<'a>) -> std::io::Result<ReadbackResponse<B>>,
) -> Result<(), BodyReadbackError> {
    verify_readback(manifest, key, identities, media, pages, open, ()).await
}

struct EnrolledEnvelope<'a> {
    authorized_root: &'a [u8; 32],
    context: &'a EnvelopeContext,
    trusted_record: &'a [u8; 230],
    retained_record: &'a [u8],
}

/// Complete version inventory from the trusted upload path, captured before
/// readback. Page version at offset i belongs to canonical page i, not a
/// provider listing position. The caller must establish custody independently;
/// neither a GET response nor an inactive inventory row establishes it.
#[derive(Clone, Copy)]
struct CompletedInventory<'a> {
    identities: [[u8; 16]; 5],
    map_id: [u8; 16],
    plaintext_len: u64,
    media_version: &'a str,
    page_versions: &'a [&'a str],
}

fn matches_version(pin: ReadbackPin<'_>, expected: &str) -> bool {
    pin.version == Some(expected)
        && expected != "null"
        && (1..=1024).contains(&expected.len())
        && expected.bytes().all(|byte| byte.is_ascii_graphic())
}

#[derive(Debug, PartialEq, Eq)]
enum EnrolledReadbackError {
    Envelope,
    Body(BodyReadbackError),
}

fn compact_uuid(id: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    id.iter()
        .flat_map(|&byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 0xf)]),
            ]
        })
        .collect()
}

/// Bind the requested pins to the independently authorized, versioned
/// completion inventory. Pins use logical keys relative to the trusted prefix.
/// No caller-supplied file key or fallback is accepted on this path.
/// Validates FLAC on the authenticated slices used for the plaintext digest,
/// finalizing it only after body EOF and digest agreement. Success authorizes
/// neither publication nor reuse.
#[allow(dead_code)] // Not mounted; the caller must supply a trusted snapshot.
async fn verify_enrolled_readback<'a, B: ReadbackBody>(
    manifest: &OriginManifest,
    envelope: EnrolledEnvelope<'_>,
    completed: &CompletedInventory<'_>,
    media: ReadbackPin<'a>,
    pages: &[(u32, ReadbackPin<'a>)],
    open: impl AsyncFnMut(ReadbackPin<'a>) -> std::io::Result<ReadbackResponse<B>>,
) -> Result<(), EnrolledReadbackError> {
    let key = enrolled_scheme2_key(
        envelope.authorized_root,
        envelope.context,
        envelope.trusted_record,
        envelope.retained_record,
        manifest,
    )
    .map_err(|_| EnrolledReadbackError::Envelope)?;
    let [_, environment, library, object, generation, provider] = envelope.context.identities;
    let identities = [environment, library, object, generation, provider];
    let lifecycle = format!("{}/{}", compact_uuid(&object), compact_uuid(&generation));
    let map_prefix = format!(
        "media-digests/{lifecycle}/{}",
        compact_uuid(manifest.map_id())
    );
    if completed.identities != identities
        || &completed.map_id != manifest.map_id()
        || completed.plaintext_len != manifest.geometry().plaintext_len()
        || pages.len() != manifest.geometry().pages() as usize
        || pages.len() != completed.page_versions.len()
        || media.object_key != format!("media/{lifecycle}")
        || !matches_version(media, completed.media_version)
        || !pages
            .iter()
            .zip(completed.page_versions)
            .all(|(&(index, pin), &version)| {
                pin.object_key == format!("{map_prefix}/{index:08x}")
                    && matches_version(pin, version)
            })
    {
        return Err(EnrolledReadbackError::Body(BodyReadbackError::Inventory));
    }
    verify_readback(
        manifest,
        &key,
        &identities,
        media,
        pages,
        open,
        FlacStream::default(),
    )
    .await
    .map_err(EnrolledReadbackError::Body)
}

async fn verify_readback<'a, B: ReadbackBody>(
    manifest: &OriginManifest,
    key: &FileKey,
    identities: &[[u8; 16]; 5],
    media: ReadbackPin<'a>,
    pages: &[(u32, ReadbackPin<'a>)],
    mut open: impl AsyncFnMut(ReadbackPin<'a>) -> std::io::Result<ReadbackResponse<B>>,
    mut validation: impl MediaValidation,
) -> Result<(), BodyReadbackError> {
    let geometry = manifest.geometry();
    let indices = pages.iter().map(|&(index, _)| index);
    let origin = manifest.candidate_page_hashes(
        identities,
        geometry.plaintext_len(),
        manifest.map_id(),
        manifest.header_bytes(),
    );
    if indices.ne(0..geometry.pages()) || origin.is_none() {
        return Err(BodyReadbackError::Inventory);
    }
    let mut keys = HashSet::with_capacity(pages.len() + 1);
    let mut pins = std::iter::once(media).chain(pages.iter().map(|&(_, pin)| pin));
    if !pins.all(|pin| {
        pin.configuration == identities[4]
            && !pin.object_key.is_empty()
            && keys.insert(pin.object_key)
            && pin.version.is_some() == media.version.is_some()
            && !matches!(pin.version, Some("" | "null"))
    }) {
        return Err(BodyReadbackError::Inventory);
    }
    drop(keys);
    let mut readback = DigestReadback::new(manifest, key).map_err(BodyReadbackError::Content)?;
    let mut media = open_pinned(&mut open, media, geometry.sealed_len()).await?;
    let mut header = [0; 48];
    read_exact(&mut media, &mut header).await?;
    for &(index, pin) in pages {
        let page = geometry.page(index).ok_or(BodyReadbackError::Geometry)?;
        let mut bytes = vec![0; page.encoded_len as usize];
        let mut body = open_pinned(&mut open, pin, u64::from(page.encoded_len)).await?;
        read_exact(&mut body, &mut bytes).await?;
        require_eof(&mut body).await?;
        drop(body);
        for chunk in page.first..page.first + page.count {
            let (len, _) = geometry.chunk(chunk).ok_or(BodyReadbackError::Geometry)?;
            let mut sealed = vec![0; len as usize];
            read_exact(&mut media, &mut sealed).await?;
            readback
                .push_validating(
                    chunk,
                    &bytes,
                    sealed,
                    (chunk == 0).then_some(header.as_slice()),
                    &mut validation,
                )
                .map_err(BodyReadbackError::Content)?;
        }
    }
    require_eof(&mut media).await?;
    readback.finish().map_err(BodyReadbackError::Content)?;
    validation
        .finish()
        .map_err(|()| BodyReadbackError::Content(ReadbackError::MediaValidation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest_test_fixture::{fixture, fixture_with_identities, id, Fixture};

    struct Body<'a>(&'a [u8], bool);

    impl ReadbackBody for Body<'_> {
        async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() && self.1 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            // Deliberately split header, ciphertext, and page across reads.
            let n = dst.len().min(self.0.len()).min(11);
            dst[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    fn pin(object_key: &str) -> ReadbackPin<'_> {
        ReadbackPin {
            configuration: id(1),
            object_key,
            version: Some("original"),
        }
    }

    async fn bodies(f: &Fixture, media: Body<'_>, page: Body<'_>) -> Result<(), BodyReadbackError> {
        validate_bodies(f, media, page, ()).await
    }

    async fn validate_bodies(
        f: &Fixture,
        media: Body<'_>,
        page: Body<'_>,
        validation: impl MediaValidation,
    ) -> Result<(), BodyReadbackError> {
        verify_readback(
            &f.manifest,
            &f.key,
            &[id(1); 5],
            pin("media"),
            &[(0, pin("page"))],
            async |request: ReadbackPin<'_>| {
                let (key, body, length) = match request.object_key {
                    "media" => ("media", &media, f.manifest.geometry().sealed_len()),
                    "page" => ("page", &page, f.pages[0].len() as u64),
                    _ => panic!("unexpected synthetic request"),
                };
                assert!(request == pin(key));
                Ok(ReadbackResponse {
                    configuration: id(1),
                    object_key: key.into(),
                    version: Some("original".into()),
                    status: 200,
                    content_length: Some(length),
                    has_content_range: false,
                    has_content_encoding: false,
                    body: Body(body.0, body.1),
                })
            },
            validation,
        )
        .await
    }

    const MEDIA: &str = "media/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd";
    const PAGE: &str = concat!(
        "media-digests/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd/",
        "02020202020240028002020202020202/00000000",
    );
    const ROOT: [u8; 32] = [2; 32];
    const MEDIA_VERSION: &str = "media-version";
    const PAGE_VERSION: &str = "page-version";

    fn completed_inventory(context: &EnvelopeContext) -> CompletedInventory<'static> {
        let [_, identities @ ..] = context.identities;
        CompletedInventory {
            identities,
            map_id: id(2),
            plaintext_len: context.plaintext_len,
            media_version: MEDIA_VERSION,
            page_versions: &[PAGE_VERSION],
        }
    }

    fn versioned_pin<'a>(object_key: &'a str, version: &'static str) -> ReadbackPin<'a> {
        ReadbackPin {
            version: Some(version),
            ..pin(object_key)
        }
    }

    fn split_flac() -> Vec<u8> {
        // Mono, 8-bit, 19 constant samples at 44.1 kHz. Padding positions
        // the first frame header across the authenticated chunk boundary.
        let mut bytes = b"fLaC\0\0\0\x22\0\x10\0\x10\0\0\0\0\0\0".to_vec();
        bytes.extend_from_slice(&((44_100_u64 << 44) | (7 << 36) | 19).to_be_bytes());
        bytes.extend_from_slice(&0x46d2be275ed2dd86ab2b5388f7346d15_u128.to_be_bytes());
        let frame_start = 65_536 - 5;
        let padding = (frame_start - bytes.len() - 4) as u32;
        bytes.extend_from_slice(&(0x8100_0000 | padding).to_be_bytes());
        bytes.resize(frame_start, 0);
        bytes.extend_from_slice(&[255, 248, 105, 2, 0, 15, 183, 0, 42, 122, 221]);
        bytes.extend_from_slice(&[255, 248, 105, 2, 1, 2, 129, 0, 42, 157, 29]);
        bytes
    }

    fn enrolled_fixture(plaintext: &[u8]) -> (Fixture, Vec<u8>, EnvelopeContext, [u8; 230]) {
        let f = fixture_with_identities(plaintext, [id(6), id(7), id(0xab), id(0xcd), id(1)]);
        assert_eq!(f.chunks.len(), 2);
        let mut media = f.header.to_vec();
        media.extend(f.chunks.iter().flatten().copied());
        let context = EnvelopeContext {
            identities: [id(9), id(6), id(7), id(0xab), id(0xcd), id(1)],
            plaintext_len: plaintext.len() as u64,
            header: f.header,
        };
        let record =
            crate::media_envelope::wrap_scheme2(&ROOT, &context, f.key.expose_secret()).unwrap();
        (f, media, context, record)
    }

    fn enrolled_response<'a>(
        request: ReadbackPin<'_>,
        media: &'a [u8],
        page: &'a [u8],
        media_len: u64,
    ) -> ReadbackResponse<Body<'a>> {
        let (bytes, length, version) = match request.object_key {
            MEDIA => (media, media_len, MEDIA_VERSION),
            PAGE => (page, page.len() as u64, PAGE_VERSION),
            _ => panic!("unexpected synthetic request"),
        };
        ReadbackResponse {
            configuration: id(1),
            object_key: request.object_key.into(),
            version: Some(version.into()),
            status: 200,
            content_length: Some(length),
            has_content_range: false,
            has_content_encoding: false,
            body: Body(bytes, false),
        }
    }

    #[tokio::test]
    async fn enrolled_readback_requires_the_exact_authenticated_envelope() {
        let (f, media_bytes, context, record) = enrolled_fixture(&split_flac());
        let mut altered = record;
        altered[229] ^= 1;
        let bad_object = MEDIA.replacen("ab", "AB", 1);
        let bad_generation = MEDIA.replacen("cd", "ab", 1);
        let bad_map = PAGE.replacen("/02", "/03", 1);
        let bad_index = PAGE.replacen("/00000000", "/00000001", 1);
        let envelope = || Err(EnrolledReadbackError::Envelope);
        let inventory = || Err(EnrolledReadbackError::Body(BodyReadbackError::Inventory));
        for (root, trusted, retained, media, page, expected) in [
            (&ROOT, &record, &record, MEDIA, PAGE, Ok(())),
            (&ROOT, &record, &altered, MEDIA, PAGE, envelope()),
            (&ROOT, &altered, &altered, MEDIA, PAGE, envelope()),
            (&[3; 32], &record, &record, MEDIA, PAGE, envelope()),
            (
                &ROOT,
                &record,
                &record,
                bad_object.as_str(),
                PAGE,
                inventory(),
            ),
            (
                &ROOT,
                &record,
                &record,
                bad_generation.as_str(),
                PAGE,
                inventory(),
            ),
            (
                &ROOT,
                &record,
                &record,
                MEDIA,
                bad_map.as_str(),
                inventory(),
            ),
            (
                &ROOT,
                &record,
                &record,
                MEDIA,
                bad_index.as_str(),
                inventory(),
            ),
        ] {
            let mut opens = 0;
            let result = verify_enrolled_readback(
                &f.manifest,
                EnrolledEnvelope {
                    authorized_root: root,
                    context: &context,
                    trusted_record: trusted,
                    retained_record: retained,
                },
                &completed_inventory(&context),
                versioned_pin(media, MEDIA_VERSION),
                &[(0, versioned_pin(page, PAGE_VERSION))],
                async |request: ReadbackPin<'_>| {
                    opens += 1;
                    Ok(enrolled_response(
                        request,
                        &media_bytes,
                        &f.pages[0],
                        f.manifest.geometry().sealed_len(),
                    ))
                },
            )
            .await;
            assert_eq!(opens, if expected.is_ok() { 2 } else { 0 });
            assert_eq!(result, expected);
        }
    }

    #[tokio::test]
    async fn enrolled_readback_refuses_pins_outside_completed_inventory_before_io() {
        let (f, _, context, record) = enrolled_fixture(&split_flac());
        let completed = completed_inventory(&context);
        let media = versioned_pin(MEDIA, MEDIA_VERSION);
        let page = versioned_pin(PAGE, PAGE_VERSION);
        let wrong_identity = (0..completed.identities.len()).map(|field| {
            let mut inventory = completed;
            inventory.identities[field] = id(3);
            inventory
        });
        let wrong_inventory = [
            CompletedInventory {
                map_id: id(3),
                ..completed
            },
            CompletedInventory {
                plaintext_len: completed.plaintext_len + 1,
                ..completed
            },
            CompletedInventory {
                page_versions: &[],
                ..completed
            },
            CompletedInventory {
                page_versions: &[PAGE_VERSION, "extra"],
                ..completed
            },
        ];
        let wrong_pins = [
            (completed, versioned_pin(MEDIA, PAGE_VERSION), page),
            (completed, media, versioned_pin(PAGE, MEDIA_VERSION)),
            (
                completed,
                ReadbackPin {
                    version: None,
                    ..media
                },
                page,
            ),
            (
                CompletedInventory {
                    media_version: "null",
                    ..completed
                },
                versioned_pin(MEDIA, "null"),
                page,
            ),
        ];
        let cases = wrong_identity
            .chain(wrong_inventory)
            .map(|inventory| (inventory, media, page))
            .chain(wrong_pins);
        for (inventory, media, page) in cases {
            let result = verify_enrolled_readback::<Body<'_>>(
                &f.manifest,
                EnrolledEnvelope {
                    authorized_root: &ROOT,
                    context: &context,
                    trusted_record: &record,
                    retained_record: &record,
                },
                &inventory,
                media,
                &[(0, page)],
                async |_| panic!("inventory mismatch opened an object"),
            )
            .await;
            assert_eq!(
                result,
                Err(EnrolledReadbackError::Body(BodyReadbackError::Inventory))
            );
        }
    }

    #[tokio::test]
    async fn enrolled_readback_requires_valid_flac_and_clean_eof() {
        let valid = split_flac();
        let mut wrong_checksum = valid.clone();
        wrong_checksum[26] ^= 1;
        let invalid = || {
            Err(EnrolledReadbackError::Body(BodyReadbackError::Content(
                ReadbackError::MediaValidation,
            )))
        };
        let trailing = Err(EnrolledReadbackError::Body(BodyReadbackError::Trailing));
        for (plaintext, extra, expected) in [
            (valid.as_slice(), false, Ok(())),
            (&valid[..valid.len() - 1], false, invalid()),
            (wrong_checksum.as_slice(), false, invalid()),
            (wrong_checksum.as_slice(), true, trailing),
        ] {
            let (f, mut media, context, record) = enrolled_fixture(plaintext);
            if extra {
                media.push(0);
            }
            let result = verify_enrolled_readback(
                &f.manifest,
                EnrolledEnvelope {
                    authorized_root: &ROOT,
                    context: &context,
                    trusted_record: &record,
                    retained_record: &record,
                },
                &completed_inventory(&context),
                versioned_pin(MEDIA, MEDIA_VERSION),
                &[(0, versioned_pin(PAGE, PAGE_VERSION))],
                async |request| {
                    Ok(enrolled_response(
                        request,
                        &media,
                        &f.pages[0],
                        f.manifest.geometry().sealed_len(),
                    ))
                },
            )
            .await;
            assert_eq!(result, expected);
        }
    }

    #[tokio::test]
    async fn validation_observes_only_authenticated_frames_and_finishes_after_eof() {
        struct Probe<'a> {
            remaining: &'a [u8],
            chunks: usize,
            finished: bool,
            refuse_chunk: bool,
            refuse_finish: bool,
        }

        impl MediaValidation for &mut Probe<'_> {
            fn push(&mut self, plaintext: &[u8]) -> Result<(), ()> {
                let (expected, rest) = self.remaining.split_at(plaintext.len());
                assert_eq!(plaintext, expected);
                self.remaining = rest;
                self.chunks += 1;
                if self.refuse_chunk {
                    Err(())
                } else {
                    Ok(())
                }
            }

            fn finish(self) -> Result<(), ()> {
                assert!(self.remaining.is_empty());
                self.finished = true;
                if self.refuse_finish {
                    Err(())
                } else {
                    Ok(())
                }
            }
        }

        let mut plaintext = vec![5; 65_537];
        plaintext[65_536] = 8;
        let f = fixture(&plaintext);
        let mut media = f.header.to_vec();
        media.extend(f.chunks.iter().flatten().copied());
        let mut corrupt = media.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        let mut trailing = media.clone();
        trailing.push(0);
        let invalid = BodyReadbackError::Content(ReadbackError::MediaValidation);
        for (wire, refuse_chunk, refuse_finish, chunks, finished, outcome) in [
            (&media, false, false, 2, true, Ok(())),
            (
                &media,
                true,
                false,
                1,
                false,
                Err(BodyReadbackError::Content(ReadbackError::MediaValidation)),
            ),
            (&media, false, true, 2, true, Err(invalid)),
            (
                &trailing,
                false,
                false,
                2,
                false,
                Err(BodyReadbackError::Trailing),
            ),
            (
                &corrupt,
                false,
                false,
                1,
                false,
                Err(BodyReadbackError::Content(ReadbackError::Frame(
                    RangeError::DigestMismatch,
                ))),
            ),
        ] {
            let mut probe = Probe {
                remaining: &plaintext,
                chunks: 0,
                finished: false,
                refuse_chunk,
                refuse_finish,
            };
            assert_eq!(
                validate_bodies(&f, Body(wire, false), Body(&f.pages[0], false), &mut probe).await,
                outcome
            );
            assert_eq!((probe.chunks, probe.finished), (chunks, finished));
        }
    }

    #[tokio::test]
    async fn invalid_inventory_is_refused_before_opening_any_body() {
        let f = fixture(b"abc");
        let media = pin("media");
        let mut wrong_config = pin("page");
        wrong_config.configuration = id(2);
        let mut unversioned = pin("page");
        unversioned.version = None;
        let mut null_version = pin("page");
        null_version.version = Some("null");
        let mut wrong_identity = [id(1); 5];
        wrong_identity[0] = id(2);
        for (identities, pages) in [
            ([id(1); 5], vec![]),
            ([id(1); 5], vec![(0, pin("page")), (1, pin("extra"))]),
            ([id(1); 5], vec![(1, pin("page"))]),
            ([id(1); 5], vec![(0, media)]),
            ([id(1); 5], vec![(0, wrong_config)]),
            ([id(1); 5], vec![(0, unversioned)]),
            ([id(1); 5], vec![(0, null_version)]),
            (wrong_identity, vec![(0, pin("page"))]),
        ] {
            let open = async |_| -> std::io::Result<ReadbackResponse<Body<'static>>> {
                panic!("invalid inventory opened a body")
            };
            assert_eq!(
                verify_pinned_readback(&f.manifest, &f.key, &identities, media, &pages, open).await,
                Err(BodyReadbackError::Inventory)
            );
        }
    }

    #[tokio::test]
    async fn pinned_open_rejects_mismatched_response_metadata() {
        for (status, length, version, key, configuration, range, encoding) in [
            (206, Some(4), Some("original"), "page", id(1), false, false),
            (200, Some(5), Some("original"), "page", id(1), false, false),
            (200, None, Some("original"), "page", id(1), false, false),
            (200, Some(4), None, "page", id(1), false, false),
            (200, Some(4), Some("other"), "page", id(1), false, false),
            (200, Some(4), Some("original"), "other", id(1), false, false),
            (200, Some(4), Some("original"), "page", id(2), false, false),
            (200, Some(4), Some("original"), "page", id(1), true, false),
            (200, Some(4), Some("original"), "page", id(1), false, true),
        ] {
            let mut open = async |_| {
                Ok(ReadbackResponse {
                    configuration,
                    object_key: key.into(),
                    version: version.map(str::to_owned),
                    status,
                    content_length: length,
                    has_content_range: range,
                    has_content_encoding: encoding,
                    body: (),
                })
            };
            let result = open_pinned(&mut open, pin("page"), 4).await;
            assert_eq!(result, Err(BodyReadbackError::Response));
        }
    }

    #[tokio::test]
    async fn readback_bodies_require_clean_exact_eof() {
        let f = fixture(&vec![5; 65_537]);
        let mut media = f.header.to_vec();
        media.extend(f.chunks.iter().flatten().copied());
        let page = &f.pages[0];
        assert_eq!(
            bodies(&f, Body(&media, false), Body(page, false)).await,
            Ok(())
        );
        for (m, p, expected) in [
            (
                Body(&media[..media.len() - 1], false),
                Body(page, false),
                BodyReadbackError::Truncated,
            ),
            (
                Body(&media, false),
                Body(&page[..page.len() - 1], false),
                BodyReadbackError::Truncated,
            ),
            (
                Body(&media, true),
                Body(page, false),
                BodyReadbackError::Transport,
            ),
            (
                Body(&media, false),
                Body(page, true),
                BodyReadbackError::Transport,
            ),
        ] {
            assert_eq!(bodies(&f, m, p).await, Err(expected));
        }
        let mut extra_page = page.clone();
        extra_page.push(0);
        assert_eq!(
            bodies(&f, Body(&media, false), Body(&extra_page, false)).await,
            Err(BodyReadbackError::Trailing)
        );
        media.push(0);
        assert_eq!(
            bodies(&f, Body(&media, false), Body(page, false)).await,
            Err(BodyReadbackError::Trailing)
        );
    }

    fn first(readback: &mut DigestReadback<'_>, f: &Fixture) {
        readback
            .push(0, &f.pages[0], f.chunks[0].clone(), Some(&f.header))
            .unwrap();
    }

    #[test]
    fn complete_readback_requires_every_expected_frame_and_original_digest() {
        let mut plaintext = vec![5; 65_537];
        plaintext[65_536] = 8;
        let f = fixture(&plaintext);
        let mut partial = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut partial, &f);
        assert_eq!(partial.finish(), Err(ReadbackError::Incomplete));

        let verify = |expected: Option<&[u8; 32]>| {
            let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
            if let Some(expected) = expected {
                readback.expected = expected;
            }
            first(&mut readback, &f);
            readback
                .push(1, &f.pages[0], f.chunks[1].clone(), None)
                .unwrap();
            readback.finish()
        };
        assert_eq!(verify(None), Ok(()));
        let mut wrong_digest = *f.manifest.plaintext_sha256();
        wrong_digest[0] ^= 1;
        assert_eq!(
            verify(Some(&wrong_digest)),
            Err(ReadbackError::PlaintextDigest)
        );
    }

    #[test]
    fn header_on_later_frame_poisons_readback() {
        let f = fixture(&vec![5; 65_537]);
        let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut readback, &f);
        assert_eq!(
            readback.push(1, &f.pages[0], f.chunks[1].clone(), Some(&f.header)),
            Err(ReadbackError::Frame(RangeError::Header))
        );
        assert_eq!(
            readback.push(1, &f.pages[0], f.chunks[1].clone(), None),
            Err(ReadbackError::Failed)
        );
        assert_eq!(readback.finish(), Err(ReadbackError::Failed));
    }

    #[test]
    fn refused_or_surplus_frame_poisons_readback() {
        let f = fixture(&vec![5; 65_537]);
        let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut readback, &f);
        let mut changed = f.chunks[1].clone();
        *changed.last_mut().unwrap() ^= 1;
        assert_eq!(
            readback.push(1, &f.pages[0], changed, None),
            Err(ReadbackError::Frame(RangeError::DigestMismatch))
        );
        assert_eq!(
            readback.push(1, &f.pages[0], f.chunks[1].clone(), None),
            Err(ReadbackError::Failed)
        );
        assert_eq!(readback.finish(), Err(ReadbackError::Failed));

        let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut readback, &f);
        readback
            .push(1, &f.pages[0], f.chunks[1].clone(), None)
            .unwrap();
        assert_eq!(
            readback.push(2, &f.pages[0], f.chunks[1].clone(), None),
            Err(ReadbackError::Frame(RangeError::InvalidChunk))
        );
        assert_eq!(readback.finish(), Err(ReadbackError::Failed));
    }
}
